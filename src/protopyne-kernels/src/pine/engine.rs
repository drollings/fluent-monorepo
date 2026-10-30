//! Engine facade, runtime plumbing, run driver, and selection surface.
//!
//! [`PineEngine`] owns one run: configuration, injected datasets, engine
//! selection, and the per-run tree state it drives bar by bar.

use std::collections::HashMap;
use std::sync::Arc;


#[cfg(any(test, feature = "oracle"))]
use super::builtins_state::RunBuiltins;

use protopyne_types::Bar;
use protopyne_types::EpochMs;
use protopyne_types::Timeframe;

#[cfg(any(test, feature = "oracle"))]
use super::oracle::EngineState;
use super::semantics::refused;
#[cfg(any(test, feature = "oracle"))]
use super::semantics::Legs;
use super::syntax::PineProgram;
use crate::error::KernelError;
use crate::inputs::InputSet;
use crate::native_libs::{FetchCounters, NativeOptions};
use crate::runtime::{PineRuntime, PineRuntimeConfig};
use crate::value::PineValue;
use crate::vars::ModuleKey;

/// Fail-closed bound for `for` total iterations and `while` iterations.
/// Breaching loops refuse instead of hanging the run.
pub const MAX_LOOP_ITERATIONS: usize = 10_000;

/// Fail-closed bound for nested user-function/method calls. The language
/// forbids recursion; the bound turns accidental cycles into reasons.
pub const MAX_CALL_DEPTH: usize = 64;

/// Module identity for a scope key: the linked module key, or `None` for an
/// unlinked program.
pub(crate) fn module_key(module: &str) -> Option<ModuleKey> {
    (!module.is_empty()).then(|| module.to_owned())
}

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
    /// Every `strategy.entry/exit/close/close_all` call in execution order:
    /// harness observations of the broker dialogue, never venue authority.
    pub strategy_calls: Vec<StrategyCall>,
}

/// One recorded `strategy.*` broker call: the harness observation of what
/// the strategy asked its broker to do on one closed bar. Quantities,
/// prices, and comments ride along verbatim; nothing here ever reaches a
/// venue — the native backtester replays the same tape through real fills
/// and the parity harness compares the two decision streams.
#[derive(Debug, Clone, PartialEq)]
pub struct StrategyCall {
    /// Bar index that committed the call.
    pub bar: usize,
    /// `entry`, `exit`, `close`, or `close_all`.
    pub kind: &'static str,
    /// Order id (`exit` also carries `from_entry`).
    pub id: String,
    /// `strategy.long`/`strategy.short` direction, when the call names one.
    pub direction: Option<String>,
    /// `qty`/`qty_percent` legs, when present.
    pub qty: Option<f64>,
    /// `qty_percent` leg, when present.
    pub qty_percent: Option<f64>,
    /// Price legs (`limit`, `stop`, `profit`, `loss`), when present.
    pub limit: Option<f64>,
    /// Stop leg, when present.
    pub stop: Option<f64>,
    /// Take-profit leg, when present.
    pub profit: Option<f64>,
    /// Stop-loss leg, when present.
    pub loss: Option<f64>,
    /// Comment legs (`comment`, `comment_profit`, `comment_loss`, ...).
    pub comment: Option<String>,
    /// Entry id this exit closes, when named.
    pub from_entry: Option<String>,
}

/// One captured `plot()` series: title and display flag plus one value per
/// evaluated bar (`None` entries are Pine `na`).
#[derive(Debug, Clone)]
pub struct PlotCapture {
    /// Call-site key (textual arguments), distinguishing same-titled plots.
    pub(crate) site: String,
    /// The `title` argument, or a positional fallback.
    pub title: String,
    /// The last-seen `display` argument handle, when given.
    pub display: Option<PineValue>,
    /// One value per evaluated bar, in order.
    pub values: Vec<Option<PineValue>>,
}

