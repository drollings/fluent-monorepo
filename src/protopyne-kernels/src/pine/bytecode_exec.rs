//! Bytecode execution loop (scalar): runs fully-lowered programs with
//! byte-identity against the tree-walk.
//!
//! Design notes (all verified against `oracle/`):
//!
//! * **One execution path.** A link refuses any program containing a
//!   `Fallback`; a linked program runs whole on the loop. Only an oracle
//!   build falls back to the tree-walk on a refused link — production
//!   refuses with a typed load-time error instead of switching engines.
//! * **Single global namespace.** Like the tree-walk's single
//!   `self.variables` map, plain globals share one column space across
//!   modules; `var`/`varip` declarations stay module-scoped (link-time
//!   interning), matching the tree-walk's per-module `VarStore` scoping.
//! * **No per-bar hashing on the hot path.** Variables, series, and
//!   history address dense column indices resolved once at link time.
//!   Names survive only for snapshots, refusal text, and call-site keys.
//! * **Reused, never reimplemented:** `binary`/`unary` (three-valued
//!   `na` logic included), `int_cast`/`float_cast`/`color_cast`,
//!   `TaBridge::call` (identical site strings), `builtins_core::call`,
//!   `InputSet` (single first-call-wins store), `PlotCapture`,
//!   `MAX_CALL_DEPTH`, `Legs`, and the `TooManyBars` / tick-length /
//!   unknown-call refusal shapes.
//! * **Call frames discard, with one journaled exception.** A function-local
//!   write never reaches a module column, so the frame itself needs no
//!   teardown; the one exception is a nested call that writes a *caller's*
//!   binding, which journals the entry value and reverts it on return exactly
//!   like the tree-walk. Collection write-backs then persist to caller homes,
//!   also like the tree-walk.

use super::bind::ArgBinder;
use super::builtins_state::RunBuiltins;
use super::bytecode::{
    BinOp, BuiltinNamespace, BuiltinVarKind, CallTarget, CallTypeArg, CompiledBlock,
    CompiledProgram, ConstId, ConstValue, FillLeg, Instruction, LegKind, SlotId, SymbolId,
    SymbolTable, UnOp,
};
use super::eval::{
    leg_or_nan, switch_eq, whole_number, BarColorCapture, FillCapture, HlineCapture, Legs,
    PlotCandleCapture, PlotCapture, PlotShapeCapture, MAX_CALL_DEPTH, MAX_LOOP_ITERATIONS,
};
use super::request::RequestState;
use super::syntax::{BinaryOp, UnaryOp};
use crate::context::RunCtx;
use crate::indicator::{fin_or_none, nan_hole, BarTick};
use crate::value::{PineValue, UserInstance};
use crate::{ColorHandle, EnumHandle, InputSet, KernelError};
use protopyne_types::Bar;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Run configuration: mirrors the tree-walk run contract (warmup bars
/// advance unseen, over-capacity refuses instead of truncating).
#[derive(Debug, Clone)]
pub struct VmConfig {
    /// Chart symbol backing `syminfo.tickerid`. Retained as the one
    /// chart-symbol source for a run: the loop carries it alongside the
    /// period so a chart-scope read can never disagree with the tree-walk's
    /// `EngineState.chart_symbol`.
    pub symbol: String,
    /// Chart period backing `timeframe.period` and the `time()` session
    /// check.
    pub period: String,
    /// Warmup bars skipped before the first evaluated bar.
    pub warmup: u64,
    /// Maximum bars evaluated in one run.
    pub max_bars: u64,
}

impl VmConfig {
    /// Defaults matching the headless proofs (SPY scope, zero warmup).
    #[must_use]
    pub fn headless() -> Self {
        Self {
            symbol: "SPY".to_owned(),
            period: "1".to_owned(),
            warmup: 0,
            max_bars: 10_000,
        }
    }

    /// Run configuration from the engine's runtime config: the one
    /// adapter whole-run and mixed construction share, so chart scope,
    /// warmup, and capacity can never drift between the two selections.
    #[must_use]
    pub fn from_runtime(config: &crate::runtime::PineRuntimeConfig) -> Self {
        Self {
            symbol: config.chart_symbol.clone(),
            period: config.chart_period.clone(),
            warmup: config.warmup_bars,
            max_bars: config.max_bars,
        }
    }
}

/// Link-time refusal: the program needs the tree-walk (unlowered
/// constructs, unresolvable targets). Never a semantic refusal — those
/// surface identically at run time from either engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkError {
    /// A module still carries `Fallback` markers: run it tree-walk.
    Fallback {
        /// Owning module key.
        module: String,
    },
    /// A cross-module call names an unknown module or function.
    UnresolvedCall {
        /// Calling module.
        from: String,
        /// Missing target.
        target: String,
    },
    /// A jump, constant, or slot index points outside its table.
    BadIndex {
        /// Owning module.
        module: String,
        /// What pointed outside.
        what: &'static str,
    },
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fallback { module } => {
                write!(
                    f,
                    "module {module} needs the tree-walk (unlowered constructs remain)"
                )
            }
            Self::UnresolvedCall { from, target } => {
                write!(
                    f,
                    "call from {from} to unlinked {target} runs on the tree-walk"
                )
            }
            Self::BadIndex { module, what } => write!(
                f,
                "module {module} has an out-of-range {what}: runs on the tree-walk"
            ),
        }
    }
}

impl std::error::Error for LinkError {}

/// Owning module of a link refusal, for load-time diagnostics that name
/// the refusing module without rewording the linker's own text. Only the
/// oracle-free production mapping calls it; every other shape answers
/// through the tree.
impl LinkError {
    #[cfg(not(any(test, feature = "oracle")))]
    pub(crate) fn module_name(&self) -> &str {
        match self {
            Self::Fallback { module } | Self::BadIndex { module, .. } => module,
            Self::UnresolvedCall { from, .. } => from,
        }
    }
}

/// Tiny link-failure constructors: every validation arm names the same
/// variants through these, so the wording (in `Display` above) and the
/// construction sites cannot drift apart.
impl LinkError {
    /// A module still carries `Fallback` markers.
    fn fallback(module: &str) -> Self {
        Self::Fallback {
            module: module.to_owned(),
        }
    }

    /// A cross-module call names an unknown module or function.
    fn unresolved_call(from: &str, target_key: &str, callee: &str) -> Self {
        Self::UnresolvedCall {
            from: from.to_owned(),
            target: format!("{target_key}/{callee}"),
        }
    }

    /// A jump, constant, or slot index points outside its table.
    fn bad_index(module: &str, what: &'static str) -> Self {
        Self::BadIndex {
            module: module.to_owned(),
            what,
        }
    }
}

/// T1 [confidence] tier-up gate: a column rides the float mirror only
/// after this many consecutive float-or-absence observations. The count is
/// a calibration constant, never a belief: the scalar rows stay correct
/// on every bar before, during, and after tier-up, and the calibration
/// suite pins this value (changing it re-runs that suite).
pub const TIER_STABLE_BARS: u32 = 4;

/// T3 [correctness] split-brain guard: a run enters the loop only when
/// the compiled instruction fraction across linked modules reaches this
/// coverage. At 1.0 any fallback anywhere keeps the whole run on the
/// tree-walk — a static over-approximation of the dynamic guard (it
/// refuses strictly more often, never less), so fast and slow state
/// never coexist in one run. Pinned by the calibration suite.
pub const SPLIT_BRAIN_COVERAGE: f64 = 1.0;

/// Contiguous numeric column with hole discipline: finite values ride as
/// themselves, everything else (absence, non-numeric cells) rides as the
/// shared NaN hole. Reads map the hole back to absence through the shared
/// `fin_or_none`, so a hole never compares equal to zero and out-of-range
/// reads stay absence instead of wrapping.
#[derive(Debug, Clone, Default)]
pub struct FloatSeries {
    values: Vec<f64>,
}

impl FloatSeries {
    /// Empty column.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one optional number, mapping absence to the hole.
    pub fn push_number(&mut self, value: Option<f64>) {
        self.values.push(nan_hole(value));
    }

    /// Append one engine cell: floats ride as themselves, anything else
    /// (absence included) rides as the hole. Integer and other typed cells
    /// deliberately become holes here — the scalar column stays the source
    /// of truth for those, and the purity flag steers reads back to it.
    pub fn push_value(&mut self, value: &Option<PineValue>) {
        let number = match value {
            Some(PineValue::Float(v)) => Some(*v),
            _ => None,
        };
        self.push_number(number);
    }

    /// Read one slot: the hole (or an out-of-range index) reads as absence.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<f64> {
        self.values.get(index).copied().and_then(fin_or_none)
    }

    /// Number of committed slots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether no slots are committed yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// One linked module: blocks with validated indices plus the interned
/// global namespace the M4 loop addresses directly.
#[derive(Clone)]
struct VmModule {
    key: String,
    blocks: Vec<CompiledBlock>,
    entry: usize,
    /// Function name → entry block (mirrors the compiled map).
    functions: HashMap<String, usize>,
    /// Overloaded names: calls to these resolve at run time.
    overloaded: HashSet<String>,
    /// Method bodies by (receiver type, method), mirrored from the
    /// compiled map for the link-time dispatch index.
    methods: HashMap<(String, String), super::bytecode::MethodEntry>,
    /// Global name → interned column (legs keep `LEG_*` ids).
    globals: HashMap<String, usize>,
    /// Interned columns with intrabar persistence (reset per bar).
    varip: HashSet<usize>,
    /// Interned persistent columns (`var`/`varip` declarations): assigning
    /// `na` clears the cell instead of storing an explicit absence, so
    /// snapshots omit cleared cells while the `recorded` predicate stays true.
    persistent: HashSet<usize>,
    /// Global columns this module ever writes (post-remap), sorted: the
    /// run's commit fold and snapshot view iterate this list instead of the
    /// whole interned namespace.
    written_columns: Vec<usize>,
    /// Ordered parameter names per block is read from
    /// `CompiledBlock.params` directly; no link-time copy is kept.
    /// Shared constants (handle-cloned once at link, never per run).
    consts: std::sync::Arc<super::bytecode::ConstPool>,
    /// Resolved constant values, one per pool entry: materialized once at
    /// link (including enum member resolution through the module symbols)
    /// so the per-bar `Const` arm is a single indexed clone instead of a
    /// pool match plus symbol resolves.
    const_vals: Vec<Option<PineValue>>,
    /// Shared symbol table (handle-cloned once at link, never per run).
    symbols: std::sync::Arc<SymbolTable>,
}

/// A linked program: modules in execution order with resolved namespaces.
#[derive(Clone)]
pub struct VmProgram {
    modules: Vec<VmModule>,
    order: Vec<usize>,
    /// Module key to position, so named calls resolve without scanning.
    module_index: HashMap<String, usize>,
    /// Compiled instruction fraction across linked modules (1.0 means
    /// fallback-free). The T3 split-brain guard below refuses anything
    /// under [`SPLIT_BRAIN_COVERAGE`].
    coverage: f64,
    /// Method dispatch index by (receiver type, method): the owning module
    /// plus its resolved entry (body block, parameter layout, receiver
    /// alias). Every module's table composes here (later modules overwrite,
    /// mirroring the tree-walk's registration order), so the loop type-tests
    /// receivers against one static target set and dispatch is one lookup.
    method_index: HashMap<(String, String), ResolvedMethod>,
}

impl VmProgram {
    /// Every global column any module writes, sorted: the run's commit fold
    /// and snapshot view walk this instead of the whole namespace.
    #[must_use]
    fn written_columns(&self) -> Vec<usize> {
        let mut columns: Vec<usize> = self
            .modules
            .iter()
            .flat_map(|module| module.written_columns.iter().copied())
            .collect();
        columns.sort_unstable();
        columns.dedup();
        columns
    }

    /// Compiled instruction fraction at link time (1.0 is fully compiled).
    #[must_use]
    pub fn coverage(&self) -> f64 {
        self.coverage
    }
}

/// Link-cache capacity: distinct linked programs held process-wide.
/// Small on purpose — one entry per distinct module set; fixtures,
/// benches, and suites reuse a handful, and the LRU sheds the rest.
const LINK_CACHE_CAPACITY: usize = 64;

/// Memoized compile/link front door: content-hash the linked module set,
/// then compile and link once per distinct program. The cached unit is
/// the link outcome itself — linked program or recorded refusal — because
/// both are deterministic pure functions of the module keys, alias
/// tables, and ASTs: `VmConfig`, bars, inputs, injections, and tick
/// stamps only enter at run time, so one cached link serves every run
/// configuration byte-identically. Refusals are cached too (a program
/// that cannot link never will), which is where fallback-only fixtures
/// win: they skip the recompile as well as the relink. Bounded and
/// thread-safe through the shared [`LoadCache`](common_core::cache::LoadCache)
/// (write-through `get`/`insert`; the load closure below never runs).
/// Concurrent misses for one key compile independently and the last
/// insert wins — both inserts hold the identical program, so no run can
/// observe a mix.
/// Bounded memo table for [`LINK_CACHE`]: content key to the link
/// outcome (linked program or recorded refusal).
type LinkCache = common_core::cache::LoadCache<
    [u8; 32],
    Option<std::sync::Arc<VmProgram>>,
    std::convert::Infallible,
>;

static LINK_CACHE: std::sync::OnceLock<LinkCache> = std::sync::OnceLock::new();

/// Length-prefix one segment so concatenated fields cannot collide
/// (`"ab" + "c"` hashes differently from `"a" + "bc"`).
fn push_framed(buf: &mut Vec<u8>, segment: &[u8]) {
    buf.extend_from_slice(&(segment.len() as u64).to_le_bytes());
    buf.extend_from_slice(segment);
}

/// Content key identifying one linked module set: module keys in link
/// order, alias tables sorted, and each AST's parse-sealed content digest.
/// The digest is sealed once at parse time, so keying costs one pass over
/// keys and aliases — never a re-render of the tree. The digest is full
/// width (256-bit), so two distinct programs never share an entry: the
/// control corpus pins 200 distinct programs to 200 distinct keys.
pub(crate) fn link_cache_key(modules: &[super::eval::LinkedModule]) -> [u8; 32] {
    let mut buf = Vec::new();
    for module in modules {
        push_framed(&mut buf, module.key.as_bytes());
        let mut aliases: Vec<(&String, &String)> = module.aliases.iter().collect();
        aliases.sort();
        for (name, target) in aliases {
            push_framed(&mut buf, name.as_bytes());
            push_framed(&mut buf, target.as_bytes());
        }
        push_framed(&mut buf, &module.program.content_hash());
    }
    common_core::hash::blake3_hash(&buf)
}

/// Look up a linked program by content key, building it on a miss through
/// `load` (which runs outside the cache lock) and remembering the
/// outcome — link success or recorded refusal — for later calls.
/// Returns the shared program: every caller runs the same allocation.
pub(crate) fn cached_link(
    key: [u8; 32],
    load: impl FnOnce() -> Option<std::sync::Arc<VmProgram>>,
) -> Option<std::sync::Arc<VmProgram>> {
    let cache = LINK_CACHE.get_or_init(|| {
        LinkCache::new(LINK_CACHE_CAPACITY, |_: &[u8; 32]| {
            Ok::<_, std::convert::Infallible>(None)
        })
        .expect("nonzero link-cache capacity")
    });
    if let Some(hit) = cache.get(&key) {
        return hit;
    }
    let linked = load();
    cache.insert(key, linked.clone());
    linked
}

