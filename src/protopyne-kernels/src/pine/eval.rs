//! Bar-by-bar Pine executor over the runtime driver.
//!
//! [`PineEngine`] runs one parsed program through
//! [`PineRuntime::run_over`](crate::runtime::PineRuntime::run_over): warmup
//! bars advance unseen, then every evaluated bar executes the whole program
//! against a fresh cursor. Regular variables persist across bars (Pine
//! series semantics) in engine state; `var`/`varip` persist through
//! [`VarStore`](crate::vars::VarStore) on the per-run context, so parallel
//! runs stay isolated.
//!
//! Covered subset: the reference runtime's statements and expressions
//! (`var`/assignment/reassignment, `if`/`for`/`while`, ternary, arithmetic,
//! comparisons, string concat, `close[1]`-style offsets over legs and over
//! prior variable values, `na`/`nz`/`fixnan`) with identical observable
//! numbers where the reference is correct; the v6 surface (`switch` over
//! enum and scalar scrutinees, `enum`/`const`/`type` declarations, user
//! functions and `method` dispatch with `this` receivers, tuple literals and
//! destructuring, multi-`plot` capture, `runtime.error`); and namespaced
//! bridges (`ta.*` through kernels, scalar/structural namespaces through the
//! core bridge). Deliberate corrections, each pinned by a test: boolean
//! positions refuse numbers (the reference treats nonzero as `true`); `na`
//! in boolean positions follows Pine exactly (failing `and` annihilates,
//! passing `or` annihilates, `na` ternary conditions yield `na`, `na`
//! `if`/`while` conditions skip); `for` bounds must be `int` (the reference
//! truncates floats); `var` initializes on the first *evaluated* bar
//! (warmup-aware); `var x = na` holds absence until assigned; a zero
//! denominator reads absence (`na`), never an abort, so shipped code that
//! divides zero-initialized fields before their first fetch propagates
//! through the existing `na`/`nz` guards. Unbridged namespaces and unknown
//! functions/methods still refuse with a reason.
//! The per-bar hot path stays on concrete `&[f64]` legs; no `dyn` per bar.

use std::collections::{HashMap, HashSet};

use protopyne_types::Bar;
use protopyne_types::Timeframe;

use super::syntax::{
    AssignOp, BinaryOp, BuiltinVar, CallArg, FieldDecl, Param, PineExpr, PineProgram, PineStmt,
    SwitchStmt, UnaryOp,
};
use crate::context::RunCtx;
use crate::error::KernelError;
use crate::inputs::InputSet;
use crate::market_context::MarketContextKey;
use crate::mtf::MtfStore;
use crate::native_libs::{
    BarContextServe, FetchCounters, LeaseKey, LeasedRing, MethodCtx, NativeOptions,
    TickLegs,
};
use crate::runtime::{PineRuntime, PineRuntimeConfig};
use crate::value::{ColorHandle, DrawingHandle, DrawingKind, EnumHandle, FieldTable, NativeCall, PineValue, UserInstance, nz};

/// Fail-closed bound for `for` total iterations and `while` iterations.
/// Breaching loops refuse instead of hanging the run.
pub const MAX_LOOP_ITERATIONS: usize = 10_000;

/// Fail-closed bound for nested user-function/method calls. The language
/// forbids recursion; the bound turns accidental cycles into reasons.
pub const MAX_CALL_DEPTH: usize = 64;

/// Float equality tolerance, mirroring the reference comparator exactly.
const EPSILON: f64 = f64::EPSILON;

/// One evaluated bar: cursor plus the merged variable namespace
/// (regular variables overlaid with persistent `var`/`varip` bindings).
#[derive(Debug, Clone)]
pub struct BarSnapshot {
    /// Data cursor of the evaluated bar.
    pub bar: usize,
    /// Merged namespace after the bar (`None` values are Pine `na`).
    pub values: HashMap<String, Option<PineValue>>,
}

/// Outcome of one engine run: evaluated-bar count plus per-bar snapshots.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    /// Bars evaluated (warmup-skipped bars excluded).
    pub evaluated: usize,
    /// One snapshot per evaluated bar, in order.
    pub snapshots: Vec<BarSnapshot>,
    /// Every `plot()` call site in first-seen order, with per-bar values.
    pub plots: Vec<PlotCapture>,
    /// Every `plotcandle()` call site in first-seen order, with per-bar legs.
    pub plotcandles: Vec<PlotCandleCapture>,
    /// Every `barcolor()` call site in first-seen order, with per-bar values.
    pub barcolors: Vec<BarColorCapture>,
    /// Every `fill()` call site in first-seen order, with per-bar regions.
    pub fills: Vec<FillCapture>,
    /// Every `plotshape()` call site in first-seen order, with per-bar values.
    pub plotshapes: Vec<PlotShapeCapture>,
    /// Every `hline()` call site in first-seen order, with per-bar values.
    pub hlines: Vec<HlineCapture>,
    /// Every drawing lifecycle event in execution order (created, updated,
    /// deleted with the object key and call arguments), rendering nothing.
    pub draw_events: Vec<DrawingEvent>,
}

/// One captured `plot()` series: title and display flag plus one value per
/// evaluated bar (`None` entries are Pine `na`).
#[derive(Debug, Clone)]
pub struct PlotCapture {
    /// Call-site key (textual arguments), distinguishing same-titled plots.
    site: String,
    /// The `title` argument, or a positional fallback.
    pub title: String,
    /// The last-seen `display` argument handle, when given.
    pub display: Option<PineValue>,
    /// One value per evaluated bar, in order.
    pub values: Vec<Option<PineValue>>,
}

impl RunOutcome {
    /// Read one cell of one evaluated bar: `None` when the bar or name is
    /// unknown, else the value (`None` inside is Pine `na`).
    #[must_use]
    pub fn value_at(&self, bar: usize, name: &str) -> Option<Option<PineValue>> {
        self.snapshots
            .get(bar)
            .and_then(|snapshot| snapshot.values.get(name))
            .cloned()
    }
}

/// One captured `plotshape()` marker: title plus one value per evaluated
/// bar (`None` entries are Pine `na`). Style/location/text arguments ride
/// the call but capture like `plot`'s color/linewidth: evaluated, then
/// dropped — the marker positions are the proof-relevant data.
#[derive(Debug, Clone)]
pub struct PlotShapeCapture {
    /// Call-site key (textual arguments), distinguishing same-titled shapes.
    site: String,
    /// The `title` argument, or a positional fallback.
    pub title: String,
    /// One value per evaluated bar, in order.
    pub values: Vec<Option<PineValue>>,
}

/// One captured `hline()` level: title plus one value per evaluated bar
/// (`None` entries are Pine `na`). Levels are usually constant, but the
/// per-bar recording matches every other capture.
#[derive(Debug, Clone)]
pub struct HlineCapture {
    /// Call-site key (textual arguments), distinguishing same-titled lines.
    site: String,
    /// The `title` argument, or a positional fallback.
    pub title: String,
    /// One value per evaluated bar, in order.
    pub values: Vec<Option<PineValue>>,
}

/// One captured `plotcandle()` series: four legs plus the last-seen display
/// handles, one entry per evaluated bar (`None` entries are Pine `na`).
#[derive(Debug, Clone)]
pub struct PlotCandleCapture {
    /// Call-site key (textual arguments), distinguishing same-titled candles.
    site: String,
    /// The `title` argument, or a positional fallback.
    pub title: String,
    /// The last-seen `display` argument handle, when given.
    pub display: Option<PineValue>,
    /// The last-seen `color` argument, when given.
    pub color: Option<PineValue>,
    /// The last-seen `wickcolor` argument, when given.
    pub wickcolor: Option<PineValue>,
    /// The last-seen `bordercolor` argument, when given.
    pub bordercolor: Option<PineValue>,
    /// One open per evaluated bar, in order.
    pub opens: Vec<Option<PineValue>>,
    /// One high per evaluated bar, in order.
    pub highs: Vec<Option<PineValue>>,
    /// One low per evaluated bar, in order.
    pub lows: Vec<Option<PineValue>>,
    /// One close per evaluated bar, in order.
    pub closes: Vec<Option<PineValue>>,
}

/// One captured `barcolor()` series: the conditional color per evaluated bar
/// (`None` entries are Pine `na`, never a default color).
#[derive(Debug, Clone)]
pub struct BarColorCapture {
    /// Call-site key (textual arguments), distinguishing same-titled paints.
    site: String,
    /// The `title` argument, or a positional fallback.
    pub title: String,
    /// One value per evaluated bar, in order.
    pub values: Vec<Option<PineValue>>,
}

/// One captured `fill()` pairing: the two plot sites it shades plus the
/// per-bar region between their values (an array pair, or `na` when both
/// legs are absent).
#[derive(Debug, Clone)]
pub struct FillCapture {
    /// Call-site key (textual arguments), distinguishing same-titled fills.
    site: String,
    /// The `title` argument, or a positional fallback.
    pub title: String,
    /// First plot's site key.
    pub first: String,
    /// Second plot's site key.
    pub second: String,
    /// First plot's title, resolved from its capture.
    pub first_title: String,
    /// Second plot's title, resolved from its capture.
    pub second_title: String,
    /// The last-seen `color` argument, when given.
    pub color: Option<PineValue>,
    /// One region per evaluated bar, in order.
    pub values: Vec<Option<PineValue>>,
}

/// Which lifecycle transition a drawing event records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrawingEventKind {
    /// `.new` minted an object.
    Created,
    /// A setter or `table.cell` mutated an object.
    Updated,
    /// `.delete` removed an object.
    Deleted,
}

/// One recorded drawing lifecycle row: which bar, which transition, which
/// object, with the call arguments as data. Rendering never happens.
#[derive(Debug, Clone)]
pub struct DrawingEvent {
    /// Evaluated-bar index at record time.
    pub bar: usize,
    /// Which transition this row records.
    pub kind: DrawingEventKind,
    /// Minting namespace (`label`, `table`, `box`, `line`).
    pub object: String,
    /// Stable object key (`label#3`).
    pub key: String,
    /// Call arguments in order (positional first, then named).
    pub args: Vec<DrawingArg>,
}

/// One recorded drawing argument: optional name plus the evaluated value
/// (`None` inside is Pine `na`).
#[derive(Debug, Clone)]
pub struct DrawingArg {
    /// Argument name (`None` for positional arguments).
    pub name: Option<String>,
    /// Evaluated value.
    pub value: Option<PineValue>,
}

/// One live drawing object: its namespace plus current properties. Setters
/// mutate properties in place; getters read them back; `.delete` drops the
/// entry (later reads refuse instead of resurrecting it).
#[derive(Debug, Clone)]
struct DrawingObject {
    kind: DrawingKind,
    props: HashMap<String, Option<PineValue>>,
}

/// Concrete `f64` legs for one run, converted once up front (same
/// decimal-to-float reading as the kernel batch path).
struct Legs {
    open: Vec<f64>,
    high: Vec<f64>,
    low: Vec<f64>,
    close: Vec<f64>,
    volume: Vec<f64>,
    time: Vec<f64>,
    hl2: Vec<f64>,
    hlc3: Vec<f64>,
    ohlc4: Vec<f64>,
}

impl Legs {
    fn of(bars: &[Bar]) -> Self {
        use rust_decimal::prelude::ToPrimitive as _;
        let conv = |d: rust_decimal::Decimal| d.to_f64().unwrap_or(f64::NAN);
        let open: Vec<f64> = bars.iter().map(|b| conv(b.o)).collect();
        let high: Vec<f64> = bars.iter().map(|b| conv(b.h)).collect();
        let low: Vec<f64> = bars.iter().map(|b| conv(b.l)).collect();
        let close: Vec<f64> = bars.iter().map(|b| conv(b.c)).collect();
        let volume: Vec<f64> = bars.iter().map(|b| b.volume as f64).collect();
        let time: Vec<f64> = bars.iter().map(|b| b.ts.timestamp() as f64).collect();
        let mix = |xs: &[f64], ys: &[f64], zs: Option<&[f64]>| {
            xs.iter()
                .zip(ys.iter())
                .enumerate()
                .map(|(i, (x, y))| match zs {
                    Some(ws) => (x + y + ws[i]) / 3.0,
                    None => (x + y) / 2.0,
                })
                .collect()
        };
        let hl2 = mix(&high, &low, None);
        let hlc3 = mix(&high, &low, Some(&close));
        let ohlc4 = open
            .iter()
            .zip(high.iter())
            .zip(low.iter())
            .zip(close.iter())
            .map(|(((o, h), l), c)| (o + h + l + c) / 4.0)
            .collect();
        Self {
            open,
            high,
            low,
            close,
            volume,
            time,
            hl2,
            hlc3,
            ohlc4,
        }
    }

    /// Borrow one named leg (`open/high/low/close/volume/time/hl2/hlc3/ohlc4`).
    fn named(&self, name: &str) -> Option<&[f64]> {
        match name {
            "open" => Some(&self.open),
            "high" => Some(&self.high),
            "low" => Some(&self.low),
            "close" => Some(&self.close),
            "volume" => Some(&self.volume),
            "time" => Some(&self.time),
            "hl2" => Some(&self.hl2),
            "hlc3" => Some(&self.hlc3),
            "ohlc4" => Some(&self.ohlc4),
            _ => None,
        }
    }
}

/// Per-run mutable state: series variables, histories, `var` records,
/// per-call-site `fixnan` memory, the `ta.*` bridge (fresh per run, so
/// kernel states never cross runs), the chart scope copied from the runtime
/// config (symbol/timeframe backing the `syminfo`/`timeframe` reads), the
/// input registry seeded from the run's
/// inputs (so `input.*` declarations and caller overrides share one store),
/// the v6 declaration tables (enums, consts, types, functions, methods,
/// exports), the owning module of the statements under execution plus every
/// module's alias table (so `ALIAS.fn` binds to the exporting module while
/// bare calls stay module-local), the method-receiver stack, the nested-call
/// One registered method: the owning module plus params and body. The
/// owner travels with the body so bare helper calls resolve lexically where
/// the method was declared, not where the receiver happens to be held.
/// Bodies ride `Arc` so per-call dispatch clones a pointer, never the AST.
type MethodDef = (String, Vec<Param>, std::sync::Arc<Vec<PineStmt>>);

/// One standalone-function overload: params and body. Same-named functions
/// stack in source order; dispatch picks the first arity-compatible
/// overload whose declared parameter types match the call values. Bodies
/// ride `Arc` so per-call dispatch clones a pointer, never the AST.
type FuncDef = (Vec<Param>, std::sync::Arc<Vec<PineStmt>>);

/// site stack (which qualifies stateful bridge sites per call path), per-bar
/// function-local values for history reads, snapshots, plot captures, and
/// the first error.
struct EngineState {
    variables: HashMap<String, Option<PineValue>>,
    history: HashMap<String, Vec<Option<PineValue>>>,
    var_kind: HashMap<String, bool>,
    fixnan_memory: HashMap<String, Option<PineValue>>,
    ta_bridge: Option<super::builtins_ta::TaBridge>,
    chart_symbol: String,
    chart_period: String,
    core_inputs: InputSet,
    enums: HashMap<String, HashMap<String, Option<PineValue>>>,
    consts: HashSet<String>,
    types: HashMap<String, Vec<FieldDecl>>,
    /// Standalone functions by `(owning module, name)`: each name holds
    /// its overloads in source order for signature dispatch.
    functions: HashMap<(String, String), Vec<FuncDef>>,
    /// Methods by `(receiver type, method)`: the owning module travels with
    /// the body (see [`MethodDef`]).
    methods: HashMap<(String, String), MethodDef>,
    exported: HashSet<String>,
    current_module: String,
    module_aliases: HashMap<(String, String), String>,
    receiver: Vec<ReceiverSlot>,
    call_stack: Vec<String>,
    /// Per-bar function-local values for history reads, keyed by call site
    /// (`site#name`) inside a call and by bare name at the top level, so a
    /// helper's locals never read the caller's past for a same-named
    /// binding (and vice versa).
    frame_history: HashMap<String, Option<PineValue>>,
    /// Loop nesting depth for `break`: incremented by `for`/`while`, reset
    /// across function/method call boundaries so a `break` binds lexically.
    loop_depth: usize,
    /// Set by `break`, consumed by the innermost enclosing loop; statement
    /// lists stop through [`stopped`](Self::stopped) while it is set.
    break_flag: bool,
    /// Per-update tick timestamps (epoch milliseconds), parallel to the
    /// run's bars. Non-empty selects tick/realtime emulation: `timenow`
    /// reads these instead of the `time` leg, and the cursor flags follow
    /// bar-time changes rather than positions.
    tick_times: Vec<i64>,
    /// Harness-registered lower-timeframe datasets served to
    /// `request.security_lower_tf` (copied from the engine at run start).
    ltf_injections: Vec<InjectedDataset>,
    /// Harness-registered per-timeframe datasets served to
    /// `request.security` under confirmed-bar semantics (copied from the
    /// engine at run start, same shape as the pull registry).
    security_injections: Vec<InjectedDataset>,
    /// Native-execution opt-in copied from the engine at run start (default
    /// Pine): consulted once per instance-method call, never mutated
    /// mid-run.
    native: NativeOptions,
    /// Ring-lease versions per leased object slot: bumped on every
    /// Pine-side write to the slot (the single bump rule — see
    /// [`EngineState::bump_lease`]), so a cached resident ring whose
    /// version skews re-hydrates instead of serving stale state.
    lease_versions: HashMap<LeaseKey, u64>,
    /// Cached resident rings per leased slot with the table version they
    /// were synced at. A version match serves with zero hydrate; any other
    /// outcome drops the entry.
    leased_rings: HashMap<LeaseKey, LeasedRing>,
    /// Commit-vs-fallback telemetry for the leased `fetch` seam, reset per
    /// run with the rest of the state. Read through
    /// [`PineEngine::fetch_counters`]; observed, never asserted.
    fetch_counters: FetchCounters,
    /// Chart bars of the current run, cloned only when the run links the
    /// context library (the provider's resident tape — per-run state reset
    /// by [`EngineState::fresh`], never shared across runs).
    run_bars: Vec<Bar>,
    /// Resident shared store behind natively served context reads: fed
    /// incrementally with chart bars through the cursor, so steady-state
    /// reads extend the tail instead of rebuilding history.
    apex_store: MtfStore,
    /// Chart bars loaded into `apex_store`.
    apex_loaded: usize,
    /// Unconditional top-level declaration sites pre-registered before the
    /// first bar, as `(module index, statement index)`: the bar loop skips
    /// them, since re-execution would only re-collapse identical shapes.
    decl_skip: HashSet<(usize, usize)>,
    /// Plain frame writes journaled per call as `(name, entry value)`
    /// with absence for unbound names: the teardown reverts exactly what
    /// the body touched, without cloning the whole frame up front.
    /// The outer `None` means unbound, distinct from bound-to-`na` —
    /// hence the nested option.
    #[allow(clippy::option_option)]
    journal: Vec<(String, Option<Option<PineValue>>)>,
    /// Journal suffix owned by the innermost call (`None` outside calls):
    /// only writes at or after the base journal, so nested calls revert
    /// their own suffix and leave the caller's entries alone.
    journal_base: Option<usize>,
    /// Suppress journaling while parameter binding and write-backs run
    /// (those restore through the shadow list and caller homes instead).
    journal_hold: bool,
    /// Variable-to-map-entry provenance for `map.get` handles: `var ->
    /// (map variable, key encoding, key value)`. A method call through a
    /// tagged variable writes the mutated instance back to the map entry
    /// as well as the variable, giving Pine reference behavior. Rebinding
    /// the variable from anything but `map.get` clears its tag.
    map_prov: HashMap<String, (String, String, PineValue)>,
    /// Variable-to-plot-site tags: `var -> plot@args` for variables bound
    /// from a `plot()` call, so `fill(plot_var, ...)` pairs the same sites
    /// as the inline form. `plot()` itself evaluates to absence (its value
    /// is captured, not returned), so the tag is the only pairing record.
    /// Rebinding from anything but `plot()` clears the tag.
    plot_vars: HashMap<String, String>,
    plots: Vec<PlotCapture>,
    snapshots: Vec<BarSnapshot>,
    plotcandles: Vec<PlotCandleCapture>,
    barcolors: Vec<BarColorCapture>,
    fills: Vec<FillCapture>,
    plotshapes: Vec<PlotShapeCapture>,
    hlines: Vec<HlineCapture>,
    draw_events: Vec<DrawingEvent>,
    drawings: HashMap<u64, DrawingObject>,
    next_drawing_id: u64,
    error: Option<KernelError>,
}

impl EngineState {
    fn fresh() -> Self {
        Self {
            variables: HashMap::new(),
            history: HashMap::new(),
            var_kind: HashMap::new(),
            fixnan_memory: HashMap::new(),
            ta_bridge: None,
            chart_symbol: crate::runtime::default_chart_symbol(),
            chart_period: crate::runtime::default_chart_period(),
            core_inputs: InputSet::new(),
            enums: HashMap::new(),
            consts: HashSet::new(),
            types: HashMap::new(),
            functions: HashMap::new(),
            methods: HashMap::new(),
            exported: HashSet::new(),
            current_module: String::new(),
            module_aliases: HashMap::new(),
            receiver: Vec::new(),
            call_stack: Vec::new(),
            frame_history: HashMap::new(),
            loop_depth: 0,
            break_flag: false,
            tick_times: Vec::new(),
            ltf_injections: Vec::new(),
            security_injections: Vec::new(),
            native: NativeOptions {
                enabled: false,
                only_symbols: None,
            },
            lease_versions: HashMap::new(),
            leased_rings: HashMap::new(),
            fetch_counters: FetchCounters::default(),
            run_bars: Vec::new(),
            apex_store: MtfStore::new(),
            apex_loaded: 0,
            decl_skip: HashSet::new(),
            journal: Vec::new(),
            journal_base: None,
            journal_hold: false,
            map_prov: HashMap::new(),
            plot_vars: HashMap::new(),
            plots: Vec::new(),
            snapshots: Vec::new(),
            plotcandles: Vec::new(),
            barcolors: Vec::new(),
            fills: Vec::new(),
            plotshapes: Vec::new(),
            hlines: Vec::new(),
            draw_events: Vec::new(),
            drawings: HashMap::new(),
            next_drawing_id: 0,
            error: None,
        }
    }
}

/// One linked module: a parsed program plus the importer-local alias table
/// (`ALIAS` to owning module key) from the import graph. Modules execute in
/// dependency-first order; declarations share one run namespace while user
/// functions stay namespaced by their owning module key.
#[derive(Debug, Clone)]
pub struct LinkedModule {
    /// Owning module key (graph identity, e.g. `publisher/name`).
    pub key: String,
    /// Parsed program for execution.
    pub program: PineProgram,
    /// Importer-local aliases to exporting module keys.
    pub aliases: HashMap<String, String>,
}

impl LinkedModule {
    /// Bundle one parsed program with its alias table for linked execution.
    #[must_use]
    pub fn new(
        key: &str,
        program: PineProgram,
        aliases: HashMap<String, String>,
    ) -> Self {
        Self {
            key: key.to_owned(),
            program,
            aliases,
        }
    }
}

/// One injected lower-timeframe dataset served to
/// `request.security_lower_tf` headlessly: six parallel columns over the
/// same rows (timestamps in epoch milliseconds, prices and volumes as
/// floats). The harness registers exactly the window a pull may observe;
/// `calc_bars_count` tail-takes from it.
#[derive(Debug, Clone)]
pub struct LtfDataset {
    /// Row timestamps, epoch milliseconds.
    pub times: Vec<i64>,
    /// Row opens.
    pub opens: Vec<f64>,
    /// Row highs.
    pub highs: Vec<f64>,
    /// Row lows.
    pub lows: Vec<f64>,
    /// Row closes.
    pub closes: Vec<f64>,
    /// Row volumes.
    pub volumes: Vec<f64>,
}

impl LtfDataset {
    /// Row count; all six columns always agree (enforced at injection).
    #[must_use]
    pub fn len(&self) -> usize {
        self.times.len()
    }

    /// True when no rows are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.times.is_empty()
    }

    /// Build an injection dataset from closed bars: millisecond timestamps
    /// plus float legs converted exactly like kernel windows
    /// ([`OwnedOhlcv::from_bars`](crate::indicator::OwnedOhlcv::from_bars)),
    /// so injected security columns agree with chart legs bit-for-bit. The
    /// harness constructor for [`inject_security`](PineEngine::inject_security)
    /// and [`inject_lower_tf`](PineEngine::inject_lower_tf) datasets;
    /// headless suites share it instead of copying the conversion.
    #[must_use]
    pub fn of_bars(bars: &[Bar]) -> Self {
        let legs = crate::indicator::OwnedOhlcv::from_bars(bars);
        Self {
            times: bars.iter().map(|bar| bar.ts.timestamp_millis()).collect(),
            opens: legs.open,
            highs: legs.high,
            lows: legs.low,
            closes: legs.close,
            volumes: legs.volume,
        }
    }
}