impl PlotCapture {
    /// New capture for one `plot()` site: title bound at first sight,
    /// display updated per bar, values appended per bar. The compiled
    /// loop is the only constructor caller today (the tree-walk builds
    /// captures inline); the single copy stays here so both engines
    /// share the shape.
    #[cfg(feature = "bytecode")]
    #[must_use]
    pub(crate) fn new(site: String, title: String) -> Self {
        Self {
            site,
            title,
            display: None,
            values: Vec::new(),
        }
    }
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
    pub(crate) site: String,
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
    pub(crate) site: String,
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
    pub(crate) site: String,
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
    pub(crate) site: String,
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
    pub(crate) site: String,
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
    /// Property names this row wrote. Creation and deletion rows write none
    /// here — creation geometry rides `args`, deletion removes the object —
    /// while an update row names the setter's properties (`set_x` writes
    /// `x`, `set_xy1` writes `x1` and `y1`), so a consumer can tell a
    /// repaint from a move without a setter table of its own.
    pub props: Vec<String>,
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
    pub fn new(key: &str, program: PineProgram, aliases: HashMap<String, String>) -> Self {
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
    /// Row bucket-open timestamps.
    pub times: Vec<EpochMs>,
    /// Row bucket-close timestamps, one per row: the instant the row's bucket
    /// finished. [`None`] means the close is unknown, and no visibility
    /// predicate may treat such a row as confirmed.
    pub closes_at: Vec<Option<EpochMs>>,
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
    /// Row count; every column always agrees with it (enforced at injection).
    #[must_use]
    pub fn len(&self) -> usize {
        self.times.len()
    }

    /// True when no rows are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.times.is_empty()
    }