/// Link a compiled program for execution: verify every block is
/// fallback-free with in-range targets, intern the shared global
/// namespace, and resolve cross-module calls. Any gap fails the whole
/// link — there is no partial VM run, so the caller runs the tree-walk
/// (oracle builds) or refuses typed (production) instead.
pub fn link(program: &CompiledProgram, order: &[&str]) -> Result<VmProgram, LinkError> {
    // Every linked module must exist. Coverage is measured over every
    // linked instruction; the T3 split-brain guard below refuses the
    // whole run unless it reaches `SPLIT_BRAIN_COVERAGE`, so compiled and
    // tree-walked state never mix within a run.
    //
    let mut total: usize = 0;
    let mut fallback: usize = 0;
    let mut first_fallback: Option<String> = None;
    for key in order {
        let module = program.get(key).ok_or_else(|| LinkError::UnresolvedCall {
            from: "<root>".to_owned(),
            target: (*key).to_owned(),
        })?;
        for block in &module.blocks {
            for instr in &block.instrs {
                total += 1;
                if matches!(instr, Instruction::Fallback { .. }) {
                    fallback += 1;
                    if first_fallback.is_none() {
                        first_fallback = Some((*key).to_owned());
                    }
                }
            }
        }
    }
    let coverage = if total == 0 {
        1.0
    } else {
        (total - fallback) as f64 / total as f64
    };
    if coverage < SPLIT_BRAIN_COVERAGE {
        return Err(LinkError::fallback(
            &first_fallback.unwrap_or_else(|| "<root>".to_owned()),
        ));
    }
    // Intern the shared global namespace: plain names share one column
    // across modules (the tree-walk's single `variables` map), while
    // `var`/`varip` declarations stay module-scoped (the tree-walk's
    // per-module `VarStore` cells). A name is module-scoped when any
    // `GStoreInit` in that module targets it.
    let mut shared: HashMap<String, usize> = HashMap::new();
    let mut qualified: HashMap<(usize, String), usize> = HashMap::new();
    let mut next_column = super::bytecode::LEG_COUNT as usize;
    let mut modules = Vec::with_capacity(order.len());
    for (position, key) in order.iter().enumerate() {
        let module = program.get(key).expect("linked module present");
        // Reverse map: per-module slot id → name.
        let mut by_slot: HashMap<SlotId, &String> = HashMap::new();
        for (name, slot) in &module.globals {
            by_slot.insert(*slot, name);
        }
        // Module-scoped var names: any name targeted by `GStoreInit`.
        let mut scoped: HashSet<String> = HashSet::new();
        // Global slots this module writes at all (remapped to interning
        // columns below): the run's written-list source.
        let mut written_slots: Vec<SlotId> = Vec::new();
        for block in &module.blocks {
            for instr in &block.instrs {
                match instr {
                    Instruction::GStoreInit { slot, .. } => {
                        written_slots.push(*slot);
                        if let Some(name) = by_slot.get(slot) {
                            scoped.insert((*name).clone());
                        }
                    }
                    Instruction::GStore { slot, .. }
                    | Instruction::GStoreLoop { slot, .. }
                    | Instruction::GReassign { slot, .. } => {
                        written_slots.push(*slot);
                    }
                    _ => {}
                }
            }
        }
        // Home-written columns: a journaled caller-home write and a
        // call-argument write-back home can land on a shared column that
        // no `GStore` names. A home whose name resolves to one of this
        // module's globals joins the written list — unconditionally for
        // journal targets (globals resolve first), and for frame-first
        // call homes only when no frame slot of the executing block can
        // shadow the name. Names with no global reference elsewhere fall
        // out of the remap below, so the rule only ever adds columns the
        // run can actually address.
        for block in &module.blocks {
            for instr in &block.instrs {
                match instr {
                    Instruction::JournalWrite { home, .. } => {
                        if let Some(name) = module.symbols.resolve(*home) {
                            if let Some(slot) = module.globals.get(name) {
                                written_slots.push(*slot);
                            }
                        }
                    }
                    Instruction::Call { homes, .. } => {
                        for home in homes.iter().flatten() {
                            if let Some(name) = module.symbols.resolve(*home) {
                                let shadowed = block.frame_names.contains_key(name)
                                    || block.params.iter().any(|param| param == name);
                                if !shadowed {
                                    if let Some(slot) = module.globals.get(name) {
                                        written_slots.push(*slot);
                                    }
                                }
                            }
                        }
                    }
                    Instruction::InstanceCall { homes, home, .. } => {
                        for home in homes.iter().flatten() {
                            if let Some(name) = module.symbols.resolve(*home) {
                                let shadowed = block.frame_names.contains_key(name)
                                    || block.params.iter().any(|param| param == name);
                                if !shadowed {
                                    if let Some(slot) = module.globals.get(name) {
                                        written_slots.push(*slot);
                                    }
                                }
                            }
                        }
                        if let Some(name) = match home {
                            Some(
                                super::bytecode::CallHome::Var { name, .. }
                                | super::bytecode::CallHome::VarPath { name, .. },
                            ) => module.symbols.resolve(*name),
                            _ => None,
                        } {
                            let shadowed = block.frame_names.contains_key(name)
                                || block.params.iter().any(|param| param == name);
                            if !shadowed {
                                if let Some(slot) = module.globals.get(name) {
                                    written_slots.push(*slot);
                                }
                            }
                        }
                    }
                    Instruction::FieldPut { home, .. } => {
                        if let Some(name) = match home {
                            super::bytecode::CallHome::Var { name, .. }
                            | super::bytecode::CallHome::VarPath { name, .. } => {
                                module.symbols.resolve(*name)
                            }
                            _ => None,
                        } {
                            let shadowed = block.frame_names.contains_key(name)
                                || block.params.iter().any(|param| param == name);
                            if !shadowed {
                                if let Some(slot) = module.globals.get(name) {
                                    written_slots.push(*slot);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        // Remap every global reference to its interned column.
        let mut remap: HashMap<SlotId, usize> = HashMap::new();
        // Collect every referenced global slot first (deterministic order).
        let mut seen: Vec<SlotId> = Vec::new();
        let mut seen_set: HashSet<SlotId> = HashSet::new();
        let note = |slot: SlotId, seen: &mut Vec<SlotId>, seen_set: &mut HashSet<SlotId>| {
            if slot.0 >= super::bytecode::LEG_COUNT && seen_set.insert(slot) {
                seen.push(slot);
            }
        };
        for block in &module.blocks {
            for instr in &block.instrs {
                match instr {
                    Instruction::GLoad { slot, .. }
                    | Instruction::GStore { slot, .. }
                    | Instruction::GReassign { slot, .. }
                    | Instruction::GStoreLoop { slot, .. }
                    | Instruction::GStoreInit { slot, .. } => note(*slot, &mut seen, &mut seen_set),
                    Instruction::History { src, .. } => note(*src, &mut seen, &mut seen_set),
                    Instruction::DottedGet {
                        flat: Some(flat), ..
                    }
                    | Instruction::AliasFlatRead { flat, .. }
                    | Instruction::EnumRead { flat, .. } => note(*flat, &mut seen, &mut seen_set),
                    _ => {}
                }
            }
        }
        seen.sort_by_key(|slot| slot.0);
        for slot in seen {
            let name = by_slot
                .get(&slot)
                .map_or_else(|| format!("<anonymous#{}>", slot.0), |name| (*name).clone());
            let column = if scoped.contains(&name) {
                *qualified.entry((position, name)).or_insert_with(|| {
                    let id = next_column;
                    next_column += 1;
                    id
                })
            } else {
                *shared.entry(name).or_insert_with(|| {
                    let id = next_column;
                    next_column += 1;
                    id
                })
            };
            remap.insert(slot, column);
        }
        let mut names: Vec<Option<String>> = vec![None; next_column];
        let mut varip: HashSet<usize> = HashSet::new();
        let mut persistent: HashSet<usize> = HashSet::new();
        for (name, slot) in &module.globals {
            if let Some(column) = remap.get(slot) {
                names[*column] = Some(name.clone());
                if module.varip.contains(slot) {
                    varip.insert(*column);
                }
                if scoped.contains(name) {
                    persistent.insert(*column);
                }
            }
        }
        let mut blocks = module.blocks.clone();
        for block in &mut blocks {
            for instr in &mut block.instrs {
                match instr {
                    Instruction::GLoad { slot, .. }
                    | Instruction::GStore { slot, .. }
                    | Instruction::GReassign { slot, .. }
                    | Instruction::GStoreLoop { slot, .. }
                    | Instruction::GStoreInit { slot, .. } => {
                        *slot = SlotId(remap.get(slot).copied().unwrap_or(slot.0 as usize) as u32);
                    }
                    Instruction::History { src, .. } => {
                        if src.0 >= super::bytecode::LEG_COUNT {
                            *src = SlotId(remap.get(src).copied().unwrap_or(src.0 as usize) as u32);
                        }
                    }
                    Instruction::DottedGet {
                        flat: Some(flat), ..
                    }
                    | Instruction::AliasFlatRead { flat, .. }
                    | Instruction::EnumRead { flat, .. } => {
                        *flat = SlotId(remap.get(flat).copied().unwrap_or(flat.0 as usize) as u32);
                    }
                    _ => {}
                }
            }
        }
        modules.push(VmModule {
            key: (*key).to_owned(),
            blocks,
            entry: module.entry,
            functions: module.functions.clone(),
            overloaded: module.overloaded.clone(),
            methods: module.methods.clone(),
            symbols: module.symbols.clone(),
            const_vals: {
                // Resolve once at link: the per-bar arm below clones the
                // cached value instead of matching the pool plus resolving
                // enum members through symbols every bar.
                let mut vals = Vec::with_capacity(module.consts.len());
                for index in 0..module.consts.len() {
                    let id = ConstId(index as u32);
                    vals.push(match module.consts.get(id) {
                        Some(ConstValue::Float(value)) => Some(PineValue::Float(*value)),
                        Some(ConstValue::Int(value)) => Some(PineValue::Int(*value)),
                        Some(ConstValue::Bool(value)) => Some(PineValue::Bool(*value)),
                        Some(ConstValue::Str(text)) => Some(PineValue::Str(text.clone())),
                        Some(ConstValue::Color(packed)) => {
                            Some(PineValue::ColorHandle(ColorHandle::new(*packed)))
                        }
                        Some(ConstValue::Enum { namespace, member }) => {
                            let namespace =
                                module.symbols.resolve(*namespace).unwrap_or("?");
                            let member = module.symbols.resolve(*member).unwrap_or("?");
                            Some(PineValue::EnumHandle(EnumHandle::new(namespace, member)))
                        }
                        // The `na` literal loads absence, exactly like the
                        // tree-walk's unwritten temp (missing ids fail
                        // closed at link validation; `None` here is
                        // defense in depth).
                        Some(ConstValue::Na) | None => None,
                    });
                }
                vals
            },
            globals: module
                .globals
                .iter()
                .filter_map(|(name, slot)| remap.get(slot).map(|column| (name.clone(), *column)))
                .collect(),
            varip,
            persistent,
            written_columns: {
                let mut columns: Vec<usize> = written_slots
                    .iter()
                    .filter_map(|slot| remap.get(slot).copied())
                    .collect();
                columns.sort_unstable();
                columns.dedup();
                columns
            },
            consts: module.consts.clone(),
        });
    }
    // Validate references: jump/call/const targets land inside their
    // tables, and every named call resolves to a linked, fallback-free
    // module (fallback-freedom was verified above, so resolution here is
    // total — the check stays as defense in depth). The index is built
    // before validation so named calls resolve without scanning.
    let mut module_index = HashMap::new();
    for (position, module) in modules.iter().enumerate() {
        module_index.insert(module.key.clone(), position);
    }
    for (position, module) in modules.iter().enumerate() {
        for block in &module.blocks {
            for instr in &block.instrs {
                match instr {
                    Instruction::Jump { target }
                    | Instruction::JumpIfFalse { target, .. }
                    | Instruction::JumpIfNa { target, .. } => {
                        if *target >= block.instrs.len() {
                            return Err(LinkError::bad_index(&module.key, "jump target"));
                        }
                    }
                    Instruction::Const { value, .. } => {
                        if module.consts.get(*value).is_none() {
                            return Err(LinkError::bad_index(&module.key, "const id"));
                        }
                    }
                    Instruction::Plot {
                        title,
                        site,
                        display,
                        ..
                    } => {
                        if module.consts.get(*site).is_none() {
                            return Err(LinkError::bad_index(&module.key, "plot const"));
                        }
                        match title {
                            super::bytecode::PlotTitle::Const(id) => {
                                if module.consts.get(*id).is_none() {
                                    return Err(LinkError::bad_index(
                                        &module.key,
                                        "plot title const",
                                    ));
                                }
                            }
                            super::bytecode::PlotTitle::Slot(slot) => {
                                if slot.0 as usize >= block.slots as usize {
                                    return Err(LinkError::bad_index(
                                        &module.key,
                                        "plot title slot",
                                    ));
                                }
                            }
                        }
                        if let Some(slot) = display {
                            if slot.0 as usize >= block.slots as usize {
                                return Err(LinkError::bad_index(&module.key, "plot display slot"));
                            }
                        }
                    }
                    Instruction::GStore { plot_tag, .. }
                    | Instruction::GReassign { plot_tag, .. } => {
                        if let Some(tag) = plot_tag {
                            if module.consts.get(*tag).is_none() {
                                return Err(LinkError::bad_index(&module.key, "plot tag const"));
                            }
                        }
                    }
                    Instruction::PlotTag { tag, .. } => {
                        if tag.is_some_and(|tag| module.consts.get(tag).is_none()) {
                            return Err(LinkError::bad_index(&module.key, "plot tag const"));
                        }
                    }
                    Instruction::PlotCandle { title, site, .. }
                    | Instruction::BarColor { title, site, .. }
                    | Instruction::PlotShape { title, site, .. }
                    | Instruction::Hline { title, site, .. }
                    | Instruction::Fill { title, site, .. } => {
                        if title.is_some_and(|id| module.consts.get(id).is_none())
                            || module.consts.get(*site).is_none()
                        {
                            return Err(LinkError::bad_index(&module.key, "capture const"));
                        }
                    }
                    Instruction::Call { target, args, .. } => {
                        let (target_module, target_block) = match target {
                            CallTarget::Block(index) => (position, *index),
                            CallTarget::Named {
                                module: callee_module,
                                name,
                            } => {
                                let caller_symbols = &module.symbols;
                                let callee_name = caller_symbols.resolve(*name).unwrap_or("?");
                                let target_key: &str = match callee_module {
                                    Some(id) => caller_symbols.resolve(*id).unwrap_or("?"),
                                    None => module.key.as_str(),
                                };
                                let target_position = match module_index.get(target_key) {
                                    Some(position) => *position,
                                    None => {
                                        return Err(LinkError::unresolved_call(
                                            &module.key,
                                            target_key,
                                            callee_name,
                                        ));
                                    }
                                };
                                if modules[target_position].overloaded.contains(callee_name) {
                                    return Err(LinkError::unresolved_call(
                                        &module.key,
                                        target_key,
                                        callee_name,
                                    ));
                                }
                                let target_block = *modules[target_position]
                                    .functions
                                    .get(callee_name)
                                    .ok_or_else(|| {
                                        LinkError::unresolved_call(
                                            &module.key,
                                            target_key,
                                            callee_name,
                                        )
                                    })?;
                                (target_position, target_block)
                            }
                            CallTarget::Resolved { module, block } => {
                                (*module as usize, *block as usize)
                            }
                            CallTarget::Builtin { .. } => continue,
                        };
                        let target_params = &modules[target_module]
                            .blocks
                            .get(target_block)
                            .ok_or_else(|| LinkError::bad_index(&module.key, "call target block"))?
                            .params
                            .len();
                        if args.len() != *target_params {
                            return Err(LinkError::bad_index(&module.key, "call arity"));
                        }
                        let _ = target_module;
                    }
                    _ => {}
                }
            }
        }
    }
    let order: Vec<usize> = (0..modules.len()).collect();
    // Method dispatch index: every module's table composes here, later
    // modules overwriting — mirroring the tree-walk's registration order.
    // The resolved entry travels with the target, so dispatch is one lookup
    // (parameters, receiver alias, and body block all arrive together).
    let mut method_index = HashMap::new();
    for (position, module) in modules.iter().enumerate() {
        for ((type_name, method), entry) in &module.methods {
            method_index.insert(
                (type_name.clone(), method.clone()),
                ResolvedMethod {
                    module: position,
                    entry: entry.clone(),
                },
            );
        }
    }
    // Resolve every in-program named target once: the run then dispatches
    // on `(program module, block)` with no table lookup, and `Named` stays
    // only for calls that leave the program (the mixed boundary). The
    // function tables are snapshotted first so the rewrite can borrow each
    // module's blocks mutably.
    let function_tables: Vec<HashMap<String, usize>> = modules
        .iter()
        .map(|module| module.functions.clone())
        .collect();
    for module in &mut *modules {
        let module_key = module.key.clone();
        let symbols = module.symbols.clone();
        for block in &mut module.blocks {
            for instr in &mut block.instrs {
                let Instruction::Call { target, .. } = instr else {
                    continue;
                };
                let CallTarget::Named {
                    module: callee_module,
                    name,
                } = *target
                else {
                    continue;
                };
                let callee_name = symbols.resolve(name).unwrap_or("?");
                let target_key: &str = match callee_module {
                    Some(id) => symbols.resolve(id).unwrap_or("?"),
                    None => module_key.as_str(),
                };
                let Some(target_position) = module_index.get(target_key).copied() else {
                    // Unresolvable here: the link scan below refuses it
                    // with the unknown-target wording.
                    continue;
                };
                let Some(target_block) = function_tables[target_position].get(callee_name).copied()
                else {
                    continue;
                };
                *target = CallTarget::Resolved {
                    module: target_position as u32,
                    block: target_block as u32,
                };
            }
        }
    }
    Ok(VmProgram {
        modules,
        order,
        module_index,
        coverage,
        method_index,
    })
}

/// One resolved method target: the program module plus the entry that
/// carries the body block, parameter layout, and receiver alias.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedMethod {
    pub(crate) module: usize,
    pub(crate) entry: super::bytecode::MethodEntry,
}

/// One caller home a journaled write may target: a shared global
/// column (nested reads observe it, exactly like the tree-walk's
/// shared namespace) or a caller frame slot for names with no column
/// (nested readers fall back to the tree-walk there, exactly like the
/// unbound-variable rule on the tree side).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JournalHome {
    /// Shared global column.
    Global(usize),
    /// Caller frame by absolute stack index plus slot.
    CallerFrame { frame: usize, slot: usize },
}

/// One logged write: the home plus its entry value, so the return
/// teardown restores exactly what the call observed on entry. The log
/// keeps the first write per home per call, mirroring the tree-walk's
/// journal suffix discipline.
#[derive(Debug, Clone)]
struct JournalEntry {
    home: JournalHome,
    /// Entry value: nested option for globals (never-written vs
    /// written-absent), flattened once for frame slots.
    #[allow(clippy::option_option)]
    old: Option<Option<PineValue>>,
}

impl JournalEntry {
    fn same_home(&self, home: &JournalHome) -> bool {
        self.home == *home
    }
}

/// The loop's own address for one directly writable home slot. The
/// semantic home model ([`super::home::Home`]) wraps these, so nested
/// paths and map entries share their take/put table with the tree-walk.
#[derive(Debug, Clone, PartialEq, Eq)]
enum VmSlot {
    /// Live receiver slot by depth.
    Receiver(usize),
    /// Caller (or own) frame slot.
    Frame { frame: usize, slot: usize },
    /// Per-instance `var`/`varip` cell; the name rides along for snapshot
    /// text and native-serve bookkeeping.
    Cell {
        key: VarInstKey,
        is_varip: bool,
        name: String,
    },
    /// Shared global column.
    Column(usize),
}

impl super::home::HomeSlot for VmSlot {
    fn lease_key(&self) -> Option<crate::native_libs::LeaseKey> {
        // Native serving is tree-walk only: the loop never leases.
        None
    }
}

/// A plain value home (not an object take): where a map variable lives so a
/// provenance write can replace it in place.
#[derive(Debug, Clone, Copy)]
enum ValueSlot {
    /// Named frame slot at an absolute frame index.
    Frame { frame: usize, slot: usize },
    /// Shared global column.
    Column(usize),
}

impl super::home::HomeStore<VmSlot> for Run {
    fn put_slot(&mut self, slot: &VmSlot, instance: UserInstance) -> Result<(), KernelError> {
        match slot {
            VmSlot::Receiver(depth) => {
                if let Some(slot) = self.receiver.get_mut(*depth) {
                    slot.instance = instance;
                }
            }
            VmSlot::Frame { frame, slot } => {
                if let Some(frame) = self.frames.get_mut(*frame) {
                    if *slot < frame.slots.len() {
                        frame.slots[*slot] = Some(PineValue::Object(instance));
                    }
                }
            }
            VmSlot::Cell {
                key,
                is_varip,
                name,
            } => {
                self.seed_cell(
                    key.clone(),
                    FrameVarCell {
                        name: name.clone(),
                        value: Some(PineValue::Object(instance)),
                        is_varip: *is_varip,
                    },
                );
            }
            VmSlot::Column(column) => {
                self.global_set(*column, Some(PineValue::Object(instance)));
            }
        }
        Ok(())
    }

    fn put_this_field(&mut self, field: &str, instance: UserInstance) -> Result<(), KernelError> {
        let slot = self
            .receiver
            .last_mut()
            .ok_or_else(super::eval::this_unbound)?;
        slot.instance
            .fields
            .insert(field.to_owned(), Some(PineValue::Object(instance)));
        Ok(())
    }

    fn put_map_entry(
        &mut self,
        map_var: &str,
        key: &str,
        key_value: &PineValue,
        instance: UserInstance,
    ) -> Result<(), KernelError> {
        Run::put_map_entry(self, map_var, key, key_value, instance);
        Ok(())
    }
}

/// One live call frame: private slots only (no caller state leaks in, so the
/// frame itself discards on return; a nested call that writes a *caller's*
/// binding journals that home through the call journal instead).
/// Slot protocol: `SlotId` values copy; carried values move. Object and
/// drawing values ride by handle — cloning one bumps the shared table
/// without copying it, and the first mutation through a shared handle
/// copies (the `FieldTable` contract, composed here, never a second
/// copy-on-write scheme). No new encoding, no new table, no new map.
struct Frame {
    slots: Vec<Option<PineValue>>,
}

/// Call scratch buffers owned by one run: taken and returned around every
/// call so a refusal (which ends the run) never leaves a borrowed buffer.
#[derive(Default)]
pub(crate) struct Scratch {
    /// Evaluated argument values, in call-site order.
    pub(crate) values: Vec<Option<PineValue>>,
    /// The same values as named bridge arguments for the builtin dispatcher.
    pub(crate) core_args: Vec<super::builtins_core::CoreArg>,
}

/// One live method receiver: the instance plus the parameter name its
/// body spells it by. Reads and nested calls through the alias hit the
/// one live copy, never a stale clone — mirroring the tree-walk's
/// receiver stack discipline.
#[derive(Debug, Clone)]
struct ReceiverSlot {
    instance: UserInstance,
    alias: String,
}

/// Identity of one in-function `var`/`varip` instance: the call path
/// that reached the declaring body plus the declaration site within it.
/// Same declaration, same call path, same cell — mirroring the
/// tree-walk's `ScopeKey` without sharing its representation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct VarInstKey {
    /// Caller chain, outermost first: (module, block, call-site pc).
    path: Vec<(usize, usize, usize)>,
    /// Declaring body and declaration ordinal within it.
    home: (usize, usize, u32),
}

/// Identity of one plain frame local's per-call history: the open call
/// path plus the declaring body and name. Keyed by name (plain locals
/// have no declaration ordinal) and kept apart from `VarInstKey` so a
/// cell ordinal can never collide with a symbol id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LocalHistKey {
    /// Caller chain, outermost first: (module, block, call-site pc).
    path: Vec<(usize, usize, usize)>,
    /// Declaring body (module, block).
    body: (usize, usize),
    /// Local or parameter name.
    name: SymbolId,
}

/// One per-instance cell: the current value. Clearing (`na` assign,
/// `varip` bar reset) drops the entry but keeps the seeded mark, so a
/// later declaration is still a no-op — mirroring the tree-walk's store
/// clear under registration.
#[derive(Debug, Clone)]
struct FrameVarCell {
    /// Bound name (snapshot views).
    name: String,
    /// Current instance value; `None` is Pine `na`.
    value: Option<PineValue>,
    /// Intrabar cells reset at the bar advance.
    is_varip: bool,
}

/// Capture family tag for the unified capture index: one
/// `(family, site)` map replaces six per-family maps, and the family
/// selects the backing vector at the single dispatch in
/// [`Run::capture_row`]. Distinct from [`CallTarget`]-adjacent
/// categorization: this names the run-time backing store, not the reason
/// a capture fell back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum CaptureFamily {
    Plot,
    PlotCandle,
    BarColor,
    PlotShape,
    Hline,
    Fill,
}

/// One capture's title operand: a compile-time constant (the capture
/// families whose titles must be literals) or a `plot` title value
/// evaluated per bar.
enum CaptureTitle {
    /// Static title constant, or the family default when absent.
    Const(Option<ConstId>),
    /// Evaluated `plot` title value: a string names the series, anything
    /// else falls back to the family default at first sight.
    Value(Option<PineValue>),
}

impl CaptureFamily {
    /// Family name shared by the title/site resolvers, so every family
    /// names the same call the same way.
    fn name(self) -> &'static str {
        match self {
            Self::Plot => "plot",
            Self::PlotCandle => "plotcandle",
            Self::BarColor => "barcolor",
            Self::PlotShape => "plotshape",
            Self::Hline => "hline",
            Self::Fill => "fill",
        }
    }
}

/// Per-run VM state. Columns and snapshots mirror the tree-walk merge
/// discipline exactly: current values persist across bars, snapshots
/// record written names, history columns advance per evaluated bar.
pub(crate) struct Run {
    /// The linked program, shared: a run owns its handle so it can be held
    /// across stack frames (the engine's mixed run, a tree-initiated
    /// function call) without borrowing the frame that created it.
    program: Arc<VmProgram>,
    legs: Legs,
    /// Tick stamps per evaluated bar in tick runs (Pine-native millis);
    /// empty on closed-bar runs. Serves `timenow` exactly like the
    /// tree-walk, while the time leg keeps bar-open times for flags,
    /// `time`, and request windows.
    tick_stamps: Vec<i64>,
    /// Current per-column values (`None` = never written).
    #[allow(clippy::option_option)]
    current: Vec<Option<Option<PineValue>>>,
    /// Ever-written columns (the `recorded` predicate).
    written: Vec<bool>,
    /// Per-column history, one slot per evaluated bar.
    columns: Vec<Vec<Option<PineValue>>>,
    /// Contiguous float mirror of `columns`: one `FloatSeries` per global
    /// column, one slot per evaluated bar, NaN for every hole. Numeric
    /// history reads hit this instead of the boxed scalar rows.
    float_cols: Vec<FloatSeries>,
    /// Per-column float purity: still true only while every written cell
    /// was absence or a non-NaN float. The first integer, boolean, string,
    /// collection, or NaN-float cell retires the column to the scalar path
    /// for the rest of the run — the scalar rows stay byte-identical there.
    float_pure: Vec<bool>,
    /// Consecutive float-or-absence observations per column: the T1
    /// tier-up counter. The float mirror serves history only once this
    /// reaches [`TIER_STABLE_BARS`]; any other cell resets it to zero and
    /// retires the column (see T2 below).
    stable: Vec<u32>,
    /// Whether this run may tier columns at all. Closed-bar runs tier;
    /// tick-emulation runs stay scalar by calibration rule (unvalidated
    /// for tiering), while keeping byte-identical snapshots.
    float_allowed: bool,
    /// T1/T2 observability for the calibration suite: history reads
    /// served from the float mirror vs the scalar rows, plus T2 deopt
    /// firings (first retirement per column). Test-only accounting —
    /// never a cache, never persisted, dropped with the run.
    float_hits: u64,
    scalar_hits: u64,
    deopts: u64,
    /// Reusable call scratch: argument values and the named bridge view.
    /// A call takes the buffers, fills them, and returns them, so the hot
    /// path allocates nothing once the widest call shape has been seen.
    scratch: Scratch,
    /// Global columns any module writes, sorted at link: the commit fold and
    /// the snapshot view iterate this list, never the whole namespace.
    written_cols: Vec<usize>,
    /// Sorted per-instance cell order for the snapshot view, rebuilt only
    /// when a new cell is seeded.
    cell_order: Vec<VarInstKey>,
    /// Interned column display names (`None` for legs).
    names: Vec<Option<String>>,
    /// Interned columns with intrabar persistence (reset per bar).
    varip: HashSet<usize>,
    /// Persistent columns (`var`/`varip`): cleared cells read `na` but are
    /// omitted from snapshots, matching the tree-walk's occupied-only view.
    persistent: HashSet<usize>,
    /// Evaluated-bar snapshots.
    snapshots: Vec<super::eval::BarSnapshot>,
    /// Plot captures in first-seen order.
    plots: Vec<super::eval::PlotCapture>,
    /// Candle captures in first-seen order, with the same index map.
    plotcandles: Vec<PlotCandleCapture>,
    /// Bar-color captures in first-seen order, with the same index map.
    barcolors: Vec<BarColorCapture>,
    /// Fill captures in first-seen order, with the same index map.
    fills: Vec<FillCapture>,
    /// Shape captures in first-seen order, with the same index map.
    plotshapes: Vec<PlotShapeCapture>,
    /// Hline captures in first-seen order, with the same index map.
    hlines: Vec<HlineCapture>,
    /// Capture site constant id to row index, keyed by family then site:
    /// one outer map replaces six per-family maps. The family selects the
    /// backing vector, so identical site ids across families never
    /// collide — exactly the old separate-map behavior. The inner map
    /// keys by owned site text so lookups borrow (`&str`, no clone on
    /// the per-bar hit path) and inserts move the resolved key once.
    capture_index: HashMap<CaptureFamily, HashMap<String, usize>>,
    /// Plot tags by global column: variables bound straight from a
    /// `plot()` call tag to that capture site, mirroring the tree-walk's
    /// plot-variable discipline for `fill()` pairing.
    plot_tags: HashMap<usize, u32>,
    /// Map-entry provenance by variable name: `name -> (map variable,
    /// canonical key encoding, key value)`. A variable bound from
    /// `map.get` writes method-receiver mutations back into the entry as
    /// well as the variable, exactly like the tree-walk's tag.
    map_prov: HashMap<String, (String, String, PineValue)>,
    /// Plot tags by bound name for in-function bindings: the value stays
    /// frame-local while the site lives run-globally, mirroring the
    /// tree-walk's single tag map. Checked before column tags when
    /// pairing `fill()` legs.
    body_plot_tags: HashMap<String, super::bytecode::ConstId>,
    /// Per-instance in-function `var`/`varip` values by call path plus
    /// declaration site. Entries hold only live values: clearing drops
    /// the entry while the seeded mark persists below.
    frame_vars: HashMap<VarInstKey, FrameVarCell>,
    /// Seeded per-instance cells: declaration executed at least once for
    /// this instance. Reads of unseeded names refuse; values and past
    /// advance only for seeded keys — mirroring the tree-walk's
    /// registration surviving a store clear.
    var_seeded: HashSet<VarInstKey>,
    /// Per-instance past, one slot per evaluated bar, backfilled with
    /// absence at seeding so indices align with absolute bars.
    var_hist: HashMap<VarInstKey, Vec<Option<PineValue>>>,
    /// Per-call plain-local past, one slot per evaluated bar. Seeded on
    /// the first return that records the call site and extended at every
    /// bar advance, exactly like the tree-walk's qualified frame history.
    local_hist: HashMap<LocalHistKey, Vec<Option<PineValue>>>,
    /// This bar's recorded local values by key: the return records the
    /// call's final slot state, and `commit_bar` folds it into the past.
    local_hist_now: HashMap<LocalHistKey, Option<PineValue>>,
    /// Call-path stack for cell identity: one (caller module, caller
    /// block, call-site pc) per open call, pushed on entry alongside the
    /// journal base and popped on return.
    var_path: Vec<(usize, usize, usize)>,
    /// Live method receivers, innermost last: `this` and alias reads hit
    /// the one live copy; nested calls compose exactly like the tree
    /// stack, popping on return alongside the journal suffix.
    receiver: Vec<ReceiverSlot>,
    /// Recycled call frames: popped frames return here instead of
    /// dropping, and fresh frames reuse them instead of allocating.
    frame_pool: Vec<Frame>,
    /// Per-call write log over caller homes, reverted in reverse order
    /// on return: journaled writes log the entry value once per home
    /// and write through, mirroring the tree-walk's journal stack.
    journal: Vec<JournalEntry>,
    /// Journal suffix owned by the innermost call (`None` outside
    /// calls): only writes at or after the base journal, so nested
    /// calls revert their own suffix and leave the caller's entries
    /// alone — exactly the tree-walk's base discipline.
    journal_base: Option<usize>,
    /// Caller identity stack, one entry per open call: the caller
    /// module, block, and frame index. Nested journal writes resolve
    /// caller homes nearest-first through each frame's named slots,
    /// mirroring the tree-walk's shared-namespace shadowing. Pushed on
    /// entry alongside the journal base, popped on return.
    callers: Vec<(usize, usize, usize)>,
    /// Live call stack (module/keyed call sites for `ta.*` identity).
    call_stack: Vec<String>,
    /// Incrementally maintained joined caller stack (`a>b>c`): extended
    /// at call boundaries, truncated on return, so `ta` calls never
    /// rejoin per call. Reads exactly like the tree-walk's per-bar join
    /// by construction (same pieces, same order, same separator).
    stack_prefix: String,
    /// Truncate marks for `stack_prefix`, one per open call.
    prefix_lens: Vec<usize>,
    /// Reused stacked-site buffer: `{stack_prefix}>{site}` composes here
    /// per `ta` call without allocating in steady state.
    site_scratch: String,
    /// Chart symbol backing chart-scope reads (`syminfo.tickerid`).
    chart_symbol: String,
    /// Chart period backing `time()` session reads and `timeframe.*`.
    chart_period: String,
    frames: Vec<Frame>,
    module: usize,
    block: usize,
    pc: usize,
    cursor: usize,
    completed: usize,
    first_bar: bool,
}

/// Three-valued branch used by `JumpIfFalse`/`JumpIfNa`: `None` jumps on
/// `JumpIfNa` only; non-`bool` values refuse naming the construct,
/// exactly like the tree-walk's `branch_of`.
/// Three-valued branch used by `JumpIfFalse`/`JumpIfNa`. Delegates to
/// the tree-walk's `branch_of` so both engines name refusals through
/// one implementation.
fn branch_bool(value: Option<&PineValue>, what: &str) -> Result<Option<bool>, KernelError> {
    super::eval::branch_of(what, value)
}

impl Run {
    /// Fresh per-run state over a linked program: unified global
    /// namespace, per-column history mirrors, and empty capture tables.
    /// Shared by the single-engine loop and mixed execution alike, so
    /// both observe identical starting state. The run-scoped builtin store
    /// and the input registry are passed by the caller, never cloned here.
    pub(crate) fn fresh(program: Arc<VmProgram>, legs: Legs, float_allowed: bool) -> Self {
        let mut names: Vec<Option<String>> = vec![None; LEG_COUNT_GLOBAL];
        let mut varip: HashSet<usize> = HashSet::new();
        let mut persistent: HashSet<usize> = HashSet::new();
        let mut width = LEG_COUNT_GLOBAL;
        for module in &program.modules {
            for (name, column) in &module.globals {
                while names.len() <= *column {
                    names.push(None);
                }
                names[*column] = Some(name.clone());
                width = width.max(names.len());
            }
            for column in &module.varip {
                varip.insert(*column);
                while names.len() <= *column {
                    names.push(None);
                }
                width = width.max(names.len());
            }
            for column in &module.persistent {
                persistent.insert(*column);
                while names.len() <= *column {
                    names.push(None);
                }
                width = width.max(names.len());
            }
        }
        if width == 0 {
            width = 1;
        }
        let written_cols = program.written_columns();
        Self {
            program,
            capture_index: HashMap::new(),
            frame_pool: Vec::new(),
            legs,
            tick_stamps: Vec::new(),
            current: vec![None; width],
            written: vec![false; width],
            columns: vec![Vec::new(); width],
            float_cols: vec![FloatSeries::new(); width],
            float_pure: vec![true; width],
            stable: vec![0; width],
            float_allowed,
            float_hits: 0,
            scalar_hits: 0,
            deopts: 0,
            scratch: Scratch::default(),
            written_cols,
            cell_order: Vec::new(),
            names,
            varip,
            persistent,
            snapshots: Vec::new(),
            plots: Vec::new(),
            plotcandles: Vec::new(),
            barcolors: Vec::new(),
            fills: Vec::new(),
            plotshapes: Vec::new(),
            hlines: Vec::new(),
            plot_tags: HashMap::new(),
            map_prov: HashMap::new(),
            body_plot_tags: HashMap::new(),
            frame_vars: HashMap::new(),
            var_seeded: HashSet::new(),
            var_hist: HashMap::new(),
            local_hist: HashMap::new(),
            local_hist_now: HashMap::new(),
            var_path: Vec::new(),
            receiver: Vec::new(),
            journal: Vec::new(),
            journal_base: None,
            callers: Vec::new(),
            call_stack: Vec::new(),
            stack_prefix: String::new(),
            prefix_lens: Vec::new(),
            site_scratch: String::new(),
            chart_symbol: String::new(),
            chart_period: String::new(),
            frames: Vec::new(),
            module: 0,
            block: 0,
            pc: 0,
            cursor: 0,
            completed: 0,
            first_bar: true,
        }
    }

    /// Fresh per-run state over a linked program with its run
    /// configuration applied: the one constructor whole-run and mixed
    /// execution share, so chart scope (and anything later added to the
    /// run config) can never differ between the two selections.
    pub(crate) fn fresh_with_config(
        program: Arc<VmProgram>,
        legs: Legs,
        float_allowed: bool,
        config: &VmConfig,
    ) -> Self {
        let mut run = Self::fresh(program, legs, float_allowed);
        run.chart_symbol.clone_from(&config.symbol);
        run.chart_period.clone_from(&config.period);
        run
    }

    /// Take a frame from the pool (or a fresh one), sized for `slots`.
    fn frame_take(&mut self, slots: usize) -> Frame {
        let mut frame = self.frame_pool.pop().unwrap_or(Frame { slots: Vec::new() });
        frame.slots.clear();
        frame.slots.resize(slots, None);
        frame
    }

    /// Return a frame to the pool.
    fn frame_return(&mut self, frame: Frame) {
        self.frame_pool.push(frame);
    }

    /// Read one frame slot: the slot keeps its value and the caller gets
    /// a shared copy. Scalars copy by value; objects and drawings share
    /// one resident table through the handle (one `Arc` bump, never a
    /// deep copy) until a mutation separates them.
    fn frame_get(&self, slot: SlotId) -> Option<PineValue> {
        self.frames
            .last()
            .and_then(|frame| frame.slots.get(slot.0 as usize).cloned())
            .flatten()
    }

    /// Borrow one frame slot without cloning: the hot arithmetic arms read
    /// both operands through here so a `BinOp` pays no `PineValue` clones
    /// before the shared `binary` leaf (which returns an owned value).
    /// The borrow ends before the destination store, so callers compute
    /// the value in a block scope first, then `frame_set`.
    fn frame_ref(&self, slot: SlotId) -> Option<&PineValue> {
        self.frames
            .last()
            .and_then(|frame| frame.slots.get(slot.0 as usize))
            .and_then(|slot| slot.as_ref())
    }

    fn frame_set(&mut self, slot: SlotId, value: Option<PineValue>) {
        if let Some(frame) = self.frames.last_mut() {
            let index = slot.0 as usize;
            if index >= frame.slots.len() {
                frame.slots.resize(index + 1, None);
            }
            frame.slots[index] = value;
        }
    }

    fn global_get(&self, column: usize) -> Option<PineValue> {
        self.current.get(column).cloned().flatten().flatten()
    }

    fn global_set(&mut self, column: usize, value: Option<PineValue>) {
        // Every column served here owns link-time history slots: the
        // link scan lists `GStore` targets and statically resolvable
        // home-written columns alike, so a later history read finds
        // committed slots instead of absence. The check is debug-only;
        // the release path stays allocation-free.
        debug_assert!(
            self.written_cols.contains(&column),
            "global_set targets column {column} with no link-time history slot"
        );
        if column >= self.current.len() {
            self.current.resize(column + 1, None);
            self.written.resize(column + 1, false);
            self.float_cols.resize_with(column + 1, FloatSeries::new);
            self.float_pure.resize(column + 1, true);
            self.stable.resize(column + 1, 0);
        }
        // T2 [correctness] deopt: the first cell the float mirror cannot
        // round-trip retires the column immediately — on outcome shape,
        // never on counts. A NaN float retires too, so the mirror's NaN
        // always means the scalar held absence — never a stored NaN
        // payload — and the fast path stays byte-identical by construction.
        // Purity and stability track the value shape on every run; tiering
        // itself stays gated behind T1 below (plus the tick-mode rule).
        let keeps_purity = match &value {
            None => true,
            Some(PineValue::Float(v)) => !v.is_nan(),
            _ => false,
        };
        if keeps_purity {
            self.stable[column] = self.stable[column].saturating_add(1);
        } else {
            if self.float_pure[column] {
                self.deopts += 1;
            }
            self.float_pure[column] = false;
            self.stable[column] = 0;
        }
        if self.persistent.contains(&column) {
            // Persistent cells clear on `na`: the initialization mark
            // (`written`) stays so history reads see `na` instead of
            // refusing, but the cell is unoccupied so snapshots omit it —
            // exactly the tree-walk's `clear_var`/`clear_varip` discipline.
            if value.is_none() {
                self.current[column] = None;
            } else {
                self.current[column] = Some(value);
            }
        } else {
            self.current[column] = Some(value);
        }
        self.written[column] = true;
    }

    fn leg(&self, index: usize) -> Option<f64> {
        LegKind::from_index(index as u32)
            .and_then(|kind| self.legs.named(kind.name()))
            .and_then(|leg| leg.get(self.cursor).copied())
    }

    /// One borrow-scoped program handle: a shared clone so the reference
    /// outlives the run's own mutable use (the typed equivalent of the
    /// borrowed program the entry points used before runs owned a handle).
    fn program_handle(&self) -> Arc<VmProgram> {
        Arc::clone(&self.program)
    }

    fn tick(&self) -> BarTick {
        BarTick {
            open: leg_or_nan(&self.legs, "open", self.cursor),
            high: leg_or_nan(&self.legs, "high", self.cursor),
            low: leg_or_nan(&self.legs, "low", self.cursor),
            close: leg_or_nan(&self.legs, "close", self.cursor),
            volume: leg_or_nan(&self.legs, "volume", self.cursor),
        }
    }

    /// Execute one bar across every module in link order.
    pub(crate) fn exec_bar(
        &mut self,
        ctx: &mut RunCtx<'_>,
        builtins: &mut RunBuiltins,
    ) -> Result<(), KernelError> {
        for position in 0..self.program.order.len() {
            let module = self.program.order[position];
            self.exec_module(module, ctx, builtins)?;
        }
        Ok(())
    }

    /// Execute one linked module's entry block for the current bar.
    pub(crate) fn exec_module(
        &mut self,
        module: usize,
        ctx: &mut RunCtx<'_>,
        builtins: &mut RunBuiltins,
    ) -> Result<(), KernelError> {
        let entry = self.program.modules[module].entry;
        let slots = self.program.modules[module].blocks[entry].slots as usize;
        let frame = self.frame_take(slots);
        self.frames.push(frame);
        self.module = module;
        self.block = entry;
        self.pc = 0;
        self.first_bar = self.completed == 0;
        let result = self.exec_block(ctx, builtins);
        if let Some(frame) = self.frames.pop() {
            self.frame_return(frame);
        }
        result?;
        Ok(())
    }

    /// Execute one non-control instruction (collections, objects, captures,
    /// journaled cells, requests, and the rest) and leave the cursor to the
    /// caller. Split out so the block loop's frame carries only the hot
    /// instruction shapes: a recursive run nests this frame instead of the
    /// whole dispatch, which keeps debug-build depth well inside the stack.
    fn exec_data(
        &mut self,
        program: &VmProgram,
        instr: &Instruction,
        ctx: &mut RunCtx<'_>,
        builtins: &mut RunBuiltins,
    ) -> Result<(), KernelError> {
        let _ = (&ctx, &builtins);
        match instr {
            Instruction::JournalWrite {
                home,
                src,
                mirror,
                strict,
            } => {
                // Loop-side journal write over a caller home: resolve
                // the home (module globals first, then the caller's
                // frame params — mirroring the tree-walk's single
                // shared namespace as seen from the callee), log the
                // entry value once per call, and write through. The
                // mirror keeps same-bar reads identical without
                // another lookup. Homes the tree would leave
                // unbound refuse through the shared constructor on
                // strict rebinds; plain binds fall back to a fresh
                // mirror instead, so nested readers still fall back
                // to the tree-walk exactly like the unbound case.
                let value = self.frame_get(*src);
                let name: &str = program.modules[self.module]
                    .symbols
                    .resolve(*home)
                    .unwrap_or("");
                self.journal_write_through(name, value.clone(), *strict)?;
                self.frame_set(*mirror, value);
            }
            Instruction::PlotTag { name, tag } => {
                // In-function tag write: the site lives run-globally
                // while every value stays where the binding put it —
                // exactly the tree-walk's tag-on-assign split. Never
                // journaled, never reverted: tags outlive the call on
                // both engines.
                let name: &str = program.modules[self.module]
                    .symbols
                    .resolve(*name)
                    .unwrap_or("");
                match tag {
                    Some(site) => {
                        self.body_plot_tags.insert(name.to_owned(), *site);
                    }
                    None => {
                        self.body_plot_tags.remove(name);
                    }
                }
            }
            Instruction::VarDecl {
                ordinal,
                is_varip,
                src,
                name,
            } => {
                // Seed the per-instance cell on first evaluation, never
                // again: the seeded mark itself is the guard, so a later
                // declaration of the same name is a no-op — exactly the
                // tree-walk's once-per-scope registration. Absent
                // initializers register without a value entry, so
                // snapshots omit them like the tree-walk's store.
                let value = self.frame_get(*src);
                let name: &str = program.modules[self.module]
                    .symbols
                    .resolve(*name)
                    .unwrap_or("");
                let key = VarInstKey {
                    path: self.var_path.clone(),
                    home: (self.module, self.block, *ordinal),
                };
                if self.var_seeded.insert(key.clone()) {
                    if value.is_some() {
                        self.seed_cell(
                            key.clone(),
                            FrameVarCell {
                                name: name.to_owned(),
                                value,
                                is_varip: *is_varip,
                            },
                        );
                    }
                    self.var_hist.insert(key, vec![None; self.completed]);
                }
            }
            Instruction::VarStore {
                ordinal,
                is_varip,
                src,
                name,
                strict,
            } => {
                let value = self.frame_get(*src);
                let name_text: &str = program.modules[self.module]
                    .symbols
                    .resolve(*name)
                    .unwrap_or("");
                let key = VarInstKey {
                    path: self.var_path.clone(),
                    home: (self.module, self.block, *ordinal),
                };
                if self.var_seeded.contains(&key) {
                    // Seeded cells store (absence clears the value entry
                    // but keeps the seeded mark, mirroring the
                    // tree-walk's store clear under registration).
                    if value.is_some() {
                        self.seed_cell(
                            key,
                            FrameVarCell {
                                name: name_text.to_owned(),
                                value,
                                is_varip: *is_varip,
                            },
                        );
                    } else {
                        self.frame_vars.remove(&key);
                    }
                } else if *strict {
                    return Err(super::eval::reassign_undeclared(name_text));
                } else {
                    // Plain bind before the declaration executed: the
                    // tree-walk falls back to a plain frame write, so
                    // the loop seeds the cell and journals the write
                    // through for nested observers.
                    self.var_seeded.insert(key.clone());
                    if value.is_some() {
                        self.seed_cell(
                            key.clone(),
                            FrameVarCell {
                                name: name_text.to_owned(),
                                value: value.clone(),
                                is_varip: *is_varip,
                            },
                        );
                    }
                    self.var_hist.insert(key, vec![None; self.completed]);
                    self.journal_write_through(name_text, value, false)?;
                }
            }
            Instruction::VarRead {
                dst,
                ordinal,
                name,
                hist,
            } => {
                let name_text: &str = program.modules[self.module]
                    .symbols
                    .resolve(*name)
                    .unwrap_or("");
                let key = VarInstKey {
                    path: self.var_path.clone(),
                    home: (self.module, self.block, *ordinal),
                };
                // Only truly unseeded names refuse: a seeded cell
                // without a value reads absence, exactly like the
                // tree-walk's persistent read.
                if !self.var_seeded.contains(&key) {
                    return Err(super::eval::undefined_variable(name_text));
                }
                let current = self
                    .frame_vars
                    .get(&key)
                    .and_then(|cell| cell.value.clone());
                let value = match hist {
                    None => current,
                    Some(back) => {
                        if *back < 0 {
                            return Err(super::eval::history_negative_offset(*back));
                        }
                        let back = *back as usize;
                        if let Some(PineValue::Array(items)) = current.as_ref() {
                            match items.get(back).cloned() {
                                Some(item) => item,
                                None => {
                                    return Err(super::eval::array_index_out_of_bounds(back));
                                }
                            }
                        } else if back == 0 {
                            current
                        } else {
                            self.completed
                                .checked_sub(back)
                                .and_then(|at| self.var_hist.get(&key)?.get(at).cloned())
                                .flatten()
                        }
                    }
                };
                self.frame_set(*dst, value);
            }
            Instruction::FieldGet { dst, obj, field } => {
                // Shared-table read: the handle shares the resident
                // table (no copy), and the first mutation through a
                // shared handle copies — the `FieldTable` contract the
                // protocol pins. Refusals route through the shared
                // constructors with the tree-walk's exact wordings.
                let field: &str = program.modules[self.module]
                    .symbols
                    .resolve(*field)
                    .unwrap_or("");
                let value = match self.frame_get(*obj) {
                    Some(PineValue::Object(instance)) => match instance.fields.get(field) {
                        Some(bound) => bound.clone(),
                        None => {
                            return Err(super::eval::unknown_field_on(field, &instance.type_name));
                        }
                    },
                    Some(other) => {
                        return Err(super::eval::field_needs_object(field, &other));
                    }
                    None => return Err(super::eval::field_read_on_na(field)),
                };
                self.frame_set(*dst, value);
            }
            Instruction::ObjNew {
                dst,
                type_name,
                fields,
            } => {
                // Construction evaluates field defaults per call at
                // the call site, exactly like the tree-walk's
                // field-by-field build. The fresh instance rides by
                // handle from birth — no copy, ever.
                let type_text: &str = program.modules[self.module]
                    .symbols
                    .resolve(*type_name)
                    .unwrap_or("");
                let mut bound = HashMap::with_capacity(fields.len());
                for (field, slot) in fields {
                    let name: &str = program.modules[self.module]
                        .symbols
                        .resolve(*field)
                        .unwrap_or("");
                    bound.insert(name.to_owned(), self.frame_get(*slot));
                }
                self.frame_set(
                    *dst,
                    Some(PineValue::Object(UserInstance::new(type_text, bound))),
                );
            }
            Instruction::ThisRead { dst } => {
                let value = match self.receiver.last() {
                    Some(slot) => Some(PineValue::Object(slot.instance.clone())),
                    None => return Err(super::eval::this_unbound()),
                };
                self.frame_set(*dst, value);
            }
            Instruction::ThisFieldGet { dst, field } => {
                let field: &str = program.modules[self.module]
                    .symbols
                    .resolve(*field)
                    .unwrap_or("");
                let slot = self.receiver.last().ok_or_else(super::eval::this_unbound)?;
                let value = match slot.instance.fields.get(field) {
                    Some(bound) => bound.clone(),
                    None => {
                        return Err(super::eval::unknown_this_field(field));
                    }
                };
                self.frame_set(*dst, value);
            }
            Instruction::ThisFieldSet { field, src } => {
                let value = self.frame_get(*src);
                let field: &str = program.modules[self.module]
                    .symbols
                    .resolve(*field)
                    .unwrap_or("");
                let slot = self
                    .receiver
                    .last_mut()
                    .ok_or_else(super::eval::this_unbound)?;
                if !slot.instance.fields.contains_key(field) {
                    return Err(super::eval::unknown_this_field(field));
                }
                slot.instance.fields.insert(field.to_owned(), value);
            }
            Instruction::AliasRead {
                dst,
                name,
                var_ordinal,
            } => {
                let name: &str = program.modules[self.module]
                    .symbols
                    .resolve(*name)
                    .unwrap_or("");
                // Nearest enclosing receiver with this alias wins,
                // mirroring the tree-walk's top-down stack search.
                if let Some(slot) = self.receiver.iter().rev().find(|slot| slot.alias == name) {
                    self.frame_set(*dst, Some(PineValue::Object(slot.instance.clone())));
                } else {
                    // No live slot: plain frame bindings, then shared
                    // columns, then per-instance cells — mirroring the
                    // tree-walk's namespace read once aliases are out
                    // of the picture. Truly unbound names refuse.
                    let value = self.alias_fallback(name, *var_ordinal)?;
                    self.frame_set(*dst, value);
                }
            }
            Instruction::AliasFieldGet {
                dst,
                name,
                field,
                var_ordinal,
            } => {
                let name_text: &str = program.modules[self.module]
                    .symbols
                    .resolve(*name)
                    .unwrap_or("");
                let field_text: &str = program.modules[self.module]
                    .symbols
                    .resolve(*field)
                    .unwrap_or("");
                // Nearest enclosing receiver first, exactly like the
                // tree-walk's top-down stack search.
                if let Some(slot) = self
                    .receiver
                    .iter()
                    .rev()
                    .find(|slot| slot.alias == name_text)
                {
                    let value = match slot.instance.fields.get(field_text) {
                        Some(bound) => bound.clone(),
                        None => {
                            return Err(super::eval::unknown_dotted_field(name_text, field_text));
                        }
                    };
                    self.frame_set(*dst, value);
                } else {
                    let value = self.alias_dotted_fallback(name_text, field_text, *var_ordinal)?;
                    self.frame_set(*dst, value);
                }
            }
            Instruction::FieldPut { home, field, src } => {
                // Single-statement take-mutate-put: the take follows
                // the method take order without logging (nothing must
                // revert), a missing field refuses after restoring the
                // instance, and take misses refuse naming the target —
                // exactly the tree-walk's dotted-write edges.
                let value = self.frame_get(*src);
                let field: &str = program.modules[self.module]
                    .symbols
                    .resolve(*field)
                    .unwrap_or("");
                let namespace = self.home_display(home);
                let namespace = namespace.as_str();
                match self.take_method_home(home, false)? {
                    Some((mut instance, taken)) => {
                        if !instance.fields.contains_key(field) {
                            self.write_method_home(&taken, instance)?;
                            return Err(super::eval::unknown_dotted_field(namespace, field));
                        }
                        instance.fields.insert(field.to_owned(), value);
                        self.write_method_home(&taken, instance)?;
                    }
                    None => {
                        return Err(super::eval::reassign_undeclared(namespace));
                    }
                }
            }
            Instruction::Plot {
                title,
                src,
                site,
                display,
            } => {
                let value = self.frame_get(*src);
                let display = (*display).and_then(|slot| self.frame_get(slot));
                let title = match title {
                    super::bytecode::PlotTitle::Const(id) => {
                        CaptureTitle::Const(Some(*id))
                    }
                    super::bytecode::PlotTitle::Slot(slot) => {
                        CaptureTitle::Value(self.frame_get(*slot))
                    }
                };
                let index = self.capture_row(
                    CaptureFamily::Plot,
                    *site,
                    &title,
                    |run, title_text, site_key| {
                        run.plots.push(PlotCapture::new(site_key, title_text));
                        run.plots.len() - 1
                    },
                )?;
                let plot = &mut self.plots[index];
                plot.values.push(value);
                plot.display = display;
            }
            Instruction::PlotCandle {
                open,
                high,
                low,
                close,
                title,
                site,
                display,
                color,
                wickcolor,
                bordercolor,
            } => {
                let open = self.frame_get(*open);
                let high = self.frame_get(*high);
                let low = self.frame_get(*low);
                let close = self.frame_get(*close);
                let display = (*display).and_then(|slot| self.frame_get(slot));
                let color = (*color).and_then(|slot| self.frame_get(slot));
                let wickcolor = (*wickcolor).and_then(|slot| self.frame_get(slot));
                let bordercolor = (*bordercolor).and_then(|slot| self.frame_get(slot));
                let index = self.capture_row(
                    CaptureFamily::PlotCandle,
                    *site,
                    &CaptureTitle::Const(*title),
                    |run, title_text, site_key| {
                        run.plotcandles.push(PlotCandleCapture {
                            site: site_key,
                            title: title_text,
                            display: None,
                            color: None,
                            wickcolor: None,
                            bordercolor: None,
                            opens: Vec::new(),
                            highs: Vec::new(),
                            lows: Vec::new(),
                            closes: Vec::new(),
                        });
                        run.plotcandles.len() - 1
                    },
                )?;
                let candle = &mut self.plotcandles[index];
                candle.opens.push(open);
                candle.highs.push(high);
                candle.lows.push(low);
                candle.closes.push(close);
                candle.display = display;
                candle.color = color;
                candle.wickcolor = wickcolor;
                candle.bordercolor = bordercolor;
            }
            Instruction::BarColor { src, title, site } => {
                let value = self.frame_get(*src);
                let index = self.capture_row(
                    CaptureFamily::BarColor,
                    *site,
                    &CaptureTitle::Const(*title),
                    |run, title_text, site_key| {
                        run.barcolors.push(BarColorCapture {
                            site: site_key,
                            title: title_text,
                            values: Vec::new(),
                        });
                        run.barcolors.len() - 1
                    },
                )?;
                self.barcolors[index].values.push(value);
            }
            Instruction::PlotShape { src, title, site } => {
                let value = self.frame_get(*src);
                let index = self.capture_row(
                    CaptureFamily::PlotShape,
                    *site,
                    &CaptureTitle::Const(*title),
                    |run, title_text, site_key| {
                        run.plotshapes.push(PlotShapeCapture {
                            site: site_key,
                            title: title_text,
                            values: Vec::new(),
                        });
                        run.plotshapes.len() - 1
                    },
                )?;
                self.plotshapes[index].values.push(value);
            }
            Instruction::Hline { src, title, site } => {
                let value = self.frame_get(*src);
                let index = self.capture_row(
                    CaptureFamily::Hline,
                    *site,
                    &CaptureTitle::Const(*title),
                    |run, title_text, site_key| {
                        run.hlines.push(HlineCapture {
                            site: site_key,
                            title: title_text,
                            values: Vec::new(),
                        });
                        run.hlines.len() - 1
                    },
                )?;
                self.hlines[index].values.push(value);
            }
            Instruction::Fill {
                first,
                second,
                title,
                site,
                color,
            } => {
                let (first_key, first_value, first_title) = self.fill_leg(program, first)?;
                let (second_key, second_value, second_title) = self.fill_leg(program, second)?;
                let region = match (&first_value, &second_value) {
                    (None, None) => None,
                    _ => Some(PineValue::Array(vec![first_value, second_value])),
                };
                let color = (*color).and_then(|slot| self.frame_get(slot));
                let index = self.capture_row(
                    CaptureFamily::Fill,
                    *site,
                    &CaptureTitle::Const(*title),
                    |run, title_text, site_key| {
                        run.fills.push(FillCapture {
                            site: site_key,
                            title: title_text,
                            first: first_key,
                            second: second_key,
                            first_title,
                            second_title,
                            color: None,
                            values: Vec::new(),
                        });
                        run.fills.len() - 1
                    },
                )?;
                let fill = &mut self.fills[index];
                fill.values.push(region);
                fill.color = color;
            }
            Instruction::Coalesce { dst, options } => {
                let mut out = None;
                for option in options {
                    if let Some(value) = self.frame_get(*option) {
                        out = Some(value);
                        break;
                    }
                }
                self.frame_set(*dst, out);
            }
            Instruction::ForInit {
                first,
                last,
                counter,
                step,
                ascending,
                stride,
            } => {
                let first_value = self.frame_get(*first);
                let last_value = self.frame_get(*last);
                let first = whole_number("for start", first_value.as_ref())?;
                let last = whole_number("for end", last_value.as_ref())?;
                let stride_value = self.frame_get(*stride);
                let stride = whole_number("for step", stride_value.as_ref())?;
                if stride <= 0 {
                    return Err(super::eval::for_step_non_positive(stride));
                }
                // The range limit counts strided visits, exactly like the
                // tree-walk: unit strides keep the historical count.
                let count = (i128::from(last) - i128::from(first)).abs() / i128::from(stride) + 1;
                if count > MAX_LOOP_ITERATIONS as i128 {
                    return Err(super::eval::for_range_over_limit(first, last));
                }
                self.frame_set(*counter, Some(PineValue::Int(first)));
                let direction = if first <= last { 1 } else { -1 };
                self.frame_set(
                    *step,
                    Some(PineValue::Int(direction * stride)),
                );
                self.frame_set(*ascending, Some(PineValue::Bool(first <= last)));
            }
            Instruction::WhileTick { trip } => {
                let count = match self.frame_get(*trip) {
                    Some(PineValue::Int(n)) => n,
                    _ => 0,
                };
                #[allow(clippy::cast_possible_wrap)]
                if count >= MAX_LOOP_ITERATIONS as i64 {
                    return Err(super::eval::while_over_limit());
                }
                self.frame_set(*trip, Some(PineValue::Int(count + 1)));
            }
            Instruction::ForInInit { arr, len } => {
                let value = self.frame_get(*arr);
                let items = match value {
                    Some(PineValue::Array(items)) => items,
                    Some(other) => {
                        return Err(super::eval::for_in_not_array(&other));
                    }
                    None => {
                        return Err(super::eval::for_in_absent());
                    }
                };
                if items.len() > MAX_LOOP_ITERATIONS {
                    return Err(super::eval::for_in_over_limit(items.len()));
                }
                #[allow(clippy::cast_possible_wrap)]
                let len_value = items.len() as i64;
                self.frame_set(*len, Some(PineValue::Int(len_value)));
            }
            Instruction::ArrayNew { dst, options } => {
                let mut bound = Vec::with_capacity(options.len());
                for option in options {
                    bound.push(self.frame_get(*option));
                }
                self.frame_set(*dst, Some(PineValue::Array(bound)));
            }
            Instruction::Destructure { dsts, arr } => {
                // Length-check plus ordered extract: the tree-walk's
                // arity and kind refusals, worded once in the shared
                // constructors. Element reads share the array value
                // protocol (cloned cells, absence preserved).
                let value = self.frame_get(*arr);
                match value {
                    Some(PineValue::Array(items)) => {
                        if items.len() != dsts.len() {
                            return Err(super::eval::destructure_arity_mismatch(
                                items.len(),
                                dsts.len(),
                            ));
                        }
                        for (dst, item) in dsts.iter().zip(items.iter()) {
                            self.frame_set(*dst, item.clone());
                        }
                    }
                    other => {
                        return Err(super::eval::destructure_non_array(other.as_ref()));
                    }
                }
            }
            Instruction::SwitchEq { dst, target, probe } => {
                let target = self.frame_get(*target);
                let probe = self.frame_get(*probe);
                self.frame_set(
                    *dst,
                    Some(PineValue::Bool(switch_eq(target.as_ref(), probe.as_ref()))),
                );
            }
            Instruction::RequestSecurity {
                dst,
                symbol,
                timeframe,
                gaps,
                lookahead,
                calc_bars,
                single,
                items,
                ..
            } => {
                let value = self.exec_request_security(
                    *symbol, *timeframe, *gaps, *lookahead, *calc_bars, *single, items, builtins,
                )?;
                if let Some(dst) = dst {
                    self.frame_set(*dst, value);
                }
            }
            Instruction::RequestLowerTf {
                dst,
                symbol,
                timeframe,
                legs,
                calc_bars,
                ..
            } => {
                let value =
                    self.exec_request_lower_tf(*symbol, *timeframe, legs, *calc_bars, builtins)?;
                if let Some(dst) = dst {
                    self.frame_set(*dst, value);
                }
            }
            Instruction::StrategyRead { dst, member } => {
                // One shared harness read: the tree-walk answers from
                // the same state, so values and refusals agree.
                let member: &str = program.modules[self.module]
                    .symbols
                    .resolve(*member)
                    .unwrap_or("");
                let value = builtins.strategy().read(member)?;
                self.frame_set(*dst, value);
            }
            Instruction::DottedGet {
                dst,
                root,
                name,
                field,
                flat,
            } => {
                // Single-dot variable read with the tree-walk's exact
                // edges: a recorded flat variable wins first, an object
                // root serves the field, a missing field names the dotted
                // path, and a non-object root falls through to the
                // undefined-variable refusal.
                let flat_column = flat
                    .map(|flat| flat.0 as usize)
                    .filter(|column| self.written.get(*column).copied().unwrap_or(false));
                let value = if let Some(column) = flat_column {
                    self.global_get(column)
                } else {
                    let symbols = &program.modules[self.module].symbols;
                    let name_text = symbols.resolve(*name).unwrap_or("");
                    let field_text = symbols.resolve(*field).unwrap_or("");
                    match self.frame_get(*root) {
                        Some(PineValue::Object(instance)) => {
                            if let Some(bound) = instance.fields.get(field_text) {
                                bound.clone()
                            } else {
                                let head =
                                    name_text.rsplit_once('.').map_or(name_text, |(h, _)| h);
                                return Err(super::eval::unknown_dotted_field(
                                    head, field_text,
                                ));
                            }
                        }
                        Some(_) | None => {
                            return Err(super::eval::undefined_variable(name_text));
                        }
                    }
                };
                self.frame_set(*dst, value);
            }
            Instruction::SetMapProv { name, tag } => {
                // Provenance tag: a `map.get` binding makes later
                // receiver mutations persist into the entry; any other
                // binding clears the tag.
                let name_text = program.modules[self.module]
                    .symbols
                    .resolve(*name)
                    .unwrap_or("")
                    .to_owned();
                match tag {
                    Some((map, key)) => {
                        let map_text = program.modules[self.module]
                            .symbols
                            .resolve(*map)
                            .unwrap_or("")
                            .to_owned();
                        let key_value = self.frame_get(*key);
                        if let Some(value) = key_value {
                            if let Some(encoding) = crate::value::map_key_of(&value) {
                                self.map_prov.insert(name_text, (map_text, encoding, value));
                            } else {
                                self.map_prov.remove(&name_text);
                            }
                        } else {
                            self.map_prov.remove(&name_text);
                        }
                    }
                    None => {
                        self.map_prov.remove(&name_text);
                    }
                }
            }
            Instruction::LocalHistory {
                dst,
                slot,
                name,
                offset,
                bare,
            } => {
                // Array bindings index by element (out of bounds refuses,
                // mirroring the run-error the language raises); anything
                // else reads per-call qualified history, falling back to
                // the bare column when the qualified key never recorded —
                // exactly the tree-walk's qualified-or-bare rule. Absent
                // indices read absence, never a refusal.
                let idx_value = match offset {
                    super::bytecode::HistOffset::Const(offset) => Some(i64::from(*offset)),
                    super::bytecode::HistOffset::Slot(offset) => match self.frame_get(*offset) {
                        None => None,
                        Some(value) => {
                            Some(super::eval::whole_number("array index", Some(&value))?)
                        }
                    },
                };
                let current = self.frame_get(*slot);
                let value = match idx_value {
                    None => None,
                    Some(idx_value) => {
                        if matches!(current, Some(PineValue::Array(_))) {
                            // A numeric index over an array binding is element
                            // access, exactly the tree-walk's offset rule.
                            let back = usize::try_from(idx_value)
                                .map_err(|_| super::eval::array_negative_index(idx_value))?;
                            match current {
                                Some(PineValue::Array(items)) => match items.get(back).cloned() {
                                    Some(item) => item,
                                    None => {
                                        return Err(super::eval::array_index_out_of_bounds(back));
                                    }
                                },
                                _ => None,
                            }
                        } else if idx_value == 0 {
                            current
                        } else {
                            let back = usize::try_from(idx_value)
                                .map_err(|_| super::eval::history_negative_offset(idx_value))?;
                            let key = LocalHistKey {
                                path: self.var_path.clone(),
                                body: (self.module, self.block),
                                name: *name,
                            };
                            match self.local_hist.get(&key) {
                                Some(slot) => {
                                    let at = self.completed.checked_sub(back);
                                    at.and_then(|at| slot.get(at).cloned()).flatten()
                                }
                                None => match bare {
                                    Some(bare) => {
                                        let column = bare.0 as usize;
                                        self.completed
                                            .checked_sub(back)
                                            .and_then(|at| {
                                                self.columns.get(column).and_then(|col| {
                                                    col.get(at).cloned()
                                                })
                                            })
                                            .flatten()
                                    }
                                    None => None,
                                },
                            }
                        }
                    }
                };
                self.frame_set(*dst, value);
            }
            _ => {}
        }
        Ok(())
    }

    /// Record the returning call's local-history slots: one entry per
    /// `history_locals` slot the compiled body reads, keyed by the open
    /// call path. The frame is still live, so the slot holds the body's
    /// final value — the same value the tree-walk records after a call.
    fn record_local_history(&mut self, module: usize, block: usize) {
        let program = self.program_handle();
        let program: &VmProgram = &program;
        let entries = &program.modules[module].blocks[block].history_locals;
        if entries.is_empty() {
            return;
        }
        let path = self.var_path.clone();
        let frame = self.frames.last();
        let mut recorded = Vec::with_capacity(entries.len());
        for (name, slot) in entries.iter() {
            let value = frame
                .and_then(|frame| frame.slots.get(*slot as usize).cloned())
                .flatten();
            recorded.push((
                LocalHistKey {
                    path: path.clone(),
                    body: (module, block),
                    name: *name,
                },
                value,
            ));
        }
        for (key, value) in recorded {
            self.local_hist
                .entry(key.clone())
                .or_insert_with(|| vec![None; self.completed]);
            self.local_hist_now.insert(key, value);
        }
    }

    /// Commit one evaluated bar: advance every history column (and its
    /// float mirror), then reset intrabar cells — mirroring `exec_bar`'s
    /// merge (current values persist; only `varip` resets). With
    /// `snapshot`, fold the written namespace into a bar snapshot first;
    /// mixed execution skips that fold and lets the tree-walk build the
    /// single merged snapshot from synced state instead.
    pub(crate) fn commit_bar(&mut self, index: usize, snapshot: bool) {
        if snapshot {
            let values = self.written_namespace();
            self.snapshots
                .push(super::eval::BarSnapshot { bar: index, values });
        }
        // Only columns something writes ever carry a history slot; an
        // unwritten column reads as `na` (its `recorded` mark stays false, so
        // a history read over it refuses as an undeclared name first).
        for position in 0..self.written_cols.len() {
            let column = self.written_cols[position];
            let value: Option<crate::value::PineValue> =
                self.current.get(column).cloned().flatten().flatten();
            // Mirror first, then move into the column: one clone saved per
            // column per bar (the mirror only borrows).
            self.float_cols[column].push_value(&value);
            self.columns[column].push(value);
        }
        for column in self.varip.clone() {
            if column < self.current.len() {
                self.current[column] = None;
            }
        }
        // Plain-local past advances one slot per bar: recorded values for
        // call sites that ran, absence for seeded sites that did not —
        // exactly the tree-walk's per-bar frame-history push.
        for (key, slot) in &mut self.local_hist {
            let value = self.local_hist_now.get(key).cloned().unwrap_or(None);
            slot.push(value);
        }
        self.local_hist_now.clear();
        // Per-instance cells advance their own past every bar; intrabar
        // cells drop their value entry but keep the seeded mark and past —
        // mirroring the tree-walk's store clear under registration.
        for key in self.var_seeded.clone() {
            let value = self
                .frame_vars
                .get(&key)
                .and_then(|cell| cell.value.clone());
            self.var_hist.entry(key.clone()).or_default().push(value);
            if self.frame_vars.get(&key).is_some_and(|cell| cell.is_varip) {
                self.frame_vars.remove(&key);
            }
        }
        self.completed += 1;
    }

    /// Current `(values, core_args)` scratch capacities: the
    /// warmup-stability suite pins that no later bar grows them.
    #[must_use]
    #[allow(dead_code)] // consumed by the call-path suite
    pub(crate) fn scratch_capacities(&self) -> (usize, usize) {
        (
            self.scratch.values.capacity(),
            self.scratch.core_args.capacity(),
        )
    }

    /// Global columns the commit fold advances, in link order. Test-facing:
    /// the snapshot-shape suite pins that unwritten columns never grow.
    #[must_use]
    #[allow(dead_code)] // consumed by the snapshot-shape suite
    pub(crate) fn committed_columns(&self) -> &[usize] {
        &self.written_cols
    }

    #[cfg(test)]
    /// Cursor of the bar under evaluation (mirrors the tree-walk cursor).
    pub(crate) fn set_cursor(&mut self, cursor: usize) {
        self.cursor = cursor;
    }

    /// Written namespace as name-to-value with the occupied-only rule:
    /// written columns only (the `recorded` predicate); persistent cells
    /// contribute only while occupied (cleared `var`/`varip` cells are
    /// omitted exactly like the tree-walk's occupied-only snapshot),
    /// while plain cells keep their explicit `na` entries.
    fn written_namespace(&self) -> HashMap<String, Option<PineValue>> {
        // Per-instance `var` cells come first (the tree-walk's store
        // snapshot seeds the merge); column values override on collision,
        // exactly like the tree-walk's variables-over-store order.
        let mut values = self.frame_var_snapshot();
        for position in 0..self.written_cols.len() {
            let column = self.written_cols[position];
            if !self.written.get(column).copied().unwrap_or(false) {
                continue;
            }
            if self.persistent.contains(&column) {
                let occupied = matches!(self.current.get(column), Some(Some(Some(_))));
                if !occupied {
                    continue;
                }
            }
            if let Some(name) = self.names.get(column).cloned().flatten() {
                // Engine-internal spill columns carry history but never
                // reach snapshots: both engines omit these names so
                // outcomes stay user-visible-identical.
                if super::eval::is_hidden_series(&name) {
                    continue;
                }
                let value: Option<PineValue> =
                    self.current.get(column).cloned().flatten().flatten();
                values.insert(name, value);
            }
        }
        values
    }

    /// Outermost value per bound name over the per-instance cells: the
    /// shortest call path wins, then the earliest declaration, with
    /// intrabar cells overlaying bar-persistent ones on collision —
    /// mirroring the tree-walk's store snapshot view.
    fn frame_var_snapshot(&self) -> HashMap<String, Option<PineValue>> {
        // The order is cached at seed time (`resort_cells`), so a snapshot
        // costs no sort: a bar with no new declaration just walks it.
        let mut out = HashMap::with_capacity(self.frame_vars.len());
        for key in &self.cell_order {
            let Some(cell) = self.frame_vars.get(key) else {
                continue;
            };
            if cell.is_varip {
                continue;
            }
            out.insert(cell.name.clone(), cell.value.clone());
        }
        for key in &self.cell_order {
            let Some(cell) = self.frame_vars.get(key) else {
                continue;
            };
            if !cell.is_varip {
                continue;
            }
            out.insert(cell.name.clone(), cell.value.clone());
        }
        out
    }

    /// Seed one per-instance cell and refresh the snapshot order. New cells
    /// are rare (one per declaration site), so the sort sits at the seed
    /// rather than in every bar's snapshot.
    fn seed_cell(&mut self, key: VarInstKey, cell: FrameVarCell) {
        self.frame_vars.insert(key, cell);
        self.resort_cells();
    }

    /// Rebuild the sorted cell order: shortest call path first, then the
    /// earliest declaration within it.
    fn resort_cells(&mut self) {
        self.cell_order = self.frame_vars.keys().cloned().collect();
        self.cell_order.sort_by(|left, right| {
            (left.path.len(), left.home).cmp(&(right.path.len(), right.home))
        });
    }

    #[cfg(test)]
    /// Current written namespace as name-to-value, using the same
    /// occupied-only rule as [`commit_bar`](Self::commit_bar): persistent
    /// cells contribute only while occupied, plain cells keep explicit
    /// `na`. Test-facing: the snapshot-shape suite pins the rule.
    pub(crate) fn snapshot_values(&self) -> HashMap<String, Option<PineValue>> {
        self.written_namespace()
    }


    /// Execute the current block to completion (jumps stay inside) or
    /// until a `Return` carries the value out.
    fn exec_block(
        &mut self,
        ctx: &mut RunCtx<'_>,
        builtins: &mut RunBuiltins
    ) -> Result<Option<PineValue>, KernelError> {
        // One handle for the whole block: the borrow is of the local handle,
        // never of `self`, so the instruction borrow below never conflicts
        // with execution — and the block loop costs no refcount traffic.
        let program = self.program_handle();
        let program: &VmProgram = &program;
        loop {
            let block = &program.modules[self.module].blocks[self.block];
            if self.pc >= block.instrs.len() {
                return Ok(None);
            }
            let instr = &block.instrs[self.pc];
            match instr {
                Instruction::Const { dst, value } => {
                    // Link-resolved cache: one indexed clone per execution
                    // (out-of-range ids fail closed — link validation
                    // rejects them up front, this is defense in depth).
                    let value = program.modules[self.module]
                        .const_vals
                        .get(value.0 as usize)
                        .cloned()
                        .ok_or_else(|| {
                            KernelError::Eval("bytecode const id out of range".to_owned())
                        })?;
                    self.frame_set(*dst, value);
                    self.pc += 1;
                }
                Instruction::Copy { dst, src } => {
                    let value = self.frame_get(*src);
                    self.frame_set(*dst, value);
                    self.pc += 1;
                }
                Instruction::BinOp { dst, op, lhs, rhs } => {
                    // Borrowed operands: no `PineValue` clone feeds the
                    // shared leaf (it returns an owned value), and the
                    // borrow scope ends before the destination store.
                    let value = {
                        let left = self.frame_ref(*lhs);
                        let right = self.frame_ref(*rhs);
                        super::eval::binary(
                            match op {
                                BinOp::Add => BinaryOp::Add,
                                BinOp::Subtract => BinaryOp::Subtract,
                                BinOp::Multiply => BinaryOp::Multiply,
                                BinOp::Divide => BinaryOp::Divide,
                                BinOp::Modulo => BinaryOp::Modulo,
                                BinOp::Equal => BinaryOp::Equal,
                                BinOp::NotEqual => BinaryOp::NotEqual,
                                BinOp::Less => BinaryOp::Less,
                                BinOp::LessEqual => BinaryOp::LessEqual,
                                BinOp::Greater => BinaryOp::Greater,
                                BinOp::GreaterEqual => BinaryOp::GreaterEqual,
                                BinOp::And => BinaryOp::And,
                                BinOp::Or => BinaryOp::Or,
                            },
                            left,
                            right,
                        )?
                    };
                    self.frame_set(*dst, value);
                    self.pc += 1;
                }
                Instruction::UnOp { dst, op, src } => {
                    let value = {
                        let operand = self.frame_ref(*src);
                        super::eval::unary(
                            match op {
                                UnOp::Negate => UnaryOp::Negate,
                                UnOp::Not => UnaryOp::Not,
                            },
                            operand,
                        )?
                    };
                    self.frame_set(*dst, value);
                    self.pc += 1;
                }
                Instruction::Builtin { dst, var } => {
                    let value = match var {
                        BuiltinVarKind::Open => self
                            .leg(LegKind::Open.index() as usize)
                            .map(PineValue::Float),
                        BuiltinVarKind::High => self
                            .leg(LegKind::High.index() as usize)
                            .map(PineValue::Float),
                        BuiltinVarKind::Low => self
                            .leg(LegKind::Low.index() as usize)
                            .map(PineValue::Float),
                        BuiltinVarKind::Close => self
                            .leg(LegKind::Close.index() as usize)
                            .map(PineValue::Float),
                        BuiltinVarKind::Volume => self
                            .leg(LegKind::Volume.index() as usize)
                            .map(PineValue::Float),
                        BuiltinVarKind::Time => self
                            .leg(LegKind::Time.index() as usize)
                            .map(PineValue::Float),
                        BuiltinVarKind::Timenow => {
                            // Chart clock: integer tick stamps in tick
                            // runs, the time-leg float otherwise —
                            // exactly like the tree-walk's `timenow`.
                            if self.float_allowed {
                                self.leg(LegKind::Time.index() as usize)
                                    .map(PineValue::Float)
                            } else {
                                self.tick_stamps
                                    .get(self.cursor)
                                    .copied()
                                    .map(PineValue::Int)
                            }
                        }
                        BuiltinVarKind::Tr => {
                            // Legacy bare true range through the shared
                            // kernel helper — exactly like the tree-walk.
                            super::eval::true_range_at(&self.legs, self.cursor)
                        }
                        BuiltinVarKind::ChartPeriodSeconds => {
                            // Bare chart-period read through the shared
                            // resolver — exactly like the tree-walk.
                            Some(PineValue::Int(super::builtins_core::chart_period_seconds(
                                &self.chart_period,
                            )))
                        }
                        BuiltinVarKind::BarIndex => Some(PineValue::Int(self.cursor as i64)),
                        // Per-bar flags serve from the run cursor through
                        // the shared resolver: closed-bar and tick runs
                        // agree with the tree-walk by construction.
                        BuiltinVarKind::BarstateIsFirst
                        | BuiltinVarKind::BarstateIsNew
                        | BuiltinVarKind::BarstateIsConfirmed
                        | BuiltinVarKind::BarstateIsLast
                        | BuiltinVarKind::BarstateIsHistory
                        | BuiltinVarKind::BarstateIsRealtime => var
                            .barstate_member()
                            .and_then(|member| {
                                super::eval::barstate_flag_at(
                                    member,
                                    self.cursor,
                                    self.legs.time.len(),
                                    &self.legs.time,
                                    !self.float_allowed,
                                )
                            })
                            .map(PineValue::Bool),
                        // Chart scope resolves through the same shared
                        // member table the tree-walk reads.
                        other => other
                            .chart_scope()
                            .map(|scope| scope.read(&self.chart_symbol, &self.chart_period)),
                    };
                    self.frame_set(*dst, value);
                    self.pc += 1;
                }
                Instruction::History { dst, src, offset } => {
                    let value = self.history_read(*src, *offset)?;
                    self.frame_set(*dst, value);
                    self.pc += 1;
                }
                Instruction::GLoad { dst, slot } => {
                    let column = slot.0 as usize;
                    let value = self.global_get(column);
                    // Declaredness rule: legs always serve (chart window),
                    // persistent cells read as absence until seeded, and
                    // written columns serve their cell (explicit `na`
                    // included). Anything else was never recorded, so the
                    // run refuses naming the column — exactly like the
                    // tree-walk's undeclared read.
                    if value.is_none()
                        && column >= super::bytecode::LEG_COUNT as usize
                        && !self.persistent.contains(&column)
                        && !self.written.get(column).copied().unwrap_or(false)
                    {
                        let name = self
                            .names
                            .get(column)
                            .cloned()
                            .flatten()
                            .unwrap_or_else(|| format!("#{column}"));
                        return Err(super::eval::undefined_variable(&name));
                    }
                    self.frame_set(*dst, value);
                    self.pc += 1;
                }
                Instruction::GStoreLoop { slot, src } => {
                    let value = self.frame_get(*src);
                    self.global_set(slot.0 as usize, value);
                    self.pc += 1;
                }
                Instruction::GStore {
                    slot,
                    src,
                    plot_tag,
                } => {
                    let value = self.frame_get(*src);
                    let column = slot.0 as usize;
                    self.global_set(column, value);
                    // Top-level stores refresh the single tag map: any
                    // in-function tag for the same name yields to the
                    // latest write, exactly like the tree-walk.
                    if let Some(name) = self.names.get(column).cloned().flatten() {
                        self.body_plot_tags.remove(&name);
                    }
                    match plot_tag {
                        Some(tag) => {
                            self.plot_tags.insert(column, tag.0);
                        }
                        None => {
                            self.plot_tags.remove(&column);
                        }
                    }
                    self.pc += 1;
                }
                Instruction::GStoreInit { slot, src } => {
                    if self.first_bar {
                        let value = self.frame_get(*src);
                        self.global_set(slot.0 as usize, value);
                    }
                    self.pc += 1;
                }
                Instruction::GReassign {
                    slot,
                    src,
                    plot_tag,
                } => {
                    // Checked rebind: persistent cells and recorded columns
                    // take the store (with the `GStore` tag discipline);
                    // anything else was never declared here, so the run
                    // refuses naming the target — exactly like the
                    // tree-walk's rebind rule. Fires only when reached, so
                    // a dead-branch rebind stays silent.
                    let column = slot.0 as usize;
                    if !self.persistent.contains(&column)
                        && !self.written.get(column).copied().unwrap_or(false)
                    {
                        let name = self
                            .names
                            .get(column)
                            .cloned()
                            .flatten()
                            .unwrap_or_else(|| format!("#{column}"));
                        return Err(super::eval::reassign_undeclared(&name));
                    }
                    let value = self.frame_get(*src);
                    self.global_set(column, value);
                    if let Some(name) = self.names.get(column).cloned().flatten() {
                        self.body_plot_tags.remove(&name);
                    }
                    match plot_tag {
                        Some(tag) => {
                            self.plot_tags.insert(column, tag.0);
                        }
                        None => {
                            self.plot_tags.remove(&column);
                        }
                    }
                    self.pc += 1;
                }
                Instruction::InstanceCall {
                    dst,
                    recv,
                    method,
                    args,
                    homes,
                    args_debug,
                    home,
                    stmt,
                } => {
                    self.exec_method_call(
                        program,
                        *dst,
                        *recv,
                        *method,
                        args,
                        homes,
                        *args_debug,
                        home.as_ref(),
                        *stmt,
                        ctx,
                        builtins,
                    )?;
                }
                Instruction::Jump { target } => {
                    self.pc = *target;
                }
                Instruction::JumpIfFalse { cond, target, ctx } => {
                    let taken = {
                        let value = self.frame_ref(*cond);
                        matches!(
                            branch_bool(
                                value,
                                match ctx {
                                    super::bytecode::CondContext::If => "if condition",
                                    super::bytecode::CondContext::Ternary => "ternary condition",
                                    super::bytecode::CondContext::While => "while condition",
                                    super::bytecode::CondContext::SwitchPattern => "switch pattern",
                                },
                            )?,
                            Some(false)
                        )
                    };
                    if taken {
                        self.pc = *target;
                    } else {
                        self.pc += 1;
                    }
                }
                Instruction::JumpIfNa { cond, target } => {
                    if self.frame_ref(*cond).is_none() {
                        self.pc = *target;
                    } else {
                        self.pc += 1;
                    }
                }
                Instruction::Return { src } => {
                    let value = (*src).and_then(|slot| self.frame_get(slot));
                    return Ok(value);
                }
                Instruction::ArrayGet {
                    dst,
                    arr,
                    idx,
                    hist,
                } => {
                    let arr_value = self.frame_get(*arr);
                    let idx_value = self.frame_get(*idx);
                    // Absent index reads absence (never a refusal);
                    // integral floats count exactly like the tree-walk's
                    // whole-number rule.
                    let Some(idx_value) = idx_value else {
                        self.frame_set(*dst, None);
                        self.pc += 1;
                        continue;
                    };
                    let index = whole_number("array index", Some(&idx_value))?;
                    let Some(PineValue::Array(items)) = arr_value else {
                        // Not an array: read the series history when the
                        // compiler named one, else refuse exactly like
                        // the tree-walk's compound-expression rule.
                        let Some(column) = hist else {
                            return Err(KernelError::Eval(
                                "history offsets read named series, not compound expressions"
                                    .to_owned(),
                            ));
                        };
                        let value = self.history_read(
                            *column,
                            i32::try_from(index).unwrap_or(i32::MAX),
                        )?;
                        self.frame_set(*dst, value);
                        self.pc += 1;
                        continue;
                    };
                    let value = usize::try_from(index)
                        .map_err(|_| super::eval::array_negative_index(index))
                        .and_then(|slot| {
                            items
                                .get(slot)
                                .cloned()
                                .ok_or_else(|| super::eval::array_index_out_of_bounds(slot))
                        })?;
                    self.frame_set(*dst, value);
                    self.pc += 1;
                }
                Instruction::Fallback { reason, .. } => {
                    // Unreachable: the linker refuses fallback-bearing
                    // programs, so reaching one is a linker defect, never
                    // a run-time condition. Fail closed with the named
                    // reason instead of running a partial program.
                    let symbols = &program.modules[self.module].symbols;
                    return Err(KernelError::Eval(format!(
                        "unlinked fallback reached at run time: {}",
                        reason.display(symbols)
                    )));
                }
                Instruction::Refuse { reason } => {
                    // Deterministic call-shape refusal: the module stays
                    // compiled and the run stops here with the same message
                    // the tree-walk raises at this program point.
                    let text = match program.modules[self.module].consts.get(*reason) {
                        Some(super::bytecode::ConstValue::Str(text)) => text.clone(),
                        _ => "refusal reason missing".to_owned(),
                    };
                    return Err(KernelError::Eval(text));
                }
                Instruction::RuntimeRefuse { message } => {
                    // Computed refusal message: the slot holds the lowered
                    // argument, mapped exactly like the tree-walk's first
                    // argument (a string verbatim, any other value with
                    // its prefix, absence bare).
                    let text = match self.frame_get(*message) {
                        Some(PineValue::Str(text)) => text,
                        Some(other) => format!("runtime error: {other}"),
                        None => "runtime error".to_owned(),
                    };
                    return Err(KernelError::Eval(text));
                }
                Instruction::EnumMark { enm, members } => {
                    // Declare-once registration into the shared run table:
                    // the first execution wins on either engine, exactly
                    // like the tree-walk's table (conditional and duplicate
                    // declarations find it occupied and keep it).
                    let program = self.program_handle();
                    let program: &VmProgram = &program;
                    let symbols = &program.modules[self.module].symbols;
                    let enm_text = symbols.resolve(*enm).unwrap_or("").to_owned();
                    let member_set: std::collections::HashSet<String> = members
                        .iter()
                        .map(|member| symbols.resolve(*member).unwrap_or("").to_owned())
                        .collect();
                    builtins.register_enum(enm_text, member_set);
                    self.pc += 1;
                }
                Instruction::EnumRead {
                    dst,
                    enm,
                    member,
                    name,
                    flat,
                } => {
                    // Flat-first member read: a recorded flat wins, else
                    // the registered pair serves the identity handle and
                    // an unregistered pair refuses naming the full
                    // spelling — exactly like the tree-walk.
                    let column = flat.0 as usize;
                    let value = if self.written.get(column).copied().unwrap_or(false) {
                        self.global_get(column)
                    } else {
                        let program = self.program_handle();
                        let program: &VmProgram = &program;
                        let symbols = &program.modules[self.module].symbols;
                        let enm_text = symbols.resolve(*enm).unwrap_or("");
                        let member_text = symbols.resolve(*member).unwrap_or("");
                        let name_text = symbols.resolve(*name).unwrap_or("").to_owned();
                        if !builtins.enum_member_registered(enm_text, member_text) {
                            return Err(super::eval::undefined_variable(&name_text));
                        }
                        Some(PineValue::EnumHandle(EnumHandle::new(
                            enm_text,
                            member_text,
                        )))
                    };
                    self.frame_set(*dst, value);
                    self.pc += 1;
                }
                Instruction::AliasFlatRead { dst, flat, src } => {
                    // Flat-first alias read: a recorded flat wins, else
                    // the eagerly lowered remainder flows on — exactly
                    // like the tree-walk (whose own remainder evaluation
                    // already refused or served with identical wording).
                    let column = flat.0 as usize;
                    let value = if self.written.get(column).copied().unwrap_or(false) {
                        self.global_get(column)
                    } else {
                        self.frame_get(*src)
                    };
                    self.frame_set(*dst, value);
                    self.pc += 1;
                }
                Instruction::Call {
                    dst,
                    target,
                    args,
                    homes,
                    type_args,
                    site,
                } => {
                    self.exec_call(
                        program, *dst, target, args, homes, type_args, *site, ctx, builtins,
                    )?;
                }
                // Every remaining instruction is a data/effect
                // instruction that only advances the cursor: one helper
                // carries its temporaries so the block loop's own frame
                // stays small (debug-build stack depth over recursive
                // calls is bounded by the call-depth limit).
                other => {
                    self.exec_data(program, other, ctx, builtins)?;
                    self.pc += 1;
                }
            }
        }
    }

    /// Resolve a capture title: the interned literal when present, else
    /// the run-time `family:{n}` default counted exactly like the
    /// tree-walk (the family vector length before this bar appends).
    /// Resolve one capture row by (family, site): the recorded index on
    /// repeat sightings, or a fresh row built by `make` on first sight
    /// (title and site key resolved once, registered in order). The
    /// caller then pushes the bar's values and refreshes per-bar fields
    /// through the returned index — creation leaves fields unset and
    /// rows empty, so the update path runs identically on first and
    /// repeat sightings.
    fn capture_row(
        &mut self,
        family: CaptureFamily,
        site: ConstId,
        title: &CaptureTitle,
        make: impl FnOnce(&mut Self, String, String) -> usize,
    ) -> Result<usize, KernelError> {
        // Keyed by the resolved site string, never the const id: const
        // pools are per-module, so two modules' plots can share one
        // numeric id with different meanings. The tree-walk merges same
        // strings globally, so the loop resolves first and compares text.
        // The inner map borrows on lookup: no clone on the per-bar hit
        // path, one move of the resolved key on first sight.
        let site_key = self.capture_site(site, family.name())?;
        if let Some(index) = self
            .capture_index
            .get(&family)
            .and_then(|inner| inner.get(site_key.as_str()))
            .copied()
        {
            return Ok(index);
        }
        let len = self.capture_len(family);
        let title_text = self.capture_title(title, family.name(), len)?;
        let index = make(self, title_text, site_key.clone());
        self.capture_index
            .entry(family)
            .or_default()
            .insert(site_key, index);
        Ok(index)
    }

    /// Recorded rows so far in one family (the title fallback counts
    /// first sights, mirroring the run-global counters).
    fn capture_len(&self, family: CaptureFamily) -> usize {
        match family {
            CaptureFamily::Plot => self.plots.len(),
            CaptureFamily::PlotCandle => self.plotcandles.len(),
            CaptureFamily::BarColor => self.barcolors.len(),
            CaptureFamily::PlotShape => self.plotshapes.len(),
            CaptureFamily::Hline => self.hlines.len(),
            CaptureFamily::Fill => self.fills.len(),
        }
    }

    fn capture_title(
        &self,
        title: &CaptureTitle,
        family: &str,
        count: usize,
    ) -> Result<String, KernelError> {
        match title {
            CaptureTitle::Const(Some(id)) => {
                match self.program.modules[self.module].consts.get(*id) {
                    Some(ConstValue::Str(text)) => Ok(text.clone()),
                    _ => Err(KernelError::Eval(format!(
                        "{family} title must be a string"
                    ))),
                }
            }
            // `plot` titles are expressions: a string names the series, and
            // anything else (including `na`) falls back to the run-time
            // default at first sight, exactly like the tree-walk.
            CaptureTitle::Value(Some(PineValue::Str(text))) => Ok(text.clone()),
            CaptureTitle::Const(None) | CaptureTitle::Value(_) => {
                Ok(super::eval::capture_fallback_title(family, count))
            }
        }
    }

    /// Resolve a capture site key: the interned call text, identical to
    /// the tree-walk's per-family site keys.
    fn capture_site(&self, site: ConstId, family: &str) -> Result<String, KernelError> {
        match self.program.modules[self.module].consts.get(site) {
            Some(ConstValue::Str(text)) => Ok(text.clone()),
            _ => Err(KernelError::Eval(format!("{family} site must be a string"))),
        }
    }

    /// Resolve one `fill()` leg to its site key, latest value, and plot
    /// title. Plot sites look up the paired capture; variables look up
    /// the in-function tag map first (frame-local values with run-global
    /// sites), then the run-time plot tag by global column. Refusals name
    /// the same reasons as the tree-walk's fill pairer.
    fn fill_leg(
        &self,
        program: &VmProgram,
        leg: &FillLeg,
    ) -> Result<(String, Option<PineValue>, String), KernelError> {
        let site_id = match leg {
            FillLeg::PlotSite(id) => *id,
            FillLeg::Variable(id) => {
                let caller = &program.modules[self.module];
                let var = caller.symbols.resolve(*id).unwrap_or("?");
                if let Some(tag) = self.body_plot_tags.get(var) {
                    *tag
                } else {
                    let column = caller
                        .globals
                        .get(var)
                        .copied()
                        .ok_or_else(|| super::eval::fill_variable_without_plot_tag(var))?;
                    let tag = self
                        .plot_tags
                        .get(&column)
                        .copied()
                        .ok_or_else(|| super::eval::fill_variable_without_plot_tag(var))?;
                    ConstId(tag)
                }
            }
        };
        let site_key = match program.modules[self.module].consts.get(site_id) {
            Some(ConstValue::Str(text)) => text.clone(),
            _ => {
                return Err(KernelError::Eval(
                    "fill plot site must be a string".to_owned(),
                ));
            }
        };
        let Some(&index) = self
            .capture_index
            .get(&CaptureFamily::Plot)
            .and_then(|inner| inner.get(site_key.as_str()))
        else {
            return Err(super::eval::fill_plot_site_without_capture(&site_key));
        };
        let plot = &self.plots[index];
        let value = plot.values.last().cloned().unwrap_or(None);
        Ok((site_key, value, plot.title.clone()))
    }

    /// History read with tree-walk edge semantics: negative offsets
    /// refuse, legs read the chart window, bare names read the resolved
    /// column, unrecorded names refuse, arrays index with bounds checks.
    fn history_read(&mut self, src: SlotId, offset: i32) -> Result<Option<PineValue>, KernelError> {
        use super::bytecode::LEG_COUNT;
        if offset < 0 {
            return Err(super::eval::history_negative_offset(offset));
        }
        let back = offset as usize;
        let index = src.0 as usize;
        if index < LEG_COUNT as usize {
            let name = LegKind::from_index(index as u32).map_or("close", LegKind::name);
            let at = self.cursor.checked_sub(back);
            return Ok(at
                .and_then(|bar| self.legs.named(name).and_then(|leg| leg.get(bar).copied()))
                .and_then(crate::indicator::fin_or_none)
                .map(PineValue::Float));
        }
        let bound: Option<PineValue> = self.current.get(index).cloned().flatten().flatten();
        if let Some(PineValue::Array(items)) = bound.as_ref() {
            return match items.get(back) {
                Some(value) => Ok(value.clone()),
                None => Err(super::eval::array_index_out_of_bounds(back)),
            };
        }
        if bound.is_none() && !self.recorded(index) {
            let name = self
                .names
                .get(index)
                .and_then(Clone::clone)
                .unwrap_or_else(|| format!("#{index}"));
            return Err(super::eval::undefined_variable(&name));
        }
        if back == 0 {
            return Ok(bound);
        }
        // T1 [confidence] tier-up: float-pure columns serve history from
        // the contiguous mirror only once the stability counter reaches
        // `TIER_STABLE_BARS` consecutive float-or-absence observations,
        // and only on closed-bar runs (tick emulation stays scalar by
        // calibration rule). The mirror holds exactly the scalar floats
        // (purity guarantees no integer, boolean, or NaN-float cell ever
        // entered), so the indexed load is byte-identical to the boxed
        // row it replaces.
        if self.float_allowed
            && self.float_pure.get(index).copied().unwrap_or(false)
            && self.stable.get(index).copied().unwrap_or(0) >= TIER_STABLE_BARS
        {
            self.float_hits += 1;
            return Ok(self
                .completed
                .checked_sub(back)
                .and_then(|at| self.float_cols.get(index)?.get(at))
                .map(PineValue::Float));
        }
        self.scalar_hits += 1;
        Ok(self
            .completed
            .checked_sub(back)
            .and_then(|at| self.columns.get(index)?.get(at).cloned())
            .flatten())
    }

    fn recorded(&self, column: usize) -> bool {
        self.written.get(column).copied().unwrap_or(false)
    }

    /// Execute one builtin-namespace call: shared bridges, precomputed
    /// site, and the statement-position write-back rule. Kept out of
    /// `exec_call` so the user-call recursion path keeps a small native
    /// frame (deep recursion must refuse by depth, not by stack).
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn exec_builtin_call(
        &mut self,
        program: &VmProgram,
        dst: Option<SlotId>,
        namespace: BuiltinNamespace,
        method: SymbolId,
        args: &[super::bytecode::CallArgSlot],
        homes: &[Option<SymbolId>],
        type_args: &[CallTypeArg],
        site: SymbolId,
        ctx: &mut RunCtx<'_>,
        builtins: &mut RunBuiltins,
    ) -> Result<(), KernelError> {
        let symbols = &program.modules[self.module].symbols;
        let method_name: &str = symbols.resolve(method).unwrap_or("?");
        let site_text: &str = symbols.resolve(site).unwrap_or("?");
        // Single pass: one value clone per argument feeds both the
        // positional vector and the named bridge view.
        // The run's call scratch: one value clone per argument feeds both
        // the positional vector and the named bridge view, and no call
        // allocates after the widest shape has warmed the buffers.
        let mut scratch = std::mem::take(&mut self.scratch);
        let mut values = std::mem::take(&mut scratch.values);
        let mut core_args = std::mem::take(&mut scratch.core_args);
        values.clear();
        core_args.clear();
        for arg in args {
            let value = self.frame_get(arg.slot);
            values.push(value.clone());
            core_args.push(super::builtins_core::CoreArg {
                name: arg
                    .name
                    .map(|id| symbols.resolve(id).unwrap_or("?").to_owned()),
                value,
            });
        }
        let value = if namespace == BuiltinNamespace::Core {
            match method_name {
                "int" => super::eval::int_cast(values.first().and_then(|value| value.as_ref()))?,
                "float" => {
                    super::eval::float_cast(values.first().and_then(|value| value.as_ref()))?
                }
                "color" => {
                    super::eval::color_cast(values.first().and_then(|value| value.as_ref()))?
                }
                // Session time and wall-clock parts are pure reads
                // of bar state through the shared tree-walk
                // functions, so values and every refusal agree by
                // construction.
                "time" => {
                    let tick = self
                        .leg(LegKind::Time.index() as usize)
                        .map(PineValue::Float);
                    super::eval::session_time_of(&values, &self.chart_period, tick)?
                }
                "hour" => super::eval::call_wall_part(&values, 0)?,
                "minute" => super::eval::call_wall_part(&values, 1)?,
                // Per-site memory keyed on the interned argument
                // shape through the shared step: same key, same
                // discipline as the tree-walk.
                "fixnan" => {
                    let site = site_text.to_owned();
                    builtins.fixnan(&site, &values)?
                }
                // Legacy three-way choice through the shared leaf, so
                // values and every refusal agree with the tree-walk by
                // construction.
                "iff" => super::eval::iff_of(&values)?,
                _ => {
                    return Err(KernelError::Eval(format!(
                        "unknown core call 'core.{method_name}'"
                    )));
                }
            }
        } else if namespace == BuiltinNamespace::Ta {
            let ta = builtins.ta_mut()?;
            // Stacked site without per-call allocation: the empty
            // stack borrows the interned own-site text, otherwise
            // the maintained prefix and the own site compose into
            // the reused buffer. Same pieces, same order, same
            // separator as the tree-walk join, so keyed kernels
            // observe identical sites in either engine.
            let full_site: &str = if self.call_stack.is_empty() {
                site_text
            } else {
                self.site_scratch.clear();
                self.site_scratch.push_str(&self.stack_prefix);
                self.site_scratch.push('>');
                self.site_scratch.push_str(site_text);
                &self.site_scratch
            };
            let tick = self.tick();
            ta.call(full_site, method_name, &values, tick)?
        } else if namespace == BuiltinNamespace::Timeframe && method_name == "change" {
            // Adjacent-bar bucket change: pure over the two bar
            // times, served through the shared step so both
            // engines name the same refusals.
            super::builtins_core::timeframe_change(
                &values,
                self.cursor,
                self.legs.named("time").unwrap_or(&[]),
            )?
        } else if homes.first().is_some_and(Option::is_some)
            && namespace == BuiltinNamespace::Array
            && method_name == "shift"
        {
            // Statement-position split return: the shrunken array
            // persists through the generic write-back while the
            // dropped value would have been the removed head.
            super::builtins_core::shift_parts(
                "array.shift",
                values.first().and_then(Option::as_ref),
            )?
            .map(|(rest, _head)| PineValue::Array(rest))
        } else if homes.first().is_some_and(Option::is_some)
            && namespace == BuiltinNamespace::Map
            && method_name == "remove"
        {
            super::builtins_core::remove_parts(
                "map.remove",
                values.first().and_then(Option::as_ref),
                values.get(1).and_then(Option::as_ref),
            )?
            .map(|(rest, _old)| PineValue::IntMap(rest))
        } else if namespace == BuiltinNamespace::Drawing {
            // Headless drawing capture: the shared registry owns creation,
            // mutation, reads, deletion, and every refusal. The dispatch
            // member is `<namespace>.<method>` (`label.new`).
            let (family, method) = method_name
                .split_once('.')
                .map_or((method_name, method_name), |(family, method)| {
                    (family, method)
                });
            let Some(kind) = crate::value::DrawingKind::parse(family) else {
                return Err(crate::KernelError::Eval(format!(
                    "unknown drawing family '{family}'"
                )));
            };
            builtins
                .drawing_mut()
                .invoke(kind, method, &core_args, self.completed)?
        } else if namespace == BuiltinNamespace::Alert {
            // Alerts cannot fire headlessly: every argument evaluated above
            // (its refusals stand), and the harness has no delivery path, so
            // the call evaluates to `na` and records nothing.
            None
        } else if namespace == BuiltinNamespace::Strategy {
            // Headless broker capture: the shared harness state owns the
            // event shape, the declaration capital, and every refusal, so
            // the loop records the same dialogue the tree-walk records and
            // never reaches a venue.
            builtins
                .strategy_mut()
                .invoke(self.cursor, method_name, &core_args)?
        } else {
            let resolved_types: Vec<crate::pine::syntax::PineType> = type_args
                .iter()
                .map(|arg| arg.resolve(&program.modules[self.module].symbols))
                .collect();
            super::builtins_core::call(
                namespace.as_str(),
                method_name,
                site_text,
                &core_args,
                &resolved_types,
                ctx.inputs_mut(),
            )?
        };
        // Return the call scratch before the write-back: a refusal above
        // ends the run, so only the live path can reuse the buffers.
        scratch.values = values;
        scratch.core_args = core_args;
        self.scratch = scratch;
        // Statement-position write-back: the compiler records
        // homes only for mutating collection calls, and only the
        // first argument is ever the target — exactly the
        // tree-walk's first-arg rule. The write is unconditional
        // (even `na` clears the binding), unlike the user-call
        // path below, which only persists collections.
        if let Some(Some(home)) = homes.first() {
            self.write_home(*home, value.clone())?;
        }
        if let Some(dst) = dst {
            self.frame_set(dst, value);
        }
        self.pc += 1;
        Ok(())
    }

    /// Serve one headless `request.security` call from the run's injected
    /// tapes. Order and wording mirror the tree-walk's injected path:
    /// `calc_bars_count` evaluates first, then symbol, timeframe, and the
    /// two merge modes; an unregistered pair refuses naming the live fetch.
    #[allow(clippy::too_many_arguments)]
    fn exec_request_security(
        &mut self,
        symbol: SlotId,
        timeframe: SlotId,
        gaps: Option<SlotId>,
        lookahead: Option<SlotId>,
        calc_bars: Option<SlotId>,
        single: bool,
        items: &[super::bytecode::SecurityExpr],
        builtins: &mut RunBuiltins,
    ) -> Result<Option<PineValue>, KernelError> {
        let count = match calc_bars {
            Some(slot) => {
                let given = self.frame_get(slot);
                match super::eval::whole_number("calc_bars_count", given.as_ref()) {
                    Ok(count) if count > 0 => Some(usize::try_from(count).unwrap_or(usize::MAX)),
                    Ok(count) => {
                        return Err(refused(format!(
                            "request.security takes a positive calc_bars_count, got {count}"
                        )));
                    }
                    Err(_) => {
                        return Err(refused(format!(
                            "request.security takes an int calc_bars_count, got {given:?}"
                        )));
                    }
                }
            }
            None => None,
        };
        let symbol = match self.frame_get(symbol) {
            Some(PineValue::Str(symbol)) => symbol,
            other => {
                return Err(refused(format!(
                    "request.security takes a string symbol, got {other:?}"
                )));
            }
        };
        let timeframe = match self.frame_get(timeframe) {
            Some(PineValue::Str(timeframe)) => timeframe,
            other => {
                return Err(refused(format!(
                    "request.security takes a string timeframe, got {other:?}"
                )));
            }
        };
        let gaps = match gaps {
            None => "gaps_off".to_owned(),
            Some(slot) => {
                let given = self.frame_get(slot);
                match given {
                    Some(PineValue::EnumHandle(handle)) if handle.enum_name == "barmerge" => {
                        handle.member
                    }
                    other => {
                        return Err(refused(format!(
                            "request.security takes a barmerge gaps mode, got {other:?}"
                        )));
                    }
                }
            }
        };
        if gaps != "gaps_off" {
            return Err(refused(format!(
                "injected security serves barmerge.gaps_off only, got barmerge.{gaps}"
            )));
        }
        let lookahead = match lookahead {
            None => "lookahead_off".to_owned(),
            Some(slot) => {
                let given = self.frame_get(slot);
                match given {
                    Some(PineValue::EnumHandle(handle)) if handle.enum_name == "barmerge" => {
                        handle.member
                    }
                    other => {
                        return Err(refused(format!(
                            "request.security takes a barmerge lookahead mode, got {other:?}"
                        )));
                    }
                }
            }
        };
        if lookahead != "lookahead_off" {
            return Err(refused(format!(
                "injected security serves barmerge.lookahead_off only, got barmerge.{lookahead}"
            )));
        }
        let dataset = builtins
            .request()
            .security(&symbol, &timeframe)
            .ok_or_else(|| {
                refused(format!(
                    "live request.security {}",
                    super::eval::LIVE_REQUEST_TAIL
                ))
            })?;
        let window = super::eval::serve_security(
            &dataset,
            super::eval::chart_ms(&self.legs, self.cursor, !self.float_allowed),
            count,
        );
        if window.0 == window.1 {
            if single {
                return Ok(None);
            }
            return Ok(Some(PineValue::Array(vec![None; items.len()])));
        }
        let tail = super::eval::SecurityTail::of(&dataset, window, count);
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            out.push(self.eval_security_expr(item, &tail, builtins)?);
        }
        if single {
            return Ok(out.into_iter().next().flatten());
        }
        Ok(Some(PineValue::Array(out)))
    }

    /// Evaluate one compiled dataset-tail expression. Leaves and operators
    /// go through the same shared helpers the tree-walk's AST walker uses,
    /// so both engines answer one dataset identically.
    fn eval_security_expr(
        &mut self,
        expr: &super::bytecode::SecurityExpr,
        tail: &super::eval::SecurityTail<'_>,
        builtins: &mut RunBuiltins,
    ) -> Result<Option<PineValue>, KernelError> {
        use super::bytecode::SecurityExpr;
        let program = self.program_handle();
        let program: &VmProgram = &program;
        match expr {
            SecurityExpr::Leg(name) => {
                let name: &str = program.modules[self.module]
                    .symbols
                    .resolve(*name)
                    .unwrap_or("");
                if tail.column(name).is_none() {
                    return Err(refused(
                        "injected security serves bare legs, kernel-backed single-source ta calls and scalar operators only",
                    ));
                }
                Ok(super::eval::security_leg(tail, name))
            }
            SecurityExpr::Const(value) => {
                let constant = program.modules[self.module].consts.get(*value);
                let symbols = &program.modules[self.module].symbols;
                Self::const_value(constant, symbols)
            }
            SecurityExpr::Call {
                method,
                source,
                chart_args,
            } => {
                let method: &str = program.modules[self.module]
                    .symbols
                    .resolve(*method)
                    .unwrap_or("");
                let mut values: Vec<Option<PineValue>> = Vec::with_capacity(chart_args.len() + 1);
                if let Some(source) = source {
                    let name: &str = program.modules[self.module]
                        .symbols
                        .resolve(*source)
                        .unwrap_or("");
                    values.push(super::eval::security_leg(tail, name));
                }
                for slot in chart_args {
                    values.push(self.frame_get(*slot));
                }
                let history = tail.history();
                if method == "atr" {
                    let closes = history.column("close").expect("served window has close");
                    let bridge = builtins.ta_mut()?;
                    return bridge.batch_security(
                        method,
                        closes,
                        &values,
                        &history.ohlcv(),
                        history.display_count,
                    );
                }
                let Some(source) = source else {
                    return Err(refused(format!("ta.{method} takes a source leg here")));
                };
                let name: &str = program.modules[self.module]
                    .symbols
                    .resolve(*source)
                    .unwrap_or("");
                let column = history
                    .column(name)
                    .expect("source checked when the tail was taken");
                let bridge = builtins.ta_mut()?;
                bridge.batch_security(
                    method,
                    column,
                    &values,
                    &history.ohlcv(),
                    history.display_count,
                )
            }
            SecurityExpr::Binary { op, left, right } => {
                let left = self.eval_security_expr(left, tail, builtins)?;
                let right = self.eval_security_expr(right, tail, builtins)?;
                super::eval::binary(
                    super::eval::binary_op_of(*op),
                    left.as_ref(),
                    right.as_ref(),
                )
            }
        }
    }

    /// Serve one headless `request.security_lower_tf` call: one array per
    /// requested chart leg over the confirmed window, `calc_bars_count`
    /// applied to the tail exactly like the tree-walk.
    fn exec_request_lower_tf(
        &mut self,
        symbol: SlotId,
        timeframe: SlotId,
        legs: &[SymbolId],
        calc_bars: Option<SlotId>,
        builtins: &mut RunBuiltins,
    ) -> Result<Option<PineValue>, KernelError> {
        let count = match calc_bars {
            Some(slot) => {
                let given = self.frame_get(slot);
                match super::eval::whole_number("calc_bars_count", given.as_ref()) {
                    Ok(count) if count > 0 => Some(usize::try_from(count).unwrap_or(usize::MAX)),
                    Ok(count) => {
                        return Err(refused(format!(
                            "request.security_lower_tf takes a positive calc_bars_count, got {count}"
                        )));
                    }
                    Err(_) => {
                        return Err(refused(format!(
                            "request.security_lower_tf takes an int calc_bars_count, got {given:?}"
                        )));
                    }
                }
            }
            None => None,
        };
        let symbol = match self.frame_get(symbol) {
            Some(PineValue::Str(symbol)) => symbol,
            other => {
                return Err(refused(format!(
                    "request.security_lower_tf takes a string symbol, got {other:?}"
                )));
            }
        };
        let timeframe = match self.frame_get(timeframe) {
            Some(PineValue::Str(timeframe)) => timeframe,
            other => {
                return Err(refused(format!(
                    "request.security_lower_tf takes a string timeframe, got {other:?}"
                )));
            }
        };
        let program = self.program_handle();
        let program: &VmProgram = &program;
        let symbols = &program.modules[self.module].symbols;
        let mut wanted = Vec::with_capacity(legs.len());
        for leg in legs {
            let name: &str = symbols.resolve(*leg).unwrap_or("");
            if !matches!(name, "time" | "open" | "high" | "low" | "close" | "volume") {
                return Err(refused(format!(
                    "injected lower-tf has no column for leg '{name}'"
                )));
            }
            wanted.push(name);
        }
        let dataset = builtins
            .request()
            .lower_tf(&symbol, &timeframe)
            .ok_or_else(|| {
                refused(format!(
                    "live request.security_lower_tf {}",
                    super::eval::LIVE_REQUEST_TAIL
                ))
            })?;
        let window = super::eval::serve_security(
            &dataset,
            super::eval::chart_ms(&self.legs, self.cursor, !self.float_allowed),
            count,
        );
        let columns = wanted
            .iter()
            .map(|leg| {
                Some(PineValue::Array(super::eval::security_column(
                    &dataset, leg, window,
                )))
            })
            .collect();
        Ok(Some(PineValue::Array(columns)))
    }

    /// Convert one constant-pool entry to a runtime value. Shared by the
    /// `Const` instruction and the dataset-tail plan so a literal cannot
    /// mean two things.
    fn const_value(
        constant: Option<&super::bytecode::ConstValue>,
        symbols: &super::bytecode::SymbolTable,
    ) -> Result<Option<PineValue>, KernelError> {
        use super::bytecode::ConstValue;
        Ok(match constant {
            Some(ConstValue::Float(v)) => Some(PineValue::Float(*v)),
            Some(ConstValue::Int(v)) => Some(PineValue::Int(*v)),
            Some(ConstValue::Bool(v)) => Some(PineValue::Bool(*v)),
            Some(ConstValue::Str(text)) => Some(PineValue::Str(text.clone())),
            Some(ConstValue::Color(packed)) => Some(PineValue::ColorHandle(
                crate::value::ColorHandle::new(*packed),
            )),
            Some(ConstValue::Enum { namespace, member }) => {
                let namespace = symbols.resolve(*namespace).unwrap_or("?");
                let member = symbols.resolve(*member).unwrap_or("?");
                Some(PineValue::EnumHandle(crate::value::EnumHandle::new(
                    namespace, member,
                )))
            }
            // The `na` literal reads as absence, exactly like the
            // tree-walk's unwritten temp.
            Some(ConstValue::Na) => None,
            None => {
                return Err(KernelError::Eval(
                    "bytecode const id out of range".to_owned(),
                ));
            }
        })
    }

    /// Execute one call: builtins delegate to the shared bridges with the
    /// precomputed site; user functions open a frame, bind positionally
    /// (the compiler normalized named/defaulted arguments), enforce the
    /// depth bound, and write back collection homes on return. Arguments
    /// arrive by slice and names resolve through the caller table.
    #[allow(clippy::too_many_arguments)]
    fn exec_call(
        &mut self,
        program: &VmProgram,
        dst: Option<SlotId>,
        target: &super::bytecode::CallTarget,
        args: &[super::bytecode::CallArgSlot],
        homes: &[Option<SymbolId>],
        type_args: &[CallTypeArg],
        site: SymbolId,
        ctx: &mut RunCtx<'_>,
        builtins: &mut RunBuiltins,
    ) -> Result<(), KernelError> {
        use super::bytecode::CallTarget;
        // `program` rides in from the block loop's single handle: no
        // refcount traffic per call, and table borrows below never
        // conflict with execution.
        let symbols = &program.modules[self.module].symbols;
        match target {
            CallTarget::Builtin { namespace, method } => self.exec_builtin_call(
                program, dst, *namespace, *method, args, homes, type_args, site, ctx, builtins,
            ),
            CallTarget::Block(index) => {
                let module = self.module;
                self.exec_user_call(program, module, *index, args, homes, site, dst, ctx, builtins)
            }
            CallTarget::Resolved { module, block } => self.exec_user_call(
                program,
                *module as usize,
                *block as usize,
                args,
                homes,
                site,
                dst,
                ctx,
                builtins,
            ),
            CallTarget::Named { module, name } => {
                let callee = symbols.resolve(*name).unwrap_or("?");
                let target_key: &str = match module {
                    Some(id) => symbols.resolve(*id).unwrap_or("?"),
                    None => program.modules[self.module].key.as_str(),
                };
                let position = match program.module_index.get(target_key) {
                    Some(position) => *position,
                    None => {
                        // Unresolvable named target at run time: the
                        // linker validates every named target up front,
                        // so reaching here means the program was linked
                        // against modules that are no longer there.
                        // Fail closed naming the target, never guess.
                        return Err(KernelError::Eval(format!(
                            "unknown import target '{target_key}/{callee}'"
                        )));
                    }
                };
                let index = *program.modules[position]
                    .functions
                    .get(callee)
                    .ok_or_else(|| {
                        KernelError::Eval(format!("unknown import target '{target_key}/{callee}'"))
                    })?;
                self.exec_user_call(
                    program, position, index, args, homes, site, dst, ctx, builtins,
                )
            }
        }
    }

    /// Execute one user function: fresh (pooled) frame, positional binding
    /// (the compiler normalized named/defaulted arguments), depth bound,
    /// collection write-back to caller homes.
    #[allow(clippy::too_many_arguments)]
    fn exec_user_call(
        &mut self,
        program: &VmProgram,
        module: usize,
        block: usize,
        args: &[super::bytecode::CallArgSlot],
        homes: &[Option<SymbolId>],
        site: SymbolId,
        dst: Option<SlotId>,
        ctx: &mut RunCtx<'_>,
        builtins: &mut RunBuiltins,
    ) -> Result<(), KernelError> {
        if self.call_stack.len() >= MAX_CALL_DEPTH {
            return Err(super::eval::call_depth_over_limit());
        }
        let frame_slots = program.modules[module].blocks[block].slots as usize;
        let params_len = program.modules[module].blocks[block].params.len();
        // Reuse the run's argument buffer: a call costs no allocation once
        // the widest call shape has been seen.
        let mut values = std::mem::take(&mut self.scratch.values);
        values.clear();
        values.extend(args.iter().map(|arg| self.frame_get(arg.slot)));
        let mut frame = self.frame_take(frame_slots);
        let bound = ArgBinder::bind_frame("block", block, &mut frame.slots, 0, params_len, &values);
        self.scratch.values = values;
        bound?;
        // The stack owns its entries: resolve the site once per call.
        let site_text = program.modules[self.module]
            .symbols
            .resolve(site)
            .unwrap_or("?")
            .to_owned();
        // Extend the maintained join before pushing: truncate marks pair
        // with stack entries, so a return restores the caller prefix
        // without rebuilding it.
        self.prefix_lens.push(self.stack_prefix.len());
        if !self.stack_prefix.is_empty() {
            self.stack_prefix.push('>');
        }
        self.stack_prefix.push_str(&site_text);
        self.call_stack.push(site_text);
        let caller = std::mem::replace(&mut self.module, module);
        let caller_block = std::mem::replace(&mut self.block, block);
        let caller_pc = std::mem::replace(&mut self.pc, 0);
        self.frames.push(frame);
        // Open the per-call journal suffix and record the caller for
        // home resolution: nested calls compose exactly like the
        // tree-walk's journal stack, and only this suffix reverts.
        let outer_base = self.journal_base.replace(self.journal.len());
        // The caller frame is the new top's predecessor: pops only
        // remove the top, so this index stays valid for the call.
        // Always pushed (balanced by the pop below) even where no
        // caller frame exists; resolution simply skips it then.
        let caller_frame = self.frames.len().saturating_sub(2);
        self.callers.push((caller, caller_block, caller_frame));
        // Per-instance `var` identity extends the same way: the caller
        // site (module, block, pc) names the path prefix, so recursion
        // gets fresh cells per activation exactly like the tree-walk's
        // call-site keying.
        self.var_path.push((caller, caller_block, caller_pc));
        let result = self.exec_block(ctx, builtins);
        // Record the body's local history while the frame is live.
        self.record_local_history(module, block);
        // Capture each parameter's final cell before the frame recycles:
        // slots open with the bound arguments in parameter order, so
        // `finals[i]` is parameter `i`'s value at return.
        let mut finals = Vec::with_capacity(params_len);
        if let Some(frame) = self.frames.pop() {
            finals.extend(frame.slots.iter().take(params_len).cloned());
            self.frame_return(frame);
        }
        // Frame teardown: journaled body writes revert in reverse order
        // first (restoring caller homes to entry state), mirroring the
        // tree-walk's journal teardown; the held write-back harvest
        // below then persists collection results, also in tree order.
        if let Some(base) = self.journal_base {
            while self.journal.len() > base {
                let logged = self.journal.pop().expect("length checked");
                match logged.home {
                    JournalHome::Global(column) => {
                        if column < self.current.len() {
                            self.current[column].clone_from(&logged.old);
                        }
                    }
                    JournalHome::CallerFrame { frame, slot } => {
                        if let Some(frame) = self.frames.get_mut(frame) {
                            if slot < frame.slots.len() {
                                frame.slots[slot] = logged.old.clone().flatten();
                            }
                        }
                    }
                }
            }
        }
        self.journal_base = outer_base;
        self.callers.pop();
        self.var_path.pop();
        self.module = caller;
        self.block = caller_block;
        self.pc = caller_pc + 1;
        self.call_stack.pop();
        if let Some(len) = self.prefix_lens.pop() {
            self.stack_prefix.truncate(len);
        }
        let value = result?;
        // Write-back: each parameter's final collection value flows back
        // to its own caller home — mirroring the tree-walk, which writes
        // back per-parameter finals (never the return value, never across
        // parameters). `homes[i]` is the caller variable bound to
        // parameter `i` (`None` for defaulted or non-variable arguments,
        // which never persist).
        for (final_, home) in finals.iter().zip(homes.iter()) {
            if let (Some(written), Some(home)) = (final_, home) {
                if written.is_collection_writeback() {
                    self.write_home(*home, Some(written.clone()))?;
                }
            }
        }
        if let Some(dst) = dst {
            self.frame_set(dst, value);
        }
        Ok(())
    }


    /// Take one method receiver out of its home. Variable roots follow the
    /// engine's take order (live alias, caller frame, per-instance cell,
    /// shared column); nested paths and map-entry aliases then ride the
    /// shared home model, so both engines rebuild the same chain and
    /// persist the same entry. `None` means the path holds no object
    /// (unknown name, unknown field, non-object value); taken pieces
    /// restore before declining, so a failed take never unbinds.
    fn take_method_home(
        &mut self,
        home: &super::bytecode::CallHome,
        journal: bool,
    ) -> Result<Option<(UserInstance, super::home::Home<VmSlot>)>, KernelError> {
        use super::bytecode::CallHome;
        let program = self.program_handle();
        let program: &VmProgram = &program;
        let symbols = &program.modules[self.module].symbols;
        match home {
            CallHome::Innermost => {
                let depth = self
                    .receiver
                    .len()
                    .checked_sub(1)
                    .ok_or_else(super::eval::this_unbound)?;
                let instance = self.receiver[depth].instance.clone();
                Ok(Some((
                    instance,
                    super::home::Home::Slot(VmSlot::Receiver(depth)),
                )))
            }
            CallHome::ThisField { field } => {
                let field_text = symbols.resolve(*field).unwrap_or("").to_owned();
                let depth = self
                    .receiver
                    .len()
                    .checked_sub(1)
                    .ok_or_else(super::eval::this_unbound)?;
                let taken = self.receiver[depth]
                    .instance
                    .fields
                    .get_mut(&field_text)
                    .map(Option::take);
                match taken {
                    Some(Some(PineValue::Object(instance))) => {
                        Ok(Some((instance, super::home::Home::ThisField(field_text))))
                    }
                    other => {
                        self.receiver[depth]
                            .instance
                            .fields
                            .insert(field_text, other.flatten());
                        Ok(None)
                    }
                }
            }
            CallHome::Var { name, var_ordinal } => {
                let name_text = symbols.resolve(*name).unwrap_or("").to_owned();
                let Some((slot, instance)) = self.take_slot(&name_text, *var_ordinal, journal)
                else {
                    return Ok(None);
                };
                let root = self.prov_home(&name_text, super::home::Home::Slot(slot));
                Ok(Some((instance, root)))
            }
            CallHome::VarPath {
                name,
                var_ordinal,
                path,
            } => {
                let name_text = symbols.resolve(*name).unwrap_or("").to_owned();
                let Some((slot, instance)) = self.take_slot(&name_text, *var_ordinal, journal)
                else {
                    return Ok(None);
                };
                let root = self.prov_home(&name_text, super::home::Home::Slot(slot));
                let fields: Vec<String> = path
                    .iter()
                    .map(|field| symbols.resolve(*field).unwrap_or("").to_owned())
                    .collect();
                super::home::take_nested(self, root, instance, &fields)
            }
        }
    }

    /// Resolve and take one variable root in the engine's take order:
    /// live receiver alias, caller frame slot, per-instance cell, shared
    /// column. Frame and column takes log their entry value for the return
    /// revert when `journal` is set.
    fn take_slot(
        &mut self,
        name_text: &str,
        var_ordinal: Option<u32>,
        journal: bool,
    ) -> Option<(VmSlot, UserInstance)> {
        let program = self.program_handle();
        let program: &VmProgram = &program;
        // Live alias first, exactly like the tree-walk's receiver search.
        if let Some(depth) = self
            .receiver
            .iter()
            .rposition(|slot| slot.alias == name_text)
        {
            let instance = self.receiver[depth].instance.clone();
            return Some((VmSlot::Receiver(depth), instance));
        }
        // Named slots of the body under execution (parameters and locals)
        // shadow outer names, exactly like the tree-walk's frame namespace.
        if let Some(slot) = program.modules[self.module].blocks[self.block]
            .frame_names
            .get(name_text)
        {
            let slot = *slot as usize;
            let value = self
                .frames
                .last()
                .and_then(|frame| frame.slots.get(slot).cloned())
                .flatten();
            if let Some(PineValue::Object(instance)) = value {
                let frame = self.frames.len().saturating_sub(1);
                self.take_frame_slot(frame, slot, journal);
                return Some((VmSlot::Frame { frame, slot }, instance));
            }
            // A named but non-object local shadows every outer home.
            return None;
        }
        // Caller frame slots (parameters and body-bound locals).
        if let Some(&(caller_module, caller_block, caller_frame)) = self.callers.last() {
            let caller = &program.modules[caller_module];
            if caller.blocks.len() > caller_block {
                if let Some(slot) = caller.blocks[caller_block].frame_names.get(name_text) {
                    let slot = *slot as usize;
                    let value = self
                        .frames
                        .get(caller_frame)
                        .and_then(|frame| frame.slots.get(slot).cloned())
                        .flatten();
                    if let Some(PineValue::Object(instance)) = value {
                        self.take_frame_slot(caller_frame, slot, journal);
                        return Some((
                            VmSlot::Frame {
                                frame: caller_frame,
                                slot,
                            },
                            instance,
                        ));
                    }
                    return None;
                }
            }
        }
        // Per-instance cells by declaration ordinal.
        if let Some(ordinal) = var_ordinal {
            let key = VarInstKey {
                path: self.var_path.clone(),
                home: (self.module, self.block, ordinal),
            };
            let cell = self.frame_vars.get(&key)?;
            if let Some(PineValue::Object(instance)) = cell.value.clone() {
                let is_varip = cell.is_varip;
                self.frame_vars.remove(&key);
                return Some((
                    VmSlot::Cell {
                        key,
                        is_varip,
                        name: name_text.to_owned(),
                    },
                    instance,
                ));
            }
            return None;
        }
        // Shared columns last, mirroring the single-namespace take.
        if let Some(column) = program.modules[self.module].globals.get(name_text) {
            let column = *column;
            let value = self.current.get(column).cloned().flatten().flatten();
            if let Some(PineValue::Object(instance)) = value {
                if journal {
                    let base = self.journal_base.unwrap_or(0);
                    let logged = self.journal[base..]
                        .iter()
                        .any(|logged| logged.same_home(&JournalHome::Global(column)));
                    if !logged {
                        let old = self.current.get(column).cloned().flatten();
                        self.journal.push(JournalEntry {
                            home: JournalHome::Global(column),
                            old,
                        });
                    }
                }
                if column < self.current.len() {
                    self.current[column] = None;
                }
                return Some((VmSlot::Column(column), instance));
            }
        }
        None
    }

    /// Source spelling of one home: the receiver for `this` calls, the
    /// dotted path for nested receivers, the variable name otherwise. Used
    /// only in refusal text, so both engines name the same target.
    fn home_display(&self, home: &super::bytecode::CallHome) -> String {
        use super::bytecode::CallHome;
        let program = self.program_handle();
        let program: &VmProgram = &program;
        let symbols = &program.modules[self.module].symbols;
        match home {
            CallHome::Var { name, .. } => symbols.resolve(*name).unwrap_or("?").to_owned(),
            CallHome::Innermost => "this".to_owned(),
            CallHome::VarPath { name, path, .. } => {
                let mut text = symbols.resolve(*name).unwrap_or("?").to_owned();
                for field in path {
                    text.push('.');
                    text.push_str(symbols.resolve(*field).unwrap_or("?"));
                }
                text
            }
            CallHome::ThisField { field } => {
                format!("this.{}", symbols.resolve(*field).unwrap_or("?"))
            }
        }
    }

    /// Attach map-entry provenance to a taken home: a variable tagged by a
    /// `map.get` assignment writes its mutated instance back to the entry
    /// as well as the variable.
    fn prov_home(&self, name: &str, home: super::home::Home<VmSlot>) -> super::home::Home<VmSlot> {
        match self.map_prov.get(name) {
            Some((map_var, key, key_value)) => super::home::Home::MapEntry {
                inner: Box::new(home),
                map_var: map_var.clone(),
                key: key.clone(),
                key_value: key_value.clone(),
            },
            None => home,
        }
    }

    /// Remove one caller frame slot known to hold an object, logging its
    /// entry value once per call for the return revert unless the caller
    /// completes take and put-back in one statement. Returns the logged
    /// entry.
    // Three states: unlogged, logged-absence, logged-value. The nesting
    // is the journal contract (mirrors `JournalEntry::old`).
    #[allow(clippy::option_option)]
    fn take_frame_slot(
        &mut self,
        frame: usize,
        slot: usize,
        journal: bool,
    ) -> Option<Option<PineValue>> {
        let base = self.journal_base.unwrap_or(0);
        let target = JournalHome::CallerFrame { frame, slot };
        let logged = self.journal[base..]
            .iter()
            .any(|logged| logged.same_home(&target));
        let old = self
            .frames
            .get(frame)
            .and_then(|frame| frame.slots.get(slot).cloned())
            .flatten();
        if journal && !logged {
            self.journal.push(JournalEntry {
                home: target,
                old: old.clone().map(Some),
            });
        }
        if let Some(frame) = self.frames.get_mut(frame) {
            if slot < frame.slots.len() {
                frame.slots[slot] = None;
            }
        }
        old.map(Some)
    }

    /// Read one method-alias name with no live receiver slot: plain
    /// frame bindings first, then the shared column, then the
    /// per-instance cell — mirroring the tree-walk's namespace read
    /// once aliases are out of the picture. Truly unbound names refuse
    /// naming the variable.
    fn alias_fallback(
        &self,
        name: &str,
        var_ordinal: Option<u32>,
    ) -> Result<Option<PineValue>, KernelError> {
        let program = self.program_handle();
        let program: &VmProgram = &program;
        let caller = &program.modules[self.module];
        if let Some(slot) = caller.blocks[self.block].frame_names.get(name) {
            if let Some(cell) = self
                .frames
                .last()
                .and_then(|frame| frame.slots.get(*slot as usize))
            {
                return Ok(cell.clone());
            }
        }
        if let Some(column) = caller.globals.get(name) {
            if let Some(cell) = self.current.get(*column) {
                return Ok(cell.clone().flatten());
            }
        }
        if let Some(ordinal) = var_ordinal {
            let key = VarInstKey {
                path: self.var_path.clone(),
                home: (self.module, self.block, ordinal),
            };
            if self.var_seeded.contains(&key) {
                return Ok(self
                    .frame_vars
                    .get(&key)
                    .and_then(|cell| cell.value.clone()));
            }
        }
        Err(super::eval::undefined_variable(name))
    }

    /// Read one method-alias field with no live receiver slot: shared
    /// dotted bindings answer first, then the head instance reads
    /// through its table — mirroring the tree-walk's namespace-first
    /// order. Anything else refuses naming the full path, exactly like
    /// the tree-walk.
    fn alias_dotted_fallback(
        &self,
        name: &str,
        field: &str,
        var_ordinal: Option<u32>,
    ) -> Result<Option<PineValue>, KernelError> {
        let program = self.program_handle();
        let program: &VmProgram = &program;
        let caller = &program.modules[self.module];
        let dotted = format!("{name}.{field}");
        if let Some(slot) = caller.blocks[self.block].frame_names.get(&dotted) {
            if let Some(cell) = self
                .frames
                .last()
                .and_then(|frame| frame.slots.get(*slot as usize))
            {
                return Ok(cell.clone());
            }
        }
        if let Some(column) = caller.globals.get(&dotted) {
            if let Some(cell) = self.current.get(*column) {
                return Ok(cell.clone().flatten());
            }
        }
        let head = self.alias_head_value(name, var_ordinal);
        match head {
            Some(PineValue::Object(instance)) => match instance.fields.get(field) {
                Some(bound) => Ok(bound.clone()),
                None => Err(super::eval::unknown_dotted_field(name, field)),
            },
            _ => Err(super::eval::undefined_variable(&dotted)),
        }
    }

    /// Resolve one alias head with no live receiver slot: the current
    /// frame, then the shared column, then the per-instance cell —
    /// mirroring the tree-walk's namespace read once aliases are out of
    /// the picture.
    fn alias_head_value(&self, name: &str, var_ordinal: Option<u32>) -> Option<PineValue> {
        let program = self.program_handle();
        let program: &VmProgram = &program;
        let caller = &program.modules[self.module];
        if let Some(slot) = caller.blocks[self.block].frame_names.get(name) {
            if let Some(cell) = self
                .frames
                .last()
                .and_then(|frame| frame.slots.get(*slot as usize))
            {
                return cell.clone();
            }
        }
        if let Some(column) = caller.globals.get(name) {
            if let Some(cell) = self.current.get(*column) {
                return cell.clone().flatten();
            }
        }
        if let Some(ordinal) = var_ordinal {
            let key = VarInstKey {
                path: self.var_path.clone(),
                home: (self.module, self.block, ordinal),
            };
            if self.var_seeded.contains(&key) {
                return self
                    .frame_vars
                    .get(&key)
                    .and_then(|cell| cell.value.clone());
            }
        }
        None
    }

    /// Invoke one instance method: take the receiver out of its caller
    /// home, dispatch on its runtime type, bind the receiver-skipped
    /// parameters, run the lowered body under the frame protocol, and
    /// write the mutated copy back. Rvalue calls drop the mutation and
    /// keep only the return. Parameter write-backs harvest before the
    /// receiver write-back, mirroring the tree-walk's teardown order.
    #[allow(clippy::too_many_arguments)]
    fn exec_method_call(
        &mut self,
        program: &VmProgram,
        dst: Option<SlotId>,
        recv: SlotId,
        method: SymbolId,
        args: &[super::bytecode::CallArgSlot],
        homes: &[Option<SymbolId>],
        args_debug: ConstId,
        home: Option<&super::bytecode::CallHome>,
        stmt: bool,
        ctx: &mut RunCtx<'_>,
        builtins: &mut RunBuiltins,
    ) -> Result<(), KernelError> {
        if self.call_stack.len() >= MAX_CALL_DEPTH {
            return Err(super::eval::call_depth_over_limit());
        }
        let method_name: &str = program.modules[self.module]
            .symbols
            .resolve(method)
            .unwrap_or("?");
        let args_text: String = match program.modules[self.module].consts.get(args_debug) {
            Some(super::bytecode::ConstValue::Str(text)) => text.clone(),
            _ => String::new(),
        };
        // Take the receiver out of its caller home first (rvalues
        // evaluate in place with no home to write back to). A take miss
        // reports the unknown method with the caller spelling, exactly
        // like the tree-walk.
        let (instance, taken_home) = match home {
            None => match self.frame_get(recv) {
                Some(PineValue::Object(instance)) => (instance, None),
                Some(other) => {
                    return Err(super::eval::method_needs_object(method_name, &other));
                }
                None => return Err(super::eval::method_called_on_na(method_name)),
            },
            Some(home) => {
                let namespace = self.home_display(home);
                if let Some((instance, taken)) = self.take_method_home(home, true)? {
                    (instance, Some(taken))
                } else {
                    // Collection receiver (`a.size()`,
                    // `w.items.push(x)`): the takes above only serve
                    // objects, so a collection dispatches to its bridge
                    // with the evaluated receiver first. Reads serve;
                    // statement-position mutators write back through
                    // the home — exactly like the tree-walk.
                    if self
                        .exec_collection_call(
                            Some(home),
                            recv,
                            method_name,
                            args,
                            dst,
                            &args_text,
                            stmt,
                            ctx,
                        )?
                        .is_some()
                    {
                        return Ok(());
                    }
                    return Err(super::eval::unknown_method_on(&namespace, method_name));
                }
            }
        };
        // Runtime type-test dispatch against the link-time target set.
        let type_name = instance.type_name.clone();
        let resolved = match program
            .method_index
            .get(&(type_name.clone(), method_name.to_owned()))
        {
            Some(target) => target.clone(),
            None => return Err(super::eval::unknown_method_on(&type_name, method_name)),
        };
        let (target_module, target_block) = (resolved.module, resolved.entry.block);
        let (params, receiver_alias) = (resolved.entry.params, resolved.entry.receiver);
        // Bind named, then positional, then literal defaults — the
        // tree-walk's exact order. Computed defaults arrive already
        // evaluated by name (the compiler fills unanimous ones at the
        // call site); missing non-defaulted parameters refuse, and an
        // unfilled computed one (impossible from the compiler's own
        // calls) refuses like a missing argument rather than guessing
        // a value.
        let mut values: Vec<Option<PineValue>> = Vec::with_capacity(params.len());
        let mut bound_homes: Vec<Option<SymbolId>> = Vec::with_capacity(params.len());
        let mut arg_used = vec![false; args.len()];
        for (param, default) in &params {
            if let Some(index) = args.iter().position(|arg| {
                arg.name.as_ref().is_some_and(|name| {
                    program.modules[self.module]
                        .symbols
                        .resolve(*name)
                        .unwrap_or("?")
                        == param.as_str()
                })
            }) {
                arg_used[index] = true;
                let arg = &args[index];
                values.push(self.frame_get(arg.slot));
                bound_homes.push(homes.get(index).copied().flatten());
                continue;
            }
            let mut filled = None;
            for (index, arg) in args.iter().enumerate() {
                if !arg_used[index] && arg.name.is_none() {
                    arg_used[index] = true;
                    filled = Some(index);
                    break;
                }
            }
            if let Some(index) = filled {
                let arg = &args[index];
                values.push(self.frame_get(arg.slot));
                bound_homes.push(homes.get(index).copied().flatten());
                continue;
            }
            match default {
                Some(id) => {
                    let value = match program.modules[target_module].consts.get(*id) {
                        Some(super::bytecode::ConstValue::Float(v)) => Some(PineValue::Float(*v)),
                        Some(super::bytecode::ConstValue::Int(v)) => Some(PineValue::Int(*v)),
                        Some(super::bytecode::ConstValue::Bool(v)) => Some(PineValue::Bool(*v)),
                        Some(super::bytecode::ConstValue::Str(text)) => {
                            Some(PineValue::Str(text.clone()))
                        }
                        Some(super::bytecode::ConstValue::Color(packed)) => {
                            Some(PineValue::ColorHandle(ColorHandle::new(*packed)))
                        }
                        // Folded enum defaults (defining-scope members):
                        // the handle is static, so the loop serves the
                        // same value the tree-walk binds caller-side. A
                        // folded `na` fills absence through the shared
                        // fallthrough below, exactly like an unmapped
                        // constant.
                        Some(super::bytecode::ConstValue::Enum { namespace, member }) => {
                            let symbols = &program.modules[target_module].symbols;
                            let namespace = symbols.resolve(*namespace).unwrap_or("?");
                            let member = symbols.resolve(*member).unwrap_or("?");
                            Some(PineValue::EnumHandle(crate::value::EnumHandle::new(
                                namespace, member,
                            )))
                        }
                        _ => None,
                    };
                    values.push(value);
                    bound_homes.push(None);
                }
                // Unfilled non-literal defaults cannot evaluate on the
                // loop: their calls keep the fallback at compile time, so
                // reaching one here names the missing parameter instead
                // of guessing a value.
                None => return Err(super::eval::method_arg_missing(param)),
            }
        }
        for (index, arg) in args.iter().enumerate() {
            if arg_used[index] {
                continue;
            }
            if arg.name.is_some() {
                let given = arg.name.map_or("?", |id| {
                    program.modules[self.module]
                        .symbols
                        .resolve(id)
                        .unwrap_or("?")
                });
                return Err(super::eval::method_unknown_argument(given));
            }
            return Err(super::eval::method_too_many_positionals());
        }
        // Call-site identity composes the receiver type with the
        // precomputed argument rendering — byte-identical to the
        // tree-walk's stacked sites, so keyed state (ta kernels,
        // call-site vars) partitions identically in either engine.
        let site_text = format!("{type_name}.{method_name}@{args_text}");
        self.prefix_lens.push(self.stack_prefix.len());
        if !self.stack_prefix.is_empty() {
            self.stack_prefix.push('>');
        }
        self.stack_prefix.push_str(&site_text);
        self.call_stack.push(site_text);
        let caller = std::mem::replace(&mut self.module, target_module);
        let caller_block = std::mem::replace(&mut self.block, target_block);
        let caller_pc = std::mem::replace(&mut self.pc, 0);
        let frame_slots = program.modules[target_module].blocks[target_block].slots as usize;
        let mut frame = self.frame_take(frame_slots);
        // The receiver occupies slot 0; the binder owns the offset rule so
        // no call path can drift by one slot.
        ArgBinder::bind_frame(
            "method block",
            target_block,
            &mut frame.slots,
            1,
            values.len(),
            &values,
        )?;
        self.frames.push(frame);
        let outer_base = self.journal_base.replace(self.journal.len());
        let caller_frame = self.frames.len().saturating_sub(2);
        self.callers.push((caller, caller_block, caller_frame));
        self.var_path.push((caller, caller_block, caller_pc));
        // `this` binds the live receiver slot (never a frame binding),
        // with the declared alias for nested reads.
        self.receiver.push(ReceiverSlot {
            instance,
            alias: receiver_alias,
        });
        let result = self.exec_block(ctx, builtins);
        // Record the body's local history while the frame is live.
        self.record_local_history(target_module, target_block);
        // Capture each parameter's final cell before the frame recycles:
        // bound parameters start at slot one (slot zero stays empty —
        // `this` lives on the receiver stack, never in a frame slot).
        let mut finals = Vec::with_capacity(params.len());
        if let Some(frame) = self.frames.pop() {
            finals.extend(frame.slots.iter().skip(1).take(params.len()).cloned());
            self.frame_return(frame);
        }
        // Frame teardown: journaled body writes revert in reverse order
        // first (restoring caller homes to entry state), mirroring the
        // tree-walk's journal teardown; write-backs below then persist.
        if let Some(base) = self.journal_base {
            while self.journal.len() > base {
                let logged = self.journal.pop().expect("length checked");
                match logged.home {
                    JournalHome::Global(column) => {
                        if column < self.current.len() {
                            self.current[column].clone_from(&logged.old);
                        }
                    }
                    JournalHome::CallerFrame { frame, slot } => {
                        if let Some(frame) = self.frames.get_mut(frame) {
                            if slot < frame.slots.len() {
                                frame.slots[slot] = logged.old.clone().flatten();
                            }
                        }
                    }
                }
            }
        }
        self.journal_base = outer_base;
        self.callers.pop();
        self.var_path.pop();
        let back = self.receiver.pop().expect("receiver pushed above");
        self.module = caller;
        self.block = caller_block;
        self.pc = caller_pc + 1;
        self.call_stack.pop();
        if let Some(len) = self.prefix_lens.pop() {
            self.stack_prefix.truncate(len);
        }
        let value = result?;
        // Write-backs run unjournaled (held): parameter collections
        // persist first, then the mutated receiver returns to its home —
        // mirroring the tree-walk's teardown order.
        for (final_, home) in finals.iter().zip(bound_homes.iter()) {
            if let (Some(written), Some(home)) = (final_, home) {
                if written.is_collection_writeback() {
                    self.write_home(*home, Some(written.clone()))?;
                }
            }
        }
        if let Some(taken) = taken_home {
            self.write_method_home(&taken, back.instance)?;
        }
        if let Some(dst) = dst {
            self.frame_set(dst, value);
        }
        Ok(())
    }

    /// Put one mutated receiver back through its home: the shared model
    /// rebuilds nested chains outward, persists a map entry first, and
    /// stores the root slot.
    fn write_method_home(
        &mut self,
        home: &super::home::Home<VmSlot>,
        receiver: UserInstance,
    ) -> Result<(), KernelError> {
        home.put(self, receiver)
    }

    /// Collection receiver dispatch for method calls: the object takes
    /// above only serve instances, so an array/matrix/map receiver runs
    /// its namespace bridge with the evaluated receiver first — mirroring
    /// the tree-walk's collection path argument for argument (receiver
    /// first, then call order; the site renders the same
    /// `{bridge}.{method}` key). Statement-position mutators write back
    /// through the home with split-aware stores; value reads leave the
    /// holder untouched. Returns `None` when the receiver is no
    /// collection (the caller keeps its unknown-method refusal).
    #[allow(clippy::too_many_arguments)]
    fn exec_collection_call(
        &mut self,
        home: Option<&super::bytecode::CallHome>,
        recv: SlotId,
        method_name: &str,
        args: &[super::bytecode::CallArgSlot],
        dst: Option<SlotId>,
        site: &str,
        stmt: bool,
        ctx: &mut RunCtx<'_>,
    ) -> Result<Option<()>, KernelError> {
        // The receiver reads through its home, not the `recv` temp: the
        // nested routes leave that temp unwritten (the take reads through
        // the home instead), so the home is the authoritative value on
        // every route. Rvalues (no home) read the evaluated temp.
        let receiver = match home {
            Some(home) => Self::read_home_value(self, home),
            None => self.frame_get(recv),
        };
        let bridge = match &receiver {
            Some(PineValue::Array(_)) => "array",
            Some(PineValue::Matrix(_)) => "matrix",
            Some(PineValue::IntMap(_) | PineValue::Map(_)) => "map",
            _ => return Ok(None),
        };
        let program = self.program_handle();
        let program: &VmProgram = &program;
        let mut core_args = Vec::with_capacity(args.len() + 1);
        core_args.push(super::builtins_core::CoreArg {
            name: None,
            value: receiver.clone(),
        });
        for arg in args {
            let name = arg.name.map(|id| {
                program.modules[self.module]
                    .symbols
                    .resolve(id)
                    .unwrap_or("?")
                    .to_owned()
            });
            core_args.push(super::builtins_core::CoreArg {
                name,
                value: self.frame_get(arg.slot),
            });
        }
        let value = super::builtins_core::call(
            bridge,
            method_name,
            site,
            &core_args,
            &[],
            ctx.inputs_mut(),
        )?;
        if stmt {
            if let Some(home) = home {
                if super::eval::writes_back_collection(bridge, method_name) {
                    let key = if bridge == "map" && method_name == "remove" {
                        args.first().map(|arg| self.frame_get(arg.slot))
                    } else {
                        None
                    };
                    let stored = super::builtins_core::split_rest(
                        bridge,
                        method_name,
                        receiver.as_ref(),
                        key.as_ref().and_then(|slot| slot.as_ref()),
                    )?
                    .unwrap_or(value.clone());
                    if !self.put_home_value(home, stored) {
                        return Ok(None);
                    }
                }
            }
        }
        if let Some(dst) = dst {
            self.frame_set(dst, value);
        }
        self.pc += 1;
        Ok(Some(()))
    }

/// Read one method-home receiver value through its home without
/// taking: the nested routes leave the `recv` temp unwritten, so the
/// home chain is the authoritative read on every route. Mirrors the take
/// order; `None` on any miss.
fn read_home_value(run: &Run, home: &super::bytecode::CallHome) -> Option<PineValue> {
    use super::bytecode::CallHome;
    let program = run.program_handle();
    let program: &VmProgram = &program;
    let text = |id: super::bytecode::SymbolId| {
        program.modules[run.module]
            .symbols
            .resolve(id)
            .unwrap_or("")
            .to_owned()
    };
    match home {
        CallHome::Innermost => None,
        CallHome::ThisField { field } => {
            let field = text(*field);
            run.receiver
                .last()
                .and_then(|slot| slot.instance.fields.get(&field))
                .cloned()
                .flatten()
        }
        CallHome::Var { name, var_ordinal } => run.read_root_value(&text(*name), *var_ordinal),
        CallHome::VarPath {
            name,
            var_ordinal,
            path,
        } => {
            let name_text = text(*name);
            let fields: Vec<String> = path.iter().map(|field| text(*field)).collect();
            let root = run.read_root_value(&name_text, *var_ordinal)?;
            let mut current = &root;
            for field in &fields {
                let PineValue::Object(instance) = current else {
                    return None;
                };
                let next = instance.fields.get(field).and_then(|slot| slot.as_ref())?;
                current = next;
            }
            Some(current.clone())
        }
    }
}

/// Set one leaf through an object chain, creating nothing: every level
/// must already hold an object with the named field, exactly like the
/// tree-walk's chain walk. Returns `false` on any miss. Recursion (not a
/// loop) threads the mutable borrow through the chain levels.
fn set_nested_value(current: &mut PineValue, fields: &[String], value: Option<PineValue>) -> bool {
    let Some((field, rest)) = fields.split_first() else {
        return false;
    };
    let PineValue::Object(instance) = current else {
        return false;
    };
    if rest.is_empty() {
        if !instance.fields.contains_key(field) {
            return false;
        }
        instance.fields.insert(field.clone(), value);
        return true;
    }
    match instance
        .fields
        .get_mut(field)
        .and_then(|slot| slot.as_mut())
    {
        Some(next) => Self::set_nested_value(next, rest, value),
        None => false,
    }
}

    /// Write one collection-call result back through a method-call home,
    /// in take order (live alias skipped: alias slots hold instances, so
    /// a collection never takes that route). Frame and column stores
    /// write in place, unjournaled like every take/put pair — holder
    /// persistence (var store, top level, frame discard) decides, exactly
    /// like the tree-walk. Returns `false` when the chain is unwalkable
    /// (the caller keeps its unknown-method refusal).
    fn put_home_value(
        &mut self,
        home: &super::bytecode::CallHome,
        value: Option<PineValue>,
    ) -> bool {
        use super::bytecode::CallHome;
        let program = self.program_handle();
        let program: &VmProgram = &program;
        match home {
            CallHome::Innermost => true,
            CallHome::ThisField { field } => {
                let field_text = program.modules[self.module]
                    .symbols
                    .resolve(*field)
                    .unwrap_or("")
                    .to_owned();
                let Some(slot) = self.receiver.last_mut() else {
                    return false;
                };
                slot.instance.fields.insert(field_text, value);
                true
            }
            CallHome::Var { name, var_ordinal } => {
                let name_text = program.modules[self.module]
                    .symbols
                    .resolve(*name)
                    .unwrap_or("")
                    .to_owned();
                self.put_root_value(&name_text, *var_ordinal, value)
            }
            CallHome::VarPath {
                name,
                var_ordinal,
                path,
            } => {
                let name_text = program.modules[self.module]
                    .symbols
                    .resolve(*name)
                    .unwrap_or("")
                    .to_owned();
                let fields: Vec<String> = path
                    .iter()
                    .map(|field| {
                        program.modules[self.module]
                            .symbols
                            .resolve(*field)
                            .unwrap_or("")
                            .to_owned()
                    })
                    .collect();
                let Some(mut root) = self.read_root_value(&name_text, *var_ordinal) else {
                    return false;
                };
                if !Self::set_nested_value(&mut root, &fields, value) {
                    return false;
                }
                self.put_root_value(&name_text, *var_ordinal, Some(root))
            }
        }
    }

    /// Read one method-home root value in take order: live receiver
    /// alias, current frame, caller frames, per-instance cells, shared
    /// columns. No take, no log — the caller takes or writes through the
    /// same order.
    fn read_root_value(
        &self,
        name_text: &str,
        var_ordinal: Option<u32>,
    ) -> Option<PineValue> {
        let program = self.program_handle();
        let program: &VmProgram = &program;
        if let Some(depth) = self
            .receiver
            .iter()
            .rposition(|slot| slot.alias == name_text)
        {
            return Some(PineValue::Object(
                self.receiver[depth].instance.clone(),
            ));
        }
        if let Some(slot) = program.modules[self.module].blocks[self.block]
            .frame_names
            .get(name_text)
        {
            let slot = *slot as usize;
            return self
                .frames
                .last()
                .and_then(|frame| frame.slots.get(slot).cloned())
                .flatten();
        }
        if let Some(&(caller_module, caller_block, caller_frame)) = self.callers.last() {
            let caller = &program.modules[caller_module];
            if caller.blocks.len() > caller_block {
                if let Some(slot) = caller.blocks[caller_block].frame_names.get(name_text) {
                    let slot = *slot as usize;
                    return self
                        .frames
                        .get(caller_frame)
                        .and_then(|frame| frame.slots.get(slot).cloned())
                        .flatten();
                }
            }
        }
        if let Some(ordinal) = var_ordinal {
            let key = VarInstKey {
                path: self.var_path.clone(),
                home: (self.module, self.block, ordinal),
            };
            if let Some(cell) = self.frame_vars.get(&key) {
                return cell.value.clone();
            }
        }
        if let Some(column) = program.modules[self.module].globals.get(name_text) {
            return self.current.get(*column).cloned().flatten().flatten();
        }
        None
    }

    /// Store one method-home root value where it was read: live receiver
    /// aliases write the live slot, then frame, caller frames, cells,
    /// and columns as read. Unjournaled like every take/put pair.
    /// Returns `false` when no home holds the name.
    fn put_root_value(
        &mut self,
        name_text: &str,
        var_ordinal: Option<u32>,
        value: Option<PineValue>,
    ) -> bool {
        let program = self.program_handle();
        let program: &VmProgram = &program;
        if let Some(depth) = self
            .receiver
            .iter()
            .rposition(|slot| slot.alias == name_text)
        {
            // Alias slots hold instances; a whole-root collection store
            // never addresses one (the take would have hit), so only an
            // object restores through here.
            if let Some(PineValue::Object(instance)) = value {
                if let Some(slot) = self.receiver.get_mut(depth) {
                    slot.instance = instance;
                    return true;
                }
            }
            return false;
        }
        if let Some(slot) = program.modules[self.module].blocks[self.block]
            .frame_names
            .get(name_text)
        {
            let slot = *slot as usize;
            if let Some(frame) = self.frames.last_mut() {
                if slot < frame.slots.len() {
                    frame.slots[slot] = value;
                    return true;
                }
            }
            return false;
        }
        if let Some(&(caller_module, caller_block, caller_frame)) = self.callers.last() {
            let caller = &program.modules[caller_module];
            if caller.blocks.len() > caller_block {
                if let Some(slot) = caller.blocks[caller_block].frame_names.get(name_text) {
                    let slot = *slot as usize;
                    if let Some(frame) = self.frames.get_mut(caller_frame) {
                        if slot < frame.slots.len() {
                            frame.slots[slot] = value;
                            return true;
                        }
                    }
                    return false;
                }
            }
        }
        if let Some(ordinal) = var_ordinal {
            let key = VarInstKey {
                path: self.var_path.clone(),
                home: (self.module, self.block, ordinal),
            };
            if let Some(cell) = self.frame_vars.get_mut(&key) {
                cell.value = value;
                return true;
            }
        }
        if let Some(column) = program.modules[self.module].globals.get(name_text) {
            let column = *column;
            self.global_set(column, value);
            return true;
        }
        false
    }

    /// The loop's slot store: the shared home model calls back here for
    /// every direct slot, receiver field, and map-entry write.
    fn put_map_entry(
        &mut self,
        map_var: &str,
        key: &str,
        key_value: &PineValue,
        instance: UserInstance,
    ) {
        let Some(slot) = self.value_slot(map_var) else {
            // The tree-walk skips silently when the holder vanished: the
            // variable home already carries the mutation.
            return;
        };
        let current = match slot {
            ValueSlot::Frame { frame, slot } => self
                .frames
                .get(frame)
                .and_then(|frame| frame.slots.get(slot).cloned())
                .flatten(),
            ValueSlot::Column(column) => self.current.get(column).cloned().flatten().flatten(),
        };
        if let Some(PineValue::Map(mut entries)) = current {
            entries.insert(
                key.to_owned(),
                (key_value.clone(), Some(PineValue::Object(instance))),
            );
            let stored = Some(PineValue::Map(entries));
            match slot {
                ValueSlot::Frame { frame, slot } => {
                    if let Some(frame) = self.frames.get_mut(frame) {
                        if slot < frame.slots.len() {
                            frame.slots[slot] = stored;
                        }
                    }
                }
                ValueSlot::Column(column) => self.global_set(column, stored),
            }
        }
    }

    /// Resolve one named value home in the current body: a named frame slot
    /// first, then a shared column (the same order `write_home` uses).
    fn value_slot(&self, name: &str) -> Option<ValueSlot> {
        let caller = &self.program.modules[self.module];
        if let Some(slot) = caller.blocks[self.block].frame_names.get(name) {
            return Some(ValueSlot::Frame {
                frame: self.frames.len().checked_sub(1)?,
                slot: *slot as usize,
            });
        }
        caller.globals.get(name).copied().map(ValueSlot::Column)
    }

    /// Resolve a caller home: frame locals (parameters) first, then the
    /// caller module's globals — mirroring the tree-walk's assign path
    /// for bare names and plain dotted variables (`this.*` stays
    /// tree-only until the object model lowers). Homes arrive interned
    /// and resolve through the caller table; only the fallible lookup
    /// allocates, never the steady path.
    fn write_home(&mut self, home: SymbolId, value: Option<PineValue>) -> Result<(), KernelError> {
        let program = self.program_handle();
        let program: &VmProgram = &program;
        let caller = &program.modules[self.module];
        let name: &str = caller.symbols.resolve(home).unwrap_or("");
        // A `this.field` argument home writes the live receiver, exactly
        // like the tree-walk's dotted assign path.
        if let Some(field) = name.strip_prefix("this.") {
            let slot = self
                .receiver
                .last_mut()
                .ok_or_else(super::eval::this_unbound)?;
            if !slot.instance.fields.contains_key(field) {
                return Err(super::eval::unknown_this_field(field));
            }
            slot.instance.fields.insert(field.to_owned(), value);
            return Ok(());
        }
        if let Some(position) = caller.blocks[self.block]
            .params
            .iter()
            .position(|param| param.as_str() == name)
        {
            if let Some(frame) = self.frames.last_mut() {
                if position < frame.slots.len() {
                    frame.slots[position] = value;
                    return Ok(());
                }
            }
        }
        // Named body locals shadow globals and take the write in place:
        // a statement-position collection call on a local (`array.shift`
        // over a frame array) persists through this same home rule.
        if let Some(slot) = caller.blocks[self.block].frame_names.get(name) {
            if let Some(frame) = self.frames.last_mut() {
                let slot = *slot as usize;
                if slot < frame.slots.len() {
                    frame.slots[slot] = value;
                    return Ok(());
                }
            }
        }
        let column =
            caller.globals.get(name).copied().ok_or_else(|| {
                KernelError::Eval(format!("write-back home {name:?} is not bound"))
            })?;
        self.global_set(column, value);
        Ok(())
    }

    /// Resolve one journaled write to its caller home: module globals
    /// first (so nested reads observe the write through the shared
    /// column exactly like the tree-walk's shared namespace), then the
    /// caller frames nearest-first through each frame's named slots
    /// (parameters and body-bound locals), mirroring shadowing.
    /// `None` means the tree-walk would have no binding either — the
    /// caller decides refusal vs fresh mirror.
    fn resolve_journal_home(&self, name: &str) -> Option<JournalHome> {
        let program = self.program_handle();
        let program: &VmProgram = &program;
        if let Some(column) = program.modules[self.module].globals.get(name) {
            return Some(JournalHome::Global(*column));
        }
        for &(caller_module, caller_block, caller_frame) in self.callers.iter().rev() {
            let caller = &program.modules[caller_module];
            if caller.blocks.len() <= caller_block {
                continue;
            }
            if let Some(slot) = caller.blocks[caller_block].frame_names.get(name) {
                return Some(JournalHome::CallerFrame {
                    frame: caller_frame,
                    slot: *slot as usize,
                });
            }
        }
        None
    }

    /// Write one value through to a caller home (global column or caller
    /// frame slot), logging the entry value once per call for the return
    /// revert. Unresolvable homes refuse strict callers through the shared
    /// constructor and stay silent for plain binds (the mirror or cell
    /// already carries the value), mirroring the tree-walk's unbound
    /// rules. Shared by journaled writes and unseeded `var` stores.
    fn journal_write_through(
        &mut self,
        name: &str,
        value: Option<PineValue>,
        strict: bool,
    ) -> Result<(), KernelError> {
        match self.resolve_journal_home(name) {
            Some(JournalHome::Global(column)) => {
                let base = self.journal_base.unwrap_or(0);
                let logged = self.journal[base..]
                    .iter()
                    .any(|logged| logged.same_home(&JournalHome::Global(column)));
                if !logged {
                    let old = self.current.get(column).cloned().flatten();
                    self.journal.push(JournalEntry {
                        home: JournalHome::Global(column),
                        old,
                    });
                }
                if column < self.current.len() {
                    self.current[column] = Some(value);
                }
            }
            Some(JournalHome::CallerFrame { frame, slot }) => {
                let target = JournalHome::CallerFrame { frame, slot };
                let base = self.journal_base.unwrap_or(0);
                let logged = self.journal[base..]
                    .iter()
                    .any(|logged| logged.same_home(&target));
                if !logged {
                    let old = self
                        .frames
                        .get(frame)
                        .and_then(|frame| frame.slots.get(slot).cloned())
                        .flatten();
                    self.journal.push(JournalEntry {
                        home: target,
                        old: old.map(Some),
                    });
                }
                if let Some(frame) = self.frames.get_mut(frame) {
                    if slot < frame.slots.len() {
                        frame.slots[slot] = value;
                    }
                }
            }
            None if strict => {
                return Err(super::eval::reassign_undeclared(name));
            }
            None => {}
        }
        Ok(())
    }
}