/// One registered dataset: the exact `(symbol, timeframe)` pair it answers.
/// Shared by the lower-timeframe pull registry and the `request.security`
/// registry; both are harness configuration, never run state.
#[derive(Debug, Clone)]
pub struct InjectedDataset {
    symbol: String,
    timeframe: String,
    dataset: LtfDataset,
}

/// The executor. Owns its runtime config; all per-run state resets on
/// [`run`](Self::run), so one engine serves sequential runs safely.
/// Registered datasets (lower-timeframe and security) survive across runs
/// (they are harness configuration, not run state).
pub struct PineEngine {
    runtime: PineRuntime,
    state: EngineState,
    ltf_injections: Vec<InjectedDataset>,
    security_injections: Vec<InjectedDataset>,
    native: NativeOptions,
}

impl PineEngine {
    /// Bind an engine to a runtime config (warmup enforced by the driver).
    #[must_use]
    pub fn new(config: PineRuntimeConfig) -> Self {
        Self {
            runtime: PineRuntime::new(config),
            state: EngineState::fresh(),
            ltf_injections: Vec::new(),
            security_injections: Vec::new(),
            // Execution defaults to Pine: the link-time default governs
            // routing views, but running a body natively is an explicit
            // per-engine opt-in, so every existing driver keeps proving
            // exactly what it proved before.
            native: NativeOptions {
                enabled: false,
                only_symbols: None,
            },
        }
    }

    /// Opt one engine into native execution of served instance methods
    /// (today: the `Bars.fetch` tick path). Symbol subsets refuse, mirroring
    /// the whole-library rule at the type level. Absent this call the
    /// engine evaluates Pine byte-identically to before.
    pub fn set_native_options(&mut self, options: NativeOptions) -> Result<(), KernelError> {
        if options.only_symbols.is_some() {
            return Err(refused(
                "native overrides apply whole-library: symbol subsets refuse, pass None",
            ));
        }
        self.native = options;
        Ok(())
    }

    /// Check one injected dataset's columns agree in length (empty serves
    /// empty); ragged datasets refuse fail-closed here, never mid-run.
    fn check_dataset(
        what: &str,
        symbol: &str,
        timeframe: &str,
        dataset: &LtfDataset,
    ) -> Result<(), KernelError> {
        let len = dataset.len();
        for (column, column_len) in [
            ("opens", dataset.opens.len()),
            ("highs", dataset.highs.len()),
            ("lows", dataset.lows.len()),
            ("closes", dataset.closes.len()),
            ("volumes", dataset.volumes.len()),
        ] {
            if column_len != len {
                return Err(refused(format!(
                    "injected {what} dataset for {symbol}/{timeframe} is ragged: {column} has {column_len} rows for {len} timestamps"
                )));
            }
        }
        Ok(())
    }

    /// Register one lower-timeframe dataset for `(symbol, timeframe)`:
    /// later `request.security_lower_tf` calls with that pair are served
    /// from it instead of refusing. Columns must agree in length (empty
    /// serves empty arrays); anything else refuses fail-closed here,
    /// never mid-run.
    pub fn inject_lower_tf(
        &mut self,
        symbol: &str,
        timeframe: &str,
        dataset: LtfDataset,
    ) -> Result<(), KernelError> {
        Self::check_dataset("lower-tf", symbol, timeframe, &dataset)?;
        if let Some(slot) = self
            .ltf_injections
            .iter_mut()
            .find(|slot| slot.symbol == symbol && slot.timeframe == timeframe)
        {
            slot.dataset = dataset;
        } else {
            self.ltf_injections.push(InjectedDataset {
                symbol: symbol.to_owned(),
                timeframe: timeframe.to_owned(),
                dataset,
            });
        }
        Ok(())
    }

    /// Register one per-timeframe dataset for `(symbol, timeframe)`:
    /// later `request.security` calls with that pair are served from the
    /// confirmed prefix (timestamps at or before the current chart bar)
    /// instead of refusing. Unregistered pairs keep the live-fetch
    /// refusal. Same column contract as [`inject_lower_tf`](Self::inject_lower_tf).
    pub fn inject_security(
        &mut self,
        symbol: &str,
        timeframe: &str,
        dataset: LtfDataset,
    ) -> Result<(), KernelError> {
        Self::check_dataset("security", symbol, timeframe, &dataset)?;
        if let Some(slot) = self
            .security_injections
            .iter_mut()
            .find(|slot| slot.symbol == symbol && slot.timeframe == timeframe)
        {
            slot.dataset = dataset;
        } else {
            self.security_injections.push(InjectedDataset {
                symbol: symbol.to_owned(),
                timeframe: timeframe.to_owned(),
                dataset,
            });
        }
        Ok(())
    }

    /// Evaluate `program` over `bars` with `inputs`, returning per-bar
    /// snapshots. The first refusal aborts the run with its reason. A
    /// single-module run links one module under the local key with no
    /// aliases, so bare calls resolve exactly as before.
    pub fn run(
        &mut self,
        program: &PineProgram,
        bars: &[Bar],
        inputs: InputSet,
    ) -> Result<RunOutcome, KernelError> {
        self.run_linked(
            &[LinkedModule::new("local", program.clone(), HashMap::new())],
            bars,
            inputs,
        )
    }

    /// Evaluate linked `modules` (dependency-first) over `bars` with
    /// `inputs`, returning per-bar snapshots. Every bar executes each
    /// module's statements in order, so dependency declarations (enums,
    /// consts, types, functions, methods) are visible to dependents from
    /// the first bar; user functions stay namespaced by owning module and
    /// only `export`-reachable names run through aliases. The first
    /// refusal aborts the run with its reason.
    pub fn run_linked(
        &mut self,
        modules: &[LinkedModule],
        bars: &[Bar],
        inputs: InputSet,
    ) -> Result<RunOutcome, KernelError> {
        self.run_linked_inner(modules, bars, Vec::new(), inputs)
    }

    /// Evaluate linked `modules` over per-tick updates: `bars` carries one
    /// entry per tick (legs track the bar-so-far OHLC, cumulative volume,
    /// and the constant bar-open time), while `tick_millis` carries the
    /// tick timestamps `timenow` reads. Ticks sharing a bar-open time form
    /// one realtime bar: `barstate.isnew` fires on the first tick only,
    /// `isconfirmed` on the last, and the run reports `ishistory=false` /
    /// `isrealtime=true`, mirroring a live feed. The `time` leg runs in
    /// Pine-native milliseconds here (closed-bar runs keep seconds).
    /// Length mismatch refuses fail-closed.
    pub fn run_linked_ticks(
        &mut self,
        modules: &[LinkedModule],
        bars: &[Bar],
        tick_millis: &[i64],
        inputs: InputSet,
    ) -> Result<RunOutcome, KernelError> {
        if tick_millis.len() != bars.len() {
            return Err(refused(format!(
                "tick run needs one timestamp per bar, got {} stamps for {} bars",
                tick_millis.len(),
                bars.len()
            )));
        }
        self.run_linked_inner(modules, bars, tick_millis.to_vec(), inputs)
    }

    fn run_linked_inner(
        &mut self,
        modules: &[LinkedModule],
        bars: &[Bar],
        tick_times: Vec<i64>,
        inputs: InputSet,
    ) -> Result<RunOutcome, KernelError> {
        self.state = EngineState::fresh();
        self.state.chart_symbol.clone_from(&self.runtime.config().chart_symbol);
        self.state.chart_period.clone_from(&self.runtime.config().chart_period);
        self.state.tick_times = tick_times;
        self.state.ltf_injections.clone_from(&self.ltf_injections);
        self.state
            .security_injections
            .clone_from(&self.security_injections);
        self.state.native.clone_from(&self.native);
        // The context provider's resident tape: clone the chart bars only
        // when the run links the library, so non-context runs pay nothing.
        if modules.iter().any(|module| module.key == "showmethegrail/libApex") {
            self.state.run_bars = bars.to_vec();
        }
        // The input registry seeds from the run's inputs so caller overrides
        // are visible to `input.*` declarations from the first bar.
        self.state.core_inputs = inputs.clone();
        for module in modules {
            for (alias, target) in &module.aliases {
                self.state.module_aliases.insert(
                    (module.key.clone(), alias.clone()),
                    target.clone(),
                );
            }
        }
        // Unconditional top-level declarations register once, before the
        // first bar, so per-bar runs skip them; a malformed declaration
        // aborts the run here with the same reason bar zero would carry.
        self.state.decl_skip = self.state.predeclare(modules)?;
        let mut legs = Legs::of(bars);
        if self.state.tick_mode() {
            // Tick emulation runs the `time` leg in Pine-native
            // milliseconds (exact below 2^53); closed-bar runs keep the
            // seconds domain pinned by the runtime-shim suite.
            legs.time = bars
                .iter()
                .map(|bar| bar.ts.timestamp_millis() as f64)
                .collect();
        }
        let PineEngine { runtime, state, .. } = &mut *self;
        let evaluated = runtime.run_over(bars, inputs, |ctx| {
            state.exec_bar(modules, ctx, &legs);
        })?;
        if let Some(err) = state.error.take() {
            return Err(err);
        }
        Ok(RunOutcome {
            evaluated,
            snapshots: std::mem::take(&mut state.snapshots),
            plots: std::mem::take(&mut state.plots),
            plotcandles: std::mem::take(&mut state.plotcandles),
            barcolors: std::mem::take(&mut state.barcolors),
            fills: std::mem::take(&mut state.fills),
            plotshapes: std::mem::take(&mut state.plotshapes),
            hlines: std::mem::take(&mut state.hlines),
            draw_events: std::mem::take(&mut state.draw_events),
        })
    }

    /// Names bound by `export` in the last run, for import-graph consumers.
    #[must_use]
    pub fn exported_names(&self) -> Vec<String> {
        self.state.exported.iter().cloned().collect()
    }

    /// Commit-vs-fallback telemetry for the leased `fetch` seam over the
    /// last run (resets with each run). For the acceptance record and
    /// benches: observed, never asserted.
    #[must_use]
    pub fn fetch_counters(&self) -> FetchCounters {
        self.state.fetch_counters
    }
}

impl EngineState {
    /// Tick/realtime emulation is on when the run carries per-update tick
    /// timestamps: cursor flags follow bar-time changes and `timenow`
    /// reads tick times instead of the `time` leg.
    fn tick_mode(&self) -> bool {
        !self.tick_times.is_empty()
    }

    /// Register one standalone function under the current module:
    /// identical re-declarations collapse, so only a genuinely new
    /// signature extends the overload list.
    fn declare_func(
        &mut self,
        name: &str,
        params: &[super::syntax::Param],
        body: &[super::syntax::PineStmt],
    ) {
        let slot = self
            .functions
            .entry((self.current_module.clone(), name.to_owned()))
            .or_default();
        if !slot
            .iter()
            .any(|(known_params, known_body)| {
                known_params == params && known_body.as_slice() == body
            })
        {
            slot.push((params.to_vec(), std::sync::Arc::new(body.to_vec())));
        }
    }

    /// Register one method under its receiver type (module qualifiers
    /// compare by trailing segment). Untyped receivers refuse.
    fn declare_method(
        &mut self,
        name: &str,
        params: &[super::syntax::Param],
        body: &[super::syntax::PineStmt],
    ) -> Result<(), KernelError> {
        let receiver_type = params.first().and_then(|param| {
            param
                .type_name
                .as_ref()
                .map(|decl| trailing_type(&decl.name).to_owned())
        });
        match receiver_type {
            Some(type_name) => {
                self.methods.insert(
                    (type_name, name.to_owned()),
                    (
                        self.current_module.clone(),
                        params.to_vec(),
                        std::sync::Arc::new(body.to_vec()),
                    ),
                );
                Ok(())
            }
            None => Err(refused(format!(
                "method '{name}' needs a typed receiver ('Type this')"
            ))),
        }
    }

    /// Register one user-type shape under the current module.
    fn declare_type(&mut self, name: &str, fields: &[super::syntax::FieldDecl]) {
        self.types.insert(name.to_owned(), fields.to_vec());
    }

    /// Register every unconditional top-level declaration once, before the
    /// first bar: pure shapes (`FuncDecl`, `MethodDecl`, `TypeDecl`,
    /// including through `export`) land in exactly the state their per-bar
    /// execution would leave, so per-bar runs skip them. Anything
    /// effectful or conditional (`ConstDecl` evaluates, `EnumDecl` binds
    /// member values, nested declarations) stays per-bar. Returns the
    /// `(module index, statement index)` sites the bar loop skips.
    fn predeclare(
        &mut self,
        modules: &[LinkedModule],
    ) -> Result<HashSet<(usize, usize)>, KernelError> {
        let mut skip = HashSet::new();
        for (position, module) in modules.iter().enumerate() {
            self.current_module.clone_from(&module.key);
            for (index, stmt) in module.program.statements.iter().enumerate() {
                let (decl, was_export) = match stmt {
                    PineStmt::ExportDecl(inner) => (inner.as_ref(), true),
                    other => (other, false),
                };
                let done = match decl {
                    PineStmt::FuncDecl { name, params, body } => {
                        self.declare_func(name, params, body);
                        true
                    }
                    PineStmt::MethodDecl { name, params, body } => {
                        self.declare_method(name, params, body)?;
                        true
                    }
                    PineStmt::TypeDecl { name, fields } => {
                        self.declare_type(name, fields);
                        true
                    }
                    _ => false,
                };
                if done {
                    if was_export {
                        if let Some(name) = export_label(decl) {
                            self.exported.insert(name);
                        }
                    }
                    skip.insert((position, index));
                }
            }
        }
        Ok(skip)
    }

    /// Execute one bar across every linked module in order, then record its
    /// snapshot and extend histories. Declaration statements re-run
    /// idempotently (declare-once tables), so dependency modules need no
    /// separate registration pass.
    fn exec_bar(&mut self, modules: &[LinkedModule], ctx: &mut RunCtx<'_>, legs: &Legs) {
        // Function-local values never survive the call, so last bar's frame
        // history must not leak into this bar's extension.
        self.frame_history.clear();
        if self.error.is_none() {
            for (position, module) in modules.iter().enumerate() {
                self.current_module.clone_from(&module.key);
                for (index, stmt) in module.program.statements.iter().enumerate() {
                    if self.decl_skip.contains(&(position, index)) {
                        continue;
                    }
                    self.exec_stmt(stmt, ctx, legs);
                    if self.error.is_some() {
                        break;
                    }
                }
                if self.error.is_some() {
                    break;
                }
            }
        }
        let mut merged = ctx.vars().snapshot();
        merged.extend(self.variables.iter().map(|(k, v)| (k.clone(), v.clone())));
        let completed = self.snapshots.len();
        let mut keyed: Vec<String> = merged.keys().cloned().collect();
        for name in self.frame_history.keys() {
            if !merged.contains_key(name) {
                keyed.push(name.clone());
            }
        }
        for name in keyed {
            let slot = self.history.entry(name).or_default();
            while slot.len() < completed {
                slot.push(None);
            }
        }
        for (name, slot) in &mut self.history {
            let pushed = merged
                .get(name)
                .or_else(|| self.frame_history.get(name))
                .cloned()
                .unwrap_or(None);
            slot.push(pushed);
        }
        self.snapshots.push(BarSnapshot {
            bar: ctx.cursor(),
            values: merged,
        });
    }

    /// Execute one statement; the first refusal sticks and halts the run.
    fn exec_stmt(&mut self, stmt: &PineStmt, ctx: &mut RunCtx<'_>, legs: &Legs) {
        if self.error.is_some() {
            return;
        }
        let result = self.exec_stmt_inner(stmt, ctx, legs);
        if let Err(err) = result {
            self.error = Some(err);
        }
    }

    /// Statement lists stop when a refusal is latched or a `break` is
    /// pending: the innermost enclosing loop consumes the flag.
    fn stopped(&self) -> bool {
        self.error.is_some() || self.break_flag
    }