    /// Build an injection dataset from closed bars of one timeframe: bucket
    /// opens, one session-aware close per bar, and float legs converted exactly
    /// like kernel windows
    /// ([`OwnedOhlcv::from_bars`](crate::indicator::OwnedOhlcv::from_bars)),
    /// so injected security columns agree with chart legs bit-for-bit. The
    /// harness constructor for [`inject_security`](PineEngine::inject_security)
    /// and [`inject_lower_tf`](PineEngine::inject_lower_tf) datasets;
    /// headless suites share it instead of copying the conversion.
    ///
    /// The close of each bar is the end of its own bucket, clipped to the
    /// exchange session close — the shared bucketing rule, never a duration
    /// guess.
    #[must_use]
    pub fn of_bars(bars: &[Bar], timeframe: Timeframe) -> Self {
        let legs = crate::indicator::OwnedOhlcv::from_bars(bars);
        let times: Vec<EpochMs> = bars
            .iter()
            .map(|bar| EpochMs::from(bar.open_time()))
            .collect();
        let stamps: Vec<i64> = times.iter().map(|stamp| stamp.get()).collect();
        let closes_at: Vec<Option<EpochMs>> =
            crate::bars::bucket_closes(&stamps, timeframe.duration_ms())
                .into_iter()
                .map(|close| close.map(EpochMs::from))
                .collect();
        Self {
            times,
            closes_at,
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
    pub(crate) symbol: String,
    pub(crate) timeframe: String,
    /// Shared so serving one window never copies the tape: a lookup hands out
    /// a handle, and the borrow it grants outlives the mutable evaluation it
    /// is read during.
    pub(crate) dataset: Arc<LtfDataset>,
}

/// Execution selection for one engine: the compiled loop or the tree-walk
/// oracle. The default is the compiled loop, so every driver runs one
/// execution path unless it explicitly opts into the oracle for proof.
/// The tree-walk variant exists only where the interpreter does (test
/// and `oracle` builds): production code cannot name another engine,
/// so no production run can silently switch or retire at run time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg(feature = "bytecode")]
pub enum BytecodeMode {
    /// The compiled slot VM: compile once per distinct program (memoized
    /// by content hash, so repeated runs skip the pipeline); linked
    /// programs run the loop, and anything else refuses typed in
    /// production or answers on the tree where the oracle exists.
    #[default]
    Compiled,
    /// Evaluate by AST-walking every bar. The oracle opt-out: available
    /// where the tree interpreter exists (test and `oracle` builds), and
    /// unnameable everywhere else.
    #[cfg(any(test, feature = "oracle"))]
    TreeWalk,
}

/// One evaluator seam for tests: the contract every engine honors —
/// linked modules over bars (or per-tick updates) with caller inputs,
/// reporting per-bar snapshots or the first refusal. The facade
/// implements it for production; the tree oracle implements it as the
/// differential proof. Production code depends on the facade, never on
/// this trait or the oracle behind it.
pub trait PineEvaluator {
    /// Evaluate linked `modules` (dependency-first) over `bars` with
    /// `inputs`, returning per-bar snapshots or the first refusal.
    fn run_linked(
        &mut self,
        modules: &[LinkedModule],
        bars: &[Bar],
        inputs: InputSet,
    ) -> Result<RunOutcome, KernelError>;
    /// Evaluate linked `modules` over per-tick updates, mirroring a live
    /// feed; length mismatch refuses fail-closed.
    fn run_linked_ticks(
        &mut self,
        modules: &[LinkedModule],
        bars: &[Bar],
        tick_millis: &[i64],
        inputs: InputSet,
    ) -> Result<RunOutcome, KernelError>;
}

impl PineEvaluator for PineEngine {
    fn run_linked(
        &mut self,
        modules: &[LinkedModule],
        bars: &[Bar],
        inputs: InputSet,
    ) -> Result<RunOutcome, KernelError> {
        PineEngine::run_linked(self, modules, bars, inputs)
    }

    fn run_linked_ticks(
        &mut self,
        modules: &[LinkedModule],
        bars: &[Bar],
        tick_millis: &[i64],
        inputs: InputSet,
    ) -> Result<RunOutcome, KernelError> {
        PineEngine::run_linked_ticks(self, modules, bars, tick_millis, inputs)
    }
}

/// The executor. Owns its runtime config; all per-run state resets on
/// [`run`](Self::run), so one engine serves sequential runs safely.
/// Registered datasets (lower-timeframe and security) survive across runs
/// (they are harness configuration, not run state).
pub struct PineEngine {
    runtime: PineRuntime,
    #[cfg(any(test, feature = "oracle"))]
    state: EngineState,
    ltf_injections: Vec<InjectedDataset>,
    security_injections: Vec<InjectedDataset>,
    native: NativeOptions,
    /// Execution selection (see [`BytecodeMode`]). Sticky across runs;
    /// per-run state still resets every run.
    #[cfg(feature = "bytecode")]
    bytecode_mode: BytecodeMode,
}

impl PineEngine {
    /// Bind an engine to a runtime config (warmup enforced by the driver).
    #[must_use]
    pub fn new(config: PineRuntimeConfig) -> Self {
        Self {
            runtime: PineRuntime::new(config),
            #[cfg(any(test, feature = "oracle"))]
            state: EngineState::fresh(),
            ltf_injections: Vec::new(),
            security_injections: Vec::new(),
            // Execution defaults to native where providers exist: imports
            // of libraries with Rust implementations run the Rust code,
            // with fail-closed Decline back to Pine per call. Explicit
            // `enabled: false` keeps Pine-oracle runs.
            native: NativeOptions {
                enabled: true,
                only_symbols: None,
            },
            #[cfg(feature = "bytecode")]
            bytecode_mode: BytecodeMode::Compiled,
        }
    }

    /// Select the execution loop for subsequent runs: the default compiled
    /// loop, or the tree-walk oracle where it exists. The selection never
    /// changes a signature or a run contract — both selections report
    /// byte-identical outcomes on lowered programs; unlowered programs run
    /// the tree where the oracle exists and refuse typed elsewhere.
    #[cfg(feature = "bytecode")]
    pub fn set_bytecode_mode(&mut self, mode: BytecodeMode) {
        self.bytecode_mode = mode;
    }

    /// Select native execution of served instance methods (default on:
    /// libraries with Rust providers run natively, everything else stays
    /// Pine through fail-closed Decline). Symbol subsets refuse, mirroring
    /// the whole-library rule at the type level. Pass explicit
    /// `enabled: false` for Pine-oracle runs.
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
            ("closes_at", dataset.closes_at.len()),
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
            slot.dataset = Arc::new(dataset);
        } else {
            self.ltf_injections.push(InjectedDataset {
                symbol: symbol.to_owned(),
                timeframe: timeframe.to_owned(),
                dataset: Arc::new(dataset),
            });
        }
        Ok(())
    }