/// Tier observability for one run: which columns tiered up, how many
/// history reads each path served, and how often the T2 guard retired a
/// column. Counts only — the scalar rows stay the source of truth, so the
/// probe can never change the outcome it observes. Nothing here feeds a
/// cache or a persisted artifact: the run owns its counters and drops
/// them with its columns.
#[derive(Debug, Clone, Default)]
pub struct ProbeReport {
    /// Columns tiered at end of run (written, still pure, stability at or
    /// past [`TIER_STABLE_BARS`], closed-bar run).
    pub tiered_columns: usize,
    /// Tier-eligible columns at end of run (written and still pure).
    pub pure_columns: usize,
    /// User history reads served from the float mirror (T1 path).
    pub float_reads: u64,
    /// User history reads served from the scalar rows (pre-tier or T2 path).
    pub scalar_reads: u64,
    /// T2 deopt firings (first retirement per column).
    pub deopts: u64,
    /// Link-time compiled fraction (the T3 input).
    pub coverage: f64,
}

fn probe_of(run: &Run, program: &VmProgram) -> ProbeReport {
    let mut tiered = 0;
    let mut pure = 0;
    for column in LEG_COUNT_GLOBAL..run.current.len() {
        if !run.written.get(column).copied().unwrap_or(false) {
            continue;
        }
        if !run.float_pure.get(column).copied().unwrap_or(false) {
            continue;
        }
        pure += 1;
        if run.float_allowed && run.stable.get(column).copied().unwrap_or(0) >= TIER_STABLE_BARS {
            tiered += 1;
        }
    }
    ProbeReport {
        tiered_columns: tiered,
        pure_columns: pure,
        float_reads: run.float_hits,
        scalar_reads: run.scalar_hits,
        deopts: run.deopts,
        coverage: program.coverage(),
    }
}