    /// Run one numeric-`for` iteration: bind the counter, run the body,
    /// and report whether the loop must stop (`break` or a latched
    /// refusal). Shared by the ascending and descending directions so the
    /// two never fork.
    fn for_step(
        &mut self,
        variable: &str,
        at: i64,
        body: &[PineStmt],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> bool {
        self.assign(ctx, variable, Some(PineValue::Int(at)));
        for stmt in body {
            self.exec_stmt(stmt, ctx, legs);
            if self.stopped() {
                break;
            }
        }
        self.stopped()
    }

    #[allow(clippy::too_many_lines)]
    fn exec_stmt_inner(
        &mut self,
        stmt: &PineStmt,
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<(), KernelError> {
        match stmt {
            PineStmt::VarDeclaration {
                name,
                value,
                is_varip,
                ..
            } => {
                // Initializer runs exactly once per run, on the first
                // evaluated bar — warmup bars never seed it.
                if !self.var_kind.contains_key(name) {
                    let init = self.eval_expr(value, ctx, legs)?;
                    self.var_kind.insert(name.clone(), *is_varip);
                    if let Some(present) = init {
                        // A fresh object at this home invalidates any lease
                        // the slot may hold (defensive: tables start empty
                        // per run, but declarations also evaluate in the
                        // predeclare pass).
                        if matches!(present, PineValue::Object(_)) {
                            self.bump_lease_key(LeaseKey::var_store(name, *is_varip));
                        }
                        if *is_varip {
                            ctx.vars_mut().set_varip(name, present);
                        } else {
                            ctx.vars_mut().set_var(name, present);
                        }
                    }
                }
                Ok(())
            }
            PineStmt::Declaration { name, value, .. } | PineStmt::Assignment { name, value } => {
                let assigned = self.eval_expr(value, ctx, legs)?;
                self.assign(ctx, name, assigned.clone());
                self.refresh_prov(ctx, legs, name, value, assigned.as_ref())?;
                self.refresh_plot_var(name, value);
                Ok(())
            }
            PineStmt::MultiDeclaration { decls } => {
                for decl in decls {
                    let assigned = match &decl.value {
                        Some(value) => self.eval_expr(value, ctx, legs)?,
                        None => None,
                    };
                    self.assign(ctx, &decl.name, assigned.clone());
                    if let Some(value) = &decl.value {
                        self.refresh_prov(ctx, legs, &decl.name, value, assigned.as_ref())?;
                        self.refresh_plot_var(&decl.name, value);
                    } else {
                        self.map_prov.remove(&decl.name);
                        self.plot_vars.remove(&decl.name);
                    }
                }
                Ok(())
            }
            PineStmt::MultiAssign { items } => {
                // Each item runs through its single-assignment rule, so
                // `:=` target checks and `this.*` paths behave identically.
                for item in items {
                    let single = match item.op {
                        AssignOp::Assign => PineStmt::Assignment {
                            name: item.name.clone(),
                            value: Box::new(item.value.clone()),
                        },
                        AssignOp::Reassign => PineStmt::Reassign {
                            name: item.name.clone(),
                            value: Box::new(item.value.clone()),
                        },
                    };
                    self.exec_stmt(&single, ctx, legs);
                    if self.error.is_some() {
                        break;
                    }
                }
                Ok(())
            }
            PineStmt::Reassign { name, value } => {
                if name == "this" || name.starts_with("this.") {
                    let assigned = self.eval_expr(value, ctx, legs)?;
                    return self.assign_path(ctx, name, assigned);
                }
                // Receiver-alias field writes (`level.aged := 0`, and the
                // `+=` family desugared onto them) hit the live slot; the
                // right-hand side already reads through the same alias.
                if let Some((head, field)) = name.split_once('.') {
                    if !field.contains('.') && self.receiver_slot(head).is_some() {
                        let assigned = self.eval_expr(value, ctx, legs)?;
                        if self.write_alias_field(head, field, assigned) {
                            return Ok(());
                        }
                        return Err(refused(format!("unknown field '{head}.{field}'")));
                    }
                    // Variable-held instance field writes (`context.regime
                    // := ...`): the instance takes out of its home, gains
                    // the field, and writes back — the same take/put
                    // discipline as method receivers. Unknown fields
                    // refuse after restoring the instance.
                    if !field.contains('.') && !head.is_empty() {
                        let assigned = self.eval_expr(value, ctx, legs)?;
                        if let Some((mut instance, home)) = self.take_object(ctx, head)? {
                            if !instance.fields.contains_key(field) {
                                self.put_object(ctx, home, instance, false)?;
                                return Err(refused(format!(
                                    "unknown field '{head}.{field}'"
                                )));
                            }
                            instance.fields.insert(field.to_owned(), assigned);
                            self.put_object(ctx, home, instance, true)?;
                            return Ok(());
                        }
                    }
                }
                if self.var_kind.contains_key(name) || self.variables.contains_key(name) {
                    let assigned = self.eval_expr(value, ctx, legs)?;
                    self.assign(ctx, name, assigned.clone());
                    self.refresh_prov(ctx, legs, name, value, assigned.as_ref())?;
                    self.refresh_plot_var(name, value);
                    Ok(())
                } else {
                    Err(refused(format!(
                        "':=' targets undeclared variable '{name}'"
                    )))
                }
            }
            PineStmt::DestructureAssign { targets, value } => {
                let assigned = self.eval_expr(value, ctx, legs)?;
                match assigned {
                    Some(PineValue::Array(items)) => {
                        if items.len() != targets.len() {
                            return Err(refused(format!(
                                "destructuring arity {} does not match {} targets",
                                items.len(),
                                targets.len()
                            )));
                        }
                        for (name, slot) in targets.iter().zip(items) {
                            self.assign_path(ctx, name, slot)?;
                        }
                        Ok(())
                    }
                    Some(other) => Err(refused(format!(
                        "destructuring binds array values, got {other}"
                    ))),
                    None => Err(refused("destructuring binds array values, got na")),
                }
            }
            PineStmt::Expression(expr) => {
                self.exec_expr_stmt(expr, ctx, legs)?;
                Ok(())
            }
            PineStmt::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let block =
                    self.taken_block(condition, then_branch, else_branch.as_deref(), ctx, legs)?;
                for stmt in block {
                    self.exec_stmt(stmt, ctx, legs);
                    if self.stopped() {
                        break;
                    }
                }
                Ok(())
            }
            PineStmt::Break => {
                if self.loop_depth == 0 {
                    return Err(refused(
                        "'break' outside a 'for'/'while' body refuses instead of unwinding",
                    ));
                }
                self.break_flag = true;
                Ok(())
            }
            PineStmt::For {
                variable,
                start,
                end,
                body,
            } => {
                let first = whole_number("for start", self.eval_expr(start, ctx, legs)?.as_ref())?;
                let last = whole_number("for end", self.eval_expr(end, ctx, legs)?.as_ref())?;
                let count = (i128::from(last) - i128::from(first)).abs() + 1;
                if count > MAX_LOOP_ITERATIONS as i128 {
                    return Err(refused(format!(
                        "'for' range {first}..={last} exceeds {MAX_LOOP_ITERATIONS} iterations"
                    )));
                }
                self.loop_depth += 1;
                // The step adapts to the direction: a start past the end
                // counts down, so reverse scans (`size - 1 to 0`) iterate
                // instead of silently running zero times.
                if first <= last {
                    for i in first..=last {
                        if self.for_step(variable, i, body, ctx, legs) {
                            break;
                        }
                    }
                } else {
                    for i in (last..=first).rev() {
                        if self.for_step(variable, i, body, ctx, legs) {
                            break;
                        }
                    }
                }
                self.loop_depth -= 1;
                // A pending `break` belongs to this loop however it
                // stopped: consume it so an outer loop never observes it.
                self.break_flag = false;
                Ok(())
            }
            PineStmt::ForIn {
                targets,
                iterable,
                body,
            } => {
                let collection = self.eval_expr(iterable, ctx, legs)?;
                let items = match collection {
                    Some(PineValue::Array(items)) => items,
                    Some(other) => {
                        return Err(refused(format!(
                            "'for ... in' iterates arrays, got {other}"
                        )));
                    }
                    None => {
                        return Err(refused("'for ... in' iterates arrays, got na"));
                    }
                };
                if items.len() > MAX_LOOP_ITERATIONS {
                    return Err(refused(format!(
                        "'for ... in' over {} elements exceeds {MAX_LOOP_ITERATIONS} iterations",
                        items.len()
                    )));
                }
                // One target binds each value; a pair binds the integer
                // index beside the value. Anything else is not iteration.
                let (index_name, value_name) = match targets.as_slice() {
                    [value] => (None, value.clone()),
                    [index, value] => (Some(index.clone()), value.clone()),
                    _ => {
                        return Err(refused(
                            "'for ... in' binds one value or an index/value pair",
                        ));
                    }
                };
                self.loop_depth += 1;
                for (index, slot) in items.iter().enumerate() {
                    if let Some(name) = index_name.as_ref() {
                        #[allow(clippy::cast_possible_wrap)]
                        self.assign(ctx, name, Some(PineValue::Int(index as i64)));
                    }
                    self.assign(ctx, &value_name, slot.clone());
                    for stmt in body {
                        self.exec_stmt(stmt, ctx, legs);
                        if self.stopped() {
                            break;
                        }
                    }
                    if self.stopped() {
                        break;
                    }
                }
                self.loop_depth -= 1;
                // A pending `break` belongs to this loop however it
                // stopped: consume it so an outer loop never observes it.
                self.break_flag = false;
                Ok(())
            }
            PineStmt::While { condition, body } => {
                let mut iterations: usize = 0;
                self.loop_depth += 1;
                loop {
                    // An `na` condition ends the loop, mirroring the `if`
                    // rule above; numbers still refuse.
                    let keep =
                        branch_of("while condition", self.eval_expr(condition, ctx, legs)?.as_ref())?
                            .unwrap_or(false);
                    if !keep {
                        break;
                    }
                    if iterations >= MAX_LOOP_ITERATIONS {
                        self.loop_depth -= 1;
                        self.break_flag = false;
                        return Err(refused(format!(
                            "'while' exceeded {MAX_LOOP_ITERATIONS} iterations"
                        )));
                    }
                    iterations += 1;
                    for stmt in body {
                        self.exec_stmt(stmt, ctx, legs);
                        if self.stopped() {
                            break;
                        }
                    }
                    if self.stopped() {
                        break;
                    }
                }
                self.loop_depth -= 1;
                self.break_flag = false;
                Ok(())
            }
            PineStmt::Import(_)
            | PineStmt::LibraryDecl { .. }
            | PineStmt::IndicatorDecl { .. } => {
                // Headers declare metadata for the loader and the import
                // graph; the executor ignores them. A linked run carries the
                // graph separately, so executing a program that still holds
                // its header lines is not a refusal.
                Ok(())
            }
            PineStmt::ExportDecl(inner) => {
                let label = export_label(inner);
                self.exec_stmt_inner(inner, ctx, legs)?;
                if let Some(name) = label {
                    self.exported.insert(name);
                }
                Ok(())
            }
            PineStmt::FuncDecl { name, params, body } => {
                self.declare_func(name, params, body);
                Ok(())
            }
            PineStmt::MethodDecl { name, params, body } => {
                self.declare_method(name, params, body)
            }
            PineStmt::ConstDecl { name, value, .. } => {
                // Compile-time bindings evaluate once, on the first
                // evaluated bar that declares them.
                if !self.consts.contains(name) {
                    let assigned = self.eval_expr(value, ctx, legs)?;
                    self.assign(ctx, name, assigned);
                    self.consts.insert(name.clone());
                }
                Ok(())
            }
            PineStmt::EnumDecl { name, members } => {
                if !self.enums.contains_key(name) {
                    let mut table = HashMap::with_capacity(members.len());
                    for member in members {
                        table.insert(
                            member.name.clone(),
                            self.eval_expr(&member.value, ctx, legs)?,
                        );
                    }
                    self.enums.insert(name.clone(), table);
                }
                Ok(())
            }
            PineStmt::TypeDecl { name, fields } => {
                self.declare_type(name, fields);
                Ok(())
            }
            PineStmt::Switch(switch) => {
                self.eval_switch(switch, ctx, legs, false)?;
                Ok(())
            }
        }
    }

    /// Route an assignment: persistent stores for `var`/`varip`, series
    /// variables otherwise. Assigning `na` to a persistent binding clears
    /// it (the initialization mark stays, so it is not re-seeded). Plain
    /// frame writes inside a call journal their entry value, so the call
    /// teardown reverts exactly what the body touched instead of cloning
    /// the whole frame up front.
    fn assign(&mut self, ctx: &mut RunCtx<'_>, name: &str, value: Option<PineValue>) {
        // Any object write (or any write to a leased name, including
        // clearing it) invalidates the slot's ring lease — see bump_lease.
        let key = match self.var_kind.get(name) {
            Some(&is_varip) => LeaseKey::var_store(name, is_varip),
            None => LeaseKey::variables(name),
        };
        if matches!(value, Some(PineValue::Object(_))) || self.lease_versions.contains_key(&key) {
            self.bump_lease_key(key);
        }
        if let Some(&is_varip) = self.var_kind.get(name) {
            if let Some(present) = value {
                if is_varip {
                    ctx.vars_mut().set_varip(name, present);
                } else {
                    ctx.vars_mut().set_var(name, present);
                }
            } else if is_varip {
                ctx.vars_mut().clear_varip(name);
            } else {
                ctx.vars_mut().clear_var(name);
            }
        } else {
            self.journal_frame(name);
            self.variables.insert(name.to_owned(), value);
        }
    }

    /// Journal one plain frame write for the call teardown: the first
    /// write to a name inside a call records its entry value, so teardown
    /// reverts exactly the touched names. The entry double-wraps: `None`
    /// means the name was absent and teardown removes it; `Some(slot)`
    /// restores the slot (which itself may be absence). Persistent
    /// (`var`/`varip`) writes never journal — they persist exactly like
    /// the old frame-restore discipline, which never snapshotted them
    /// either. Suppressed while parameter binding and write-backs run
    /// (those have their own restore paths).
    fn journal_frame(&mut self, name: &str) {
        if self.journal_hold {
            return;
        }
        if let Some(base) = self.journal_base {
            if !self.journal[base..].iter().any(|(known, _)| known == name) {
                let old = self.variables.get(name).cloned();
                self.journal.push((name.to_owned(), old));
            }
        }
    }

    /// Route an assignment through dotted receiver paths: `this.field`
    /// writes update the innermost method receiver (which must declare the
    /// field); plain names use the variable stores. Rebinding `this` itself
    /// refuses — methods return it, never replace it.
    fn assign_path(
        &mut self,
        ctx: &mut RunCtx<'_>,
        name: &str,
        value: Option<PineValue>,
    ) -> Result<(), KernelError> {
        if let Some(field) = name.strip_prefix("this.") {
            let slot = self
                .receiver
                .last_mut()
                .ok_or_else(|| refused("'this' is only bound inside a method"))?;
            if !slot.instance.fields.contains_key(field) {
                return Err(refused(format!("unknown field 'this.{field}'")));
            }
            slot.instance.fields.insert(field.to_owned(), value);
            return Ok(());
        }
        if name == "this" {
            return Err(refused("cannot rebind 'this'"));
        }
        // Receiver-alias field writes (`level.aged := 0`) hit the live
        // slot, exactly like `this.` writes; anything else dotted keeps
        // the undeclared-target refusal below.
        if let Some((head, field)) = name.split_once('.') {
            if !field.contains('.') && self.write_alias_field(head, field, value.clone()) {
                return Ok(());
            }
        }
        self.assign(ctx, name, value);
        Ok(())
    }

    /// Innermost receiver slot bound under `alias`, if any. Parameters
    /// shadow outer bindings, so the search runs top-down: a nested call
    /// reusing its caller's alias still reaches the nested receiver.
    fn receiver_slot(&self, alias: &str) -> Option<usize> {
        self.receiver
            .iter()
            .rposition(|slot| slot.alias == alias)
    }

    /// Write one field through a receiver alias into the live slot.
    /// `true` when `alias` names a live receiver holding the field;
    /// unknown aliases and unknown fields answer `false` so the caller
    /// keeps its own refusal.
    fn write_alias_field(&mut self, alias: &str, field: &str, value: Option<PineValue>) -> bool {
        let Some(depth) = self.receiver_slot(alias) else {
            return false;
        };
        let Some(slot) = self.receiver.get_mut(depth) else {
            return false;
        };
        if !slot.instance.fields.contains_key(field) {
            return false;
        }
        slot.instance.fields.insert(field.to_owned(), value);
        true
    }

    /// Attach map-entry provenance to a taken home: a variable tagged by
    /// a `map.get` assignment writes its mutated instance back to the map
    /// entry as well as the variable (see [`ObjectHome::MapEntry`]).
    fn prov_home(&self, name: &str, inner: ObjectHome) -> ObjectHome {
        match self.map_prov.get(name) {
            Some((map_var, key, key_value)) => ObjectHome::MapEntry {
                inner: Box::new(inner),
                map_var: map_var.clone(),
                key: key.clone(),
                key_value: key_value.clone(),
            },
            None => inner,
        }
    }

    /// Lease key for a take home, or `None` where leasing never applies
    /// (lexical receivers, nested paths, and map entries run the
    /// hydrate-every-tick bridge as today — correct, just uncached).
    fn lease_key_for(home: &ObjectHome) -> Option<LeaseKey> {
        match home {
            ObjectHome::Variables(name) => Some(LeaseKey::variables(name)),
            ObjectHome::VarStore(name, is_varip) => Some(LeaseKey::var_store(name, *is_varip)),
            ObjectHome::ThisField(_)
            | ObjectHome::NestedField { .. }
            | ObjectHome::MapEntry { .. } => None,
        }
    }

    /// Bump one slot's lease version. The single Pine-side invalidation
    /// rule: every write through [`EngineState::put_object`] (with its bump
    /// flag), [`EngineState::assign`], and `var` declarations funnels here,
    /// so no second bump logic can forget a writer. Native serves never
    /// bump — they sync the lease entry instead. Over-bumping only costs
    /// one re-hydrate; under-bumping would serve stale state, so every
    /// uncertain path bumps.
    fn bump_lease_key(&mut self, key: LeaseKey) {
        let version = self.lease_versions.entry(key).or_insert(0);
        *version = version.wrapping_add(1);
    }

    /// Bump the lease version for a take home: plain homes bump, nested
    /// and map-entry homes bump their root, lexical-receiver takes need no
    /// bump (the enclosing take brackets them and bumps on write-back).
    fn bump_lease(&mut self, home: &ObjectHome) {
        match home {
            ObjectHome::Variables(name) => {
                self.bump_lease_key(LeaseKey::variables(name));
            }
            ObjectHome::VarStore(name, is_varip) => {
                self.bump_lease_key(LeaseKey::var_store(name, *is_varip));
            }
            ObjectHome::NestedField { home, .. } => self.bump_lease(home),
            ObjectHome::MapEntry { inner, .. } => self.bump_lease(inner),
            ObjectHome::ThisField(_) => {}
        }
    }

    /// Take an instance out of its home for a method call, remembering where
    /// to write it back. `None` means the name holds no instance (the caller
    /// falls through to the unknown-method refusal).
    fn take_object(
        &mut self,
        ctx: &mut RunCtx<'_>,
        name: &str,
    ) -> Result<Option<(UserInstance, ObjectHome)>, KernelError> {
        if let Some(field) = name.strip_prefix("this.") {
            let slot = self
                .receiver
                .last_mut()
                .ok_or_else(|| refused("'this' is only bound inside a method"))?;
            let slot = slot
                .instance
                .fields
                .get_mut(field)
                .ok_or_else(|| refused(format!("unknown field 'this.{field}'")))?;
            return match slot.take() {
                Some(PineValue::Object(instance)) => {
                    Ok(Some((instance, ObjectHome::ThisField(field.to_owned()))))
                }
                other => {
                    *slot = other;
                    Ok(None)
                }
            };
        }
        if let Some(slot) = self.variables.remove(name) {
            return match slot {
                Some(PineValue::Object(instance)) => {
                    let home = self.prov_home(name, ObjectHome::Variables(name.to_owned()));
                    Ok(Some((instance, home)))
                }
                other => {
                    self.variables.insert(name.to_owned(), other);
                    Ok(None)
                }
            };
        }
        if let Some(PineValue::Object(instance)) = ctx.vars().get_var(name) {
            ctx.vars_mut().clear_var(name);
            let home = self.prov_home(name, ObjectHome::VarStore(name.to_owned(), false));
            return Ok(Some((instance, home)));
        }
        if let Some(PineValue::Object(instance)) = ctx.vars().get_varip(name) {
            ctx.vars_mut().clear_varip(name);
            let home = self.prov_home(name, ObjectHome::VarStore(name.to_owned(), true));
            return Ok(Some((instance, home)));
        }
        Ok(None)
    }

    /// Take a nested field instance (`bars.hist_l`) out of its root object
    /// for a method call, remembering the parent chain for write-back.
    /// `None` means the path holds no object (unknown root, unknown field,
    /// or non-object field) and the caller keeps its own refusal; taken
    /// pieces restore before declining so a failed take never unbinds.
    fn take_nested_object(
        &mut self,
        ctx: &mut RunCtx<'_>,
        path: &str,
    ) -> Result<Option<(UserInstance, ObjectHome)>, KernelError> {
        let Some((root, rest)) = path.split_once('.') else {
            return Ok(None);
        };
        let Some((mut current, home)) = self.take_object(ctx, root)? else {
            return Ok(None);
        };
        let mut parents: Vec<(UserInstance, String)> = Vec::new();
        for field in rest.split('.') {
            let taken = current.fields.get_mut(field).map(Option::take);
            match taken {
                Some(Some(PineValue::Object(next))) => {
                    let parent = std::mem::replace(&mut current, next);
                    parents.push((parent, field.to_owned()));
                }
                Some(other) => {
                    current.fields.insert(field.to_owned(), other);
                    self.untake_nested(ctx, home, parents, current)?;
                    return Ok(None);
                }
                None => {
                    self.untake_nested(ctx, home, parents, current)?;
                    return Ok(None);
                }
            }
        }
        let home = ObjectHome::NestedField {
            home: Box::new(home),
            parents,
        };
        Ok(Some((current, home)))
    }

    /// Restore a partially taken nested path after a declined take: the
    /// innermost taken instance returns first, rebuilding outward, and the
    /// root returns to its own home.
    fn untake_nested(
        &mut self,
        ctx: &mut RunCtx<'_>,
        home: ObjectHome,
        mut parents: Vec<(UserInstance, String)>,
        mut current: UserInstance,
    ) -> Result<(), KernelError> {
        while let Some((mut parent, field)) = parents.pop() {
            parent.fields.insert(field, Some(PineValue::Object(current)));
            current = parent;
        }
        self.put_object(ctx, home, current, false)
    }

    /// Write a (possibly mutated) instance back to the home it came from.
    /// `bump` invalidates the slot's ring lease for Pine-side writes; native
    /// serves pass `false` (they sync the lease entry instead), as do
    /// identical restores that wrote nothing.
    fn put_object(
        &mut self,
        ctx: &mut RunCtx<'_>,
        home: ObjectHome,
        instance: UserInstance,
        bump: bool,
    ) -> Result<(), KernelError> {
        if bump {
            self.bump_lease(&home);
        }
        let value = PineValue::Object(instance);
        match home {
            ObjectHome::Variables(name) => {
                self.variables.insert(name, Some(value));
            }
            ObjectHome::VarStore(name, is_varip) => {
                if is_varip {
                    ctx.vars_mut().set_varip(&name, value);
                } else {
                    ctx.vars_mut().set_var(&name, value);
                }
            }
            ObjectHome::ThisField(field) => {
                let slot = self
                    .receiver
                    .last_mut()
                    .ok_or_else(|| refused("'this' is only bound inside a method"))?;
                slot.instance.fields.insert(field, Some(value));
            }
            ObjectHome::NestedField { home, mut parents } => {
                let mut current = value;
                while let Some((mut parent, field)) = parents.pop() {
                    parent.fields.insert(field, Some(current));
                    current = PineValue::Object(parent);
                }
                let PineValue::Object(root) = current else {
                    return Err(refused("nested field home holds an object"));
                };
                self.put_object(ctx, *home, root, false)?;
            }
            ObjectHome::MapEntry {
                inner,
                map_var,
                key,
                key_value,
            } => {
                let PineValue::Object(inst) = value else {
                    return Err(refused("map entry holds an object"));
                };
                self.put_object(ctx, *inner, inst.clone(), false)?;
                self.write_map_entry(ctx, &map_var, &key, &key_value, PineValue::Object(inst));
            }
        }
        Ok(())
    }

    /// Store one mutated object back into a general-map entry: reads the
    /// map variable from either home, replaces the entry, and assigns it
    /// back through the same routing. Missing or non-map holders skip
    /// silently — the variable home already carries the mutation. Runs
    /// unjournaled: the entry update must persist, never revert.
    fn write_map_entry(
        &mut self,
        ctx: &mut RunCtx<'_>,
        map_var: &str,
        key: &str,
        key_value: &PineValue,
        value: PineValue,
    ) {
        let current = self
            .variables
            .get(map_var)
            .cloned()
            .flatten()
            .or_else(|| ctx.vars().get_var(map_var))
            .or_else(|| ctx.vars().get_varip(map_var));
        if let Some(PineValue::Map(entries)) = current {
            let mut stored = entries;
            stored.insert(key.to_owned(), (key_value.clone(), Some(value)));
            self.journal_hold = true;
            self.assign(ctx, map_var, Some(PineValue::Map(stored)));
            self.journal_hold = false;
        }
    }

    /// Refresh plot-variable tags after an assignment: a bare name bound
    /// from a `plot()` call tags the variable with the call's capture site
    /// so `fill(plot_var, ...)` pairs it; anything else clears a stale tag.
    fn refresh_plot_var(&mut self, name: &str, rhs: &PineExpr) {
        match rhs {
            PineExpr::FunctionCall { name: callee, args, .. } if callee == "plot" => {
                self.plot_vars.insert(name.to_owned(), plot_site_key(args));
            }
            _ => {
                self.plot_vars.remove(name);
            }
        }
    }

    /// Refresh map-entry provenance after an assignment: a bare name bound
    /// from `map.get(map_var, key)` evaluating to an object tags the
    /// variable so method calls write through to the entry; anything else
    /// clears a stale tag.
    fn refresh_prov(
        &mut self,
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
        name: &str,
        rhs: &PineExpr,
        assigned: Option<&PineValue>,
    ) -> Result<(), KernelError> {
        self.map_prov.remove(name);
        if !matches!(assigned, Some(PineValue::Object(_))) {
            return Ok(());
        }
        if let PineExpr::MethodCall {
            namespace,
            method,
            args,
            ..
        } = rhs
        {
            if namespace == "map" && method == "get" {
                if let [map_arg, key_arg] = args.as_slice() {
                    if let PineExpr::Variable(map_var) = &map_arg.value {
                        if let Some(key_value) = self.eval_expr(&key_arg.value, ctx, legs)? {
                            if let Some(key) = crate::value::map_key_of(&key_value) {
                                self.map_prov.insert(
                                    name.to_owned(),
                                    (map_var.clone(), key, key_value),
                                );
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Evaluate one expression; `None` is Pine `na`.
    fn eval_expr(
        &mut self,
        expr: &PineExpr,
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<Option<PineValue>, KernelError> {
        match expr {
            PineExpr::Number(v) => Ok(Some(PineValue::Float(*v))),
            PineExpr::StringLiteral(text) => Ok(Some(PineValue::Str(text.clone()))),
            PineExpr::Boolean(flag) => Ok(Some(PineValue::Bool(*flag))),
            PineExpr::Variable(name) => self.read_variable(name, ctx, legs),
            PineExpr::BuiltinVariable(var) => Ok(read_leg(legs, builtin_name(*var), ctx.cursor())),
            PineExpr::Binary { left, op, right } => {
                let first = self.eval_expr(left, ctx, legs)?;
                let second = self.eval_expr(right, ctx, legs)?;
                binary(*op, first.as_ref(), second.as_ref())
            }
            PineExpr::Unary { op, expr } => {
                let value = self.eval_expr(expr, ctx, legs)?;
                unary(*op, value.as_ref())
            }
            PineExpr::FunctionCall { name, args, .. } => self.call(name, args, ctx, legs),
            PineExpr::MethodCall {
                namespace,
                method,
                args,
                type_args,
            } => self.call_method(namespace, method, args, type_args, ctx, legs),
            PineExpr::InstanceCall {
                receiver,
                method,
                args,
                ..
            } => {
                // Chained construction runs against the evaluated instance;
                // an rvalue has no home, so the mutated copy is dropped and
                // only the returned value flows on.
                match self.eval_expr(receiver, ctx, legs)? {
                    Some(PineValue::Object(instance)) => {
                        let outcome =
                            self.run_method(instance, method, args, ctx, legs, None)?;
                        outcome.value
                    }
                    Some(other) => Err(refused(format!(
                        "method '{method}' needs an object receiver, got {other}"
                    ))),
                    None => Err(refused(format!("method '{method}' called on na"))),
                }
            }
            PineExpr::FieldRead { receiver, field } => {
                let value = self.eval_expr(receiver, ctx, legs)?;
                match value {
                    Some(PineValue::Object(instance)) => match instance.fields.get(field) {
                        Some(bound) => Ok(bound.clone()),
                        None => Err(refused(format!(
                            "unknown field '{field}' on '{}'",
                            instance.type_name
                        ))),
                    },
                    Some(other) => Err(refused(format!(
                        "field '{field}' needs an object receiver, got {other}"
                    ))),
                    None => Err(refused(format!("field '{field}' read on na"))),
                }
            }
            PineExpr::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                // An `na` condition yields `na` (Pine never branches on
                // absence); numbers still refuse.
                match branch_of(
                    "ternary condition",
                    self.eval_expr(condition, ctx, legs)?.as_ref(),
                )? {
                    Some(true) => self.eval_expr(then_expr, ctx, legs),
                    Some(false) => self.eval_expr(else_expr, ctx, legs),
                    None => Ok(None),
                }
            }
            PineExpr::ArrayAccess { array, index } => {
                let position = self.eval_expr(index, ctx, legs)?;
                let Some(at) = position else {
                    return Ok(None);
                };
                let at = whole_number("array index", Some(&at))?;
                let target = self.eval_expr(array, ctx, legs)?;
                if let Some(PineValue::Array(items)) = target {
                    let slot = usize::try_from(at)
                        .map_err(|_| refused(format!("negative array index {at}")))?;
                    match items.get(slot).cloned() {
                        Some(value) => Ok(value),
                        None => Err(refused(format!("array index {slot} out of bounds"))),
                    }
                } else {
                    let name: &str = match array.as_ref() {
                        PineExpr::Variable(name) => name,
                        PineExpr::BuiltinVariable(var) => builtin_name(*var),
                        _ => {
                            return Err(refused(
                                "history offsets read named series, not compound expressions",
                            ));
                        }
                    };
                    self.offset_name(name, at, ctx, legs)
                }
            }
            PineExpr::ArrayLiteral(items) => {
                let mut bound = Vec::with_capacity(items.len());
                for item in items {
                    bound.push(self.eval_expr(item, ctx, legs)?);
                }
                Ok(Some(PineValue::Array(bound)))
            }
            PineExpr::ColorLiteral(packed) => {
                Ok(Some(PineValue::ColorHandle(ColorHandle::new(*packed))))
            }
            PineExpr::SeriesOffset { series, offset } => {
                let name: &str = match series.as_ref() {
                    PineExpr::Variable(name) => name,
                    PineExpr::BuiltinVariable(var) => builtin_name(*var),
                    _ => {
                        return Err(refused(
                            "history offsets read named series, not compound expressions",
                        ));
                    }
                };
                self.offset_name(name, i64::from(*offset), ctx, legs)
            }
            PineExpr::Switch(switch) => self.eval_switch(switch, ctx, legs, true),
        }
    }

    /// One name lookup: series variables, then persistent stores, then the
    /// method receiver (`this`/`this.field`), `bar_index`, legs, registered
    /// enum members (trailing `Enum.Member`, any module prefix), then the
    /// opaque display constants, then the `na` constant. Anything else is an
    /// undefined variable.
    fn read_variable(
        &self,
        name: &str,
        ctx: &RunCtx<'_>,
        legs: &Legs,
    ) -> Result<Option<PineValue>, KernelError> {
        // Receiver aliases shadow every outer binding: a body spells its
        // receiver by the declared parameter name, and nested calls may
        // reuse the same spelling for their own receiver.
        if let Some((head, rest)) = name.split_once('.') {
            if !rest.contains('.') {
                if let Some(depth) = self.receiver_slot(head) {
                    return match self.receiver[depth].instance.fields.get(rest) {
                        Some(bound) => Ok(bound.clone()),
                        None => Err(refused(format!("unknown field '{head}.{rest}'"))),
                    };
                }
            }
        } else if let Some(depth) = self.receiver_slot(name) {
            return Ok(Some(PineValue::Object(self.receiver[depth].instance.clone())));
        }
        if let Some(bound) = self.variables.get(name) {
            return Ok(bound.clone());
        }
        if let Some(present) = ctx.vars().get_var(name) {
            return Ok(Some(present));
        }
        if let Some(present) = ctx.vars().get_varip(name) {
            return Ok(Some(present));
        }
        if name == "this" {
            return self
                .receiver
                .last()
                .map(|slot| {
                    Some(PineValue::Object(slot.instance.clone()))
                })
                .ok_or_else(|| refused("'this' is only bound inside a method"));
        }
        if let Some(field) = name.strip_prefix("this.") {
            let slot = self
                .receiver
                .last()
                .ok_or_else(|| refused("'this' is only bound inside a method"))?;
            return match slot.instance.fields.get(field) {
                Some(bound) => Ok(bound.clone()),
                None => Err(refused(format!("unknown field 'this.{field}'"))),
            };
        }
        if name == "bar_index" {
            let index = i64::try_from(ctx.cursor())
                .map_err(|_| refused("bar index does not fit this run"))?;
            return Ok(Some(PineValue::Int(index)));
        }
        if let Some(member) = name.strip_prefix("barstate.") {
            return barstate_flag(member, ctx, legs, self.tick_mode())
                .map(PineValue::Bool)
                .map(Some)
                .ok_or_else(|| {
                    refused(format!(
                        "unknown barstate member '{member}'; closed-bar replay supports isfirst, isnew, isconfirmed, islast, ishistory, and isrealtime"
                    ))
                });
        }
        if name == "timenow" {
            if self.tick_mode() {
                return Ok(ctx
                    .current()
                    .and(self.tick_times.get(ctx.cursor()).copied())
                    .map(PineValue::Int));
            }
            return Ok(read_leg(legs, "time", ctx.cursor()));
        }
        if name == "syminfo.tickerid" {
            return Ok(Some(PineValue::Str(self.chart_symbol.clone())));
        }
        if name == "timeframe.period" {
            return Ok(Some(PineValue::Str(self.chart_period.clone())));
        }
        if let Some(leg) = legs.named(name) {
            return Ok(read_leg_slice(leg, ctx.cursor()));
        }
        if let Some(member) = self.enum_lookup(name) {
            return Ok(Some(member));
        }
        if let Some(constant) = super::builtins_core::resolve_constant(name) {
            return Ok(Some(constant));
        }
        // Field reads on variable-held instances (`mid.price_low`): the
        // head must already bind an object; unknown fields refuse. This
        // is the value-position twin of the `this.` read above; anything
        // that is not an object falls through to the alias rule below.
        // Alias-qualified declaration reads (`UTIL.OSC_UPPER_RANGE`): the
        // head must be an import alias of the current module; the remainder
        // then reads exactly like a bare name. This spells the shared
        // declaration bindings the way the source does — functions and
        // methods keep their own dispatch gate, so nothing unexported
        // becomes reachable through here.
        if let Some((head, rest)) = name.split_once('.') {
            if !rest.contains('.') {
                let home = match self.variables.get(head) {
                    Some(slot) => slot.clone(),
                    None => ctx
                        .vars()
                        .get_var(head)
                        .or_else(|| ctx.vars().get_varip(head)),
                };
                if let Some(PineValue::Object(instance)) = home {
                    return match instance.fields.get(rest) {
                        Some(bound) => Ok(bound.clone()),
                        None => Err(refused(format!("unknown field '{head}.{rest}'"))),
                    };
                }
                if self
                    .module_aliases
                    .contains_key(&(self.current_module.clone(), head.to_owned()))
                {
                    return self.read_variable(rest, ctx, legs);
                }
            }
        }
        if name == "na" {
            return Ok(None);
        }
        // A declared persistent (`var`/`varip`) reads as absence until its
        // first assignment: `var float eit = na` binds `na`, it does not
        // leave the name undeclared. Only truly undeclared names refuse.
        if self.var_kind.contains_key(name) {
            return Ok(None);
        }
        Err(refused(format!("undefined variable '{name}'")))
    }

    /// Resolve a trailing `Enum.Member` pair against the registered enums.
    /// Longer dotted names (module aliases like `UTIL.Smoothings.WMA`)
    /// resolve on their last two segments.
    fn enum_lookup(&self, name: &str) -> Option<PineValue> {
        let mut parts = name.rsplit('.');
        let member = parts.next()?;
        let enum_name = parts.next()?;
        self.enums
            .get(enum_name)?
            .contains_key(member)
            .then(|| PineValue::EnumHandle(EnumHandle::new(enum_name, member)))
    }

    /// History and element reads by resolved name. Array bindings index by
    /// element (out of bounds refuses, mirroring the run-error the language
    /// raises); everything else reads history by bars back.
    fn offset_name(
        &self,
        name: &str,
        bars_back: i64,
        ctx: &RunCtx<'_>,
        legs: &Legs,
    ) -> Result<Option<PineValue>, KernelError> {
        if bars_back < 0 {
            return Err(refused(format!(
                "negative history offset {bars_back} is not a series read"
            )));
        }
        let back = usize::try_from(bars_back).map_err(|_| {
            refused(format!("history offset {bars_back} does not fit this run"))
        })?;
        if name == "bar_index" {
            // The cursor is a computed series, never a recorded one:
            // `bar_index[n]` reads the cursor `n` bars back, `na` when the
            // run has no such bar yet.
            return Ok(ctx
                .cursor()
                .checked_sub(back)
                .and_then(|index| i64::try_from(index).ok())
                .map(PineValue::Int));
        }
        if let Some(leg) = legs.named(name) {
            let at = ctx.cursor().checked_sub(back);
            return Ok(at.and_then(|index| read_leg_slice(leg, index)));
        }
        if let Some(Some(PineValue::Array(items))) = self.current_binding(name, ctx) {
            return match items.get(back).cloned() {
                Some(value) => Ok(value),
                None => Err(refused(format!("array index {back} out of bounds"))),
            };
        }
        let bound = self.current_binding(name, ctx);
        let recorded = self.history.contains_key(name)
            || self.var_kind.contains_key(name)
            || self
                .call_stack
                .last()
                .is_some_and(|top| self.history.contains_key(&format!("{top}#{name}")));
        if bound.is_none() && !recorded {
            return Err(refused(format!("undefined variable '{name}'")));
        }
        if back == 0 {
            return Ok(bound.flatten());
        }
        let position = self.snapshots.len().checked_sub(back);
        // The innermost scope's past wins; the bare name stays as the
        // fallback for top-level series and persistent vars shared across
        // scopes. A scope that never recorded the name contributes no
        // value rather than another scope's past.
        if let Some(top) = self.call_stack.last() {
            let qualified = format!("{top}#{name}");
            if self.history.contains_key(&qualified) {
                return Ok(position
                    .and_then(|index| {
                        self.history
                            .get(&qualified)
                            .and_then(|slot| slot.get(index).cloned())
                    })
                    .flatten());
            }
        }
        Ok(position
            .and_then(|index| self.history.get(name).and_then(|slot| slot.get(index).cloned()))
            .flatten())
    }

    /// The in-progress bar's binding: series variables, then persistent
    /// stores. The outer `None` means unbound here and now, which is
    /// distinct from bound-to-`na` — hence the nested option.
    #[allow(clippy::option_option)]
    fn current_binding(
        &self,
        name: &str,
        ctx: &RunCtx<'_>,
    ) -> Option<Option<PineValue>> {
        if let Some(bound) = self.variables.get(name) {
            return Some(bound.clone());
        }
        if let Some(present) = ctx.vars().get_var(name) {
            return Some(Some(present));
        }
        ctx.vars().get_varip(name).map(Some)
    }

    /// Calls: the `na`/`nz`/`fixnan` specials, `plot` capture, registered
    /// user functions, else a refusal naming the callee.
    fn call(
        &mut self,
        name: &str,
        args: &[CallArg],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<Option<PineValue>, KernelError> {
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.eval_expr(&arg.value, ctx, legs)?);
        }
        match name {
            "na" => match values.len() {
                0 => Ok(None),
                1 => Ok(Some(PineValue::Bool(is_absent(values[0].as_ref())))),
                _ => Err(refused("'na' takes zero or one argument")),
            },
            "nz" => match values.len() {
                1 => Ok(Some(nz(values[0].clone(), PineValue::Float(0.0)))),
                2 => {
                    let filled = match (&values[0], &values[1]) {
                        (Some(present), _) => Some(present.clone()),
                        (None, Some(default)) => Some(default.clone()),
                        (None, None) => None,
                    };
                    Ok(filled)
                }
                _ => Err(refused("'nz' takes one or two arguments")),
            },
            "fixnan" => {
                if values.len() != 1 || args.is_empty() {
                    return Err(refused("'fixnan' takes exactly one argument"));
                }
                let site = format!("{:?}", args[0].value);
                let current = values[0].clone().filter(|v| !is_absent_value(v));
                if let Some(present) = current {
                    self.fixnan_memory.insert(site, Some(present.clone()));
                    Ok(Some(present))
                } else {
                    Ok(self.fixnan_memory.get(&site).cloned().unwrap_or(None))
                }
            }
            // Whole-number cast: truncation toward zero, absence stays
            // absence; anything out of range or non-numeric refuses.
            "int" => match values.len() {
                1 => Ok(int_cast(values[0].as_ref())?),
                _ => Err(refused("'int' takes exactly one argument")),
            },
            // Float cast: integers convert exactly at market magnitudes,
            // floats pass through, absence stays absence; anything
            // non-numeric refuses (mirrors the whole-number cast above).
            "float" => match values.len() {
                1 => Ok(float_cast(values[0].as_ref())?),
                _ => Err(refused("'float' takes exactly one argument")),
            },
            // Bare color cast: a single color passes through, absence stays
            // absence; anything else refuses (indicators spell `color(na)`
            // for the transparent pen).
            "color" => match values.len() {
                1 => match &values[0] {
                    None => Ok(None),
                    Some(PineValue::ColorHandle(_)) => Ok(values[0].clone()),
                    Some(other) => Err(refused(format!("'color' takes a color, got {other}"))),
                },
                _ => Err(refused("'color' takes exactly one argument")),
            },
            "plot" => {
                if values.is_empty() {
                    return Err(refused("'plot' takes a series to draw"));
                }
                let site = plot_site_key(args);
                let series = values[0].clone();
                // Pine's title rides second positionally
                // (`plot(series, "Name", ...)`); a named `title` wins when
                // both spell it.
                let positional_title = args
                    .iter()
                    .zip(values.iter())
                    .filter(|(arg, _)| arg.name.is_none())
                    .nth(1)
                    .and_then(|(_, value)| match value {
                        Some(PineValue::Str(text)) => Some(text.clone()),
                        _ => None,
                    });
                let title = args
                    .iter()
                    .zip(values.iter())
                    .find(|(arg, _)| arg.name.as_deref() == Some("title"))
                    .and_then(|(_, value)| match value {
                        Some(PineValue::Str(text)) => Some(text.clone()),
                        _ => None,
                    })
                    .or(positional_title)
                    .unwrap_or_else(|| format!("plot:{}", self.plots.len()));
                let display = args
                    .iter()
                    .zip(values.iter())
                    .find(|(arg, _)| arg.name.as_deref() == Some("display"))
                    .and_then(|(_, value)| value.clone());
                match self.plots.iter_mut().find(|plot| plot.site == site) {
                    Some(plot) => {
                        plot.values.push(series);
                        plot.display = display;
                    }
                    None => self.plots.push(PlotCapture {
                        site,
                        title,
                        display,
                        values: vec![series],
                    }),
                }
                Ok(None)
            }
            "plotcandle" => self.call_plotcandle(args, &values),
            "hline" => {
                // Level capture mirrors `plotshape`: constant or series, one
                // value per bar under the call-site key.
                if values.is_empty() {
                    return Err(refused("'hline' takes a price to draw"));
                }
                let site = format!("hline@{args:?}");
                let series = values[0].clone();
                let title = call_title(args, &values, &format!("hline:{}", self.hlines.len()));
                match self.hlines.iter_mut().find(|line| line.site == site) {
                    Some(line) => {
                        line.values.push(series);
                    }
                    None => self.hlines.push(HlineCapture {
                        site,
                        title,
                        values: vec![series],
                    }),
                }
                Ok(None)
            }
            "plotshape" => {
                // Marker capture mirrors `plot`: the series value records
                // per bar under the call-site key; style/location/text ride
                // the call, evaluate, and drop (like `plot`'s color).
                if values.is_empty() {
                    return Err(refused("'plotshape' takes a series to draw"));
                }
                let site = format!("plotshape@{args:?}");
                let series = values[0].clone();
                let title = call_title(args, &values, &format!("plotshape:{}", self.plotshapes.len()));
                match self
                    .plotshapes
                    .iter_mut()
                    .find(|shape| shape.site == site)
                {
                    Some(shape) => {
                        shape.values.push(series);
                    }
                    None => self.plotshapes.push(PlotShapeCapture {
                        site,
                        title,
                        values: vec![series],
                    }),
                }
                Ok(None)
            }
            "barcolor" => self.call_barcolor(args, &values),
            "fill" => self.call_fill(args, &values),
            "input" => {
                // The bare overload infers its kind: a series-shaped first
                // argument (leg, variable, or offset) is the source input
                // whose default rides the live series through; literal and
                // computed defaults infer from their shape.
                let method = bare_input_method(args.first().map(|arg| &arg.value), values.first())?;
                let core_args: Vec<super::builtins_core::CoreArg> = args
                    .iter()
                    .zip(values)
                    .map(|(arg, value)| super::builtins_core::CoreArg {
                        name: arg.name.clone(),
                        value,
                    })
                    .collect();
                let site = format!("input@{args:?}");
                super::builtins_core::call("input", method, &site, &core_args, &[], &mut self.core_inputs)
            }
            "time" => self.call_session_time(&values, ctx, legs),
            // Alert declarations evaluate their condition and record
            // nothing: alerts cannot fire headlessly, and they draw
            // nothing, so there is no capture vector for them. Argument
            // evaluation errors still refuse above.
            "alertcondition" => Ok(None),
            _ if self
                .functions
                .contains_key(&(self.current_module.clone(), name.to_owned())) =>
            {
                let key = (self.current_module.clone(), name.to_owned());
                let overloads = self
                    .functions
                    .get(&key)
                    .cloned()
                    .expect("function checked above");
                let (params, body) = select_function(name, &overloads, args, &values)?;
                let site = format!("{}::{name}@{args:?}", self.current_module);
                let module = self.current_module.clone();
                self.exec_call(site, &module, &params, args, &values, &body, ctx, legs)
            }
            _ => Err(refused(format!("unknown function '{name}'"))),
        }
    }

    /// Capture one `plotcandle(open, high, low, close, ...)` bar: four
    /// positional legs plus the last-seen display handles, appended to the
    /// call site's capture. Fewer than four positional legs refuse; the
    /// call itself evaluates to absence like `plot`.
    fn call_plotcandle(
        &mut self,
        args: &[CallArg],
        values: &[Option<PineValue>],
    ) -> Result<Option<PineValue>, KernelError> {
        let legs: Vec<Option<PineValue>> = args
            .iter()
            .zip(values.iter())
            .filter(|(arg, _)| arg.name.is_none())
            .map(|(_, value)| value.clone())
            .collect();
        if legs.len() != 4 {
            return Err(refused(format!(
                "'plotcandle' takes open, high, low, close positionally, got {} positional series",
                legs.len()
            )));
        }
        let [open, high, low, close] = legs
            .try_into()
            .expect("four legs checked above");
        let site = format!("plotcandle@{args:?}");
        let title = named_title(args, values, &format!("plotcandle:{}", self.plotcandles.len()));
        let display = named_value(args, values, "display");
        let color = named_value(args, values, "color");
        let wickcolor = named_value(args, values, "wickcolor");
        let bordercolor = named_value(args, values, "bordercolor");
        match self
            .plotcandles
            .iter_mut()
            .find(|candle| candle.site == site)
        {
            Some(candle) => {
                candle.opens.push(open);
                candle.highs.push(high);
                candle.lows.push(low);
                candle.closes.push(close);
                candle.display = display;
                candle.color = color;
                candle.wickcolor = wickcolor;
                candle.bordercolor = bordercolor;
            }
            None => self.plotcandles.push(PlotCandleCapture {
                site,
                title,
                display,
                color,
                wickcolor,
                bordercolor,
                opens: vec![open],
                highs: vec![high],
                lows: vec![low],
                closes: vec![close],
            }),
        }
        Ok(None)
    }

    /// Capture one `barcolor(series, ...)` bar: the conditional color series
    /// (absence stays absence, never a default). The call itself evaluates
    /// to absence like `plot`.
    fn call_barcolor(
        &mut self,
        args: &[CallArg],
        values: &[Option<PineValue>],
    ) -> Result<Option<PineValue>, KernelError> {
        let Some(series) = args
            .iter()
            .zip(values.iter())
            .find(|(arg, _)| arg.name.is_none())
            .map(|(_, value)| value.clone())
        else {
            return Err(refused("'barcolor' takes a color series"));
        };
        let site = format!("barcolor@{args:?}");
        let title = named_title(args, values, &format!("barcolor:{}", self.barcolors.len()));
        match self.barcolors.iter_mut().find(|paint| paint.site == site) {
            Some(paint) => paint.values.push(series),
            None => self.barcolors.push(BarColorCapture {
                site,
                title,
                values: vec![series],
            }),
        }
        Ok(None)
    }

    /// Capture one `fill(plot(a), plot(b), ...)` bar: the two inline `plot`
    /// calls already executed (and captured) while evaluating the
    /// arguments, so the pairing re-derives each plot's site key from its
    /// argument list — the same key the `plot` arm recorded this bar — and
    /// stores the per-bar region between their latest values. Variables
    /// bound from a `plot()` call pair through their recorded site tag.
    /// The six-positional gradient form (`plot, plot, top, bottom,
    /// top color, bottom color`) pairs the same way; the top/bottom values
    /// parameterize the color ramp (the full call text rides the site key)
    /// while the region still spans the two plots. Anything else refuses
    /// instead of guessing a pairing.
    fn call_fill(
        &mut self,
        args: &[CallArg],
        values: &[Option<PineValue>],
    ) -> Result<Option<PineValue>, KernelError> {
        let positional: Vec<(&CallArg, &Option<PineValue>)> = args
            .iter()
            .zip(values.iter())
            .filter(|(arg, _)| arg.name.is_none())
            .collect();
        if positional.len() != 2 && positional.len() != 6 {
            return Err(refused(format!(
                "'fill' takes two plot calls (or two plus top/bottom value/color bounds), got {} positional arguments",
                positional.len()
            )));
        }
        let mut keys = Vec::with_capacity(2);
        for (arg, _) in positional.iter().take(2) {
            match &arg.value {
                PineExpr::FunctionCall { name, args: inner, .. } if name == "plot" => {
                    keys.push(plot_site_key(inner));
                }
                PineExpr::Variable(var) => match self.plot_vars.get(var) {
                    Some(site) => keys.push(site.clone()),
                    None => {
                        return Err(refused(format!(
                            "fill names variable '{var}' with no plot tag this run"
                        )));
                    }
                },
                _ => {
                    return Err(refused(
                        "'fill' pairs two plot calls like fill(plot(a), plot(b)); raw series have no plot site to pair",
                    ));
                }
            }
        }
        let mut legs = Vec::with_capacity(2);
        let mut titles = Vec::with_capacity(2);
        for key in &keys {
            let Some(plot) = self.plots.iter().find(|plot| &plot.site == key) else {
                return Err(refused(format!(
                    "fill names a plot site with no capture this bar: {key}"
                )));
            };
            titles.push(plot.title.clone());
            legs.push(plot.values.last().cloned().unwrap_or(None));
        }
        let region = match (&legs[0], &legs[1]) {
            (None, None) => None,
            _ => Some(PineValue::Array(legs)),
        };
        let site = format!("fill@{args:?}");
        let title = named_title(args, values, &format!("fill:{}", self.fills.len()));
        // The gradient form carries its inks positionally (top color, then
        // bottom color); a named `color` wins when present, the top color
        // is the fallback, the bottom the last resort.
        let gradient_ink = positional
            .get(4)
            .and_then(|(_, value)| (*value).clone())
            .or_else(|| positional.get(5).and_then(|(_, value)| (*value).clone()));
        let color = named_value(args, values, "color").or(gradient_ink);
        match self.fills.iter_mut().find(|fill| fill.site == site) {
            Some(fill) => {
                fill.values.push(region);
                fill.color = color;
            }
            None => self.fills.push(FillCapture {
                site,
                title,
                first: keys[0].clone(),
                second: keys[1].clone(),
                first_title: titles[0].clone(),
                second_title: titles[1].clone(),
                color,
                values: vec![region],
            }),
        }
        Ok(None)
    }

    /// Session-aware `time` reads. The harness injects only in-session bars,
    /// so a chart-period call returns the current bar timestamp after
    /// validating its arguments. Any other timeframe or timezone refuses
    /// instead of guessing a mapping.
    fn call_session_time(
        &self,
        values: &[Option<PineValue>],
        ctx: &RunCtx<'_>,
        legs: &Legs,
    ) -> Result<Option<PineValue>, KernelError> {
        let period = match values.first() {
            Some(Some(PineValue::Str(period))) => period,
            Some(None) => return Ok(None),
            Some(Some(other)) => {
                return Err(refused(format!("time takes a timeframe string, got {other}")));
            }
            _ => return Err(refused("time takes a timeframe and an optional session")),
        };
        if period != &self.chart_period {
            return Err(refused(format!(
                "time supports only the chart period '{}' headlessly, got {period:?}; inject that timeframe's bars as the chart run instead",
                self.chart_period
            )));
        }
        if values.len() > 2 {
            return Err(refused(
                "time timezone mapping is not supported headlessly; pass chart-period bars already selected for the session",
            ));
        }
        match values.get(1) {
            Some(Some(PineValue::Str(session))) => {
                if session.trim().is_empty() {
                    return Err(refused("time takes a non-empty session string"));
                }
                Ok(read_leg(legs, "time", ctx.cursor()))
            }
            Some(None) => Ok(None),
            Some(Some(other)) => Err(refused(format!(
                "time takes a session string, got {other}"
            ))),
            None => Ok(read_leg(legs, "time", ctx.cursor())),
        }
    }

    /// Serve an injected lower-timeframe dataset to
    /// `request.security_lower_tf(symbol, timeframe, [legs...],
    /// calc_bars_count = N)`: the expression list must name bare chart legs
    /// (the pull path asks for `[time, open, high, low, close, volume]`),
    /// answered column-wise from the registered `(symbol, timeframe)`
    /// dataset in request order, tail-taken to the last `calc_bars_count`
    /// rows. Unregistered pairs, non-leg expressions, and unknown named
    /// arguments refuse with a reason — chart bars are never substituted
    /// silently, which would erase the gaps/lookahead contract.
    fn call_security_lower_tf(
        &mut self,
        args: &[CallArg],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<Option<PineValue>, KernelError> {
        let usage = "request.security_lower_tf takes a symbol, a timeframe and a leg-expression list";
        let mut positional = args.iter().filter(|arg| arg.name.is_none());
        let symbol_arg = positional.next().ok_or_else(|| refused(usage))?;
        let timeframe_arg = positional.next().ok_or_else(|| refused(usage))?;
        let exprs_arg = positional.next().ok_or_else(|| refused(usage))?;
        if positional.next().is_some() {
            return Err(refused(format!(
                "{usage}, got {} positional arguments",
                args.iter().filter(|arg| arg.name.is_none()).count()
            )));
        }
        let mut bar_count: Option<i64> = None;
        for arg in args.iter().filter(|arg| arg.name.is_some()) {
            match arg.name.as_deref() {
                Some("calc_bars_count") => {
                    let given = self.eval_expr(&arg.value, ctx, legs)?;
                    match whole_number("calc_bars_count", given.as_ref()) {
                        Ok(count) => {
                            if count <= 0 {
                                return Err(refused(format!(
                                    "request.security_lower_tf takes a positive calc_bars_count, got {count}"
                                )));
                            }
                            bar_count = Some(count);
                        }
                        Err(_) => {
                            return Err(refused(format!(
                                "request.security_lower_tf takes an int calc_bars_count, got {given:?}"
                            )));
                        }
                    }
                }
                Some(given) => {
                    return Err(refused(format!(
                        "request.security_lower_tf takes no argument '{given}'"
                    )));
                }
                None => {}
            }
        }
        let symbol = match self.eval_expr(&symbol_arg.value, ctx, legs)? {
            Some(PineValue::Str(symbol)) => symbol,
            other => {
                return Err(refused(format!(
                    "request.security_lower_tf takes a string symbol, got {other:?}"
                )));
            }
        };
        let timeframe = match self.eval_expr(&timeframe_arg.value, ctx, legs)? {
            Some(PineValue::Str(timeframe)) => timeframe,
            other => {
                return Err(refused(format!(
                    "request.security_lower_tf takes a string timeframe, got {other:?}"
                )));
            }
        };
        let PineExpr::ArrayLiteral(items) = &exprs_arg.value else {
            return Err(refused(
                "request.security_lower_tf takes a leg-expression list like [time, open, high, low, close, volume]",
            ));
        };
        let mut legs_wanted = Vec::with_capacity(items.len());
        for item in items {
            let leg = match item {
                PineExpr::Variable(name) => name.clone(),
                PineExpr::BuiltinVariable(var) => builtin_name(*var).to_owned(),
                _ => {
                    return Err(refused(
                        "injected lower-tf serves bare chart legs only, never computed expressions",
                    ));
                }
            };
            if !matches!(
                leg.as_str(),
                "time" | "open" | "high" | "low" | "close" | "volume"
            ) {
                return Err(refused(format!(
                    "injected lower-tf has no column for leg '{leg}'"
                )));
            }
            legs_wanted.push(leg);
        }
        let dataset = self
            .ltf_injections
            .iter()
            .find(|slot| slot.symbol == symbol && slot.timeframe == timeframe)
            .map(|slot| slot.dataset.clone())
            .ok_or_else(|| {
                refused(format!(
                    "live request.security_lower_tf {LIVE_REQUEST_TAIL}"
                ))
            })?;
        // Tick emulation replays a tape position inside a larger history,
        // so only rows confirmed at the current bar-open stay visible
        // (the confirmed-bar rule — serving the whole tail would leak
        // future bars). Closed-bar runs keep the curated tail: their
        // fixtures pin the window exactly, and the seconds-domain chart
        // clock carries no millisecond comparison there.
        let mut visible = dataset.len();
        if self.tick_mode() {
            visible = dataset
                .times
                .partition_point(|t| *t <= chart_ms(legs, ctx.cursor(), true));
        }
        let mut rows = visible;
        if let Some(count) = bar_count {
            let count = usize::try_from(count).unwrap_or(usize::MAX);
            rows = rows.min(count);
        }
        let skip = visible.saturating_sub(rows);
        let take = rows;
        let mut columns = Vec::with_capacity(legs_wanted.len());
        for leg in &legs_wanted {
            let column: Vec<Option<PineValue>> = match leg.as_str() {
                "time" => dataset.times.iter().skip(skip).take(take).map(|t| Some(PineValue::Int(*t))).collect(),
                "open" => dataset.opens.iter().skip(skip).take(take).map(|v| crate::indicator::fin_or_none(*v).map(PineValue::Float)).collect(),
                "high" => dataset.highs.iter().skip(skip).take(take).map(|v| crate::indicator::fin_or_none(*v).map(PineValue::Float)).collect(),
                "low" => dataset.lows.iter().skip(skip).take(take).map(|v| crate::indicator::fin_or_none(*v).map(PineValue::Float)).collect(),
                "close" => dataset.closes.iter().skip(skip).take(take).map(|v| crate::indicator::fin_or_none(*v).map(PineValue::Float)).collect(),
                _ => dataset.volumes.iter().skip(skip).take(take).map(|v| crate::indicator::fin_or_none(*v).map(PineValue::Float)).collect(),
            };
            columns.push(Some(PineValue::Array(column)));
        }
        Ok(Some(PineValue::Array(columns)))
    }

    /// Serve one `request.security(symbol, timeframe, exprs, gaps?,
    /// lookahead?, calc_bars_count?)` call from a registered per-timeframe
    /// dataset: only confirmed bars count (timestamps at or before the
    /// current chart bar), and the last confirmed value carries forward —
    /// the headless reading of `barmerge.gaps_off` with
    /// `barmerge.lookahead_off`. Omitted merge modes default to that
    /// servable pair; any other mode refuses with a reason. A single
    /// expression returns its series; a list returns one value per
    /// expression for destructuring. An unregistered pair refuses naming
    /// the live fetch (unmatched calls keep the live-fetch refusal). Each
    /// expression evaluates against the confirmed tail: bare legs read
    /// last, kernel-backed single-source `ta.*` calls batch over the
    /// tail, and scalar operators compose pointwise — anything else
    /// refuses naming the subset instead of silently re-scoping chart
    /// state.
    fn call_security(
        &mut self,
        args: &[CallArg],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<Option<PineValue>, KernelError> {
        let usage = "request.security takes a symbol, a timeframe and an expression or expression list";
        let mut positional = args.iter().filter(|arg| arg.name.is_none());
        let symbol_arg = positional.next().ok_or_else(|| refused(usage))?;
        let timeframe_arg = positional.next().ok_or_else(|| refused(usage))?;
        let exprs_arg = positional.next().ok_or_else(|| refused(usage))?;
        let gaps_arg = positional.next();
        let lookahead_arg = positional.next();
        if positional.next().is_some() {
            return Err(refused(format!(
                "{usage}, got {} positional arguments",
                args.iter().filter(|arg| arg.name.is_none()).count()
            )));
        }
        let mut bar_count: Option<i64> = None;
        for arg in args.iter().filter(|arg| arg.name.is_some()) {
            match arg.name.as_deref() {
                Some("calc_bars_count") => {
                    let given = self.eval_expr(&arg.value, ctx, legs)?;
                    match whole_number("calc_bars_count", given.as_ref()) {
                        Ok(count) => {
                            if count <= 0 {
                                return Err(refused(format!(
                                    "request.security takes a positive calc_bars_count, got {count}"
                                )));
                            }
                            bar_count = Some(count);
                        }
                        Err(_) => {
                            return Err(refused(format!(
                                "request.security takes an int calc_bars_count, got {given:?}"
                            )));
                        }
                    }
                }
                Some(given) => {
                    return Err(refused(format!(
                        "request.security takes no argument '{given}'"
                    )));
                }
                None => {}
            }
        }
        let symbol = match self.eval_expr(&symbol_arg.value, ctx, legs)? {
            Some(PineValue::Str(symbol)) => symbol,
            other => {
                return Err(refused(format!(
                    "request.security takes a string symbol, got {other:?}"
                )));
            }
        };
        let timeframe = match self.eval_expr(&timeframe_arg.value, ctx, legs)? {
            Some(PineValue::Str(timeframe)) => timeframe,
            other => {
                return Err(refused(format!(
                    "request.security takes a string timeframe, got {other:?}"
                )));
            }
        };
        let gaps = match gaps_arg {
            None => "gaps_off".to_owned(),
            Some(arg) => match self.eval_expr(&arg.value, ctx, legs)? {
                Some(PineValue::EnumHandle(handle)) if handle.enum_name == "barmerge" => {
                    handle.member
                }
                other => {
                    return Err(refused(format!(
                        "request.security takes a barmerge gaps mode, got {other:?}"
                    )));
                }
            },
        };
        if gaps != "gaps_off" {
            return Err(refused(format!(
                "injected security serves barmerge.gaps_off only, got barmerge.{gaps}"
            )));
        }
        let lookahead = match lookahead_arg {
            None => "lookahead_off".to_owned(),
            Some(arg) => match self.eval_expr(&arg.value, ctx, legs)? {
                Some(PineValue::EnumHandle(handle)) if handle.enum_name == "barmerge" => {
                    handle.member
                }
                other => {
                    return Err(refused(format!(
                        "request.security takes a barmerge lookahead mode, got {other:?}"
                    )));
                }
            },
        };
        if lookahead != "lookahead_off" {
            return Err(refused(format!(
                "injected security serves barmerge.lookahead_off only, got barmerge.{lookahead}"
            )));
        }
        let single = !matches!(&exprs_arg.value, PineExpr::ArrayLiteral(_));
        let items: &[PineExpr] = match &exprs_arg.value {
            PineExpr::ArrayLiteral(items) => items,
            single => std::slice::from_ref(single),
        };
        let dataset = self
            .security_injections
            .iter()
            .find(|slot| slot.symbol == symbol && slot.timeframe == timeframe)
            .map(|slot| slot.dataset.clone())
            .ok_or_else(|| refused(format!("live request.security {LIVE_REQUEST_TAIL}")))?;
        // Confirmed prefix: rows at or before the current chart bar
        // (`lookahead_off` never observes the forming bar).
        let chart_ms = chart_ms(legs, ctx.cursor(), self.tick_mode());
        let confirmed = dataset.times.partition_point(|t| *t <= chart_ms);
        let mut rows = confirmed;
        if let Some(count) = bar_count {
            let count = usize::try_from(count).unwrap_or(usize::MAX);
            rows = rows.min(count);
        }
        let skip = confirmed.saturating_sub(rows);
        if rows == 0 {
            if single {
                return Ok(None);
            }
            return Ok(Some(PineValue::Array(vec![None; items.len()])));
        }
        let tail = SecurityTail {
            times: &dataset.times[skip..confirmed],
            opens: &dataset.opens[skip..confirmed],
            highs: &dataset.highs[skip..confirmed],
            lows: &dataset.lows[skip..confirmed],
            closes: &dataset.closes[skip..confirmed],
            volumes: &dataset.volumes[skip..confirmed],
        };
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            out.push(self.security_expr(item, &tail, ctx, legs)?);
        }
        if single {
            return Ok(out.into_iter().next().flatten());
        }
        Ok(Some(PineValue::Array(out)))
    }

    /// Evaluate one `request.security` expression against the confirmed
    /// tail: bare legs read the last confirmed value, `ta.*` sources must
    /// be bare legs (their column batches, never chart state), scalar
    /// operators compose pointwise. Anything else refuses naming the
    /// subset.
    fn security_expr(
        &mut self,
        expr: &PineExpr,
        tail: &SecurityTail,
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<Option<PineValue>, KernelError> {
        match expr {
            PineExpr::BuiltinVariable(var) => Ok(security_leg(tail, builtin_name(*var))),
            PineExpr::Variable(name) => match tail.column(name) {
                Some(_) => Ok(security_leg(tail, name)),
                None => Err(refused(
                    "injected security serves bare legs, kernel-backed single-source ta calls and scalar operators only",
                )),
            },
            PineExpr::Number(value) => Ok(Some(PineValue::Float(*value))),
            PineExpr::StringLiteral(text) => Ok(Some(PineValue::Str(text.clone()))),
            PineExpr::Boolean(flag) => Ok(Some(PineValue::Bool(*flag))),
            PineExpr::MethodCall {
                namespace,
                method,
                args,
                ..
            } if namespace == "ta" => {
                // Sourceless kernels (`ta.atr`) batch over the window
                // directly; every other arm takes a bare-leg source first
                // whose column batches, never chart state.
                if method == "atr" {
                    let mut values = Vec::with_capacity(args.len());
                    for arg in args {
                        values.push(self.eval_expr(&arg.value, ctx, legs)?);
                    }
                    self.ensure_ta_bridge()?;
                    let bridge = self.ta_bridge.as_ref().expect("bridge ensured above");
                    return bridge.batch_security(method, tail.closes, &values, &tail.ohlcv());
                }
                let Some(source_arg) = args.first() else {
                    return Err(refused(format!("ta.{method} takes a source leg here")));
                };
                let source_name = match &source_arg.value {
                    PineExpr::BuiltinVariable(var) => builtin_name(*var).to_owned(),
                    PineExpr::Variable(name) if tail.column(name).is_some() => name.clone(),
                    _ => {
                        return Err(refused(format!(
                            "injected security serves bare-leg ta sources, not '{:?}'",
                            source_arg.value
                        )));
                    }
                };
                let column = tail.column(&source_name).expect("leg checked above");
                let mut values = Vec::with_capacity(args.len());
                values.push(security_leg(tail, &source_name));
                for arg in &args[1..] {
                    values.push(self.eval_expr(&arg.value, ctx, legs)?);
                }
                self.ensure_ta_bridge()?;
                let bridge = self.ta_bridge.as_ref().expect("bridge ensured above");
                bridge.batch_security(method, column, &values, &tail.ohlcv())
            }
            PineExpr::Binary { left, op, right } => {
                let first = self.security_expr(left, tail, ctx, legs)?;
                let second = self.security_expr(right, tail, ctx, legs)?;
                binary(*op, first.as_ref(), second.as_ref())
            }
            _ => Err(refused(
                "injected security serves bare legs, kernel-backed single-source ta calls and scalar operators only",
            )),
        }
    }

    /// Lazily bind the `ta.*` bridge; fails only if builtins collide.
    fn ensure_ta_bridge(&mut self) -> Result<(), KernelError> {
        if self.ta_bridge.is_none() {
            match super::builtins_ta::TaBridge::new() {
                Ok(bridge) => self.ta_bridge = Some(bridge),
                Err(error) => {
                    return Err(refused(format!("ta bridge unavailable: {error}")));
                }
            }
        }
        Ok(())
    }

    /// `timeframe.change(period)`: true when the current bar opens a new
    /// period bucket versus the previous bar, bucketed by the shared
    /// core helper (day-plus buckets count UTC calendar units). `na`
    /// reads `na`; the first bar reads `false` (no previous bucket).
    fn call_timeframe_change(
        &mut self,
        args: &[CallArg],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<Option<PineValue>, KernelError> {
        let [arg] = args else {
            return Err(refused(format!(
                "timeframe.change takes one period string, got {} argument(s)",
                args.len()
            )));
        };
        let period = self.eval_expr(&arg.value, ctx, legs)?;
        let Some(period) = period else {
            return Ok(None);
        };
        let PineValue::Str(period) = &period else {
            return Err(refused(format!(
                "timeframe.change takes a period string, got {period}"
            )));
        };
        let cursor = ctx.cursor();
        if cursor == 0 {
            return Ok(Some(PineValue::Bool(false)));
        }
        let times = legs.named("time").unwrap_or(&[]);
        let (Some(&now), Some(&prev)) =
            (times.get(cursor), times.get(cursor - 1))
        else {
            return Err(refused("timeframe.change needs bar times".to_owned()));
        };
        let bucket = super::builtins_core::timeframe_bucket(period, now as i64)?;
        let prior = super::builtins_core::timeframe_bucket(period, prev as i64)?;
        Ok(Some(PineValue::Bool(bucket != prior)))
    }

    /// Record one drawing call as data. `.new` mints a recorded object and
    /// returns its handle; setters and `table.cell` mutate properties and
    /// record an update; getters read recorded properties back; `.delete`
    /// retires the object and records the deletion. Creation properties bind
    /// positionally (`label.new(x, y, text)`, `table.new(position, columns,
    /// rows)`, `box`/`line.new(x1, y1, x2, y2)`) with the rest named; every
    /// event carries the evaluated-bar index and the call arguments.
    fn call_drawing(
        &mut self,
        namespace: &str,
        method: &str,
        args: &[CallArg],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<Option<PineValue>, KernelError> {
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.eval_expr(&arg.value, ctx, legs)?);
        }
        let kind = match namespace {
            "label" => DrawingKind::Label,
            "table" => DrawingKind::Table,
            "box" => DrawingKind::Box,
            "line" => DrawingKind::Line,
            _ => {
                return Err(refused(format!("unknown method '{namespace}.{method}'")));
            }
        };
        match method {
            "new" => self.drawing_new(kind, args, &values),
            "delete" => self.drawing_delete(kind, args, &values),
            "cell" => self.drawing_cell(kind, args, &values),
            "merge_cells" => self.drawing_merge_cells(kind, args, &values),
            _ => self.drawing_access(kind, method, args, &values),
        }
    }

    /// Mint one recorded drawing object: positional creation properties bind
    /// by namespace, named arguments ride along as properties, a `Created`
    /// row records the call, and the handle returns to the script.
    fn drawing_new(
        &mut self,
        kind: DrawingKind,
        args: &[CallArg],
        values: &[Option<PineValue>],
    ) -> Result<Option<PineValue>, KernelError> {
        let positional: Vec<Option<PineValue>> = args
            .iter()
            .zip(values.iter())
            .filter(|(arg, _)| arg.name.is_none())
            .map(|(_, value)| value.clone())
            .collect();
        let names: &[&str] = match kind {
            DrawingKind::Label => &["x", "y", "text"],
            DrawingKind::Table => &["position", "columns", "rows"],
            DrawingKind::Box | DrawingKind::Line => &["x1", "y1", "x2", "y2"],
        };
        if positional.len() < names.len() {
            return Err(refused(format!(
                "'{}.new' takes {} ({} given)",
                kind.namespace(),
                names.join(", "),
                positional.len()
            )));
        }
        let id = self.next_drawing_id;
        self.next_drawing_id += 1;
        let mut props = HashMap::with_capacity(args.len());
        for (name, value) in names.iter().zip(positional.iter()) {
            props.insert((*name).to_owned(), value.clone());
        }
        for (arg, value) in args.iter().zip(values.iter()) {
            if let Some(name) = &arg.name {
                props.insert(name.clone(), value.clone());
            }
        }
        self.drawings.insert(id, DrawingObject { kind, props });
        let handle = DrawingHandle::new(kind, id);
        self.draw_events.push(DrawingEvent {
            bar: self.snapshots.len(),
            kind: DrawingEventKind::Created,
            object: kind.namespace().to_owned(),
            key: handle.key(),
            args: draw_args(args, values),
        });
        Ok(Some(PineValue::Drawing(handle)))
    }

    /// Retire one recorded drawing object: the single positional argument
    /// must be a live handle of the same namespace. A `Deleted` row records
    /// the call; later reads of the handle refuse.
    fn drawing_delete(
        &mut self,
        kind: DrawingKind,
        args: &[CallArg],
        values: &[Option<PineValue>],
    ) -> Result<Option<PineValue>, KernelError> {
        let handle = self.drawing_handle(kind, "delete", args, values)?;
        self.drawings.remove(&handle.id);
        self.draw_events.push(DrawingEvent {
            bar: self.snapshots.len(),
            kind: DrawingEventKind::Deleted,
            object: kind.namespace().to_owned(),
            key: handle.key(),
            args: draw_args(args, values),
        });
        Ok(None)
    }

    /// Record one `table.cell` write as an update row. The table must be a
    /// live handle; cell contents travel in the recorded arguments.
    fn drawing_cell(
        &mut self,
        kind: DrawingKind,
        args: &[CallArg],
        values: &[Option<PineValue>],
    ) -> Result<Option<PineValue>, KernelError> {
        if kind != DrawingKind::Table {
            return Err(refused(format!("unknown method '{}.cell'", kind.namespace())));
        }
        let handle = self.drawing_handle(kind, "cell", args, values)?;
        self.draw_events.push(DrawingEvent {
            bar: self.snapshots.len(),
            kind: DrawingEventKind::Updated,
            object: kind.namespace().to_owned(),
            key: handle.key(),
            args: draw_args(args, values),
        });
        Ok(None)
    }

    /// Record one `table.merge_cells` range merge as an update row. Merging
    /// is display-only (no Pine getter reads merge state back), so the
    /// range travels in the recorded arguments and nothing else changes.
    fn drawing_merge_cells(
        &mut self,
        kind: DrawingKind,
        args: &[CallArg],
        values: &[Option<PineValue>],
    ) -> Result<Option<PineValue>, KernelError> {
        if kind != DrawingKind::Table {
            return Err(refused(format!(
                "unknown method '{}.merge_cells'",
                kind.namespace()
            )));
        }
        let handle = self.drawing_handle(kind, "merge_cells", args, values)?;
        self.draw_events.push(DrawingEvent {
            bar: self.snapshots.len(),
            kind: DrawingEventKind::Updated,
            object: kind.namespace().to_owned(),
            key: handle.key(),
            args: draw_args(args, values),
        });
        Ok(None)
    }

    /// Run one setter or getter on a live drawing object. Setters mutate the
    /// recorded properties and log an update row; getters read a recorded
    /// property back. Anything else refuses with the unknown-method reason,
    /// so the recorder never becomes a silent sink.
    fn drawing_access(
        &mut self,
        kind: DrawingKind,
        method: &str,
        args: &[CallArg],
        values: &[Option<PineValue>],
    ) -> Result<Option<PineValue>, KernelError> {
        let positional: Vec<Option<PineValue>> = args
            .iter()
            .zip(values.iter())
            .filter(|(arg, _)| arg.name.is_none())
            .map(|(_, value)| value.clone())
            .collect();
        if args.len() != positional.len() {
            return Err(refused(format!(
                "'{}.{method}' takes positional arguments",
                kind.namespace()
            )));
        }
        // Setters map to (property, value index) pairs; getters map to one
        // property read. `set_xy` shapes write two properties at once.
        let updates: Vec<(&str, usize)> = match (kind, method) {
            (DrawingKind::Label, "set_x") => vec![("x", 1)],
            (DrawingKind::Label, "set_y") => vec![("y", 1)],
            (DrawingKind::Label, "set_textcolor") => vec![("textcolor", 1)],
            (DrawingKind::Label, "set_style") => vec![("style", 1)],
            (DrawingKind::Label, "set_tooltip") => vec![("tooltip", 1)],
            (DrawingKind::Label | DrawingKind::Box, "set_text") => vec![("text", 1)],
            (DrawingKind::Label | DrawingKind::Line, "set_color") => vec![("color", 1)],
            (DrawingKind::Box, "set_left") => vec![("left", 1)],
            (DrawingKind::Box, "set_top") => vec![("top", 1)],
            (DrawingKind::Box, "set_right") => vec![("right", 1)],
            (DrawingKind::Box, "set_bottom") => vec![("bottom", 1)],
            (DrawingKind::Box, "set_bgcolor") => vec![("bgcolor", 1)],
            (DrawingKind::Box, "set_border_color") => vec![("border_color", 1)],
            (DrawingKind::Line, "set_xy1") => vec![("x1", 1), ("y1", 2)],
            (DrawingKind::Line, "set_xy2") => vec![("x2", 1), ("y2", 2)],
            (DrawingKind::Line, "set_width") => vec![("width", 1)],
            (DrawingKind::Line, "set_x2") => vec![("x2", 1)],
            _ => vec![],
        };
        if !updates.is_empty() {
            let handle = self.drawing_handle(kind, method, args, values)?;
            let want = updates.iter().map(|(_, index)| *index).max().unwrap_or(0) + 1;
            if positional.len() != want {
                return Err(refused(format!(
                    "'{}.{method}' takes {} values ({} given)",
                    kind.namespace(),
                    want - 1,
                    positional.len() - 1
                )));
            }
            let object = self.drawings.get_mut(&handle.id).ok_or_else(|| {
                refused(format!(
                    "unknown {} object '{}'",
                    kind.namespace(),
                    handle.key()
                ))
            })?;
            for (prop, index) in &updates {
                object.props.insert((*prop).to_owned(), positional[*index].clone());
            }
            self.draw_events.push(DrawingEvent {
                bar: self.snapshots.len(),
                kind: DrawingEventKind::Updated,
                object: kind.namespace().to_owned(),
                key: handle.key(),
                args: draw_args(args, values),
            });
            return Ok(None);
        }
        let prop = match (kind, method) {
            (DrawingKind::Label, "get_x") => "x",
            (DrawingKind::Box, "get_right") => "right",
            (DrawingKind::Line, "get_x2") => "x2",
            _ => {
                return Err(refused(format!(
                    "unknown method '{}.{method}'",
                    kind.namespace()
                )));
            }
        };
        let handle = self.drawing_handle(kind, method, args, values)?;
        if positional.len() != 1 {
            return Err(refused(format!(
                "'{}.{method}' takes the object, got {} arguments",
                kind.namespace(),
                positional.len()
            )));
        }
        let object = self.drawings.get(&handle.id).ok_or_else(|| {
            refused(format!(
                "unknown {} object '{}'",
                kind.namespace(),
                handle.key()
            ))
        })?;
        Ok(object.props.get(prop).cloned().unwrap_or(None))
    }

    /// Read the leading handle argument of a drawing call: it must be
    /// present, a drawing handle, of the calling namespace, and live.
    fn drawing_handle(
        &self,
        kind: DrawingKind,
        method: &str,
        args: &[CallArg],
        values: &[Option<PineValue>],
    ) -> Result<DrawingHandle, KernelError> {
        let first = args
            .iter()
            .zip(values.iter())
            .find(|(arg, _)| arg.name.is_none())
            .and_then(|(_, value)| value.clone());
        match first {
            Some(PineValue::Drawing(handle)) if handle.kind == kind => {
                let live = self
                    .drawings
                    .get(&handle.id)
                    .is_some_and(|object| object.kind == kind);
                if live {
                    Ok(handle)
                } else {
                    Err(refused(format!(
                        "unknown {} object '{}'",
                        kind.namespace(),
                        handle.key()
                    )))
                }
            }
            Some(PineValue::Drawing(handle)) => Err(refused(format!(
                "'{}.{method}' takes a {} object, got {}",
                kind.namespace(),
                kind.namespace(),
                handle.key()
            ))),
            Some(other) => Err(refused(format!(
                "'{}.{method}' takes a {} object, got {other}",
                kind.namespace(),
                kind.namespace()
            ))),
            None => Err(refused(format!(
                "'{}.{method}' takes a {} object, got na",
                kind.namespace(),
                kind.namespace()
            ))),
        }
    }

    /// Namespaced calls: `ta.*` dispatches to the kernel bridge with the
    /// current chart tick; the scalar and structural namespaces (`math`,
    /// `str`, `input`, `array`, `map`, `color`, `timeframe`) dispatch to the
    /// stateless core bridge with evaluated named arguments; `runtime.error`
    /// raises a fail-closed refusal with its message; `this.*`, `Type.new()`,
    /// and instance-held method calls dispatch to registered methods with
    /// the receiver passed through. Every other namespace still refuses. The
    /// site key pins stream state and input declarations to the textual call
    /// site across bars, qualified by the enclosing call path so one textual
    /// `ta.*` call inside a shared helper keeps one state per caller.
    fn call_method(
        &mut self,
        namespace: &str,
        method: &str,
        args: &[CallArg],
        type_args: &[super::syntax::PineType],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<Option<PineValue>, KernelError> {
        if namespace == "runtime" && method == "error" {
            let mut values = Vec::with_capacity(args.len());
            for arg in args {
                values.push(self.eval_expr(&arg.value, ctx, legs)?);
            }
            let message = match values.first() {
                Some(Some(PineValue::Str(text))) => text.clone(),
                Some(Some(other)) => format!("runtime error: {other}"),
                _ => "runtime error".to_owned(),
            };
            return Err(refused(message));
        }
        if namespace == "request" {
            if method == "security_lower_tf" {
                return self.call_security_lower_tf(args, ctx, legs);
            }
            if method == "security" {
                return self.call_security(args, ctx, legs);
            }
            return Err(refused(format!(
                "live request.{method} {LIVE_REQUEST_TAIL}"
            )));
        }
        // `timeframe.change` reads bar times, so it runs here beside
        // `request.*` rather than in the stateless core bridge (which keeps
        // `timeframe.in_seconds`; `timeframe.period` resolves from the
        // runtime config — each leg of the namespace lives where its
        // inputs are).
        if namespace == "timeframe" && method == "change" {
            return self.call_timeframe_change(args, ctx, legs);
        }
        // Drawing namespaces record lifecycle data instead of rendering:
        // `.new` mints a recorded object, setters/`table.cell` mutate it,
        // getters read it back, `.delete` retires it. Anything outside the
        // recorded surface refuses with the unknown-method reason.
        if matches!(namespace, "label" | "table" | "box" | "line") {
            return self.call_drawing(namespace, method, args, ctx, legs);
        }
        if namespace != "ta"
            && !matches!(namespace, "math" | "str" | "input" | "array" | "matrix" | "map" | "color" | "timeframe")
        {
            if namespace == "this"
                || (method == "new" && self.types.contains_key(namespace))
            {
                return self.call_instance_method(namespace, method, args, ctx, legs);
            }
            // Imported-type construction (`ALIAS.Type.new()`) resolves the
            // alias through the caller's table, then constructs the
            // registered type exactly like a bare `Type.new()`.
            if method == "new" {
                if let Some((alias, type_name)) = namespace.split_once('.') {
                    let owner = self.current_module.clone();
                    if self
                        .module_aliases
                        .contains_key(&(owner, alias.to_owned()))
                        && self.types.contains_key(type_name)
                    {
                        return self.call_instance_method(type_name, method, args, ctx, legs);
                    }
                }
            }
            // Instance-held receivers (`sched.register(...)`) dispatch to
            // their type's methods; anything else falls through to the
            // import-alias dispatch below.
            let held = self.peek_object(ctx, namespace);
            if held {
                return self.call_instance_method(namespace, method, args, ctx, legs);
            }
            // Nested field receivers (`bars.hist_l.get`) dispatch like held
            // instances when their root is held: the instance dispatch
            // takes the field out of its parent chain. Alias and `this.`
            // namespaces keep their existing routes below and above.
            if let Some((root, _)) = namespace.split_once('.') {
                if !root.is_empty() && root != "this" && self.peek_object(ctx, root) {
                    return self.call_instance_method(namespace, method, args, ctx, legs);
                }
            }
            // Imported-library calls (`ALIAS.fn(...)`) bind through the
            // current module's alias table to the exporting module's
            // functions. The body runs under the exporting module, so its
            // bare calls resolve to its own helpers — never to the
            // importer's names.
            let owner = self.current_module.clone();
            if let Some(target) = self
                .module_aliases
                .get(&(owner, namespace.to_owned()))
                .cloned()
            {
                let key = (target.clone(), method.to_owned());
                if let Some(overloads) = self.functions.get(&key).cloned() {
                    let mut values = Vec::with_capacity(args.len());
                    for arg in args {
                        values.push(self.eval_expr(&arg.value, ctx, legs)?);
                    }
                    let (params, body) =
                        select_function(&format!("{namespace}.{method}"), &overloads, args, &values)?;
                    let site = format!("{target}::{method}@{args:?}");
                    return self.exec_call(site, &target, &params, args, &values, &body, ctx, legs);
                }
                // Method-shaped call through the alias with an explicit
                // receiver first argument (`libCandles.createMTFTable(table,
                // ...)`): the target module's method runs against the
                // receiver value under its module, exactly like an
                // instance-held call. The mutated receiver writes back to
                // a variable home when the argument names one.
                if let Some((receiver_type, (method_owner, params, body))) = self
                    .methods
                    .iter()
                    .find(|((_, name), (owner, _, _))| {
                        *name == *method && *owner == target
                    })
                    .map(|((receiver_type, _), found)| {
                        (receiver_type.clone(), found.clone())
                    })
                {
                    let mut values = Vec::with_capacity(args.len());
                    for arg in args {
                        values.push(self.eval_expr(&arg.value, ctx, legs)?);
                    }
                    // The receiver rides first and positionally; the body
                    // binds the remaining parameters exactly like an
                    // instance-held call (whose argument list never carried
                    // the receiver).
                    let (head_arg, head_value) = match (args.first(), values.first()) {
                        (Some(arg), Some(value)) if arg.name.is_none() => (arg, value),
                        _ => {
                            return Err(refused(format!(
                                "method '{namespace}.{method}' takes its '{receiver_type}' receiver first"
                            )));
                        }
                    };
                    let home = home_of(&head_arg.value);
                    // Methods whose first parameter is a primitive (`bool`,
                    // `int`, `float`, `string`, `color`) are plain namespaced
                    // functions: the head argument binds as a parameter, with
                    // no receiver slot and no writeback. Only object-typed
                    // heads take the receiver path below.
                    let primitive_head = matches!(
                        receiver_type.as_str(),
                        "bool" | "int" | "float" | "string" | "color"
                    );
                    let receiver = match head_value {
                        Some(PineValue::Object(instance))
                            if instance.type_name == receiver_type =>
                        {
                            Some(instance.clone())
                        }
                        _ if primitive_head => None,
                        other => {
                            return Err(refused(format!(
                                "method '{namespace}.{method}' needs a '{receiver_type}' receiver first, got {other:?}"
                            )));
                        }
                    };
                    let Some(head) = params.first() else {
                        return Err(refused(format!("method '{method}' needs a receiver")));
                    };
                    if let Some(instance) = receiver {
                        let site = format!("{}.{method}@{args:?}", instance.type_name);
                        self.receiver.push(ReceiverSlot {
                            instance,
                            alias: head.name.clone(),
                        });
                        let value = self.exec_call(
                            site,
                            &method_owner,
                            &params[1..],
                            &args[1..],
                            &values[1..],
                            &body,
                            ctx,
                            legs,
                        );
                        let back = self
                            .receiver
                            .pop()
                            .expect("receiver pushed above");
                        if let Some(home) = home {
                            self.assign_path(
                                ctx,
                                &home,
                                Some(PineValue::Object(back.instance)),
                            )?;
                        }
                        return value;
                    }
                    let site = format!("{namespace}.{method}@{args:?}");
                    return self.exec_call(
                        site,
                        &method_owner,
                        &params,
                        args,
                        &values,
                        &body,
                        ctx,
                        legs,
                    );
                }
                return Err(refused(format!(
                    "unknown function '{namespace}.{method}'"
                )));
            }
            // Collection method-call syntax (`items.push(x)`): a
            // variable-held array/matrix/map shares its namespace bridge.
            if let Some(value) =
                self.try_collection_call(namespace, method, args, type_args, ctx, legs)?
            {
                return Ok(value);
            }
            return Err(refused(format!("unknown method '{namespace}.{method}'")));
        }
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.eval_expr(&arg.value, ctx, legs)?);
        }
        if namespace == "ta" {
            self.ensure_ta_bridge()?;
            let own = format!("ta.{method}@{args:?}");
            let site = if self.call_stack.is_empty() {
                own
            } else {
                format!("{}>{own}", self.call_stack.join(">"))
            };
            let tick = crate::indicator::BarTick {
                open: leg_or_nan(legs, "open", ctx.cursor()),
                high: leg_or_nan(legs, "high", ctx.cursor()),
                low: leg_or_nan(legs, "low", ctx.cursor()),
                close: leg_or_nan(legs, "close", ctx.cursor()),
                volume: leg_or_nan(legs, "volume", ctx.cursor()),
            };
            return self
                .ta_bridge
                .as_mut()
                .expect("bridge built above")
                .call(&site, method, &values, tick);
        }
        let core_args: Vec<super::builtins_core::CoreArg> = args
            .iter()
            .zip(values)
            .map(|(arg, value)| super::builtins_core::CoreArg {
                name: arg.name.clone(),
                value,
            })
            .collect();
        let site = format!("{namespace}.{method}@{args:?}");
        super::builtins_core::call(namespace, method, &site, &core_args, type_args, &mut self.core_inputs)
    }

    /// Collection method-call syntax (`items.push(x)`): a variable-held
    /// array/matrix/map dispatches to its namespace bridge with the
    /// receiver prepended as the first positional argument, so
    /// instance-held and namespace forms share one implementation.
    /// Returns `None` when the namespace names no variable-held
    /// collection, leaving existing refusals untouched.
    /// Collection calls shared by statement and value position: `None`
    /// means no collection receiver matched, `Some(None)` a muted answer.
    #[allow(clippy::option_option)]
    fn try_collection_call(
        &mut self,
        namespace: &str,
        method: &str,
        args: &[CallArg],
        type_args: &[super::syntax::PineType],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<Option<Option<PineValue>>, KernelError> {
        let Some(bridge) = self.peek_collection(ctx, namespace) else {
            return Ok(None);
        };
        let receiver = self.eval_expr(&PineExpr::Variable(namespace.to_owned()), ctx, legs)?;
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.eval_expr(&arg.value, ctx, legs)?);
        }
        let mut core_args = Vec::with_capacity(args.len() + 1);
        core_args.push(super::builtins_core::CoreArg {
            name: None,
            value: receiver,
        });
        core_args.extend(args.iter().zip(values).map(|(arg, value)| {
            super::builtins_core::CoreArg {
                name: arg.name.clone(),
                value,
            }
        }));
        let site = format!("{bridge}.{method}@{args:?}");
        super::builtins_core::call(
            bridge,
            method,
            &site,
            &core_args,
            type_args,
            &mut self.core_inputs,
        )
        .map(Some)
    }

    /// Whether `name` currently holds a user-type instance (series variable,
    /// persistent binding, or receiver field), without moving it.
    fn peek_object(&self, ctx: &RunCtx<'_>, name: &str) -> bool {
        if self.receiver_slot(name).is_some() {
            return true;
        }
        if let Some(field) = name.strip_prefix("this.") {
            return self
                .receiver
                .last()
                .and_then(|slot| slot.instance.fields.get(field))
                .is_some_and(|slot| matches!(slot, Some(PineValue::Object(_))));
        }
        if let Some(slot) = self.variables.get(name) {
            return matches!(slot, Some(PineValue::Object(_)));
        }
        if let Some(present) = ctx.vars().get_var(name) {
            if matches!(present, PineValue::Object(_)) {
                return true;
            }
        }
        if let Some(present) = ctx.vars().get_varip(name) {
            if matches!(present, PineValue::Object(_)) {
                return true;
            }
        }
        false
    }

    /// Bridge namespace for a variable-held collection (`array`/`matrix`/
    /// `map`), without moving or evaluating it: instance-held collection
    /// calls (`items.push(x)`) dispatch through this to the shared bridge.
    /// Anything else (including undeclared names) reads `None`, so unknown
    /// namespaces keep their existing refusal instead of an eval error.
    fn peek_collection(&self, ctx: &RunCtx<'_>, name: &str) -> Option<&'static str> {
        let bridge = |value: Option<&PineValue>| match value {
            Some(PineValue::Array(_)) => Some("array"),
            Some(PineValue::Matrix(_)) => Some("matrix"),
            Some(PineValue::IntMap(_) | PineValue::Map(_)) => Some("map"),
            _ => None,
        };
        if let Some(field) = name.strip_prefix("this.") {
            if let Some(slot) = self
                .receiver
                .last()
                .and_then(|slot| slot.instance.fields.get(field))
            {
                return bridge(slot.as_ref());
            }
            return None;
        }
        if let Some(found) = bridge(self.variables.get(name).and_then(|slot| slot.as_ref())) {
            return Some(found);
        }
        if let Some(found) = bridge(ctx.vars().get_var(name).as_ref()) {
            return Some(found);
        }
        if let Some(found) = bridge(ctx.vars().get_varip(name).as_ref()) {
            return Some(found);
        }
        None
    }

    /// Dispatch one method call on a taken receiver: `this.*` reuses the
    /// innermost receiver, `Type.new()` constructs, and every other
    /// namespace takes the instance out of its home first. The mutated
    /// instance returns to its home before the value does.
    fn call_instance_method(
        &mut self,
        namespace: &str,
        method: &str,
        args: &[CallArg],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<Option<PineValue>, KernelError> {
        if namespace == "this" {
            // Lexical receiver: `this.*` reads and writes (including inside
            // argument expressions like `this._writeCandle(this.h, ...)`)
            // bind to the innermost enclosing method body, so the call runs
            // against a clone and writes the mutated copy back to its slot.
            // Popping first would unbind the receiver during argument
            // evaluation; indexing back keeps nested calls balanced.
            let depth = self.receiver.len();
            let receiver = self
                .receiver
                .last()
                .map(|slot| slot.instance.clone())
                .ok_or_else(|| refused("'this' is only bound inside a method"))?;
            let outcome = self.run_method(receiver, method, args, ctx, legs, None)?;
            match self.receiver.get_mut(depth - 1) {
                Some(slot) => {
                    slot.instance = outcome.receiver;
                    return outcome.value;
                }
                None => {
                    return Err(refused(
                        "'this' receiver slot vanished during a method call",
                    ));
                }
            }
        }
        if method == "new" {
            if let Some(fields) = self.types.get(namespace).cloned() {
                if !args.is_empty() {
                    return Err(refused(format!(
                        "'{namespace}.new' takes no arguments here; fields bind through methods"
                    )));
                }
                return Ok(Some(PineValue::Object(self.construct(namespace, &fields, ctx, legs)?)));
            }
        }
        // Receiver-alias dispatch (`level.updateLevelSuccess(...)`): the
        // call runs against a clone of the live slot and writes the
        // mutated copy back to its slot — the same index-back discipline
        // as the `this.` path above, so nested calls never unbind the
        // receiver their arguments still spell.
        if let Some(depth) = self.receiver_slot(namespace) {
            let receiver = self.receiver[depth].instance.clone();
            let outcome = self.run_method(receiver, method, args, ctx, legs, None)?;
            match self.receiver.get_mut(depth) {
                Some(slot) => {
                    slot.instance = outcome.receiver;
                    return outcome.value;
                }
                None => {
                    return Err(refused(
                        "receiver alias slot vanished during a method call",
                    ));
                }
            }
        }
        match self.take_object(ctx, namespace)? {
            Some((instance, home)) => {
                let lease = Self::lease_key_for(&home);
                let outcome =
                    self.run_method(instance, method, args, ctx, legs, lease)?;
                // Native serves synced the lease; Pine bodies invalidate it.
                self.put_object(ctx, home, outcome.receiver, !outcome.served_native)?;
                outcome.value
            }
            None => match self.take_nested_object(ctx, namespace)? {
                Some((instance, home)) => {
                    let lease = Self::lease_key_for(&home);
                    let outcome =
                        self.run_method(instance, method, args, ctx, legs, lease)?;
                    self.put_object(ctx, home, outcome.receiver, !outcome.served_native)?;
                    outcome.value
                }
                None => Err(refused(format!(
                    "unknown method '{namespace}.{method}'"
                ))),
            },
        }
    }

    /// Build an instance from a registered type's field defaults.
    fn construct(
        &mut self,
        type_name: &str,
        fields: &[FieldDecl],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<UserInstance, KernelError> {
        let mut bound = HashMap::with_capacity(fields.len());
        for field in fields {
            let value = match &field.default {
                Some(default) => self.eval_expr(default, ctx, legs)?,
                None => None,
            };
            bound.insert(field.name.clone(), value);
        }
        Ok(UserInstance::new(type_name, bound))
    }

    /// Attempt one natively served instance-method call. `Declined` (any
    /// guard closed, any lookup missed, any provider decline) runs the Pine
    /// body unchanged; `Served` reports the taken receiver already updated
    /// in place, alongside the call value. Guards, narrowest first: explicit opt-in only (drivers keep proving
    /// Pine by default); named args decline (positional binding would
    /// misbind them); tick-frame providers additionally need tick runs
    /// with no lower-tf injection (closed-bar `time` stays in the seconds
    /// domain the shim suite pins; pull rows would need dataset plumbing —
    /// Pine raises the identical refusal there instead). Legs-independent
    /// providers (today `libBuffers`) serve on closed-bar runs too, through
    /// the detached frame their methods never read. `Bars.fetch` on a leased
    /// slot additionally consults the resident-ring lease (see
    /// [`EngineState::serve_leased`]); every other shape runs the provider
    /// bridge exactly as before.
    fn try_native_call(
        &mut self,
        type_name: &str,
        method: &str,
        owner: &str,
        values: &[Option<PineValue>],
        args: &[CallArg],
        receiver_fields: &mut FieldTable,
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
        lease: Option<LeaseKey>,
    ) -> Result<NativeCall, KernelError> {
        if !self.native.enabled || args.iter().any(|arg| arg.name.is_some()) {
            return Ok(NativeCall::Declined);
        }
        let Some((publisher, name)) = owner.split_once('/') else {
            return Ok(NativeCall::Declined);
        };
        let Some(provider) = crate::native_libs::provider_for(publisher, name) else {
            return Ok(NativeCall::Declined);
        };
        if !provider.provides().contains(&type_name) {
            return Ok(NativeCall::Declined);
        }
        // Closed-bar context reads route with the run's bar tape, never any
        // tick frame: tick runs decline outright (intrabar legs are not
        // confirmed bars), closed-bar runs feed the resident store below.
        if provider.serves_bar_context() {
            if self.tick_mode() {
                return Ok(NativeCall::Declined);
            }
            return self.serve_bar_context(
                provider,
                type_name,
                method,
                receiver_fields,
                values,
                ctx,
            );
        }
        if !provider.needs_tick_frame() {
            return provider.call(
                type_name,
                method,
                receiver_fields,
                values,
                &crate::native_libs::MethodCtx::detached(),
            );
        }
        if !self.tick_mode() || !self.ltf_injections.is_empty() {
            return Ok(NativeCall::Declined);
        }
            let cursor = ctx.cursor();
            let leg = |leg_name: &str| {
                legs.named(leg_name)
                    .and_then(|leg| leg.get(cursor).copied())
            };
            let (Some(open), Some(high), Some(low), Some(close), Some(volume)) = (
                leg("open"),
                leg("high"),
                leg("low"),
                leg("close"),
                leg("volume"),
            ) else {
                return Ok(NativeCall::Declined);
            };
            let (Some(is_new), Some(is_confirmed)) = (
                barstate_flag("isnew", ctx, legs, true),
                barstate_flag("isconfirmed", ctx, legs, true),
            ) else {
                return Ok(NativeCall::Declined);
            };
            let Some(tick_ms) = self.tick_times.get(cursor).copied() else {
                return Ok(NativeCall::Declined);
            };
            let frame = MethodCtx {
                is_new,
                is_confirmed,
                legs: TickLegs {
                    open,
                    high,
                    low,
                    close,
                    volume,
                },
                bar_open_ms: chart_ms(legs, cursor, true),
                tick_ms,
            };
            // The resident-ring fast path: `Bars.fetch` on a leased slot
            // with no positional-argument shape to misbind (named args
            // already declined above). Anything else keeps the provider
            // bridge, whose guard stays the single shape rule.
            if type_name == "Bars"
                && method == "fetch"
                && args.is_empty()
                && publisher == "showmethegrail"
                && name == "libBars"
            {
                if let Some(key) = lease {
                    return self.serve_leased(
                        type_name,
                        method,
                        receiver_fields,
                        values,
                        &frame,
                        key,
                        provider,
                    );
                }
            }
            provider.call(type_name, method, receiver_fields, values, &frame)
    }

    /// Serve one closed-bar context read (`MarketContext.fetch` shape) over
    /// the resident store: map the chart period to its span, feed chart bars
    /// through the cursor, and hand the current store to the provider. Any
    /// unservable shape (unmapped period, uneven spacing, unordered tape)
    /// declines before touching fields, so Pine runs the body instead.
    fn serve_bar_context(
        &mut self,
        provider: &'static dyn crate::native_libs::NativeProvider,
        type_name: &str,
        method: &str,
        receiver_fields: &mut FieldTable,
        values: &[Option<PineValue>],
        ctx: &mut RunCtx<'_>,
    ) -> Result<NativeCall, KernelError> {
        let Ok(span) = Timeframe::from_pine_abbrev(&self.chart_period) else {
            return Ok(NativeCall::Declined);
        };
        let Some(now_ms) = self.apex_feed(span.duration_ms(), ctx.cursor()) else {
            return Ok(NativeCall::Declined);
        };
        let key = MarketContextKey::new(span);
        provider.serve_bar_context(BarContextServe {
            type_name,
            method,
            fields: receiver_fields,
            args: values,
            store: &mut self.apex_store,
            key: &key,
            now_ms,
        })
    }

    /// Feed the resident context store with chart bars through bar `through`
    /// (inclusive), returning the `now` that confirms the last bucket. The
    /// first feed validates and loads the whole prefix; later feeds append
    /// only the new tail, so steady-state reads extend incrementally. Bars
    /// must arrive evenly at `span_ms`: any gap, overlap, or unordered stamp
    /// declines — no resample guessing, Pine runs the body. The store is
    /// untouched on decline.
    fn apex_feed(&mut self, span_ms: i64, through: usize) -> Option<i64> {
        let want = through + 1;
        if want > self.run_bars.len() {
            return None;
        }
        // A second `fetch` on the same bar reuses the fed store.
        if self.apex_loaded >= want {
            return Some(self.run_bars[want - 1].ts.timestamp_millis() + span_ms);
        }
        let from = self.apex_loaded;
        // Validate spacing over the newly covered bars before mutating: each
        // bar must advance exactly one span past its predecessor.
        let mut prev_ts = if from == 0 {
            None
        } else {
            Some(self.run_bars[from - 1].ts.timestamp_millis())
        };
        for bar in &self.run_bars[from..want] {
            let ts = bar.ts.timestamp_millis();
            if prev_ts.is_some_and(|prev| ts - prev != span_ms) {
                return None;
            }
            prev_ts = Some(ts);
        }
        if from == 0 {
            if self.apex_store.set_base(&self.run_bars[..want]).is_err() {
                return None;
            }
        } else {
            for index in from..want {
                if self.apex_store.append_bar(&self.run_bars[index]).is_err() {
                    return None;
                }
            }
        }
        self.apex_loaded = want;
        Some(self.run_bars[want - 1].ts.timestamp_millis() + span_ms)
    }

    /// Serve one `Bars.fetch` through the resident-ring lease: a version
    /// match runs the shared fold with zero hydrate; a mismatch hydrates
    /// once through the provider bridge and caches the fresh ring. Any
    /// non-serve outcome (decline or refusal) drops the entry — the Pine
    /// body re-runs from the taken fields, and the version bump on its
    /// write-back keeps the dropped entry from ever serving stale state.
    fn serve_leased(
        &mut self,
        type_name: &str,
        method: &str,
        receiver_fields: &mut FieldTable,
        values: &[Option<PineValue>],
        frame: &MethodCtx,
        key: LeaseKey,
        provider: &'static dyn crate::native_libs::NativeProvider,
    ) -> Result<NativeCall, KernelError> {
        let current = self.lease_versions.get(&key).copied().unwrap_or(0);
        let hit = matches!(self.leased_rings.get(&key), Some(entry) if entry.version == current);
        if !hit {
            return self.serve_miss(type_name, method, receiver_fields, values, frame, key, current, provider);
        }
        let tick = crate::native_libs::fetch_tick_from_frame(frame);
        let result = {
            let Some(entry) = self.leased_rings.get_mut(&key) else {
                return self.serve_miss(
                    type_name, method, receiver_fields, values, frame, key, current, provider,
                );
            };
            if entry.version != current {
                return self.serve_miss(
                    type_name, method, receiver_fields, values, frame, key, current, provider,
                );
            }
            entry
                .ring
                .serve_fetch_tick(receiver_fields, entry.cursor, type_name, &tick)
        };
        match result {
            Ok((outcome, next)) => {
                if outcome.is_declined() {
                    self.leased_rings.remove(&key);
                    self.fetch_counters.record_fallback();
                } else {
                    if let Some(entry) = self.leased_rings.get_mut(&key) {
                        entry.cursor = next;
                    }
                    self.fetch_counters.record_commit();
                }
                Ok(outcome)
            }
            Err(err) => {
                self.leased_rings.remove(&key);
                self.fetch_counters.record_fallback();
                Err(err)
            }
        }
    }

    /// Lease miss: run the provider bridge (the single shape guard from
    /// `LibBarsProvider::call` still governs), then cache the fresh ring by
    /// re-hydrating the just-written table. The extra validation runs only
    /// on miss ticks (after Pine-side writes); a fill failure keeps today's
    /// behavior by leaving the lease empty. Every miss records one
    /// fallback: the bridge re-hydrates (or Pine runs on), never commits.
    fn serve_miss(
        &mut self,
        type_name: &str,
        method: &str,
        receiver_fields: &mut FieldTable,
        values: &[Option<PineValue>],
        frame: &MethodCtx,
        key: LeaseKey,
        current: u64,
        provider: &'static dyn crate::native_libs::NativeProvider,
    ) -> Result<NativeCall, KernelError> {
        self.fetch_counters.record_fallback();
        let outcome = provider.call(type_name, method, receiver_fields, values, frame)?;
        if outcome.is_declined() {
            return Ok(outcome);
        }
        match crate::bars::BarRing::from_pine_fields(receiver_fields) {
            Ok((ring, cursor)) => {
                self.leased_rings.insert(
                    key,
                    LeasedRing {
                        ring,
                        cursor,
                        version: current,
                    },
                );
            }
            Err(_) => {
                self.leased_rings.remove(&key);
            }
        }
        Ok(outcome)
    }

    /// Run one registered method body against a receiver: the receiver binds
    /// as `this`, remaining parameters bind positionally/named (defaults
    /// fill gaps), and the body's trailing value returns. The receiver —
    /// mutated or not — returns alongside for the caller to store back,
    /// with whether the native lease served (so the caller writes back
    /// without invalidating a synced lease, or bumps for Pine writes).
    /// `lease` carries the take home's lease key (`None` for slot-based
    /// takes, which never lease).
    fn run_method(
        &mut self,
        mut receiver: UserInstance,
        method: &str,
        args: &[CallArg],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
        lease: Option<LeaseKey>,
    ) -> Result<MethodOutcome, KernelError> {
        let key = (receiver.type_name.clone(), method.to_owned());
        let Some((owner, params, body)) = self.methods.get(&key).cloned() else {
            return Err(refused(format!(
                "unknown method '{}.{method}'",
                receiver.type_name
            )));
        };
        let Some(head) = params.first() else {
            return Err(refused(format!("method '{method}' needs a receiver")));
        };
        if let Some(decl) = head.type_name.as_ref() {
            if trailing_type(&decl.name) != receiver.type_name.as_str() {
                return Err(refused(format!(
                    "method '{method}' needs a '{}' receiver, got '{}'",
                    decl.name, receiver.type_name
                )));
            }
        }
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.eval_expr(&arg.value, ctx, legs)?);
        }
        if let NativeCall::Served(value) = self.try_native_call(
            &receiver.type_name,
            method,
            &owner,
            &values,
            args,
            &mut receiver.fields,
            ctx,
            legs,
            lease,
        )? {
            return Ok(MethodOutcome {
                receiver,
                value: Ok(value),
                served_native: true,
            });
        }
        let site = format!("{}.{method}@{args:?}", receiver.type_name);
        // The receiver's declared alias (`level` in `updateLevelTouch(
        // MarketLevel level, ...)`) rides the slot: reads, nested calls,
        // and field writes through the alias hit the one live copy, never
        // a stale clone. The slot pops on return like every frame.
        self.receiver.push(ReceiverSlot {
            instance: receiver,
            alias: head.name.clone(),
        });
        let value = self.exec_call(site, &owner, &params[1..], args, &values, &body, ctx, legs);
        let back = self
            .receiver
            .pop()
            .expect("receiver pushed above");
        Ok(MethodOutcome {
            receiver: back.instance,
            value,
            served_native: false,
        })
    }

    /// Run one user body with bound parameters: parameters bind into a fresh
    /// frame over the caller's namespace, defaults evaluate in the caller,
    /// and the body's trailing value returns. The body executes under
    /// `module`, so its bare calls resolve to the owning module's helpers;
    /// the caller's module restores on return. Locals never leak — the frame
    /// restores on return — but per-bar local values feed history reads
    /// through the frame table keyed by call site, so series offsets inside
    /// helpers observe their own prior bars exactly like top-level series
    /// do, never a same-named binding from another scope.
    fn exec_call(
        &mut self,
        site: String,
        module: &str,
        params: &[Param],
        call_args: &[CallArg],
        values: &[Option<PineValue>],
        body: &[PineStmt],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<Option<PineValue>, KernelError> {
        if self.call_stack.len() >= MAX_CALL_DEPTH {
            return Err(refused(format!(
                "call depth exceeds {MAX_CALL_DEPTH}"
            )));
        }
        let mut bindings: Vec<(String, Option<PineValue>, Option<String>)> =
            Vec::with_capacity(params.len());
        let mut positional = call_args
            .iter()
            .zip(values.iter())
            .filter(|(arg, _)| arg.name.is_none());
        for param in params {
            if let Some((hit_arg, hit_value)) = call_args
                .iter()
                .zip(values.iter())
                .find(|(arg, _)| arg.name.as_deref() == Some(param.name.as_str()))
            {
                bindings.push((
                    param.name.clone(),
                    hit_value.clone(),
                    home_of(&hit_arg.value),
                ));
            } else if let Some((next_arg, next_value)) = positional.next() {
                bindings.push((
                    param.name.clone(),
                    next_value.clone(),
                    home_of(&next_arg.value),
                ));
            } else if let Some(default) = param.default.as_ref() {
                bindings.push((
                    param.name.clone(),
                    self.eval_expr(default, ctx, legs)?,
                    None,
                ));
            } else {
                return Err(refused(format!("missing argument '{}'", param.name)));
            }
        }
        if positional.next().is_some() {
            return Err(refused("too many positional arguments"));
        }
        for arg in call_args {
            if let Some(given) = arg.name.as_ref() {
                if !params.iter().any(|param| &param.name == given) {
                    return Err(refused(format!("unknown argument '{given}'")));
                }
            }
        }
        // The caller frame stays live under the call, so globals and
        // constants resolve exactly as before; only parameter names
        // shadow, with their entry values saved for the teardown below.
        // This replaces the old clone-the-frame discipline at key-set
        // cost instead of a deep value clone per call.
        // The caller frame stays live under the call, so globals and
        // constants resolve exactly as before. Parameter names shadow
        // with their entry values saved; body writes journal through
        // `assign`, and the teardown below reverts both. Journaling stays
        // held while binding (params restore through the shadow list).
        let mut shadowed: Vec<(String, Option<Option<PineValue>>)> =
            Vec::with_capacity(bindings.len());
        self.journal_hold = true;
        for (name, value, _) in &bindings {
            shadowed.push((name.clone(), self.variables.remove(name)));
            self.assign(ctx, name, value.clone());
        }
        self.journal_hold = false;
        let outer_base = self.journal_base.replace(self.journal.len());
        let base = self.journal.len();
        let caller = std::mem::replace(&mut self.current_module, module.to_owned());
        self.call_stack.push(site);
        // A `break` binds lexically: a body called from inside a loop still
        // refuses a loop-less `break` instead of unwinding the caller.
        let saved_depth = std::mem::replace(&mut self.loop_depth, 0);
        let saved_break = std::mem::replace(&mut self.break_flag, false);
        let value = self.exec_body_value(body, ctx, legs);
        // Collections ride by reference: parameters bound from variable
        // homes write their final collection value back, so standalone
        // calls mutate caller arrays exactly like method receivers do.
        // Scalars stay by value — rebinding a name inside never travels.
        // Captured before the frame teardown and applied after it: the
        // teardown reverts caller homes to entry state, which would wipe
        // a nested call's write-back to its caller's parameter.
        let mut write_backs: Vec<(String, Option<PineValue>)> = Vec::new();
        for (name, _, home) in &bindings {
            if let Some(target) = home {
                let current = self.variables.get(name).cloned().flatten();
                if matches!(
                    current,
                    Some(PineValue::Array(_) | PineValue::IntMap(_) | PineValue::Map(_) | PineValue::Matrix(_))
                ) {
                    write_backs.push((target.clone(), current));
                }
            }
        }
        // Frame diffs record before the pop so the keys carry this call's
        // site: each scope's past stays its own. Parameters record
        // unconditionally (a parameter shadowing an outer same-valued
        // binding still advances its own history); body-touched names
        // record from the journal. Untouched caller bindings skip the
        // record — the bar extension falls back to their identical live
        // values, so their series advance exactly as before.
        for (name, _, _) in &bindings {
            let key = self.frame_key(name);
            if let Some(current) = self.variables.get(name).cloned() {
                self.frame_history.insert(key, current);
            }
        }
        let touched: Vec<String> = self.journal[base..]
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        for name in &touched {
            let key = self.frame_key(name);
            if let Some(current) = self.variables.get(name).cloned() {
                self.frame_history.insert(key, current);
            }
        }
        self.call_stack.pop();
        self.current_module = caller;
        self.loop_depth = saved_depth;
        self.break_flag = saved_break;
        // Frame teardown: journaled body writes revert first (restoring
        // rebound parameters to their bound values), then shadowed
        // parameters unbind (restoring shadowed caller values). Plain
        // locals vanish; caller bindings the body never touched were
        // never journaled and stand untouched — exactly the old
        // clone-and-restore outcome at write-proportional cost.
        while self.journal.len() > base {
            let (name, old) = self.journal.pop().expect("length checked");
            match old {
                Some(stored) => {
                    self.variables.insert(name, stored);
                }
                None => {
                    self.variables.remove(&name);
                }
            }
        }
        for (name, old) in shadowed {
            match old {
                Some(stored) => {
                    self.variables.insert(name, stored);
                }
                None => {
                    self.variables.remove(&name);
                }
            }
        }
        self.journal_base = outer_base;
        // Write-backs run unjournaled (held): they assign caller homes
        // and must persist, never revert.
        self.journal_hold = true;
        let mut write_error = None;
        for (target, written) in write_backs {
            if let Err(error) = self.assign_path(ctx, &target, written) {
                write_error = Some(error);
                break;
            }
        }
        self.journal_hold = false;
        if let Some(error) = write_error {
            return Err(error);
        }
        value
    }

    /// History key for one frame binding: the innermost call site prefixes
    /// the name inside a call, while top-level series keep their bare name.
    fn frame_key(&self, name: &str) -> String {
        match self.call_stack.last() {
            Some(top) => format!("{top}#{name}"),
            None => name.to_owned(),
        }
    }

    /// Execute a body, returning its trailing value: the last bare
    /// expression (or nested `switch`) wins; every other statement runs for
    /// effect only. This is the `=>` body rule for functions, methods, and
    /// expression switches alike.
    fn exec_body_value(
        &mut self,
        stmts: &[PineStmt],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<Option<PineValue>, KernelError> {
        let mut value = None;
        for stmt in stmts {
            value = self.exec_stmt_value(stmt, ctx, legs)?;
            if self.stopped() {
                break;
            }
        }
        self.check_slot()?;
        Ok(value)
    }

    /// Evaluate one expression statement, sharing the write-back rule
    /// across positions: mutating collection calls persist their result to
    /// a variable target, giving statement calls reference-type behavior
    /// (`map.put(m, ...)` mutates the binding); nested calls stay
    /// functional. Value position yields the written value, so a trailing
    /// collection call inside a body both mutates and returns exactly like
    /// statement position mutates. Read-only calls (`size`, `get`, ...)
    /// never write back: persisting their return would clobber the
    /// collection with one element.
    fn exec_expr_stmt(
        &mut self,
        expr: &PineExpr,
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<Option<PineValue>, KernelError> {
        if let PineExpr::MethodCall {
            namespace,
            method,
            args,
            type_args: _,
        } = expr
        {
            if let Some(PineExpr::Variable(target)) =
                args.first().map(|arg| &arg.value)
            {
                if namespace == "array" && method == "shift" {
                    return self.exec_shift_stmt(target, args, ctx, legs);
                }
                if namespace == "map" && method == "remove" {
                    return self.exec_map_remove_stmt(target, args, ctx, legs);
                }
                if writes_back_collection(namespace, method) {
                    let written = self.eval_expr(expr, ctx, legs)?;
                    self.assign_path(ctx, target, written.clone())?;
                    return Ok(written);
                }
            }
            // Instance-held collection calls (`items.push(x)`) persist to
            // the receiver variable exactly like the namespace form.
            if let Some(bridge) = self.peek_collection(ctx, namespace) {
                if writes_back_collection(bridge, method) {
                    let written = self.eval_expr(expr, ctx, legs)?;
                    self.assign_path(ctx, namespace, written.clone())?;
                    return Ok(written);
                }
            }
        }
        self.eval_expr(expr, ctx, legs)
    }

    /// Run one `array.shift(target, ...)` statement: the removal persists
    /// to the variable home while the call yields the removed head. The
    /// generic path cannot serve `shift` because it stores the return
    /// value, which here is the element — not the shrunken array.
    fn exec_shift_stmt(
        &mut self,
        target: &str,
        args: &[CallArg],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<Option<PineValue>, KernelError> {
        if args.len() != 1 {
            return Err(refused("'array.shift' takes one array"));
        }
        let current = self.eval_expr(&args[0].value, ctx, legs)?;
        match super::builtins_core::shift_parts("array.shift", current.as_ref())? {
            Some((rest, head)) => {
                self.assign_path(ctx, target, Some(PineValue::Array(rest)))?;
                Ok(head)
            }
            None => Ok(None),
        }
    }

    /// Run one `map.remove(target, key)` statement: the removal persists
    /// to the variable home while the call yields the removed value. Same
    /// split-return shape as `array.shift`, so it rides its own path for
    /// the same reason.
    fn exec_map_remove_stmt(
        &mut self,
        target: &str,
        args: &[CallArg],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<Option<PineValue>, KernelError> {
        if args.len() != 2 {
            return Err(refused("'map.remove' takes a map and key"));
        }
        let current = self.eval_expr(&args[0].value, ctx, legs)?;
        let key = self.eval_expr(&args[1].value, ctx, legs)?;
        match super::builtins_core::remove_parts("map.remove", current.as_ref(), key.as_ref())? {
            Some((rest, old)) => {
                self.assign_path(ctx, target, Some(PineValue::IntMap(rest)))?;
                Ok(old)
            }
            None => Ok(None),
        }
    }

    /// Pick the taken `if` branch for both positions: an `na` condition
    /// takes no branch (the language never executes on `na`); numbers still
    /// refuse — only `bool` branches.
    fn taken_block<'a>(
        &mut self,
        condition: &PineExpr,
        then_branch: &'a [PineStmt],
        else_branch: Option<&'a [PineStmt]>,
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<&'a [PineStmt], KernelError> {
        let taken =
            branch_of("if condition", self.eval_expr(condition, ctx, legs)?.as_ref())?
                .unwrap_or(false);
        Ok(if taken {
            then_branch
        } else {
            else_branch.unwrap_or(&[])
        })
    }

    /// Execute one body statement for its value: bare expressions, nested
    /// switches, and a trailing `if` yield; everything else runs for effect
    /// and yields nothing. An `if` yields its taken branch's trailing value
    /// (absence when no branch is taken), so a conditional as the last body
    /// statement returns its branch exactly like the language does.
    fn exec_stmt_value(
        &mut self,
        stmt: &PineStmt,
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<Option<PineValue>, KernelError> {
        let value = match stmt {
            PineStmt::Expression(expr) => self.exec_expr_stmt(expr, ctx, legs)?,
            PineStmt::Switch(switch) => self.eval_switch(switch, ctx, legs, true)?,
            PineStmt::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let block =
                    self.taken_block(condition, then_branch, else_branch.as_deref(), ctx, legs)?;
                self.exec_body_value(block, ctx, legs)?
            }
            _ => {
                self.exec_stmt(stmt, ctx, legs);
                None
            }
        };
        self.check_slot()?;
        Ok(value)
    }

    /// Surface a refusal collected through the statement slot as a value
    /// error, so nested evaluation fails closed with its reason.
    fn check_slot(&mut self) -> Result<(), KernelError> {
        if let Some(err) = self.error.take() {
            Err(err)
        } else {
            Ok(())
        }
    }

    /// Evaluate one `switch`: first-match wins, the bare-`=>` default arm
    /// catches the rest, and no match yields `na`. Statement switches run
    /// the arm for effect; expression switches additionally return the arm
    /// body's trailing value.
    fn eval_switch(
        &mut self,
        switch: &SwitchStmt,
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
        want_value: bool,
    ) -> Result<Option<PineValue>, KernelError> {
        let scrutinee = match &switch.scrutinee {
            Some(target) => Some(self.eval_expr(target, ctx, legs)?),
            None => None,
        };
        for arm in &switch.arms {
            let matched = match &arm.pattern {
                None => true,
                Some(pattern) => {
                    let probe = self.eval_expr(pattern, ctx, legs)?;
                    match &scrutinee {
                        // Bare form: first truthy arm wins, with the same
                        // bool/na rules as `if` conditions.
                        None => branch_of("switch pattern", probe.as_ref())?.unwrap_or(false),
                        Some(target) => switch_eq(target.as_ref(), probe.as_ref()),
                    }
                }
            };
            if matched {
                if want_value {
                    return self.exec_body_value(&arm.body, ctx, legs);
                }
                for stmt in &arm.body {
                    self.exec_stmt(stmt, ctx, legs);
                    if self.stopped() {
                        break;
                    }
                }
                return Ok(None);
            }
        }
        Ok(None)
    }
}

/// One refusal with a reason, fail-closed instead of miscompiled.
fn refused(message: impl Into<String>) -> KernelError {
    KernelError::Eval(message.into())
}

/// Collection calls whose return is the mutated collection: statement (and
/// trailing-value) position persists the return to a variable target,
/// giving Pine reference-type behavior. Read-only calls are excluded on
/// purpose — persisting e.g. a `size` answer would overwrite the
/// collection with one number.
fn writes_back_collection(namespace: &str, method: &str) -> bool {
    match namespace {
        "array" => matches!(
            method,
            "set" | "push" | "fill" | "clear" | "sort" | "reverse"
        ),
        "matrix" => matches!(method, "set"),
        "map" => matches!(method, "put" | "clear"),
        _ => false,
    }
}

/// Variable home of one call argument, when the argument names an
/// assignable home (bare or dotted): collections bound from such homes
/// write back on return, giving Pine reference-type behavior.
fn home_of(expr: &PineExpr) -> Option<String> {
    match expr {
        PineExpr::Variable(name) => Some(name.clone()),
        _ => None,
    }
}

/// Pick one standalone-function overload for a call: a lone overload runs
/// exactly like before (today's arity refusals and leniency preserved);
/// several overloads dispatch to the first whose arity binds and whose
/// declared parameter types match the call values, else refuse naming the
/// overload count instead of running the wrong body.
fn select_function(
    display: &str,
    overloads: &[FuncDef],
    args: &[CallArg],
    values: &[Option<PineValue>],
) -> Result<FuncDef, KernelError> {
    if overloads.len() == 1 {
        return Ok(overloads[0].clone());
    }
    overloads
        .iter()
        .find(|(params, _)| overload_matches(params, args, values))
        .cloned()
        .ok_or_else(|| {
            refused(format!(
                "no overload of '{display}' matches the given arguments ({} overloads)",
                overloads.len()
            ))
        })
}

/// Arity plus declared-type fit, mirroring [`exec_call`]'s binding walk:
/// named arguments match by name, positionals fill the rest in order,
/// defaults cover the remainder, and every provided value clears its
/// parameter's declared type.
fn overload_matches(
    params: &[Param],
    args: &[CallArg],
    values: &[Option<PineValue>],
) -> bool {
    let mut positional = args
        .iter()
        .zip(values.iter())
        .filter(|(arg, _)| arg.name.is_none());
    let mut taken_positional = 0usize;
    for param in params {
        if let Some((_, value)) = args
            .iter()
            .zip(values.iter())
            .find(|(arg, _)| arg.name.as_deref() == Some(param.name.as_str()))
        {
            if !param_matches(param, value.as_ref()) {
                return false;
            }
        } else if let Some((_, value)) = positional.next() {
            taken_positional += 1;
            if !param_matches(param, value.as_ref()) {
                return false;
            }
        } else if param.default.is_none() {
            return false;
        }
    }
    let given_positional = args.iter().filter(|arg| arg.name.is_none()).count();
    if given_positional != taken_positional {
        return false;
    }
    args.iter()
        .filter_map(|arg| arg.name.as_ref())
        .all(|given| params.iter().any(|param| &param.name == given))
}

/// One argument against its declared parameter type: untyped parameters
/// and absent values match anything; `int` widens into `float` exactly
/// like the language promotes, while every other kind must name itself
/// (dotted enum/type names match their trailing segment).
fn param_matches(param: &Param, value: Option<&PineValue>) -> bool {
    let Some(decl) = param.type_name.as_ref() else {
        return true;
    };
    let Some(present) = value else {
        return true;
    };
    match decl.name.as_str() {
        // Engine number literals lower as floats (`Number(3.0)`), so an
        // `int` parameter accepts integral floats — matching on value,
        // not representation. Fractional floats still refuse.
        "int" => match present {
            PineValue::Int(_) => true,
            PineValue::Float(v) => v.is_finite() && v.fract() == 0.0,
            _ => false,
        },
        "float" => matches!(present, PineValue::Float(_) | PineValue::Int(_)),
        "bool" => matches!(present, PineValue::Bool(_)),
        "string" => matches!(present, PineValue::Str(_)),
        "array" => matches!(present, PineValue::Array(_)),
        "matrix" => matches!(present, PineValue::Matrix(_)),
        "map" => matches!(present, PineValue::IntMap(_) | PineValue::Map(_)),
        "color" => matches!(present, PineValue::ColorHandle(_)),
        other => match present {
            PineValue::Object(instance) => {
                instance.type_name == *other
                    || trailing_type(other) == instance.type_name.as_str()
            },
            PineValue::EnumHandle(handle) => {
                handle.enum_name == *other
                    || trailing_type(other) == handle.enum_name.as_str()
            }
            _ => false,
        },
    }
}

/// Trailing segment of a (possibly module-qualified) type name:
/// method registration and receiver checks compare `ManagedTable`, never
/// the `libApex.ManagedTable` spelling the declaration used.
fn trailing_type(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// One confirmed `request.security` window: parallel tail slices of the
/// registered dataset, ending at the last confirmed bar.
struct SecurityTail<'a> {
    times: &'a [i64],
    opens: &'a [f64],
    highs: &'a [f64],
    lows: &'a [f64],
    closes: &'a [f64],
    volumes: &'a [f64],
}

impl<'a> SecurityTail<'a> {
    /// Borrow one leg column by chart-leg name.
    fn column(&self, name: &str) -> Option<&'a [f64]> {
        match name {
            "open" => Some(self.opens),
            "high" => Some(self.highs),
            "low" => Some(self.lows),
            "close" => Some(self.closes),
            "volume" => Some(self.volumes),
            _ => None,
        }
    }

    /// Own the window as whole-series input for kernel batches.
    fn ohlcv(&self) -> crate::indicator::OwnedOhlcv {
        crate::indicator::OwnedOhlcv {
            open: self.opens.to_vec(),
            high: self.highs.to_vec(),
            low: self.lows.to_vec(),
            close: self.closes.to_vec(),
            volume: self.volumes.to_vec(),
        }
    }
}

/// Last confirmed value of one leg: floats map non-finite to absence,
/// `time` reads integer milliseconds like the pull registry's column.
fn security_leg(tail: &SecurityTail, name: &str) -> Option<PineValue> {
    if name == "time" {
        return tail.times.last().copied().map(PineValue::Int);
    }
    tail.column(name)
        .and_then(|column| column.last().copied())
        .and_then(crate::indicator::fin_or_none)
        .map(PineValue::Float)
}

/// Shared tail of every live-`request.*` refusal: the fetch alternative is
/// harness injection, never a network call. The head names the method.
const LIVE_REQUEST_TAIL: &str = "is refused headlessly; inject multi-timeframe bars and tick data through the run instead of a network fetch";

/// Infer the bare `input(...)` overload from its first argument.
/// Series-shaped arguments (legs, variables, offsets) are the source input,
/// whose default is the live per-bar series expression rather than a stored
/// scalar; literals infer from their shape; computed defaults infer from
/// their value kind. A variable first argument always reads as source: for a
/// constant variable the live value equals the frozen default, while for a
/// series variable only the live read is correct. Missing or collection
/// defaults refuse with a reason.
fn bare_input_method(
    first: Option<&PineExpr>,
    value: Option<&Option<PineValue>>,
) -> Result<&'static str, KernelError> {
    match first {
        None => Err(refused("'input' takes a default value")),
        Some(
            PineExpr::BuiltinVariable(_)
            | PineExpr::Variable(_)
            | PineExpr::SeriesOffset { .. },
        ) => Ok("source"),
        Some(PineExpr::Number(v)) => {
            if v.fract() == 0.0
                && (-9_007_199_254_740_992.0..=9_007_199_254_740_992.0).contains(v)
            {
                Ok("int")
            } else {
                Ok("float")
            }
        }
        Some(PineExpr::Boolean(_)) => Ok("bool"),
        Some(PineExpr::StringLiteral(_)) => Ok("string"),
        Some(_) => match value {
            Some(Some(PineValue::EnumHandle(_))) => Ok("enum"),
            Some(Some(PineValue::ColorHandle(_))) => Ok("color"),
            Some(Some(PineValue::Int(_))) => Ok("int"),
            Some(Some(PineValue::Float(_))) => Ok("float"),
            Some(Some(PineValue::Bool(_))) => Ok("bool"),
            Some(Some(PineValue::Str(_))) => Ok("string"),
            _ => Err(refused(
                "'input' takes a default int, float, bool, string, source, enum member, or color",
            )),
        },
    }
}

/// Outcome of one method-call dispatch: the receiver to store back, the
/// call value, and whether the native lease served. The flag routes the
/// write-back: native serves keep the synced lease (`put_object` without a
/// bump), Pine bodies invalidate it.
struct MethodOutcome {
    /// Receiver to store back to its home.
    receiver: UserInstance,
    /// Call value (`None` is Pine `na`).
    value: Result<Option<PineValue>, KernelError>,
    /// Whether the native lease served this call.
    served_native: bool,
}

/// Where a method-call receiver lives while the call runs, so the
/// (possibly mutated) instance returns to the same home afterwards.
#[derive(Debug)]
enum ObjectHome {    /// Series variable in engine state.
    Variables(String),
    /// Persistent `var` (`false`) or `varip` (`true`) binding.
    VarStore(String, bool),
    /// Field of the innermost receiver on the stack.
    ThisField(String),
    /// Field path under a taken root object (`bars.hist_l`): the mutated
    /// field instance writes back into its parent chain before the root
    /// returns to its own home, so method calls on nested field objects
    /// mutate the stored object exactly like Pine references.
    NestedField {
        /// The root home the field path was taken from.
        home: Box<ObjectHome>,
        /// Outer-to-inner `(parent, field)` chain with the callee taken
        /// out of the innermost parent.
        parents: Vec<(UserInstance, String)>,
    },
    /// Variable aliasing one map entry: the mutated instance writes back
    /// to its variable home Link and to the map entry, so method calls
    /// through `map.get` handles mutate the stored object exactly like
    /// Pine references (instead of a dead copy).
    MapEntry {
        /// The variable home the instance was taken from.
        inner: Box<ObjectHome>,
        /// Variable holding the map.
        map_var: String,
        /// Canonical key encoding.
        key: String,
        /// Original key value.
        key_value: PineValue,
    },
}

/// One live method receiver: the instance plus the alias the body bound
/// it under (`level` in `updateLevelTouch(MarketLevel level, ...)`).
/// Reads, nested calls, and field writes through the alias hit this slot,
/// so the body always observes one live copy, never a stale clone.
#[derive(Debug)]
struct ReceiverSlot {
    instance: UserInstance,
    alias: String,
}

/// Pack one drawing call's evaluated arguments as a lifecycle row: names
/// preserved, values cloned, absence kept as absence.
fn draw_args(args: &[CallArg], values: &[Option<PineValue>]) -> Vec<DrawingArg> {
    args.iter()
        .zip(values.iter())
        .map(|(arg, value)| DrawingArg {
            name: arg.name.clone(),
            value: value.clone(),
        })
        .collect()
}

/// Read one named drawing argument (`title`, `color`, `display`, ...):
/// the evaluated value, whatever it is (absence included).
fn named_value(
    args: &[CallArg],
    values: &[Option<PineValue>],
    name: &str,
) -> Option<PineValue> {
    args.iter()
        .zip(values.iter())
        .find(|(arg, _)| arg.name.as_deref() == Some(name))
        .and_then(|(_, value)| value.clone())
}

/// Read a `title` argument as text, falling back when absent or non-string.
fn named_title(args: &[CallArg], values: &[Option<PineValue>], fallback: &str) -> String {
    match named_value(args, values, "title") {
        Some(PineValue::Str(text)) => text,
        _ => fallback.to_owned(),
    }
}

/// Read a plot-family title: a named `title` wins; otherwise the second
/// positional string (`hline(price, "Name", ...)`, `plot(series, "Name",
/// ...)`); otherwise the fallback. Shared by the recorders so all three
/// name the same call the same way.
fn call_title(args: &[CallArg], values: &[Option<PineValue>], fallback: &str) -> String {
    if let Some(PineValue::Str(text)) = named_value(args, values, "title") {
        return text;
    }
    let positional_second = args
        .iter()
        .zip(values.iter())
        .filter(|(arg, _)| arg.name.is_none())
        .nth(1)
        .and_then(|(_, value)| match value {
            Some(PineValue::Str(text)) => Some(text.clone()),
            _ => None,
        });
    positional_second.unwrap_or_else(|| fallback.to_owned())
}

/// The capture key for one `plot()` call site: the debug rendering of its
/// arguments, shared by the plot recorder, the plot-variable tags, and the
/// fill pairer so all three name the same site.
fn plot_site_key(args: &[CallArg]) -> String {
    format!("plot@{args:?}")
}

/// The name an `export` wrapper publishes, for the import graph to consume.
fn export_label(inner: &PineStmt) -> Option<String> {
    match inner {
        PineStmt::FuncDecl { name, .. }
        | PineStmt::MethodDecl { name, .. }
        | PineStmt::ConstDecl { name, .. }
        | PineStmt::EnumDecl { name, .. }
        | PineStmt::TypeDecl { name, .. } => Some(name.clone()),
        _ => None,
    }
}

/// Boolean positions take `Bool` only and pass `na` through as "no branch":
/// `None` means the condition is absent (callers skip), numbers still
/// refuse — the reference truthiness is a correction, not a copy.
fn branch_of(what: &str, value: Option<&PineValue>) -> Result<Option<bool>, KernelError> {
    match value {
        Some(PineValue::Bool(flag)) => Ok(Some(*flag)),
        None => Ok(None),
        Some(other) => Err(refused(format!(
            "'{what}' needs a bool, got {other}"
        ))),
    }
}

/// `switch` arm matching: first-match wins across numbers (with the
/// comparator tolerance), strings, bools, and enum members. Absence never
/// matches, and mismatched kinds never match — unmatched switches yield
/// `na` through the default arm or the empty result.
fn switch_eq(first: Option<&PineValue>, second: Option<&PineValue>) -> bool {
    match (first, second) {
        (Some(PineValue::Int(a)), Some(PineValue::Int(b))) => a == b,
        (Some(PineValue::Float(a)), Some(PineValue::Float(b))) => (a - b).abs() < EPSILON,
        (Some(a), Some(b))
            if matches!(a, PineValue::Float(_) | PineValue::Int(_))
                && matches!(b, PineValue::Float(_) | PineValue::Int(_)) =>
        {
            match (a.as_float(), b.as_float()) {
                (Some(x), Some(y)) => (x - y).abs() < EPSILON,
                _ => false,
            }
        }
        (Some(PineValue::Str(a)), Some(PineValue::Str(b))) => a == b,
        (Some(PineValue::Bool(a)), Some(PineValue::Bool(b))) => a == b,
        (Some(PineValue::EnumHandle(a)), Some(PineValue::EnumHandle(b))) => {
            a.enum_name == b.enum_name && a.member == b.member
        }
        _ => false,
    }
}

/// Three-valued `and`/`or` for one absent side (both-present pairs keep the
/// strict bool table in [`binary`]): a failing `and` annihilates to `false`,
/// a passing `or` annihilates to `true`, everything else stays `na`.
/// Non-bool operands refuse — numbers are never truthy, even beside `na`.
fn kleene_absent(
    op: BinaryOp,
    left: Option<&PineValue>,
    right: Option<&PineValue>,
) -> Result<Option<PineValue>, KernelError> {
    let mut decided: Option<bool> = None;
    for side in [left, right] {
        match side {
            Some(PineValue::Bool(flag)) => {
                let annihilates = matches!(op, BinaryOp::And) && !flag
                    || matches!(op, BinaryOp::Or) && *flag;
                if annihilates {
                    decided = Some(*flag);
                }
            }
            Some(other) => {
                return Err(refused(format!(
                    "operator '{op}' needs bools, got {other}"
                )));
            }
            None => {}
        }
    }
    Ok(decided.map(PineValue::Bool))
}

/// `for` bounds are whole numbers: `int`s, or integral floats. Fractional
/// bounds refuse (the reference truncation is not copied).
fn whole_number(what: &str, value: Option<&PineValue>) -> Result<i64, KernelError> {
    match value {
        Some(PineValue::Int(v)) => Ok(*v),
        // Integral and range-checked above the cast, so the conversion below
        // is exact; fractional or out-of-range floats refuse instead.
        #[allow(clippy::cast_possible_truncation)]
        Some(PineValue::Float(f))
            if f.fract() == 0.0 && *f >= i64::MIN as f64 && *f <= i64::MAX as f64 =>
        {
            Ok(*f as i64)
        }
        None => Err(refused(format!("'{what}' is na"))),
        Some(other) => Err(refused(format!(
            "'{what}' needs an int, got {other}"
        ))),
    }
}

/// Whole-number cast: truncation toward zero, absence stays absence;
/// non-finite and out-of-range floats refuse, anything non-numeric refuses.
fn int_cast(value: Option<&PineValue>) -> Result<Option<PineValue>, KernelError> {
    match value {
        None => Ok(None),
        Some(PineValue::Int(v)) => Ok(Some(PineValue::Int(*v))),
        Some(PineValue::Float(v)) if v.is_nan() => Ok(None),
        Some(PineValue::Float(v)) if v.is_finite() => {
            // `2^63` is exactly representable; anything at or past it (or
            // at/below `-2^63`) cannot land in an `i64`.
            const LIMIT: f64 = 9_223_372_036_854_776_576.0;
            let truncated = v.trunc();
            if (-LIMIT..LIMIT).contains(&truncated) {
                #[allow(clippy::cast_possible_truncation)]
                Ok(Some(PineValue::Int(truncated as i64)))
            } else {
                Err(refused(format!("'int' overflows on {v}")))
            }
        }
        Some(other) => Err(refused(format!("'int' takes a number, got {other}"))),
    }
}

/// Float cast: integers convert, floats pass through, absence stays
/// absence; anything non-numeric refuses instead of inventing a value.
fn float_cast(value: Option<&PineValue>) -> Result<Option<PineValue>, KernelError> {
    match value {
        None => Ok(None),
        Some(PineValue::Int(v)) => Ok(Some(PineValue::Float(*v as f64))),
        Some(PineValue::Float(v)) if v.is_nan() => Ok(None),
        Some(PineValue::Float(v)) => Ok(Some(PineValue::Float(*v))),
        Some(other) => Err(refused(format!("'float' takes a number, got {other}"))),
    }
}

/// Absence on legs and in memory: `None`, or a `NaN` float.
fn is_absent(value: Option<&PineValue>) -> bool {
    match value {
        None => true,
        Some(PineValue::Float(v)) => v.is_nan(),
        Some(_) => false,
    }
}

/// Absence inside a present slot (a `NaN` float).
fn is_absent_value(value: &PineValue) -> bool {
    matches!(value, PineValue::Float(v) if v.is_nan())
}

/// One leg read at a data index; `NaN` legs are `na`.
fn read_leg_slice(leg: &[f64], index: usize) -> Option<PineValue> {
    leg.get(index)
        .and_then(|v| crate::indicator::fin_or_none(*v).map(PineValue::Float))
}

/// Closed-bar cursor flags. Every evaluated bar is a new confirmed history
/// bar; warmup bars never reach this table, so `isfirst` stays false when
/// the driver skips bar zero. Tick/realtime emulation instead follows the
/// `time` leg: `isnew` fires where the bar-open time changes (the first
/// tick of each bar), `isconfirmed` where the next update opens a new bar
/// (the last tick), and the run reports history-false/realtime-true,
/// mirroring a live feed over historical ticks.
fn barstate_flag(member: &str, ctx: &RunCtx<'_>, legs: &Legs, tick_mode: bool) -> Option<bool> {
    let cursor = ctx.cursor();
    let last = ctx.bar_count().checked_sub(1)?;
    let time_at = |at: usize| legs.time.get(at).copied();
    // Millisecond timestamps are exact integers in `f64`, so bitwise
    // equality is the correct bar-identity test here — no epsilon.
    let changed_since_prev = || {
        cursor
            .checked_sub(1)
            .and_then(|prev| match (time_at(prev), time_at(cursor)) {
                (Some(before), Some(now)) => Some(before.to_bits() != now.to_bits()),
                _ => None,
            })
            .unwrap_or(true)
    };
    let changes_before_next = || match (time_at(cursor), legs.time.get(cursor + 1).copied()) {
        (Some(now), Some(next)) => now.to_bits() != next.to_bits(),
        _ => true,
    };
    match member {
        "isfirst" => Some(cursor == 0),
        "isnew" => Some(if tick_mode { changed_since_prev() } else { true }),
        "isconfirmed" => Some(if tick_mode {
            changes_before_next()
        } else {
            true
        }),
        "ishistory" => Some(!tick_mode),
        "islast" => Some(cursor == last),
        "isrealtime" => Some(tick_mode),
        _ => None,
    }
}

/// One named-leg read at the cursor.
fn read_leg(legs: &Legs, name: &str, cursor: usize) -> Option<PineValue> {
    legs.named(name)
        .and_then(|leg| read_leg_slice(leg, cursor))
}

/// One named-leg raw float at the cursor (`NaN` when absent).
fn leg_or_nan(legs: &Legs, name: &str, cursor: usize) -> f64 {
    legs.named(name)
        .and_then(|leg| leg.get(cursor).copied())
        .unwrap_or(f64::NAN)
}

/// Current chart-bar time in milliseconds for injection lookups: tick
/// emulation legs already run in Pine-native milliseconds, while
/// closed-bar legs run in whole seconds (pinned runtime-shim behavior)
/// and scale here. Datasets stamp milliseconds, exact below 2^53.
#[allow(clippy::cast_possible_truncation)]
fn chart_ms(legs: &Legs, cursor: usize, tick_mode: bool) -> i64 {
    let stamp = leg_or_nan(legs, "time", cursor);
    if tick_mode {
        stamp as i64
    } else {
        (stamp * 1000.0) as i64
    }
}

/// Static name of one built-in series variable.
fn builtin_name(var: BuiltinVar) -> &'static str {
    match var {
        BuiltinVar::Open => "open",
        BuiltinVar::High => "high",
        BuiltinVar::Low => "low",
        BuiltinVar::Close => "close",
        BuiltinVar::Volume => "volume",
        BuiltinVar::Time => "time",
    }
}

/// Binary evaluation: `na` propagates through arithmetic; comparisons are
/// two-valued (an absent side reads false, except `!=` which reads true —
/// the reference never returns `na` from a comparison, so `x == na` is
/// false and `na()` stays the only absence test); mixed kinds outside the
/// reference table refuse instead of coercing.
#[allow(clippy::too_many_lines)]
fn binary(
    op: BinaryOp,
    left: Option<&PineValue>,
    right: Option<&PineValue>,
) -> Result<Option<PineValue>, KernelError> {
    if left.is_none() || right.is_none() {
        // Three-valued connectives decide on one absent side; comparisons
        // answer without it; everything else propagates `na`.
        return match op {
            BinaryOp::And | BinaryOp::Or => kleene_absent(op, left, right),
            BinaryOp::Equal
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual => Ok(Some(PineValue::Bool(false))),
            BinaryOp::NotEqual => Ok(Some(PineValue::Bool(true))),
            _ => Ok(None),
        };
    }
    let (Some(first), Some(second)) = (left, right) else {
        return Ok(None);
    };
    match (first, second) {
        (PineValue::Float(a), PineValue::Float(b)) => float_binary(op, *a, *b),
        (PineValue::Int(a), PineValue::Int(b)) => int_binary(op, *a, *b),
        (mix_a, mix_b)
            if matches!(mix_a, PineValue::Float(_) | PineValue::Int(_))
                && matches!(mix_b, PineValue::Float(_) | PineValue::Int(_)) =>
        {
            let (Some(a), Some(b)) = (first.as_float(), second.as_float()) else {
                return Ok(None);
            };
            float_binary(op, a, b)
        }
        (PineValue::Bool(a), PineValue::Bool(b)) => Ok(Some(PineValue::Bool(match op {
            BinaryOp::And => *a && *b,
            BinaryOp::Or => *a || *b,
            BinaryOp::Equal => a == b,
            BinaryOp::NotEqual => a != b,
            _ => {
                return Err(refused(format!(
                    "operator '{op}' does not apply to bools"
                )));
            }
        }))),
        (PineValue::Str(a), PineValue::Str(b)) => Ok(Some(match op {
            BinaryOp::Add => PineValue::Str(format!("{a}{b}")),
            BinaryOp::Equal => PineValue::Bool(a == b),
            BinaryOp::NotEqual => PineValue::Bool(a != b),
            BinaryOp::Less => PineValue::Bool(a < b),
            BinaryOp::LessEqual => PineValue::Bool(a <= b),
            BinaryOp::Greater => PineValue::Bool(a > b),
            BinaryOp::GreaterEqual => PineValue::Bool(a >= b),
            _ => {
                return Err(refused(format!(
                    "operator '{op}' does not apply to strings"
                )));
            }
        })),
        (PineValue::Str(text), other)
            if matches!(other, PineValue::Float(_) | PineValue::Int(_)) =>
        {
            string_number_concat(op, text, other)
        }
        (other, PineValue::Str(text))
            if matches!(other, PineValue::Float(_) | PineValue::Int(_)) =>
        {
            match op {
                BinaryOp::Add => Ok(Some(PineValue::Str(format!("{other}{text}")))),
                _ => Err(refused(format!(
                    "operator '{op}' does not apply to numbers and strings"
                ))),
            }
        }
        (PineValue::Bool(_), _) | (_, PineValue::Bool(_))
            if matches!(op, BinaryOp::And | BinaryOp::Or) =>
        {
            Err(refused(format!(
                "operator '{op}' needs bools on both sides, got {first} and {second}"
            )))
        }
        (PineValue::Drawing(a), PineValue::Drawing(b)) => Ok(Some(PineValue::Bool(match op {
            BinaryOp::Equal => a == b,
            BinaryOp::NotEqual => a != b,
            _ => {
                return Err(refused(format!(
                    "operator '{op}' does not apply to drawing objects"
                )));
            }
        }))),
        (PineValue::EnumHandle(a), PineValue::EnumHandle(b)) => Ok(Some(PineValue::Bool(match op {
            BinaryOp::Equal => a.enum_name == b.enum_name && a.member == b.member,
            BinaryOp::NotEqual => a.enum_name != b.enum_name || a.member != b.member,
            _ => {
                return Err(refused(format!(
                    "operator '{op}' does not apply to enum members"
                )));
            }
        }))),
        _ => Err(refused(format!(
            "operator '{op}' does not apply to {first} and {second}"
        ))),
    }
}

/// `number + string` in either order, mirroring the reference concat.
fn string_number_concat(
    op: BinaryOp,
    text: &str,
    second: &PineValue,
) -> Result<Option<PineValue>, KernelError> {
    match op {
        BinaryOp::Add => Ok(Some(PineValue::Str(format!("{text}{second}")))),
        _ => Err(refused(format!(
            "operator '{op}' does not apply to strings and numbers"
        ))),
    }
}

/// Float arithmetic and comparisons; equality keeps the reference tolerance.
/// Boolean connectives refuse floats (the reference truthiness is a
/// correction, not a copy).
fn float_binary(op: BinaryOp, a: f64, b: f64) -> Result<Option<PineValue>, KernelError> {
    Ok(Some(match op {
        BinaryOp::Add => PineValue::Float(a + b),
        BinaryOp::Subtract => PineValue::Float(a - b),
        BinaryOp::Multiply => PineValue::Float(a * b),
        // A zero denominator reads absence, not a refusal: shipped code
        // divides zero-initialized fields before their first fetch (the
        // market-context `atr` on bar 0), and Pine answers `na`/`inf`
        // there, never an abort. Absence propagates through the existing
        // `na`/`nz` guards; infinities never enter the engine, so no
        // `inf` contagion audit is owed downstream.
        BinaryOp::Divide => {
            if b == 0.0 {
                return Ok(None);
            }
            PineValue::Float(a / b)
        }
        BinaryOp::Modulo => {
            if b == 0.0 {
                return Ok(None);
            }
            PineValue::Float(a % b)
        }
        BinaryOp::Equal => PineValue::Bool((a - b).abs() < EPSILON),
        BinaryOp::NotEqual => PineValue::Bool((a - b).abs() >= EPSILON),
        BinaryOp::Less => PineValue::Bool(a < b),
        BinaryOp::LessEqual => PineValue::Bool(a <= b),
        BinaryOp::Greater => PineValue::Bool(a > b),
        BinaryOp::GreaterEqual => PineValue::Bool(a >= b),
        BinaryOp::And | BinaryOp::Or => {
            return Err(refused(format!(
                "operator '{op}' needs bools, got numbers"
            )));
        }
    }))
}

/// Integer arithmetic (`/` widens to float, as in Pine); connectives refuse.
fn int_binary(op: BinaryOp, a: i64, b: i64) -> Result<Option<PineValue>, KernelError> {
    Ok(Some(match op {
        BinaryOp::Add => PineValue::Int(a.checked_add(b).ok_or_else(|| {
            refused("integer addition overflowed")
        })?),
        BinaryOp::Subtract => PineValue::Int(a.checked_sub(b).ok_or_else(|| {
            refused("integer subtraction overflowed")
        })?),
        BinaryOp::Multiply => PineValue::Int(a.checked_mul(b).ok_or_else(|| {
            refused("integer multiplication overflowed")
        })?),
        BinaryOp::Divide => {
            if b == 0 {
                return Ok(None);
            }
            PineValue::Float(a as f64 / b as f64)
        }
        BinaryOp::Modulo => {
            if b == 0 {
                return Ok(None);
            }
            PineValue::Int(a.checked_rem(b).ok_or_else(|| {
                refused("integer remainder overflowed")
            })?)
        }
        BinaryOp::Equal => PineValue::Bool(a == b),
        BinaryOp::NotEqual => PineValue::Bool(a != b),
        BinaryOp::Less => PineValue::Bool(a < b),
        BinaryOp::LessEqual => PineValue::Bool(a <= b),
        BinaryOp::Greater => PineValue::Bool(a > b),
        BinaryOp::GreaterEqual => PineValue::Bool(a >= b),
        BinaryOp::And | BinaryOp::Or => {
            return Err(refused(format!(
                "operator '{op}' needs bools, got numbers"
            )));
        }
    }))
}

/// Unary evaluation: negation on numbers, `not` on bools only.
fn unary(op: UnaryOp, value: Option<&PineValue>) -> Result<Option<PineValue>, KernelError> {
    if value.is_none() {
        return Ok(None);
    }
    let Some(present) = value else {
        return Ok(None);
    };
    match (op, present) {
        (UnaryOp::Negate, PineValue::Float(v)) => Ok(Some(PineValue::Float(-*v))),
        (UnaryOp::Negate, PineValue::Int(v)) => Ok(Some(PineValue::Int(v.checked_neg().ok_or_else(
            || refused("integer negation overflowed"),
        )?))),
        (UnaryOp::Not, PineValue::Bool(flag)) => Ok(Some(PineValue::Bool(!*flag))),
        (UnaryOp::Not, _) => Err(refused(format!(
            "operator '{op}' needs a bool, got {present}"
        ))),
        _ => Err(refused(format!("operator '{op}' does not apply to {present}"))),
    }
}

#[cfg(test)]
#[path = "../../tests/pine_eval.rs"]
mod tests;

/// Injection-proof suite: the DVP higher-timeframe fetch served from
/// injected datasets, with look-ahead/gap calibration and refusal pins.
#[cfg(test)]
#[path = "../../tests/pine_dvp_security.rs"]
mod dvp_security_tests;

/// Task-indicator cross-checks: indicator outputs that reduce to kernel
/// expressions assert against `polars::Expr` references; the rest are
/// named golden-only with reasons.
#[cfg(test)]
#[path = "../../tests/pine_task_parity.rs"]
mod task_parity_tests;
/// v6 surface suite: switch, enums, user functions/methods, tuples, plots.
#[cfg(test)]
#[path = "../../tests/pine_v6.rs"]
mod v6_tests;

/// Headless indicator proof: the linked library triple over real history.
#[cfg(test)]
#[path = "../../tests/pine_fastmas.rs"]
mod fastmas_tests;

/// Calibration gate: bridge coverage, accept/reject tallies, controls.
#[cfg(test)]
#[path = "../../tests/pine_calibration.rs"]
mod calibration_tests;

/// Headless runtime suite: cursor flags, injected time, chart scope, session
/// and source inputs, and live-fetch refusal.
#[cfg(test)]
#[path = "../../tests/pine_runtime_shims.rs"]
mod runtime_shim_tests;

/// Real-library suite: the shipped utility library linked with drivers.
#[cfg(test)]
#[path = "../../tests/pine_libutil.rs"]
mod libutil_tests;

/// Real-library suite: the shipped moving-average library linked with
/// drivers, pinned against kernel twins and hand steps.
#[cfg(test)]
#[path = "../../tests/pine_libmas.rs"]
mod libmas_tests;

// Real-library suite: the shipped tick-engine library linked with
// drivers, pinned against hand-computed expectations.
#[cfg(test)]
#[path = "../../tests/pine_libbars.rs"]
mod libbars_tests;

// Drawing-capture suite: plotcandle/barcolor/fill captures plus the
// label/table/box/line lifecycle recorders, pinned as data.
#[cfg(test)]
#[path = "../../tests/pine_draw_captures.rs"]
mod draw_tests;

// Market-core suite: the shipped apex library linked with drivers,
// pinned against kernel twins, hand-computed branches, and a labeled
// regime fixture set.
#[cfg(test)]
#[path = "../../tests/pine_libapex.rs"]
mod libapex_tests;

// Buffer-library suite: the shipped buffers library linked with drivers,
// pinned against hand-computed ring/array/statistics expectations.
#[cfg(test)]
#[path = "../../tests/pine_libbuffers.rs"]
mod libbuffers_tests;