    /// Register one per-timeframe dataset for `(symbol, timeframe)`:
    /// later `request.security` calls with that pair are served from the
    /// confirmed prefix (buckets closed at or before the current chart
    /// instant) instead of refusing. Unregistered pairs keep the live-fetch
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
            slot.dataset = Arc::new(dataset);
        } else {
            self.security_injections.push(InjectedDataset {
                symbol: symbol.to_owned(),
                timeframe: timeframe.to_owned(),
                dataset: Arc::new(dataset),
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

    /// Attempt one run through the compiled loop: compile every linked
    /// module, link in dependency order, and run. The compile/link
    /// pipeline is memoized by content hash, so repeated runs of one
    /// program pay it once; per-run inputs (bars, `InputSet`, config,
    /// ticks) still flow fresh every call. `None` means the linker
    /// refused (unlowered constructs remain) — the caller answers on the
    /// tree where the oracle exists and refuses typed elsewhere, so fast
    /// and slow state never mix in one run. `Some` carries the loop's
    /// outcome, success or refusal, verbatim.
    #[cfg(feature = "bytecode")]
    /// Compile every linked module with shared registries: the one
    /// compile step the loop and the refusal diagnostic share, so the
    /// diagnostic names the same program the loop refused.
    #[cfg(feature = "bytecode")]
    fn compile_modules(modules: &[LinkedModule]) -> super::bytecode::CompiledProgram {
        let sources = modules
            .iter()
            .map(|module| (module.key.as_str(), &module.program, &module.aliases))
            .collect::<Vec<_>>();
        let enums = super::bytecode_compile::EnumRegistry::build(&sources);
        let decls = super::bytecode_compile::DeclRegistry::build(&sources);
        let exports = super::bytecode_compile::ExportRegistry::build(&sources);
        let mut program = super::bytecode::CompiledProgram::new();
        for module in modules {
            program.insert(super::bytecode_compile::compile_module(
                &module.key,
                &module.program,
                &module.aliases,
                &enums,
                &decls,
                &exports,
            ));
        }
        program
    }

    #[cfg(feature = "bytecode")]
    fn try_bytecode(
        &self,
        modules: &[LinkedModule],
        bars: &[Bar],
        tick_times: Option<&[i64]>,
        inputs: InputSet,
    ) -> Option<Result<RunOutcome, KernelError>> {
        // Fail-closed first: the loop validates the run configuration
        // exactly like the tree-walk's driver, so an unparseable chart
        // scope refuses before the first bar on both engines.
        if let Err(error) = self.runtime.config().validate() {
            return Some(Err(error));
        }
        let key = super::bytecode_exec::link_cache_key(modules);
        let linked = super::bytecode_exec::cached_link(key, || {
            let program = Self::compile_modules(modules);
            let order: Vec<&str> = modules.iter().map(|module| module.key.as_str()).collect();
            super::bytecode_exec::link(&program, &order)
                .ok()
                .map(std::sync::Arc::new)
        });
        let linked = linked?;
        let config = self.runtime.config();
        let vm_config = super::bytecode_exec::VmConfig::from_runtime(config);
        let ticks = tick_times.filter(|stamps| !stamps.is_empty());
        let requests =
            super::request::RequestState::new(&self.security_injections, &self.ltf_injections);
        Some(
            super::bytecode_exec::run_linked_with_requests(
                &linked, bars, inputs, &vm_config, ticks, &requests,
            )
            .0,
        )
    }










    #[cfg(any(test, feature = "oracle"))]
    fn run_linked_inner(
        &mut self,
        modules: &[LinkedModule],
        bars: &[Bar],
        tick_times: Vec<i64>,
        inputs: InputSet,
    ) -> Result<RunOutcome, KernelError> {
        self.state = EngineState::fresh();
        self.state
            .chart_symbol
            .clone_from(&self.runtime.config().chart_symbol);
        self.state
            .chart_period
            .clone_from(&self.runtime.config().chart_period);
        self.state.tick_times = tick_times;
        self.state.native.clone_from(&self.native);
        // The context provider's resident tape: clone the chart bars only
        // when the run links the library, so non-context runs pay nothing.
        if modules
            .iter()
            .any(|module| module.key == "showmethegrail/libApex")
        {
            self.state.run_bars = bars.to_vec();
        }
        // The input registry seeds from the run's inputs so caller overrides
        // are visible to `input.*` declarations from the first bar.
        for module in modules {
            for (alias, target) in &module.aliases {
                self.state
                    .module_aliases
                    .insert((module.key.clone(), alias.clone()), target.clone());
            }
        }
        // Unconditional top-level declarations register once, before the
        // first bar, so per-bar runs skip them; a malformed declaration
        // aborts the run here with the same reason bar zero would carry.
        // Fail-closed first: the compiled selection below never runs a
        // program the tree-walk would refuse to link.
        self.state.decl_skip = self.state.predeclare(modules)?;
        // Compiled selection: linked programs run the loop; anything else
        // answers on the tree below, so both selections report
        // byte-identical outcomes.
        #[cfg(feature = "bytecode")]
        if self.bytecode_mode == BytecodeMode::Compiled {
            if let Some(outcome) = self.try_bytecode(
                modules,
                bars,
                (!self.state.tick_times.is_empty()).then_some(self.state.tick_times.as_slice()),
                inputs.clone(),
            ) {
                return outcome;
            }
        }
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
        let mut builtins =
            RunBuiltins::with_requests(&self.security_injections, &self.ltf_injections);
        let PineEngine { runtime, state, .. } = &mut *self;
        let evaluated = runtime.run_over(bars, inputs, |ctx| {
            state.exec_bar(modules, ctx, &legs, &mut builtins);
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
            draw_events: builtins.drawing_mut().take_events(),
            strategy_calls: builtins.strategy_mut().take_calls(),
        })
    }

    /// No-oracle build shape with the compiled loop: without the tree
    /// interpreter there is exactly one engine, so every run links and
    /// runs the loop; a link refusal carries the linker's diagnostic as
    /// a typed error. The selection is always the compiled loop here
    /// (the tree-walk variant does not exist in this build), asserted
    /// rather than branched.
    #[cfg(all(not(any(test, feature = "oracle")), feature = "bytecode"))]
    // `tick_times` stays by value for signature parity with the oracle
    // body (the shared callers forward an owned vector); only borrowed here.
    #[allow(clippy::needless_pass_by_value)]
    fn run_linked_inner(
        &mut self,
        modules: &[LinkedModule],
        bars: &[Bar],
        tick_times: Vec<i64>,
        inputs: InputSet,
    ) -> Result<RunOutcome, KernelError> {
        if let Some(outcome) = self.try_bytecode(modules, bars, Some(&tick_times), inputs) {
            return outcome;
        }
            // The link refused: re-run it uncached to name the construct.
            // `try_bytecode` refused first, so the re-link below refuses
            // too — the fallback diagnostic below is unreachable unless
            // linking stops being deterministic, and it refuses rather
            // than guessing.
            let program = Self::compile_modules(modules);
            let order: Vec<&str> =
                modules.iter().map(|module| module.key.as_str()).collect();
            let Err(refusal) = super::bytecode_exec::link(&program, &order)
            else {
                return Err(refused(
                    "re-link succeeded after a refused try_bytecode; refusing fail-closed",
                ));
            };
            let module = refusal.module_name().to_owned();
            return Err(KernelError::Uncompilable {
                module,
                construct: refusal.to_string(),
            });
    }

    /// No-oracle build shape without the compiled loop: neither engine
    /// exists here, so every run reports the retired tree-walk selection.
    #[cfg(all(not(any(test, feature = "oracle")), not(feature = "bytecode")))]
    fn run_linked_inner(
        &mut self,
        _modules: &[LinkedModule],
        _bars: &[Bar],
        _tick_times: Vec<i64>,
        _inputs: InputSet,
    ) -> Result<RunOutcome, KernelError> {
        Err(KernelError::EngineRetired {
            mode: "tree-walk".to_owned(),
        })
    }

    /// Names bound by `export` in the last run, for import-graph consumers.
    #[must_use]
    #[cfg(any(test, feature = "oracle"))]
    pub fn exported_names(&self) -> Vec<String> {
        self.state.exported.iter().cloned().collect()
    }

    /// No-oracle build shape: no runs execute, so no names are bound.
    #[must_use]
    #[cfg(not(any(test, feature = "oracle")))]
    pub fn exported_names(&self) -> Vec<String> {
        Vec::new()
    }


    /// Commit-vs-fallback telemetry for the leased `fetch` seam over the
    /// last run (resets with each run). For the acceptance record and
    /// benches: observed, never asserted.
    #[must_use]
    #[cfg(any(test, feature = "oracle"))]
    pub fn fetch_counters(&self) -> FetchCounters {
        self.state.fetch_counters
    }

    /// No-oracle build shape: no runs execute, so telemetry stays default.
    #[must_use]
    #[cfg(not(any(test, feature = "oracle")))]
    pub fn fetch_counters(&self) -> FetchCounters {
        FetchCounters::default()
    }
}