/// Run a linked program over chart bars: the same run contract as
/// `PineEngine::run_linked` (warmup skipped, over-capacity refused,
/// tick lengths checked), with per-bar evaluation through the compiled
/// blocks. Refusals name the same reasons as the tree-walk; a program
/// the linker rejects runs tree-walk instead (see [`link`]).
///
/// The probe twin of [`run_linked`]: same outcome, plus a [`ProbeReport`]
/// counting tier-up, path hits, and T2 deopts for calibration. The plain
/// entry point delegates here and drops the report.
#[allow(clippy::too_many_lines)]
pub fn run_linked_with_probe(
    program: &VmProgram,
    bars: &[Bar],
    inputs: InputSet,
    config: &VmConfig,
    ticks: Option<&[i64]>,
) -> (Result<super::eval::RunOutcome, KernelError>, ProbeReport) {
    run_linked_in(
        &Arc::new(program.clone()),
        bars,
        inputs,
        config,
        ticks,
        &RequestState::default(),
    )
}

/// Run one linked program carrying this run's injected request tapes. The
/// engine's compiled selection calls this so `request.*` serves exactly the
/// registered datasets; a bare call (tests, benches) carries none and every
/// `request.*` call refuses the live fetch. The program is shared, so the
/// run outlives no borrow and can be held for re-entrant function calls.
pub(crate) fn run_linked_with_requests(
    program: &Arc<VmProgram>,
    bars: &[Bar],
    inputs: InputSet,
    config: &VmConfig,
    ticks: Option<&[i64]>,
    requests: &RequestState,
) -> (Result<super::eval::RunOutcome, KernelError>, ProbeReport) {
    run_linked_in(program, bars, inputs, config, ticks, requests)
}

/// One run over one program, whatever its storage: the single body both
/// entry points above share.
fn run_linked_in(
    program: &Arc<VmProgram>,
    bars: &[Bar],
    inputs: InputSet,
    config: &VmConfig,
    ticks: Option<&[i64]>,
    requests: &RequestState,
) -> (Result<super::eval::RunOutcome, KernelError>, ProbeReport) {
    let got = bars.len() as u64;
    if got > config.max_bars {
        return (
            Err(KernelError::TooManyBars {
                got,
                max: config.max_bars,
            }),
            ProbeReport {
                coverage: program.coverage(),
                ..Default::default()
            },
        );
    }
    if let Some(stamps) = ticks {
        if stamps.len() != bars.len() {
            return (
                Err(KernelError::Eval(format!(
                    "tick run needs one timestamp per bar, got {} stamps for {} bars",
                    stamps.len(),
                    bars.len()
                ))),
                ProbeReport {
                    coverage: program.coverage(),
                    ..Default::default()
                },
            );
        }
    }
    let mut legs = Legs::of(bars);
    if ticks.is_some() {
        // Tick emulation runs the `time` leg in Pine-native milliseconds
        // (exact below 2^53); closed-bar runs keep the seconds domain —
        // exactly like the tree-walk's leg rewrite, so flags, `time`,
        // and request windows agree. Intrabar ticks share one bar-open
        // time; the per-tick stamps serve `timenow` through their own
        // field instead.
        legs.time = bars
            .iter()
            .map(|bar| bar.ts.timestamp_millis() as f64)
            .collect();
    }
    let mut run = Run::fresh_with_config(Arc::clone(program), legs, ticks.is_none(), config);
    run.tick_stamps = ticks.unwrap_or(&[]).to_vec();
    let mut ctx = RunCtx::new(bars, inputs);
    let mut builtins = if requests.is_empty() {
        RunBuiltins::new()
    } else {
        RunBuiltins::with_requests(requests.security_tapes(), requests.lower_tf_tapes())
    };
    let warmup = (config.warmup as usize).min(bars.len());
    for (index, _bar) in bars.iter().enumerate() {
        if index < warmup {
            continue;
        }
        run.cursor = index;
        run.first_bar = run.completed == 0;
        if let Err(error) = run.exec_bar(&mut ctx, &mut builtins) {
            return (Err(error), probe_of(&run, program));
        }
        run.commit_bar(index, true);
    }
    let report = probe_of(&run, program);
    (
        Ok(super::eval::RunOutcome {
            evaluated: run.completed,
            snapshots: run.snapshots,
            plots: run.plots,
            plotcandles: run.plotcandles,
            barcolors: run.barcolors,
            fills: run.fills,
            plotshapes: run.plotshapes,
            hlines: run.hlines,
            draw_events: builtins.drawing_mut().take_events(),
            strategy_calls: builtins.strategy_mut().take_calls(),
        }),
        report,
    )
}

/// Run a linked program over chart bars (see [`run_linked_with_probe`]).
/// Outcome only; the probe report is dropped.
pub fn run_linked(
    program: &VmProgram,
    bars: &[Bar],
    inputs: InputSet,
    config: &VmConfig,
    ticks: Option<&[i64]>,
) -> Result<super::eval::RunOutcome, KernelError> {
    run_linked_with_probe(program, bars, inputs, config, ticks).0
}

/// One refusal wording for the request executors (the tree-walk twin
/// builds the identical text through its own local helper).
fn refused(message: impl Into<String>) -> KernelError {
    KernelError::Eval(message.into())
}

/// Interned column count ceiling for leg ids: global ids start here.
const LEG_COUNT_GLOBAL: usize = super::bytecode::LEG_COUNT as usize;

#[cfg(test)]
#[path = "../../tests/pine_bytecode_exec.rs"]
mod exec_tests;
