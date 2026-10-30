//! Bytecode program representation for the Pine execution loop: the data
//! the compiler emits and the execution loop runs.
//!
//! Design notes:
//!
//! * Operands are dense [`SlotId`] indices, never names: the per-bar
//!   `HashMap<String, …>` traffic of `EngineState` is exactly what this
//!   representation removes. Symbolic data (call names, plot titles,
//!   fallback reasons) is interned once per compile into [`SymbolTable`];
//!   only genuinely open string literals stay inline.
//! * Control flow is flat (`Jump` / `JumpIfFalse` with block-relative
//!   targets), not structured: the loop stays a single program-counter
//!   walk with no tree recursion.
//! * One [`CompiledModule`] per source module, held in the sparse
//!   [`CompiledProgram`] map keyed exactly like `LinkedModule.key`, so
//!   module aliasing, export visibility, and multi-version namespaces
//!   survive compilation untouched (the naive-merge failure mode).
//! * [`Instruction::Fallback`] carries the unlowered source node plus a
//!   typed [`FallbackReason`]: coverage is measured, never a gate. A
//!   fallback anywhere refuses the whole run at link time and the caller
//!   runs the tree-walk instead — one compiled attempt, then one fallback,
//!   never a rung walk, so no fallback combinator is composed here.
//! * The encoded form is the executable stream only; the AST linkage for
//!   fallbacks rides the retained source program at link time (like debug
//!   info), never the wire bytes.

use crate::vars::NodeId;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Dense operand: index into the per-frame slot environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SlotId(pub u32);

/// Index into a [`ConstPool`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConstId(pub u32);

/// Compile-time constant.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    /// Floating-point literal.
    Float(f64),
    /// Integer literal.
    Int(i64),
    /// Boolean literal.
    Bool(bool),
    /// String literal (titles, site keys).
    Str(String),
    /// Color handle (packed `u32`, as `resolve_constant` yields).
    Color(u32),
    /// Enum member handle, interned: the namespace and member resolve
    /// through the owning module's [`SymbolTable`].
    Enum {
        /// Enum namespace (`display`, `size`, …).
        namespace: SymbolId,
        /// Member name.
        member: SymbolId,
    },
    /// The `na` absence literal: fills omitted `= na` defaults with
    /// absence, exactly like the tree-walk binds them.
    Na,
}

/// Deduplicated constant table per module.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConstPool {
    values: Vec<ConstValue>,
}

impl ConstPool {
    /// Empty pool.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Intern a constant, returning its id (linear scan: pools are small
    /// and compile-time only, never read per bar).
    pub fn intern(&mut self, value: ConstValue) -> ConstId {
        if let Some(index) = self.values.iter().position(|known| known == &value) {
            return ConstId(index as u32);
        }
        self.values.push(value);
        ConstId((self.values.len() - 1) as u32)
    }

    /// Read one constant.
    #[must_use]
    pub fn get(&self, id: ConstId) -> Option<&ConstValue> {
        self.values.get(id.0 as usize)
    }

    /// Number of interned constants.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether the pool is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// Numeric and logical operators: the lowerable subset of
/// `syntax::BinaryOp`, mirrored 1:1 so lowering is total over the subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    /// `+`.
    Add,
    /// `-`.
    Subtract,
    /// `*`.
    Multiply,
    /// `/`.
    Divide,
    /// `%`.
    Modulo,
    /// `==`.
    Equal,
    /// `!=`.
    NotEqual,
    /// `<`.
    Less,
    /// `<=`.
    LessEqual,
    /// `>`.
    Greater,
    /// `>=`.
    GreaterEqual,
    /// `and`.
    And,
    /// `or`.
    Or,
}

/// Unary operators: the lowerable subset of `syntax::UnaryOp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    /// `-`.
    Negate,
    /// `not`.
    Not,
}

/// Chart-leg and cursor builtins: the lowerable subset of
/// `syntax::BuiltinVar` plus the bar cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinVarKind {
    /// `open`.
    Open,
    /// `high`.
    High,
    /// `low`.
    Low,
    /// `close`.
    Close,
    /// `volume`.
    Volume,
    /// `time`.
    Time,
    /// `bar_index`.
    BarIndex,
    /// `syminfo.tickerid`: the run's chart symbol.
    ChartSymbol,
    /// `timeframe.period`: the run's chart period.
    ChartPeriod,
    /// `timeframe.isintraday`: whether the chart period is intraday.
    ChartIsIntraday,
    /// `syminfo.timezone`: the harness's mapped session zone.
    ChartTimezone,
    /// `syminfo.pointvalue`: one currency unit per point.
    ChartPointValue,
    /// `syminfo.mincontract`: whole shares.
    ChartMinContract,
    /// `barstate.isfirst`: the cursor is on the first bar.
    BarstateIsFirst,
    /// `barstate.isnew`: a new bar (every closed-bar evaluation; tick
    /// openings in tick runs).
    BarstateIsNew,
    /// `barstate.isconfirmed`: the bar is confirmed (every closed-bar
    /// evaluation; tick confirmations in tick runs).
    BarstateIsConfirmed,
    /// `barstate.islast`: the cursor is on the last bar.
    BarstateIsLast,
    /// `barstate.ishistory`: a closed-bar (non-tick) run.
    BarstateIsHistory,
    /// `barstate.isrealtime`: a tick run.
    BarstateIsRealtime,
    /// `timenow`: tick stamps in tick runs, the time leg otherwise.
    Timenow,
    /// Bare legacy `tr`: true range at the cursor.
    Tr,
    /// Bare `timeframe.in_seconds()`: the chart period in seconds.
    ChartPeriodSeconds,
}

#[cfg(feature = "bytecode")]
impl BuiltinVarKind {
    /// Chart-scope member this kind reads, if it is one. The single bridge
    /// between the wire tag and the shared resolver, so the loop and the
    /// tree-walk can never name a member differently.
    #[must_use]
    pub(crate) fn chart_scope(self) -> Option<super::builtins_core::ChartScope> {
        use super::builtins_core::ChartScope;
        match self {
            Self::ChartSymbol => Some(ChartScope::Symbol),
            Self::ChartPeriod => Some(ChartScope::Period),
            Self::ChartIsIntraday => Some(ChartScope::IsIntraday),
            Self::ChartTimezone => Some(ChartScope::Timezone),
            Self::ChartPointValue => Some(ChartScope::PointValue),
            Self::ChartMinContract => Some(ChartScope::MinContract),
            _ => None,
        }
    }

    /// Wire kind for one chart-scope member.
    #[must_use]
    pub(crate) fn from_chart_scope(scope: super::builtins_core::ChartScope) -> Self {
        use super::builtins_core::ChartScope;
        match scope {
            ChartScope::Symbol => Self::ChartSymbol,
            ChartScope::Period => Self::ChartPeriod,
            ChartScope::IsIntraday => Self::ChartIsIntraday,
            ChartScope::Timezone => Self::ChartTimezone,
            ChartScope::PointValue => Self::ChartPointValue,
            ChartScope::MinContract => Self::ChartMinContract,
        }
    }

    /// Wire kind for one `barstate.*` member, if it is a served flag.
    /// Unknown members are `None`: the tree-walk refuses them, so the
    /// compiler keeps the fallback and the tree decides.
    #[must_use]
    pub(crate) fn from_barstate_member(member: &str) -> Option<Self> {
        match member {
            "isfirst" => Some(Self::BarstateIsFirst),
            "isnew" => Some(Self::BarstateIsNew),
            "isconfirmed" => Some(Self::BarstateIsConfirmed),
            "islast" => Some(Self::BarstateIsLast),
            "ishistory" => Some(Self::BarstateIsHistory),
            "isrealtime" => Some(Self::BarstateIsRealtime),
            _ => None,
        }
    }

    /// `barstate.*` member this kind serves, if it is one. The single
    /// bridge between the wire kind and the shared flag resolver, so the
    /// loop and the tree-walk can never name a flag differently.
    #[must_use]
    pub(crate) fn barstate_member(self) -> Option<&'static str> {
        match self {
            Self::BarstateIsFirst => Some("isfirst"),
            Self::BarstateIsNew => Some("isnew"),
            Self::BarstateIsConfirmed => Some("isconfirmed"),
            Self::BarstateIsLast => Some("islast"),
            Self::BarstateIsHistory => Some("ishistory"),
            Self::BarstateIsRealtime => Some("isrealtime"),
            _ => None,
        }
    }
}

/// Call target: an intra-module block (linked), a named callee the
/// linker resolves across modules (imports, aliases, methods), or a
/// builtin namespace call delegated to the existing bridges. Open names
/// are interned [`SymbolId`]s; the namespace is a [`BuiltinNamespace`].
/// One `plot()` title: a pool constant for statically named series, or
/// an evaluated slot for computed titles. The wire form keeps one tag
/// per shape (`0x0B` for the constant, `0x35` for the slot), so blocks
/// encoded before the slot form existed still decode to their documented
/// meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlotTitle {
    /// Statically named series (constant pool string).
    Const(ConstId),
    /// Computed title (a string names the series; anything else falls
    /// back to the run-time `plot:{n}` default at first sight, exactly
    /// like the tree-walk).
    Slot(SlotId),
}

/// One history offset: a literal bars-back count, or an evaluated slot
/// holding one (array indices evaluate first and refuse absence, exactly
/// like the tree-walk's index-then-array order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HistOffset {
    /// Literal bars back.
    Const(i32),
    /// Value slot holding the bars back.
    Slot(SlotId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallTarget {
    /// Intra-module block index, patched at link time.
    Block(usize),
    /// Named callee: `None` module means the current module.
    Named {
        /// Owning module key, or `None` for the current module.
        module: Option<SymbolId>,
        /// Function or method name.
        name: SymbolId,
    },
    /// Link-resolved callee: `link` rewrites every in-program `Named` target
    /// into `(program module index, body block)`, so the loop dispatch needs
    /// no symbol resolution or table lookup. The compiler never emits this
    /// form; `Named` survives only for calls that leave the program.
    Resolved {
        /// Module index inside the linked program.
        module: u32,
        /// Body block index inside that module.
        block: u32,
    },
    /// Builtin namespace call (`ta.*`, `input.*`, `math.*`, …): delegated
    /// to the existing bridges at run time with the precomputed site.
    Builtin {
        /// Namespace.
        namespace: BuiltinNamespace,
        /// Method name.
        method: SymbolId,
    },
}

/// One `fill()` leg: either an inline `plot()` call (paired by its
/// precomputed capture site) or a variable bound from one (paired by its
/// run-time plot tag).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FillLeg {
    /// Inline `plot()` call: capture site constant.
    PlotSite(ConstId),
    /// Variable bound from a `plot()` call: tag resolved at run time.
    Variable(SymbolId),
}

/// One explicit generic argument, mirroring `syntax::PineType` for the
/// wire: names interned, structure shared. Only `map.new` reads these
/// today; every other builtin ignores the empty list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallTypeArg {
    /// Type name (possibly dotted, e.g. `UTIL.Smoothings`).
    pub name: SymbolId,
    /// Generic arguments.
    pub args: Vec<CallTypeArg>,
}

impl CallTypeArg {
    /// Resolve back to the syntax shape the bridges consume.
    #[must_use]
    pub fn resolve(&self, symbols: &SymbolTable) -> crate::pine::syntax::PineType {
        crate::pine::syntax::PineType {
            name: symbols.resolve(self.name).unwrap_or("?").to_owned(),
            args: self.args.iter().map(|arg| arg.resolve(symbols)).collect(),
            // Call type arguments never carry a trailing rank today; the
            // bridge resolves the spelled type as written.
            array_depth: 0,
        }
    }
}

/// One call argument: the precomputed name (for parameter binding) and
/// the evaluated value slot. Names resolve through the owning module's
/// [`SymbolTable`]; values flow at run time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallArgSlot {
    /// Argument name (`None` for positional).
    pub name: Option<SymbolId>,
    /// Value slot.
    pub slot: SlotId,
}

/// Branch context for conditional jumps: non-`bool` conditions refuse,
/// and the refusal names the construct exactly like the tree-walk's
/// `branch_of` (`"if condition"`, `"ternary condition"`,
/// `"while condition"`, `"switch pattern"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CondContext {
    /// `if` statement condition.
    If,
    /// Ternary `cond ? a : b` condition.
    Ternary,
    /// `while` loop condition.
    While,
    /// Bare-`switch` arm pattern.
    SwitchPattern,
}

/// Chart-leg identity: every series column addressable by history,
/// including the derived legs. The discriminant order matches the
/// reserved global column ids, so `index` doubles as the column id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LegKind {
    /// `open`.
    Open,
    /// `high`.
    High,
    /// `low`.
    Low,
    /// `close`.
    Close,
    /// `volume`.
    Volume,
    /// `time`.
    Time,
    /// `hl2`.
    Hl2,
    /// `hlc3`.
    Hlc3,
    /// `ohlc4`.
    Ohlc4,
    /// `wcl`.
    Wcl,
}

impl LegKind {
    /// Reserved column count: user columns start here.
    pub const COUNT: u32 = 10;

    /// Reserved global column id for this leg.
    #[must_use]
    pub fn index(self) -> u32 {
        self as u32
    }

    /// Source-level leg name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::High => "high",
            Self::Low => "low",
            Self::Close => "close",
            Self::Volume => "volume",
            Self::Time => "time",
            Self::Hl2 => "hl2",
            Self::Hlc3 => "hlc3",
            Self::Ohlc4 => "ohlc4",
            Self::Wcl => "wcl",
        }
    }

    /// Parse a source-level leg name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "open" => Some(Self::Open),
            "high" => Some(Self::High),
            "low" => Some(Self::Low),
            "close" => Some(Self::Close),
            "volume" => Some(Self::Volume),
            "time" => Some(Self::Time),
            "hl2" => Some(Self::Hl2),
            "hlc3" => Some(Self::Hlc3),
            "ohlc4" => Some(Self::Ohlc4),
            "wcl" => Some(Self::Wcl),
            _ => None,
        }
    }

    /// Resolve a reserved column id back to its leg.
    #[must_use]
    pub fn from_index(index: u32) -> Option<Self> {
        match index {
            0 => Some(Self::Open),
            1 => Some(Self::High),
            2 => Some(Self::Low),
            3 => Some(Self::Close),
            4 => Some(Self::Volume),
            5 => Some(Self::Time),
            6 => Some(Self::Hl2),
            7 => Some(Self::Hlc3),
            8 => Some(Self::Ohlc4),
            9 => Some(Self::Wcl),
            _ => None,
        }
    }
}

/// Builtin namespace with delegated lowering: calls into exactly these
/// namespaces lower to bridge calls; everything else falls back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinNamespace {
    /// `ta.*` (stateful indicator bridge).
    Ta,
    /// `input.*` (first-call-wins declarations).
    Input,
    /// `math.*`.
    Math,
    /// `str.*`.
    Str,
    /// `color.*`.
    Color,
    /// `timeframe.*`.
    Timeframe,
    /// Bare `int`/`float`/`color` casts.
    Core,
    /// `array.*` (constructors, readers, and mutators share the single
    /// input-store bridge; write-back is gated per call, never assumed).
    Array,
    /// `matrix.*` (same bridge discipline as arrays).
    Matrix,
    /// `map.*` (same bridge discipline as arrays).
    Map,
    /// `strategy.*` headless capture (declaration, broker dialogue, and the
    /// empty-book reads). The dispatch member is the namespace tail plus the
    /// method (`closedtrades.size`), so one namespace serves every member.
    Strategy,
    /// Drawing capture (`label`/`table`/`box`/`line`). The dispatch member
    /// is the source namespace plus the method (`label.new`), so one
    /// namespace serves all four families.
    Drawing,
    /// Headless `alert`/`alertcondition` evaluation. Arguments evaluate and
    /// drop; the harness has no delivery path.
    Alert,
}

impl BuiltinNamespace {
    /// Parse a source-level namespace name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "ta" => Some(Self::Ta),
            "input" => Some(Self::Input),
            "math" => Some(Self::Math),
            "str" => Some(Self::Str),
            "color" => Some(Self::Color),
            "timeframe" => Some(Self::Timeframe),
            "core" => Some(Self::Core),
            "array" => Some(Self::Array),
            "matrix" => Some(Self::Matrix),
            "map" => Some(Self::Map),
            "strategy" => Some(Self::Strategy),
            // `Drawing` and `Alert` are dispatch targets the compiler binds
            // directly; no source namespace spells them, so they have no
            // parse name.
            _ => None,
        }
    }

    /// Source-level namespace name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ta => "ta",
            Self::Input => "input",
            Self::Math => "math",
            Self::Str => "str",
            Self::Color => "color",
            Self::Timeframe => "timeframe",
            Self::Core => "core",
            Self::Array => "array",
            Self::Matrix => "matrix",
            Self::Map => "map",
            Self::Strategy => "strategy",
            Self::Drawing => "drawing",
            Self::Alert => "alert",
        }
    }
}

/// Capture or broker-dialogue call: recorded by the tree-walk, never
/// lowered to value position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaptureKind {
    /// `fill`.
    Fill,
    /// `plotshape`.
    PlotShape,
    /// `plotcandle`.
    PlotCandle,
    /// `barcolor`.
    BarColor,
    /// `hline`.
    Hline,
    /// Reserved: alerts were captured through this tag before they became
    /// a builtin namespace. Kept encoded so representations that still
    /// carry the tag round-trip unchanged.
    Alert,
    /// Reserved (see [`CaptureKind::Alert`]).
    AlertCondition,
    /// Reserved: broker dialogue was captured through this tag before it
    /// became a builtin namespace. Kept encoded so representations and
    /// fixtures that still carry the tag round-trip unchanged.
    Strategy,
}

impl CaptureKind {
    /// Parse a source-level callee name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "fill" => Some(Self::Fill),
            "plotshape" => Some(Self::PlotShape),
            "plotcandle" => Some(Self::PlotCandle),
            "barcolor" => Some(Self::BarColor),
            "hline" => Some(Self::Hline),
            _ => None,
        }
    }

    /// Source-level callee name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fill => "fill",
            Self::PlotShape => "plotshape",
            Self::PlotCandle => "plotcandle",
            Self::BarColor => "barcolor",
            Self::Hline => "hline",
            Self::Alert => "alert",
            Self::AlertCondition => "alertcondition",
            Self::Strategy => "strategy",
        }
    }
}

/// Interned open name: user globals, titles, site suffixes, method and
/// module names. Identity compares by id; text resolves through the
/// owning module's [`SymbolTable`] only at link and diagnostic time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolId(pub u32);

/// Deduplicated open-name table per compiled module, built once at
/// compile time and shared at link time. Steady-state execution never
/// interns: every id on the hot path already exists.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SymbolTable {
    names: Vec<String>,
    index: HashMap<String, u32>,
}

impl SymbolTable {
    /// Empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Intern a name, returning its id (deduplicated).
    pub fn intern(&mut self, name: &str) -> SymbolId {
        if let Some(id) = self.index.get(name) {
            return SymbolId(*id);
        }
        let id = self.names.len() as u32;
        self.names.push(name.to_owned());
        self.index.insert(name.to_owned(), id);
        SymbolId(id)
    }

    /// Resolve an id to its name.
    #[must_use]
    pub fn resolve(&self, id: SymbolId) -> Option<&str> {
        self.names.get(id.0 as usize).map(String::as_str)
    }

    /// Number of interned names.
    #[must_use]
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Whether the table holds no names.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

/// Why a subtree runs on the tree-walk instead of the loop: the typed
/// coverage escape hatch. Each variant names one unlowerable construct
/// family; `display` renders the human reason at the single diagnostic
/// site (coverage output, never a run contract).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FallbackReason {
    /// In-function global or body binding (private frames; caller-home
    /// writes journal, see `JournalWrite`).
    FunctionBodyBinding {
        /// Bound name.
        name: SymbolId,
    },
    /// Reassignment of a name never declared.
    ReassignUndeclared {
        /// Assigned name.
        name: SymbolId,
    },
    /// History read over an undeclared name.
    HistoryUndeclared {
        /// Read name.
        name: SymbolId,
    },
    /// Dotted name (library alias or enum member).
    DottedName {
        /// Full dotted path.
        name: SymbolId,
    },
    /// Bare call with no lowering (`fixnan`, `time`, `hour`, `minute`,
    /// `strategy`, `alert`, `alertcondition`).
    BareCall {
        /// Callee name.
        name: SymbolId,
    },
    /// Call to a name with no definition.
    UnknownCallee {
        /// Callee name.
        name: SymbolId,
    },
    /// Call to an overloaded name (runtime dispatch).
    OverloadedCallee,
    /// Call needing a default evaluated at run time.
    MissingDefault,
    /// Too many positional arguments.
    TooManyPositionals,
    /// Unknown named argument.
    UnknownArgument {
        /// Argument name.
        name: SymbolId,
    },
    /// Cast arity mismatch.
    CastArity {
        /// Cast name.
        name: SymbolId,
    },
    /// Cast argument shape mismatch.
    CastShape {
        /// Cast name.
        name: SymbolId,
    },
    /// `na()` arity mismatch.
    NaArity,
    /// `nz()` arity mismatch.
    NzArity,
    /// Bare `input` without a default.
    InputMissingDefault,
    /// Bare `input` whose kind needs the run-time value.
    ComputedInputDefault,
    /// Dotted write-back home.
    DottedHome,
    /// Bounded loops.
    Loops,
    /// `break` outside lowered loops.
    Break,
    /// Multi-target assignment or destructuring. (Statement and
    /// expression `switch` plus array literals and indexing lower fully
    /// now, so they carry no fallback variant.)
    TupleUnpack,
    /// Field read (object model).
    FieldRead,
    /// Instance call (object model).
    InstanceCall,
    /// Capture or broker-dialogue call in value position.
    CaptureValue {
        /// Callee namespace.
        kind: CaptureKind,
    },
    /// Capture or dialogue statement.
    CaptureStatement {
        /// Callee namespace.
        kind: CaptureKind,
    },
    /// Untitled `plot()` (default title counts captures at run time).
    PlotUntitled,
    /// Statement-position collection call with its own persistence shape
    /// (`array.shift`, `map.remove`): the tree-walk persists through a
    /// dedicated path the value-position bridge cannot reproduce.
    BuiltinStatement {
        /// Namespace name.
        namespace: SymbolId,
        /// Method name.
        method: SymbolId,
    },
    /// Capture call with a statically wrong shape (arity, non-plot fill
    /// legs): the tree-walk refuses the same call at run time.
    CaptureShape {
        /// Callee namespace.
        kind: CaptureKind,
    },
    /// Namespaced call outside the builtin set.
    OutsideBuiltin {
        /// Namespace name.
        namespace: SymbolId,
        /// Method name.
        method: SymbolId,
    },
    /// Undeclared read (na-vs-refusal decided at run time).
    UndeclaredRead,
    /// Enum member needing the runtime tables.
    EnumMember,
    /// History over a computed series.
    ComputedSeriesHistory,
    /// Unlowered call in statement position.
    StatementCall {
        /// Callee name.
        name: SymbolId,
    },
    /// Context wrapper: unlowerable call argument.
    CallArgument {
        /// Inner reason.
        inner: Box<FallbackReason>,
    },
    /// Context wrapper: unlowerable `na()` argument.
    NaArgument {
        /// Inner reason.
        inner: Box<FallbackReason>,
    },
    /// Context wrapper: unlowerable `nz()` argument.
    NzArgument {
        /// Inner reason.
        inner: Box<FallbackReason>,
    },
    /// Context wrapper: unlowerable `if` condition.
    IfCondition {
        /// Inner reason.
        inner: Box<FallbackReason>,
    },
    /// Context wrapper: unlowerable ternary condition.
    TernaryCondition {
        /// Inner reason.
        inner: Box<FallbackReason>,
    },
    /// Context wrapper: unlowerable ternary arm.
    TernaryArm {
        /// Inner reason.
        inner: Box<FallbackReason>,
    },
}

impl FallbackReason {
    /// Render the human reason. Names resolve through `symbols`; unknown
    /// ids render as their index (never panic on a stale id).
    #[must_use]
    pub fn display(&self, symbols: &SymbolTable) -> String {
        let name = |id: &SymbolId| {
            symbols
                .resolve(*id)
                .map_or_else(|| format!("#{0}", id.0), |text| format!("{text:?}"))
        };
        match self {
            Self::FunctionBodyBinding { name: id } => format!(
                "function-body binding of {} runs on the tree-walk (unlowered call frames)",
                name(id)
            ),
            Self::ReassignUndeclared { name: id } => format!(
                "reassignment of undeclared name {} runs on the tree-walk",
                name(id)
            ),
            Self::HistoryUndeclared { name: id } => {
                format!(
                    "history over undeclared name {} runs on the tree-walk",
                    name(id)
                )
            }
            Self::DottedName { name: id } => format!(
                "dotted name {} runs on the tree-walk (unlowered alias or enum member)",
                name(id)
            ),
            Self::BareCall { name: id } => {
                let text = symbols.resolve(*id).unwrap_or("?");
                format!("bare {text} runs on the tree-walk")
            }
            Self::UnknownCallee { name: id } => format!(
                "unknown callee {} resolves at run time on the tree-walk",
                name(id)
            ),
            Self::OverloadedCallee => {
                "overloaded callee dispatches at run time on the tree-walk".to_owned()
            }
            Self::MissingDefault => {
                "call needs a default evaluated at run time on the tree-walk".to_owned()
            }
            Self::TooManyPositionals => {
                "too many positional arguments runs on the tree-walk".to_owned()
            }
            Self::UnknownArgument { name: id } => {
                format!("unknown argument {} runs on the tree-walk", name(id))
            }
            Self::CastArity { name: id } => {
                let text = symbols.resolve(*id).unwrap_or("?");
                format!("{text}() arity runs on the tree-walk")
            }
            Self::CastShape { name: id } => {
                let text = symbols.resolve(*id).unwrap_or("?");
                format!("{text}() shape runs on the tree-walk")
            }
            Self::NaArity => "na() arity runs on the tree-walk".to_owned(),
            Self::NzArity => "nz() arity runs on the tree-walk".to_owned(),
            Self::InputMissingDefault => "'input' takes a default value".to_owned(),
            Self::ComputedInputDefault => {
                "computed bare input defaults run on the tree-walk (kind needs the run-time value)"
                    .to_owned()
            }
            Self::DottedHome => {
                "dotted call argument home runs on the tree-walk (unlowered assign path)".to_owned()
            }
            Self::Loops => {
                "loops run on the tree-walk (unlowered bounds and break semantics)".to_owned()
            }
            Self::Break => "break runs on the tree-walk (unlowered loop unwinding)".to_owned(),
            Self::TupleUnpack => {
                "multi-target assignment runs on the tree-walk (unlowered tuple unpacking)"
                    .to_owned()
            }
            Self::FieldRead => {
                "field read runs on the tree-walk (unlowered object model)".to_owned()
            }
            Self::InstanceCall => {
                "instance calls run on the tree-walk (unlowered object model)".to_owned()
            }
            Self::CaptureValue { kind } => format!(
                "capture and broker calls run on the tree-walk (never value-position lowered: {})",
                kind.as_str()
            ),
            Self::CaptureStatement { kind } => {
                format!("{}() captures or dialogues on the tree-walk", kind.as_str())
            }
            Self::PlotUntitled => "plot() runs on the tree-walk".to_owned(),
            Self::BuiltinStatement { namespace, method } => {
                let ns = symbols.resolve(*namespace).unwrap_or("?");
                let m = symbols.resolve(*method).unwrap_or("?");
                format!("{ns}.{m}() needs its statement form on the tree-walk")
            }
            Self::CaptureShape { kind } => format!(
                "{}() needs its documented argument shape on the tree-walk",
                kind.as_str()
            ),
            Self::OutsideBuiltin { namespace, method } => {
                let ns = symbols.resolve(*namespace).unwrap_or("?");
                let m = symbols.resolve(*method).unwrap_or("?");
                format!("{ns}.{m} runs on the tree-walk (unlowered namespaces)")
            }
            Self::UndeclaredRead => {
                "undeclared read runs on the tree-walk (na-vs-refusal decided there)".to_owned()
            }
            Self::EnumMember => {
                "enum member runs on the tree-walk (unlowered runtime tables)".to_owned()
            }
            Self::ComputedSeriesHistory => {
                "history over a computed series runs on the tree-walk".to_owned()
            }
            Self::StatementCall { name: id } => {
                let text = symbols.resolve(*id).unwrap_or("?");
                format!("{text}() runs on the tree-walk")
            }
            Self::CallArgument { inner } => {
                format!("call argument: {}", inner.display(symbols))
            }
            Self::NaArgument { inner } => {
                format!("na() argument: {}", inner.display(symbols))
            }
            Self::NzArgument { inner } => {
                format!("nz() argument: {}", inner.display(symbols))
            }
            Self::IfCondition { inner } => {
                format!("if condition: {}", inner.display(symbols))
            }
            Self::TernaryCondition { inner } => {
                format!("ternary condition: {}", inner.display(symbols))
            }
            Self::TernaryArm { inner } => {
                format!("ternary arm: {}", inner.display(symbols))
            }
        }
    }

    /// Stable family label for coverage measurement: one name per reason
    /// *shape*, with context wrappers keeping their own label so a census
    /// attributes an emission to the construct that fell back rather than
    /// to the wrapper that carried it. Lives next to the variants so a new
    /// reason cannot exist without a ledger name.
    #[must_use]
    pub fn family(&self) -> &'static str {
        match self {
            Self::FunctionBodyBinding { .. } => "FunctionBodyBinding",
            Self::ReassignUndeclared { .. } => "ReassignUndeclared",
            Self::HistoryUndeclared { .. } => "HistoryUndeclared",
            Self::DottedName { .. } => "DottedName",
            Self::BareCall { .. } => "BareCall",
            Self::UnknownCallee { .. } => "UnknownCallee",
            Self::OverloadedCallee => "OverloadedCallee",
            Self::MissingDefault => "MissingDefault",
            Self::TooManyPositionals => "TooManyPositionals",
            Self::UnknownArgument { .. } => "UnknownArgument",
            Self::CastArity { .. } => "CastArity",
            Self::CastShape { .. } => "CastShape",
            Self::NaArity => "NaArity",
            Self::NzArity => "NzArity",
            Self::InputMissingDefault => "InputMissingDefault",
            Self::ComputedInputDefault => "ComputedInputDefault",
            Self::DottedHome => "DottedHome",
            Self::Loops => "Loops",
            Self::Break => "Break",
            Self::TupleUnpack => "TupleUnpack",
            Self::FieldRead => "FieldRead",
            Self::InstanceCall => "InstanceCall",
            Self::CaptureValue { .. } => "CaptureValue",
            Self::CaptureStatement { .. } => "CaptureStatement",
            Self::PlotUntitled => "PlotUntitled",
            Self::BuiltinStatement { .. } => "BuiltinStatement",
            Self::CaptureShape { .. } => "CaptureShape",
            Self::OutsideBuiltin { .. } => "OutsideBuiltin",
            Self::UndeclaredRead => "UndeclaredRead",
            Self::EnumMember => "EnumMember",
            Self::ComputedSeriesHistory => "ComputedSeriesHistory",
            Self::StatementCall { .. } => "StatementCall",
            Self::CallArgument { .. } => "CallArgument",
            Self::NaArgument { .. } => "NaArgument",
            Self::NzArgument { .. } => "NzArgument",
            Self::IfCondition { .. } => "IfCondition",
            Self::TernaryCondition { .. } => "TernaryCondition",
            Self::TernaryArm { .. } => "TernaryArm",
        }
    }
}

/// One receiver write-back home for a method call: a caller variable
/// (alias search first, then frame slots, globals, and per-instance
/// cells — mirroring the tree-walk's take order), or the innermost
/// receiver slot for `this` calls. Rvalue calls carry no home — the
/// mutated copy drops, exactly like the tree-walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallHome {
    /// Caller variable home, with the in-function cell ordinal when the
    /// name declares one in the calling body.
    Var {
        name: SymbolId,
        var_ordinal: Option<u32>,
    },
    /// Innermost receiver slot (`this` calls).
    Innermost,
    /// Field path under a variable root (`o.box.bump()`): the engine takes
    /// the root, walks the fields, and rebuilds the chain on write-back.
    VarPath {
        /// Root variable name.
        name: SymbolId,
        /// In-function cell ordinal of the root.
        var_ordinal: Option<u32>,
        /// Field path, outermost first.
        path: Vec<SymbolId>,
    },
    /// Field of the live receiver (`this.box.bump()`, `this.field` write
    /// backs): taken out of the receiver slot and written back into it.
    ThisField {
        /// Receiver field name.
        field: SymbolId,
    },
}

impl Instruction {
    /// Stable wire-form name: diagnostics, and the round-trip coverage gate
    /// that keeps the fixture honest when a variant is added. The match is
    /// exhaustive by construction, so a new variant cannot be added without
    /// naming it here (and then covering it in the fixture).
    #[must_use]
    pub fn wire_name(&self) -> &'static str {
        match self {
            Self::Const { .. } => "const",
            Self::Copy { .. } => "copy",
            Self::BinOp { .. } => "binop",
            Self::UnOp { .. } => "unop",
            Self::Builtin { .. } => "builtin",
            Self::History { .. } => "history",
            Self::GLoad { .. } => "gload",
            Self::GReassign { .. } => "greassign",
            Self::GStoreLoop { .. } => "gstore_loop",
            Self::GStore { .. } => "gstore",
            Self::GStoreInit { .. } => "gstore_init",
            Self::Call { .. } => "call",
            Self::Jump { .. } => "jump",
            Self::JumpIfFalse { .. } => "jump_if_false",
            Self::JumpIfNa { .. } => "jump_if_na",
            Self::Return { .. } => "return",
            Self::Plot { .. } => "plot",
            Self::PlotCandle { .. } => "plot_candle",
            Self::BarColor { .. } => "bar_color",
            Self::PlotShape { .. } => "plot_shape",
            Self::Hline { .. } => "hline",
            Self::Fill { .. } => "fill",
            Self::Coalesce { .. } => "coalesce",
            Self::ForInit { .. } => "for_init",
            Self::WhileTick { .. } => "while_tick",
            Self::ForInInit { .. } => "for_in_init",
            Self::ArrayGet { .. } => "array_get",
            Self::ArrayNew { .. } => "array_new",
            Self::Destructure { .. } => "destructure",
            Self::JournalWrite { .. } => "journal_write",
            Self::PlotTag { .. } => "plot_tag",
            Self::VarDecl { .. } => "var_decl",
            Self::VarStore { .. } => "var_store",
            Self::VarRead { .. } => "var_read",
            Self::FieldGet { .. } => "field_get",
            Self::ObjNew { .. } => "obj_new",
            Self::InstanceCall { .. } => "instance_call",
            Self::ThisRead { .. } => "this_read",
            Self::ThisFieldGet { .. } => "this_field_get",
            Self::ThisFieldSet { .. } => "this_field_set",
            Self::AliasRead { .. } => "alias_read",
            Self::AliasFieldGet { .. } => "alias_field_get",
            Self::FieldPut { .. } => "field_put",
            Self::SwitchEq { .. } => "switch_eq",
            Self::Fallback { .. } => "fallback",
            Self::LocalHistory { .. } => "local_history",
            Self::DottedGet { .. } => "dotted_get",
            Self::RequestSecurity { .. } => "request_security",
            Self::RequestLowerTf { .. } => "request_lower_tf",
            Self::StrategyRead { .. } => "strategy_read",
            Self::SetMapProv { .. } => "set_map_prov",
            Self::Refuse { .. } => "refuse",
            Self::RuntimeRefuse { .. } => "runtime_refuse",
            Self::Noop => "noop",
            Self::EnumMark { .. } => "enum_mark",
            Self::EnumRead { .. } => "enum_read",
            Self::AliasFlatRead { .. } => "alias_flat_read",
        }
    }
}

/// What closeout decided about one fallback family.
///
/// The audit is exhaustive: every `FallbackReason` variant is classified
/// here, and the closeout suite asserts the registry, the compiler's
/// emission sites, and the fixture ledger agree. A deterministic-headless
/// construct may not hide behind a "retained" label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackDisposition {
    /// Still emitted for shapes the compiler cannot lower but that stay
    /// tree-walk by construction (a re-lowered variant keeps its tag).
    Lowered,
    /// No longer emitted at all: a later lowering made the shape
    /// representable. The variant and its tag stay for old encoded blocks
    /// and are never reused.
    Retired,
    /// Reserved for live/external work: a live venue dialogue, a live
    /// network fetch with no injection, or a construct whose decision needs
    /// run-time information the compiler cannot have. Each retains the
    /// tree-walk with an exact-parity proof.
    RetainedExternal,
    /// Needs per-bar or per-call information the compiler cannot see (name
    /// presence, enum registration order, a computed default's value). The
    /// tree-walk decides; the differential suite pins parity.
    RuntimeDependent,
    /// The object model (UDTs, methods, native dispatch) has no slot-VM
    /// representation; the construct keeps the tree-walk.
    ObjectModel,
    /// A wrapper that only carries an inner reason, so the inner verdict is
    /// the disposition that matters.
    Wrapper,
}

/// Every fallback family's closeout disposition, one row per variant of
/// [`FallbackReason`].
pub const FALLBACK_DISPOSITIONS: &[(&str, FallbackDisposition)] = &[
    ("function_body_binding", FallbackDisposition::ObjectModel),
    ("reassign_undeclared", FallbackDisposition::RuntimeDependent),
    ("history_undeclared", FallbackDisposition::Retired),
    ("dotted_name", FallbackDisposition::RuntimeDependent),
    ("bare_call", FallbackDisposition::Retired),
    ("unknown_callee", FallbackDisposition::Retired),
    ("overloaded_callee", FallbackDisposition::ObjectModel),
    ("missing_default", FallbackDisposition::RuntimeDependent),
    ("too_many_positionals", FallbackDisposition::Lowered),
    ("unknown_argument", FallbackDisposition::Lowered),
    ("cast_arity", FallbackDisposition::Lowered),
    ("cast_shape", FallbackDisposition::RuntimeDependent),
    ("na_arity", FallbackDisposition::Retired),
    ("nz_arity", FallbackDisposition::Retired),
    ("input_missing_default", FallbackDisposition::Retired),
    ("computed_input_default", FallbackDisposition::Retired),
    ("dotted_home", FallbackDisposition::RuntimeDependent),
    ("loops", FallbackDisposition::Lowered),
    ("break", FallbackDisposition::Lowered),
    ("tuple_unpack", FallbackDisposition::Lowered),
    ("field_read", FallbackDisposition::ObjectModel),
    ("instance_call", FallbackDisposition::ObjectModel),
    ("capture_value", FallbackDisposition::Lowered),
    ("capture_statement", FallbackDisposition::Lowered),
    ("plot_untitled", FallbackDisposition::Retired),
    ("builtin_statement", FallbackDisposition::Retired),
    ("capture_shape", FallbackDisposition::Lowered),
    ("outside_builtin", FallbackDisposition::RetainedExternal),
    ("undeclared_read", FallbackDisposition::Retired),
    ("enum_member", FallbackDisposition::RuntimeDependent),
    (
        "computed_series_history",
        FallbackDisposition::RuntimeDependent,
    ),
    ("statement_call", FallbackDisposition::Lowered),
    ("call_argument", FallbackDisposition::Wrapper),
    ("na_argument", FallbackDisposition::Wrapper),
    ("nz_argument", FallbackDisposition::Wrapper),
    ("if_condition", FallbackDisposition::Wrapper),
    ("ternary_condition", FallbackDisposition::Wrapper),
    ("ternary_arm", FallbackDisposition::Wrapper),
];

impl FallbackReason {
    /// Stable snake-case name of this family: the audit's key, and the
    /// bridge between the variant and [`FALLBACK_DISPOSITIONS`].
    #[must_use]
    pub fn wire_name(&self) -> &'static str {
        match self {
            Self::FunctionBodyBinding { .. } => "function_body_binding",
            Self::ReassignUndeclared { .. } => "reassign_undeclared",
            Self::HistoryUndeclared { .. } => "history_undeclared",
            Self::DottedName { .. } => "dotted_name",
            Self::BareCall { .. } => "bare_call",
            Self::UnknownCallee { .. } => "unknown_callee",
            Self::OverloadedCallee => "overloaded_callee",
            Self::MissingDefault => "missing_default",
            Self::TooManyPositionals => "too_many_positionals",
            Self::UnknownArgument { .. } => "unknown_argument",
            Self::CastArity { .. } => "cast_arity",
            Self::CastShape { .. } => "cast_shape",
            Self::NaArity => "na_arity",
            Self::NzArity => "nz_arity",
            Self::InputMissingDefault => "input_missing_default",
            Self::ComputedInputDefault => "computed_input_default",
            Self::DottedHome => "dotted_home",
            Self::Loops => "loops",
            Self::Break => "break",
            Self::TupleUnpack => "tuple_unpack",
            Self::FieldRead => "field_read",
            Self::InstanceCall => "instance_call",
            Self::CaptureValue { .. } => "capture_value",
            Self::CaptureStatement { .. } => "capture_statement",
            Self::PlotUntitled => "plot_untitled",
            Self::BuiltinStatement { .. } => "builtin_statement",
            Self::CaptureShape { .. } => "capture_shape",
            Self::OutsideBuiltin { .. } => "outside_builtin",
            Self::UndeclaredRead { .. } => "undeclared_read",
            Self::EnumMember => "enum_member",
            Self::ComputedSeriesHistory { .. } => "computed_series_history",
            Self::StatementCall { .. } => "statement_call",
            Self::CallArgument { .. } => "call_argument",
            Self::NaArgument { .. } => "na_argument",
            Self::NzArgument { .. } => "nz_argument",
            Self::IfCondition { .. } => "if_condition",
            Self::TernaryCondition { .. } => "ternary_condition",
            Self::TernaryArm { .. } => "ternary_arm",
        }
    }
}

/// The closeout disposition registered for one family name.
#[must_use]
pub fn fallback_disposition(name: &str) -> Option<FallbackDisposition> {
    FALLBACK_DISPOSITIONS
        .iter()
        .find(|(family, _)| *family == name)
        .map(|(_, disposition)| *disposition)
}

/// Every instruction name the wire form carries: the round-trip suite
/// asserts the fixture covers each one, so a new variant is never silently
/// left out of the codec gate.
pub const INSTRUCTION_NAMES: &[&str] = &[
    "const",
    "copy",
    "binop",
    "unop",
    "builtin",
    "history",
    "gload",
    "gstore_loop",
    "gstore",
    "gstore_init",
    "call",
    "jump",
    "jump_if_false",
    "jump_if_na",
    "return",
    "plot",
    "plot_candle",
    "bar_color",
    "plot_shape",
    "hline",
    "fill",
    "coalesce",
    "for_init",
    "while_tick",
    "for_in_init",
    "array_get",
    "array_new",
    "destructure",
    "journal_write",
    "plot_tag",
    "var_decl",
    "var_store",
    "var_read",
    "field_get",
    "obj_new",
    "instance_call",
    "this_read",
    "this_field_get",
    "this_field_set",
    "alias_read",
    "alias_field_get",
    "field_put",
    "switch_eq",
    "fallback",
    "local_history",
    "dotted_get",
    "request_security",
    "request_lower_tf",
    "strategy_read",
    "set_map_prov",
    "refuse",
    "runtime_refuse",
    "greassign",
    "enum_mark",
    "enum_read",
    "alias_flat_read",
    "noop",
];

/// One item of a headless `request.security` expression list, compiled to
/// the dataset-tail subset the harness serves. The tree-walk walks the same
/// subset over its AST; both sides read the tail through the shared
/// `security_leg`/`binary`/`batch_security` leaves, so values and refusals
/// agree by construction.
#[derive(Debug, Clone, PartialEq)]
pub enum SecurityExpr {
    /// Bare leg or named column read from the confirmed tail.
    Leg(SymbolId),
    /// Literal value from the module's constant pool.
    Const(ConstId),
    /// Kernel-backed `ta.*` call: an optional tail source leg plus the
    /// chart-scope argument slots the call site evaluated.
    Call {
        /// Interned method name (`sma`, `atr`, ...).
        method: SymbolId,
        /// First-argument tail column, when the method takes a source.
        source: Option<SymbolId>,
        /// Chart-scope argument slots after the source.
        chart_args: Vec<SlotId>,
    },
    /// Pointwise scalar operator.
    Binary {
        /// Operator.
        op: BinOp,
        /// Left operand.
        left: Box<SecurityExpr>,
        /// Right operand.
        right: Box<SecurityExpr>,
    },
}

/// One executable operation. Every operand is a dense id; no instruction
/// hashes or allocates per execution.
#[derive(Debug, Clone, PartialEq)]
pub enum Instruction {
    /// `dst = consts[value]`.
    Const {
        /// Destination slot.
        dst: SlotId,
        /// Constant id.
        value: ConstId,
    },
    /// `dst = src`.
    Copy {
        /// Destination slot.
        dst: SlotId,
        /// Source slot.
        src: SlotId,
    },
    /// `dst = lhs op rhs`.
    BinOp {
        /// Destination slot.
        dst: SlotId,
        /// Operator.
        op: BinOp,
        /// Left operand slot.
        lhs: SlotId,
        /// Right operand slot.
        rhs: SlotId,
    },
    /// `dst = op src`.
    UnOp {
        /// Destination slot.
        dst: SlotId,
        /// Operator.
        op: UnOp,
        /// Source slot.
        src: SlotId,
    },
    /// `dst = <chart leg or cursor>`.
    Builtin {
        /// Destination slot.
        dst: SlotId,
        /// Which builtin.
        var: BuiltinVarKind,
    },
    /// `dst = src[offset]` with tree-walk edge semantics (pre-window
    /// reads are `na`, never wrap, never panic).
    History {
        /// Destination slot.
        dst: SlotId,
        /// Global series slot (a leg or a user variable column).
        src: SlotId,
        /// Bar offset (negative looks back; pre-window reads are `na`,
        /// never wrap, never panic).
        offset: i32,
    },
    /// `dst = globals[slot]`: read a module-level series column for the
    /// current bar.
    GLoad {
        /// Destination frame slot.
        dst: SlotId,
        /// Global column.
        slot: SlotId,
    },
    /// `globals[slot] = src` without touching plot tags: loop-variable
    /// binding, mirroring the tree-walk's assign-without-refresh path
    /// (`for_step` and `for ... in` never refresh plot tags).
    GStoreLoop {
        /// Global column.
        slot: SlotId,
        /// Source frame slot.
        src: SlotId,
    },
    /// `globals[slot] = src`: write a module-level series column for the
    /// current bar.
    GStore {
        /// Global column.
        slot: SlotId,
        /// Source frame slot.
        src: SlotId,
        /// Plot tag when the stored value comes straight from a `plot()`
        /// call: the variable tags to that capture site for `fill()`
        /// pairing (exactly the tree-walk's plot-variable discipline).
        /// `None` clears a stale tag.
        plot_tag: Option<ConstId>,
    },
    /// `globals[slot] = src`, but only on the first evaluated bar:
    /// `var`/`varip` initializers run once (warmup bars never seed them),
    /// exactly like the tree-walk's declaration gate. Later bars skip.
    GStoreInit {
        /// Global column.
        slot: SlotId,
        /// Source frame slot.
        src: SlotId,
    },
    /// `globals[slot] = src` for a `:=` rebind: the store fires only when
    /// the column is already recorded (or persistent, whose declaration
    /// alone counts as bound); otherwise the run refuses naming the
    /// target, exactly like the tree-walk's rebind rule. A plain `=`
    /// declares, so it keeps the unconditional `GStore`; only `:=`
    /// lowers here, which is why the compiler needs no other strict
    /// shape.
    GReassign {
        /// Global column.
        slot: SlotId,
        /// Source frame slot.
        src: SlotId,
        /// Plot tag, same discipline as `GStore`.
        plot_tag: Option<ConstId>,
    },
    /// Call a user function or builtin namespace. Builtin namespace calls
    /// (`ta.*`, `input.*`) delegate to the existing bridges at run time;
    /// the instruction only names them.
    Call {
        /// Destination slot, or `None` for statement-position calls.
        dst: Option<SlotId>,
        /// Callee.
        target: CallTarget,
        /// Argument names and value slots, in call order.
        args: Vec<CallArgSlot>,
        /// Caller homes for write-back (`Some` id for bare-variable
        /// homes, `None` otherwise): after the call, a
        /// collection-typed result writes back through `assign_path`
        /// exactly like the tree-walk. Dotted homes never lower (their
        /// call falls back with a named reason).
        homes: Vec<Option<SymbolId>>,
        /// Explicit generic arguments (only `map.new` reads these today).
        type_args: Vec<CallTypeArg>,
        /// Precomputed site suffix, interned once per compile
        /// (`ta.sma@[..]`, `input@[..]`, `{module}::{name}@{args:?}`):
        /// the loop prefixes the live call stack for `ta.*`,
        /// reproducing tree-walk site identity byte-for-byte without
        /// re-rendering the AST per bar.
        site: SymbolId,
    },
    /// Unconditional jump to the block-relative instruction index.
    Jump {
        /// Destination instruction index.
        target: usize,
    },
    /// Jump when `cond` is falsy (`false` or `na`).
    JumpIfFalse {
        /// Condition slot.
        cond: SlotId,
        /// Destination instruction index.
        target: usize,
        /// Which construct owns the jump (refusal text for non-bool
        /// conditions names it, exactly like the tree-walk).
        ctx: CondContext,
    },
    /// Jump when the condition is `na`: the third arm of ternary
    /// lowering (`na` condition reads absence, never the else branch).
    JumpIfNa {
        /// Condition slot.
        cond: SlotId,
        /// Destination instruction index.
        target: usize,
    },
    /// Return from the current block.
    Return {
        /// Returned value slot, or `None` for a bare return.
        src: Option<SlotId>,
    },
    /// Emit one `plot()` capture for the current bar.
    Plot {
        /// Series title: a pool constant for statically named series, or
        /// an evaluated slot for computed titles (a string names the
        /// series; anything else, including `na`, falls back to the
        /// run-time `plot:{n}` default at first sight, exactly like the
        /// tree-walk).
        title: PlotTitle,
        /// Series value slot.
        src: SlotId,
        /// Call-site key constant (string).
        site: ConstId,
        /// Display-handle slot, or `None` when the call names none.
        display: Option<SlotId>,
    },
    /// Emit one `plotcandle()` capture for the current bar.
    PlotCandle {
        /// Series slots: open, high, low, close in order.
        open: SlotId,
        /// Series slots: open, high, low, close in order.
        high: SlotId,
        /// Series slots: open, high, low, close in order.
        low: SlotId,
        /// Series slots: open, high, low, close in order.
        close: SlotId,
        /// Title constant, or `None` for the run-time `plotcandle:{n}` default.
        title: Option<ConstId>,
        /// Call-site key constant.
        site: ConstId,
        /// Display-handle slot, or `None` when the call names none.
        display: Option<SlotId>,
        /// Color-handle slot, or `None` when the call names none.
        color: Option<SlotId>,
        /// Wick-color slot, or `None` when the call names none.
        wickcolor: Option<SlotId>,
        /// Border-color slot, or `None` when the call names none.
        bordercolor: Option<SlotId>,
    },
    /// Emit one `barcolor()` capture for the current bar.
    BarColor {
        /// Color series slot.
        src: SlotId,
        /// Title constant, or `None` for the run-time `barcolor:{n}` default.
        title: Option<ConstId>,
        /// Call-site key constant.
        site: ConstId,
    },
    /// Emit one `plotshape()` capture for the current bar.
    PlotShape {
        /// Marker series slot.
        src: SlotId,
        /// Title constant, or `None` for the run-time `plotshape:{n}` default.
        title: Option<ConstId>,
        /// Call-site key constant.
        site: ConstId,
    },
    /// Emit one `hline()` capture for the current bar.
    Hline {
        /// Level series slot.
        src: SlotId,
        /// Title constant, or `None` for the run-time `hline:{n}` default.
        title: Option<ConstId>,
        /// Call-site key constant.
        site: ConstId,
    },
    /// Emit one `fill()` capture for the current bar.
    Fill {
        /// First paired leg.
        first: FillLeg,
        /// Second paired leg.
        second: FillLeg,
        /// Title constant, or `None` for the run-time `fill:{n}` default.
        title: Option<ConstId>,
        /// Call-site key constant.
        site: ConstId,
        /// Resolved color slot (named `color` with run-time gradient
        /// fallback already applied by an explicit `Coalesce`), or `None`.
        color: Option<SlotId>,
    },
    /// `dst` takes the first present value of `options` in order, or
    /// absence when all are absent (run-time na-fallback chains like a
    /// `fill` gradient ink).
    Coalesce {
        /// Destination slot.
        dst: SlotId,
        /// Candidate value slots in preference order.
        options: Vec<SlotId>,
    },
    /// Initialize a numeric `for` loop: whole-number both bounds (the
    /// tree-walk's refusal shapes), refuse an over-long range outright,
    /// and seed the counter, signed step, and direction flag. The loop
    /// variable itself binds per iteration through `GStore`, exactly like
    /// the tree-walk's per-step assign.
    ForInit {
        /// Lowered start bound.
        first: SlotId,
        /// Lowered end bound.
        last: SlotId,
        /// Counter temp, seeded to the start bound.
        counter: SlotId,
        /// Signed step temp (direction times stride).
        step: SlotId,
        /// Direction flag temp (`first <= last`).
        ascending: SlotId,
        /// Lowered stride (`by` value, one when absent).
        stride: SlotId,
    },
    /// Tick a `while` loop: refuse past the trip budget with the
    /// tree-walk's message, else count this iteration.
    WhileTick {
        /// Trip counter temp.
        trip: SlotId,
    },
    /// Initialize a `for ... in` loop: the iterable must be an array
    /// (the tree-walk's refusals, including `na`), its length must fit
    /// the trip budget, and the length binds for the bound check.
    ForInInit {
        /// Lowered iterable value.
        arr: SlotId,
        /// Length temp.
        len: SlotId,
    },
    /// Index one array element: bounds-checked, absence stays absence.
    ArrayGet {
        /// Destination slot.
        dst: SlotId,
        /// Array value slot.
        arr: SlotId,
        /// Integer index slot.
        idx: SlotId,
        /// History fallback column when the value is not an array:
        /// `None` refuses compound expressions exactly like the
        /// tree-walk, `Some` reads the series history instead.
        hist: Option<SlotId>,
    },
    /// Pack evaluated elements into an array value, in order. Absent
    /// elements stay absent, exactly like the tree-walk's literal.
    ArrayNew {
        /// Destination slot.
        dst: SlotId,
        /// Element value slots, in order.
        options: Vec<SlotId>,
    },
    /// Unpack one array value into per-element temps: length-check plus
    /// ordered extract, mirroring the tree-walk's destructure arm. Arity
    /// and kind refusals route through the shared destructure
    /// constructors; per-item homes lower as the following `Copy` /
    /// `GStoreLoop` stores (never here).
    Destructure {
        /// Element destination slots, in target order.
        dsts: Vec<SlotId>,
        /// Array value slot.
        arr: SlotId,
    },
    /// Journaled write of one caller-visible name inside an escaping
    /// function: the loop logs the caller home's entry value once per
    /// call, writes the value through, and reverts in reverse order on
    /// return — mirroring the tree-walk's journal teardown order. The
    /// mirror temp keeps same-bar reads identical without touching the
    /// journal again.
    JournalWrite {
        /// Caller home name (resolved at run time: module globals first,
        /// then caller frame params, mirroring the tree-walk's shared
        /// namespace as seen from the callee).
        home: SymbolId,
        /// Value slot.
        src: SlotId,
        /// Frame-local mirror slot for same-bar reads.
        mirror: SlotId,
        /// Strict rebinds refuse unresolvable homes through the shared
        /// constructor; plain binds fall back to a fresh mirror instead.
        strict: bool,
    },
    /// Tag (or untag) a name for `fill()` pairing without touching any
    /// value: an in-function `plot()` value tags the run-global site
    /// while the plotted value itself stays frame-local, mirroring the
    /// tree-walk's tag-on-assign split. Clearing on non-plot writes
    /// mirrors the tree's stale-tag removal.
    PlotTag {
        /// Bound name carrying the tag.
        name: SymbolId,
        /// Capture site, or `None` to clear a stale tag.
        tag: Option<ConstId>,
    },
    /// Declare one in-function `var`/`varip` cell: seeds the
    /// per-instance cell on first evaluation, never again — mirroring
    /// the tree-walk's once-per-run initializer under the
    /// declaration-site identity.
    VarDecl {
        /// Declaration ordinal within the body block.
        ordinal: u32,
        /// Intrabar (`varip`) cells reset at the bar advance.
        is_varip: bool,
        /// Initializer value slot.
        src: SlotId,
        /// Bound name (messages and snapshot views).
        name: SymbolId,
    },
    /// Store one in-function `var`/`varip` cell: strict rebinds refuse
    /// unseeded cells through the shared constructor, plain binds seed
    /// through the journal path exactly like the tree-walk's
    /// declare-then-assign order.
    VarStore {
        /// Declaration ordinal within the body block.
        ordinal: u32,
        /// Intrabar (`varip`) cells reset at the bar advance.
        is_varip: bool,
        /// Value slot.
        src: SlotId,
        /// Bound name (messages and snapshot views).
        name: SymbolId,
        /// Strict rebinds refuse unseeded cells; plain binds fall back
        /// to the journal path instead.
        strict: bool,
    },
    /// Read one in-function `var`/`varip` cell: unseeded cells refuse
    /// naming the variable, history reads walk the per-instance past
    /// with the tree-walk's exact edges.
    VarRead {
        /// Destination slot.
        dst: SlotId,
        /// Declaration ordinal within the body block.
        ordinal: u32,
        /// Bound name (messages).
        name: SymbolId,
        /// History offset, or `None` for the current value.
        hist: Option<i32>,
    },
    /// Read one object field through the shared table: unknown fields
    /// and non-object receivers refuse through the shared constructors
    /// with the tree-walk's exact wordings.
    FieldGet {
        /// Destination slot.
        dst: SlotId,
        /// Receiver value slot.
        obj: SlotId,
        /// Field name.
        field: SymbolId,
    },
    /// Construct one user-type instance from evaluated field values:
    /// defaults evaluate per construction at the call site, exactly
    /// like the tree-walk's field-by-field build.
    ObjNew {
        /// Destination slot.
        dst: SlotId,
        /// Declared type name.
        type_name: SymbolId,
        /// Field values in declaration order.
        fields: Vec<(SymbolId, SlotId)>,
    },
    /// Call one instance method: evaluate the receiver once, dispatch on
    /// its runtime type to a link-time target, and write the mutated
    /// copy back through its home. Rvalue receivers drop the mutation
    /// and keep only the return — exactly the tree-walk's take/put
    /// lifecycle. The call-site identity composes at run time from the
    /// receiver type plus the precomputed argument rendering, byte-
    /// identical to the tree-walk's stacked sites.
    InstanceCall {
        /// Destination slot (`None` in statement position).
        dst: Option<SlotId>,
        /// Receiver value slot.
        recv: SlotId,
        /// Method name.
        method: SymbolId,
        /// Call arguments for the receiver-skipped parameters.
        args: Vec<CallArgSlot>,
        /// Per-argument write-back homes (collections persist).
        homes: Vec<Option<SymbolId>>,
        /// Precomputed argument rendering (`{args:?}`).
        args_debug: ConstId,
        /// Receiver write-back home (`None` for rvalues).
        home: Option<CallHome>,
        /// Statement tail position: mutating collection calls persist
        /// through the home while the value still flows on (the
        /// statement-then-value protocol shared with the tree-walk).
        stmt: bool,
    },
    /// Read the innermost method receiver as a value.
    ThisRead {
        /// Destination slot.
        dst: SlotId,
    },
    /// Read one field of the innermost method receiver.
    ThisFieldGet {
        /// Destination slot.
        dst: SlotId,
        /// Field name.
        field: SymbolId,
    },
    /// Write one field of the innermost method receiver in place.
    ThisFieldSet {
        /// Field name.
        field: SymbolId,
        /// Value slot.
        src: SlotId,
    },
    /// Read one method-alias binding: the nearest enclosing receiver
    /// slot with that alias wins, exactly like the tree-walk's top-down
    /// receiver search; with no live slot the frame and shared columns
    /// answer, and truly unbound names refuse.
    AliasRead {
        /// Destination slot.
        dst: SlotId,
        /// Alias name.
        name: SymbolId,
        /// In-function cell ordinal when the reading body declares one.
        var_ordinal: Option<u32>,
    },
    /// Read one method-alias field through the live slot: the nearest
    /// enclosing receiver with that alias wins, exactly like the
    /// tree-walk's top-down stack search; unknown fields refuse naming
    /// the full path. With no live slot the shared dotted bindings
    /// answer first, then the head instance reads through its table —
    /// mirroring the tree-walk's namespace-first order.
    AliasFieldGet {
        /// Destination slot.
        dst: SlotId,
        /// Alias name (method receiver spelling).
        name: SymbolId,
        /// Field name.
        field: SymbolId,
        /// In-function cell ordinal of the head when the reading body
        /// declares one.
        var_ordinal: Option<u32>,
    },
    /// Write one object field through its home: take the instance out,
    /// check the field, and write the mutated copy back — mirroring the
    /// tree-walk's take/put lifecycle. Take misses refuse naming the
    /// target exactly like the tree-walk.
    FieldPut {
        /// Receiver home.
        home: CallHome,
        /// Field name.
        field: SymbolId,
        /// Value slot.
        src: SlotId,
    },
    /// First-match switch comparison: the tree-walk's `switch_eq`
    /// (numeric tolerance, string/bool/enum identity, everything else
    /// absent) evaluated at run time.
    SwitchEq {
        /// Destination slot.
        dst: SlotId,
        /// Scrutinee value slot.
        target: SlotId,
        /// Arm pattern value slot.
        probe: SlotId,
    },
    /// Run the retained source subtree through the tree-walk instead:
    /// the coverage escape hatch. `reason` names the unlowered construct
    /// family; `node` is the parser-assigned source identity for
    /// diagnostics.
    Fallback {
        /// Unlowered construct family.
        reason: FallbackReason,
        /// Source node identity.
        node: NodeId,
    },
    /// `dst = <local name> history[offset]`: the per-call past of a
    /// plain frame local or parameter. The key is the open call path plus
    /// the body and name, so two call sites (or two activations) never
    /// share a history — exactly the tree-walk's qualified frame past.
    /// A shadowing shared column serves through `bare` when the qualified
    /// key never recorded, and dynamic offsets evaluate once per bar —
    /// both exactly like the tree-walk's qualified-or-bare rule.
    LocalHistory {
        /// Destination slot.
        dst: SlotId,
        /// Frame slot holding the local's current value.
        slot: SlotId,
        /// Local or parameter name (history key and refusal text).
        name: SymbolId,
        /// Bars back (`Const`) or value slot holding them (`Slot`); `0`
        /// reads the current value.
        offset: HistOffset,
        /// Shared column for the same spelling, when one exists: the
        /// bare-column past serves when the qualified key never recorded.
        /// `None` keeps the qualified-only shape (no shadowing column).
        bare: Option<SlotId>,
    },
    /// `dst = root.field`: a dotted variable read. Single-dot names follow
    /// the tree-walk exactly: a flat variable with the dotted spelling wins
    /// when the engine recorded one, an object root serves the field, and
    /// anything else refuses as an undefined variable naming the whole
    /// dotted spelling.
    DottedGet {
        /// Destination slot.
        dst: SlotId,
        /// Root value slot (`b` in `b.field`).
        root: SlotId,
        /// Full dotted spelling (history/refusal text).
        name: SymbolId,
        /// Field name.
        field: SymbolId,
        /// Flat write-back column for the dotted spelling, when a call
        /// home may have recorded one: a recorded flat wins over the
        /// field lookup, exactly like the tree-walk's flat-first read.
        /// `None` keeps the field-only shape (no home exists).
        flat: Option<SlotId>,
    },
    /// Serve one headless `request.security` call from the run's injected
    /// tapes: the confirmed tail feeds every expression item, and an
    /// unregistered pair refuses naming the live fetch.
    RequestSecurity {
        /// Destination slot (single value or column array).
        dst: Option<SlotId>,
        /// Symbol argument slot.
        symbol: SlotId,
        /// Timeframe argument slot.
        timeframe: SlotId,
        /// `barmerge` gaps argument slot, when given.
        gaps: Option<SlotId>,
        /// `barmerge` lookahead argument slot, when given.
        lookahead: Option<SlotId>,
        /// `calc_bars_count` slot, when given.
        calc_bars: Option<SlotId>,
        /// Whether the call asked for one expression (not a list).
        single: bool,
        /// Expression items in source order.
        items: Vec<SecurityExpr>,
        /// Call-site key (diagnostics only).
        site: SymbolId,
    },
    /// Serve one headless `request.security_lower_tf` call from the run's
    /// injected tapes as one array per requested chart leg.
    RequestLowerTf {
        /// Destination slot (array of column arrays).
        dst: Option<SlotId>,
        /// Symbol argument slot.
        symbol: SlotId,
        /// Timeframe argument slot.
        timeframe: SlotId,
        /// Requested chart-leg names in source order.
        legs: Vec<SymbolId>,
        /// `calc_bars_count` slot, when given.
        calc_bars: Option<SlotId>,
        /// Call-site key (diagnostics only).
        site: SymbolId,
    },
    /// `dst = state.read(member)` for one headless `strategy.<member>`
    /// read. The shared harness state answers, so values and refusals match
    /// the tree-walk's `strategy.*` reads exactly.
    StrategyRead {
        /// Destination slot.
        dst: SlotId,
        /// Matrix member name (`long`, `equity`, `closedtrades`, ...).
        member: SymbolId,
    },
    /// Set or clear map-entry provenance for one name. A tag makes later
    /// method calls and field writes through `name` persist into the map
    /// entry as well as the variable, exactly like the tree-walk's
    /// provenance tag; every other assignment clears it.
    SetMapProv {
        /// Variable being bound.
        name: SymbolId,
        /// `Some((map variable, evaluated key))` tags the name; `None`
        /// clears a stale tag.
        tag: Option<(SymbolId, SlotId)>,
    },
    /// Refuse the run at this program point with a fixed message. Used for
    /// deterministic call-shape refusals (unknown callee, arity, missing
    /// input default) so the module stays compiled and the refusal fires
    /// exactly where the tree-walk would raise it. Never panics; never a
    /// confidence decision.
    Refuse {
        /// Rendered refusal text (string constant).
        reason: ConstId,
    },
    /// Refuse the run at this program point with a message computed at run
    /// time. Used for `runtime.error(<expr>)`: the message slot evaluates
    /// exactly like the tree-walk's argument (a string is raised verbatim,
    /// any other value takes the `runtime error: {value}` shape, absence
    /// raises the bare wording), so the module stays compiled and a
    /// dead-branch error stays silent until reached. Never panics; never
    /// a confidence decision.
    RuntimeRefuse {
        /// Message value slot.
        message: SlotId,
    },
    /// Explicit no-op (alignment padding, patched-out jumps).
    Noop,
    /// Register one enum's member set at its declaration point, first
    /// execution winning exactly like the tree-walk's declare-once
    /// table. Later declarations of the same name (conditional,
    /// duplicate, or per-call) find the table occupied and keep it, so
    /// static declaration order never matters at run time.
    EnumMark {
        /// Enum name.
        enm: SymbolId,
        /// Declared member names, in source order.
        members: Vec<SymbolId>,
    },
    /// Read one enum member through the registered table: a recorded
    /// flat variable wins first (a call home may have recorded the
    /// dotted spelling), a registered pair serves the same identity
    /// handle the tree-walk serves, and an unregistered pair refuses
    /// naming the full spelling — exactly like the tree-walk's member
    /// lookup. Bodies read through this (call timing is unknowable
    /// statically); top-level uses keep folding to constants when the
    /// declaration order proves registration.
    EnumRead {
        /// Destination slot.
        dst: SlotId,
        /// Enum name.
        enm: SymbolId,
        /// Member name.
        member: SymbolId,
        /// Full dotted spelling (refusal text).
        name: SymbolId,
        /// Flat write-back column for the dotted spelling.
        flat: SlotId,
    },
    /// Read one import-alias-qualified name with a possible flat shadowing:
    /// a recorded flat variable wins first (a call home may have recorded
    /// the dotted spelling), otherwise the remainder evaluates through
    /// the shared bare-name rules — exactly like the tree-walk's
    /// flat-first alias read.
    AliasFlatRead {
        /// Destination slot.
        dst: SlotId,
        /// Flat write-back column for the dotted spelling.
        flat: SlotId,
        /// Remainder value slot (bare-name rules, lowered eagerly).
        src: SlotId,
    },
}

/// One function body (or the module top level): a flat instruction list
/// with its own frame layout.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompiledBlock {
    /// Instructions in execution order; jump targets index this vec.
    pub instrs: Vec<Instruction>,
    /// Frame size: params, locals, and temps occupy `0..slots`.
    pub slots: u32,
    /// Parameter names in slot order (`slots[0..params.len()]`); empty
    /// for the entry block. Shared: cloned handles never duplicate the
    /// names, so per-call binding borrows instead of allocating.
    pub params: Arc<[String]>,
    /// Named frame slots (parameters plus body-bound locals) by name:
    /// lets a nested call resolve a caller body-local home at run time,
    /// mirroring the tree-walk's shared namespace where nested calls
    /// observe caller bindings. Shared like `params`; empty for blocks
    /// whose frames hold no names.
    pub frame_names: Arc<HashMap<String, u32>>,
    /// Frame slots whose per-call past the body reads. The loop records
    /// each slot's final value on return, so a nested `x[n]` sees the past
    /// of its own call site — the tree-walk's qualified frame history.
    /// Empty for bodies with no local history reads.
    pub history_locals: Arc<[(SymbolId, u32)]>,
}

impl CompiledBlock {
    /// Empty block.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Push one instruction, returning its block-relative index (the value
    /// a later jump patches through).
    pub fn push(&mut self, instr: Instruction) -> usize {
        self.instrs.push(instr);
        self.instrs.len() - 1
    }
}

/// One linked method body: where it runs, how to bind it, and which
/// receiver name its body spells.
#[derive(Debug, Clone, PartialEq)]
pub struct MethodEntry {
    /// Body block index within the owning module.
    pub block: usize,
    /// Parameters after the receiver, with literal defaults interned.
    /// Non-literal defaults stay tree-walk when a call leaves them
    /// unfilled, exactly like plain user calls.
    pub params: Vec<(String, Option<ConstId>)>,
    /// Whether any parameter carries a non-literal default.
    pub has_computed: bool,
    /// Receiver parameter name (`this`-slot alias in method bodies).
    pub receiver: String,
}

/// One compiled module: entry block plus function blocks, the slot count,
/// and the constant pool.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledModule {
    /// Module key, exactly the `LinkedModule.key` this was compiled from.
    pub key: String,
    /// Blocks; `blocks[entry]` is the module top level.
    pub blocks: Vec<CompiledBlock>,
    /// Entry block index.
    pub entry: usize,
    /// Module-level series columns by variable name: the persistent
    /// namespace `GLoad`/`GStore` address. Global ids occupy a separate
    /// space from frame slots.
    pub globals: HashMap<String, SlotId>,
    /// Function name → body block. The linker resolves cross-module calls
    /// through this map instead of re-walking the import graph.
    pub functions: HashMap<String, usize>,
    /// Method bodies by (receiver type, method): the linker builds the
    /// runtime dispatch index from every module's table, and the loop
    /// type-tests receivers against it. Same-named methods on different
    /// types coexist here (never overloaded against each other).
    pub methods: HashMap<(String, String), MethodEntry>,
    /// Overloaded names: calls to these resolve at run time on the
    /// tree-walk (the linker refuses to bind them).
    pub overloaded: HashSet<String>,
    /// Global columns with intrabar persistence: the loop drops these
    /// at the bar advance (`VarStore::next_bar` discipline).
    pub varip: HashSet<SlotId>,
    /// Module constants, shared across links.
    pub consts: Arc<ConstPool>,
    /// Interned open names (call sites, callee names, fallback payloads),
    /// shared across links.
    pub symbols: Arc<SymbolTable>,
}

/// Reserved global columns mirroring the chart legs, so history offsets
/// over legs need no per-bar materialization: `close[1]` is a direct
/// indexed load of column [`LEG_CLOSE`].
pub const LEG_OPEN: u32 = 0;
/// Reserved global column for the `high` leg.
pub const LEG_HIGH: u32 = 1;
/// Reserved global column for the `low` leg.
pub const LEG_LOW: u32 = 2;
/// Reserved global column for the `close` leg.
pub const LEG_CLOSE: u32 = 3;
/// Reserved global column for the `volume` leg.
pub const LEG_VOLUME: u32 = 4;
/// Reserved global column for the `time` leg.
pub const LEG_TIME: u32 = 5;
/// Reserved global column for the `hl2` leg.
pub const LEG_HL2: u32 = 6;
/// Reserved global column for the `hlc3` leg.
pub const LEG_HLC3: u32 = 7;
/// Reserved global column for the `ohlc4` leg.
pub const LEG_OHLC4: u32 = 8;
/// Reserved global column for the `wcl` leg.
pub const LEG_WCL: u32 = 9;
/// Global column count reserved for legs; user variables start here.
pub const LEG_COUNT: u32 = 10;

/// Sparse compiled program: one module per linked key. The map mirrors
/// the linker's module set instead of merging it, so per-module identity
/// (aliases, versions, call-site keys) is preserved by construction.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompiledProgram {
    modules: HashMap<String, CompiledModule>,
}

impl CompiledProgram {
    /// Empty program.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert one compiled module under its key.
    pub fn insert(&mut self, module: CompiledModule) {
        self.modules.insert(module.key.clone(), module);
    }

    /// Fetch one compiled module by linked key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&CompiledModule> {
        self.modules.get(key)
    }

    /// Number of compiled modules.
    #[must_use]
    pub fn len(&self) -> usize {
        self.modules.len()
    }

    /// Whether the program holds no modules.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }
}

/// Wire-format refusal: truncated input, unknown tag, bad text, or bytes
/// past the end of the block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Input ends mid-instruction.
    UnexpectedEof,
    /// Unknown instruction tag byte.
    UnknownTag(u8),
    /// Length-prefixed text is not UTF-8.
    InvalidUtf8,
    /// Bytes remain after the block.
    TrailingBytes(usize),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnexpectedEof => write!(f, "bytecode ends mid-instruction"),
            Self::UnknownTag(tag) => write!(f, "unknown instruction tag {tag}"),
            Self::InvalidUtf8 => write!(f, "bytecode text is not UTF-8"),
            Self::TrailingBytes(n) => write!(f, "{n} trailing bytes after the block"),
        }
    }
}

impl std::error::Error for DecodeError {}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_i32(out: &mut Vec<u8>, value: i32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_i64(out: &mut Vec<u8>, value: i64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_f64(out: &mut Vec<u8>, value: f64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_str(out: &mut Vec<u8>, text: &str) {
    put_u32(out, text.len() as u32);
    out.extend_from_slice(text.as_bytes());
}

/// Encode one interned name by resolving it through `symbols` and
/// writing the text inline, so the wire form stays self-describing.
/// Unknown ids encode as empty (decode interns the same empty text).
fn put_symbol(out: &mut Vec<u8>, symbols: &SymbolTable, id: SymbolId) {
    put_str(out, symbols.resolve(id).unwrap_or(""));
}

/// Encode one optional constant id (presence byte plus payload).
fn put_opt_const(out: &mut Vec<u8>, id: Option<ConstId>) {
    match id {
        Some(value) => {
            out.push(1);
            put_u32(out, value.0);
        }
        None => out.push(0),
    }
}

/// Encode one optional slot (presence byte plus payload).
fn put_opt_slot(out: &mut Vec<u8>, slot: Option<SlotId>) {
    match slot {
        Some(value) => {
            out.push(1);
            put_u32(out, value.0);
        }
        None => out.push(0),
    }
}

/// Encode one `request.security` expression list.
fn put_security_items(out: &mut Vec<u8>, items: &[SecurityExpr], symbols: &SymbolTable) {
    put_u32(out, items.len() as u32);
    for item in items {
        put_security_expr(out, item, symbols);
    }
}

/// Encode one dataset-tail expression: a tag per shape plus its operands.
fn put_security_expr(out: &mut Vec<u8>, expr: &SecurityExpr, symbols: &SymbolTable) {
    match expr {
        SecurityExpr::Leg(name) => {
            out.push(0);
            put_symbol(out, symbols, *name);
        }
        SecurityExpr::Const(value) => {
            out.push(1);
            put_u32(out, value.0);
        }
        SecurityExpr::Call {
            method,
            source,
            chart_args,
        } => {
            out.push(2);
            put_symbol(out, symbols, *method);
            match source {
                Some(source) => {
                    out.push(1);
                    put_symbol(out, symbols, *source);
                }
                None => out.push(0),
            }
            put_u32(out, chart_args.len() as u32);
            for arg in chart_args {
                put_u32(out, arg.0);
            }
        }
        SecurityExpr::Binary { op, left, right } => {
            out.push(3);
            out.push(*op as u8);
            put_security_expr(out, left, symbols);
            put_security_expr(out, right, symbols);
        }
    }
}

/// Encode one receiver/write-back home. Shared by `InstanceCall` and
/// `FieldPut` so the two instructions can never disagree on the wire form.
fn put_call_home(out: &mut Vec<u8>, home: &CallHome, symbols: &SymbolTable) {
    match home {
        CallHome::Var { name, var_ordinal } => {
            out.push(0);
            put_symbol(out, symbols, *name);
            put_opt_u32(out, *var_ordinal);
        }
        CallHome::Innermost => out.push(1),
        CallHome::VarPath {
            name,
            var_ordinal,
            path,
        } => {
            out.push(2);
            put_symbol(out, symbols, *name);
            put_opt_u32(out, *var_ordinal);
            put_u32(out, path.len() as u32);
            for field in path {
                put_symbol(out, symbols, *field);
            }
        }
        CallHome::ThisField { field } => {
            out.push(3);
            put_symbol(out, symbols, *field);
        }
    }
}

/// Encode one optional `u32` (presence byte plus payload).
fn put_opt_u32(out: &mut Vec<u8>, value: Option<u32>) {
    match value {
        Some(value) => {
            out.push(1);
            put_u32(out, value);
        }
        None => out.push(0),
    }
}

/// Encode one call argument list with its write-back homes: shared by
/// plain calls and method calls so both spell the same wire form.
fn put_call_args(
    out: &mut Vec<u8>,
    args: &[CallArgSlot],
    homes: &[Option<SymbolId>],
    symbols: &SymbolTable,
) {
    put_u32(out, args.len() as u32);
    for arg in args {
        match &arg.name {
            Some(name) => {
                out.push(1);
                put_symbol(out, symbols, *name);
            }
            None => out.push(0),
        }
        put_u32(out, arg.slot.0);
    }
    put_u32(out, homes.len() as u32);
    for home in homes {
        match home {
            Some(path) => {
                out.push(1);
                put_symbol(out, symbols, *path);
            }
            None => out.push(0),
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl Reader<'_> {
    fn rest(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    fn byte(&mut self) -> Result<u8, DecodeError> {
        if self.position >= self.bytes.len() {
            return Err(DecodeError::UnexpectedEof);
        }
        let value = self.bytes[self.position];
        self.position += 1;
        Ok(value)
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        if self.rest() < 4 {
            return Err(DecodeError::UnexpectedEof);
        }
        let mut word = [0_u8; 4];
        word.copy_from_slice(&self.bytes[self.position..self.position + 4]);
        self.position += 4;
        Ok(u32::from_le_bytes(word))
    }

    fn i32(&mut self) -> Result<i32, DecodeError> {
        self.u32().map(|word| word as i32)
    }

    fn i64(&mut self) -> Result<i64, DecodeError> {
        if self.rest() < 8 {
            return Err(DecodeError::UnexpectedEof);
        }
        let mut word = [0_u8; 8];
        word.copy_from_slice(&self.bytes[self.position..self.position + 8]);
        self.position += 8;
        Ok(i64::from_le_bytes(word))
    }

    fn f64(&mut self) -> Result<f64, DecodeError> {
        if self.rest() < 8 {
            return Err(DecodeError::UnexpectedEof);
        }
        let mut word = [0_u8; 8];
        word.copy_from_slice(&self.bytes[self.position..self.position + 8]);
        self.position += 8;
        Ok(f64::from_le_bytes(word))
    }

    fn text(&mut self) -> Result<String, DecodeError> {
        let len = self.u32()? as usize;
        if self.rest() < len {
            return Err(DecodeError::UnexpectedEof);
        }
        let text = std::str::from_utf8(&self.bytes[self.position..self.position + len])
            .map_err(|_| DecodeError::InvalidUtf8)?;
        self.position += len;
        Ok(text.to_owned())
    }

    fn slot(&mut self) -> Result<SlotId, DecodeError> {
        self.u32().map(SlotId)
    }

    fn symbol(&mut self, symbols: &mut SymbolTable) -> Result<SymbolId, DecodeError> {
        let text = self.text()?;
        Ok(symbols.intern(&text))
    }

    fn opt_const(&mut self) -> Result<Option<ConstId>, DecodeError> {
        match self.byte()? {
            0 => Ok(None),
            1 => Ok(Some(self.const_id()?)),
            other => Err(DecodeError::UnknownTag(other)),
        }
    }

    fn opt_slot(&mut self) -> Result<Option<SlotId>, DecodeError> {
        match self.byte()? {
            0 => Ok(None),
            1 => Ok(Some(self.slot()?)),
            other => Err(DecodeError::UnknownTag(other)),
        }
    }

    fn const_id(&mut self) -> Result<ConstId, DecodeError> {
        self.u32().map(ConstId)
    }

    /// Decode one call argument list with its write-back homes, shared
    /// by plain calls and method calls.
    /// Decode one `request.security` expression list (see
    /// [`put_security_items`]).
    fn security_items(
        &mut self,
        symbols: &mut SymbolTable,
    ) -> Result<Vec<SecurityExpr>, DecodeError> {
        let count = self.u32()? as usize;
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            items.push(self.security_expr(symbols)?);
        }
        Ok(items)
    }

    /// Decode one dataset-tail expression (see [`put_security_expr`]).
    fn security_expr(&mut self, symbols: &mut SymbolTable) -> Result<SecurityExpr, DecodeError> {
        Ok(match self.byte()? {
            0 => SecurityExpr::Leg(self.symbol(symbols)?),
            1 => SecurityExpr::Const(ConstId(self.u32()?)),
            2 => {
                let method = self.symbol(symbols)?;
                let source = match self.byte()? {
                    0 => None,
                    1 => Some(self.symbol(symbols)?),
                    other => return Err(DecodeError::UnknownTag(other)),
                };
                let count = self.u32()? as usize;
                let mut chart_args = Vec::with_capacity(count);
                for _ in 0..count {
                    chart_args.push(self.slot()?);
                }
                SecurityExpr::Call {
                    method,
                    source,
                    chart_args,
                }
            }
            3 => {
                let op = decode_binop(self.byte()?)?;
                let left = Box::new(self.security_expr(symbols)?);
                let right = Box::new(self.security_expr(symbols)?);
                SecurityExpr::Binary { op, left, right }
            }
            other => return Err(DecodeError::UnknownTag(other)),
        })
    }

    /// Decode one receiver/write-back home (see [`put_call_home`]).
    fn call_home(&mut self, symbols: &mut SymbolTable) -> Result<CallHome, DecodeError> {
        Ok(match self.byte()? {
            0 => CallHome::Var {
                name: self.symbol(symbols)?,
                var_ordinal: self.opt_u32()?,
            },
            1 => CallHome::Innermost,
            2 => {
                let name = self.symbol(symbols)?;
                let var_ordinal = self.opt_u32()?;
                let count = self.u32()? as usize;
                let mut path = Vec::with_capacity(count);
                for _ in 0..count {
                    path.push(self.symbol(symbols)?);
                }
                CallHome::VarPath {
                    name,
                    var_ordinal,
                    path,
                }
            }
            3 => CallHome::ThisField {
                field: self.symbol(symbols)?,
            },
            other => return Err(DecodeError::UnknownTag(other)),
        })
    }

    /// Decode one optional `u32` (see [`put_opt_u32`]).
    fn opt_u32(&mut self) -> Result<Option<u32>, DecodeError> {
        match self.byte()? {
            0 => Ok(None),
            1 => Ok(Some(self.u32()?)),
            other => Err(DecodeError::UnknownTag(other)),
        }
    }

    fn call_args(
        &mut self,
        symbols: &mut SymbolTable,
    ) -> Result<(Vec<CallArgSlot>, Vec<Option<SymbolId>>), DecodeError> {
        let argc = self.u32()? as usize;
        let mut args = Vec::with_capacity(argc.min(64));
        for _ in 0..argc {
            let name = match self.byte()? {
                0 => None,
                1 => Some(self.symbol(symbols)?),
                other => return Err(DecodeError::UnknownTag(other)),
            };
            args.push(CallArgSlot {
                name,
                slot: self.slot()?,
            });
        }
        let homec = self.u32()? as usize;
        let mut homes = Vec::with_capacity(homec.min(64));
        for _ in 0..homec {
            homes.push(match self.byte()? {
                0 => None,
                1 => Some(self.symbol(symbols)?),
                other => return Err(DecodeError::UnknownTag(other)),
            });
        }
        Ok((args, homes))
    }
}

fn encode_const(out: &mut Vec<u8>, value: &ConstValue, symbols: &SymbolTable) {
    match value {
        ConstValue::Float(v) => {
            out.push(0);
            put_f64(out, *v);
        }
        ConstValue::Int(v) => {
            out.push(1);
            put_i64(out, *v);
        }
        ConstValue::Bool(v) => {
            out.push(2);
            out.push(u8::from(*v));
        }
        ConstValue::Str(text) => {
            out.push(3);
            put_str(out, text);
        }
        ConstValue::Color(packed) => {
            out.push(4);
            put_u32(out, *packed);
        }
        ConstValue::Enum { namespace, member } => {
            out.push(5);
            put_symbol(out, symbols, *namespace);
            put_symbol(out, symbols, *member);
        }
        ConstValue::Na => {
            out.push(6);
        }
    }
}

fn decode_const(
    reader: &mut Reader<'_>,
    symbols: &mut SymbolTable,
) -> Result<ConstValue, DecodeError> {
    match reader.byte()? {
        0 => Ok(ConstValue::Float(reader.f64()?)),
        1 => Ok(ConstValue::Int(reader.i64()?)),
        2 => Ok(ConstValue::Bool(reader.byte()? != 0)),
        3 => Ok(ConstValue::Str(reader.text()?)),
        4 => Ok(ConstValue::Color(reader.u32()?)),
        5 => Ok(ConstValue::Enum {
            namespace: reader.symbol(symbols)?,
            member: reader.symbol(symbols)?,
        }),
        6 => Ok(ConstValue::Na),
        tag => Err(DecodeError::UnknownTag(tag)),
    }
}

fn encode_namespace(namespace: BuiltinNamespace) -> u8 {
    match namespace {
        BuiltinNamespace::Ta => 0,
        BuiltinNamespace::Input => 1,
        BuiltinNamespace::Math => 2,
        BuiltinNamespace::Str => 3,
        BuiltinNamespace::Color => 4,
        BuiltinNamespace::Timeframe => 5,
        BuiltinNamespace::Core => 6,
        BuiltinNamespace::Array => 7,
        BuiltinNamespace::Matrix => 8,
        BuiltinNamespace::Map => 9,
        BuiltinNamespace::Strategy => 10,
        BuiltinNamespace::Drawing => 11,
        BuiltinNamespace::Alert => 12,
    }
}

fn decode_namespace(tag: u8) -> Result<BuiltinNamespace, DecodeError> {
    match tag {
        0 => Ok(BuiltinNamespace::Ta),
        1 => Ok(BuiltinNamespace::Input),
        2 => Ok(BuiltinNamespace::Math),
        3 => Ok(BuiltinNamespace::Str),
        4 => Ok(BuiltinNamespace::Color),
        5 => Ok(BuiltinNamespace::Timeframe),
        6 => Ok(BuiltinNamespace::Core),
        7 => Ok(BuiltinNamespace::Array),
        8 => Ok(BuiltinNamespace::Matrix),
        9 => Ok(BuiltinNamespace::Map),
        10 => Ok(BuiltinNamespace::Strategy),
        11 => Ok(BuiltinNamespace::Drawing),
        12 => Ok(BuiltinNamespace::Alert),
        other => Err(DecodeError::UnknownTag(other)),
    }
}

fn encode_capture_kind(kind: CaptureKind) -> u8 {
    match kind {
        CaptureKind::Fill => 0,
        CaptureKind::PlotShape => 1,
        CaptureKind::PlotCandle => 2,
        CaptureKind::BarColor => 3,
        CaptureKind::Hline => 4,
        CaptureKind::Alert => 5,
        CaptureKind::AlertCondition => 6,
        CaptureKind::Strategy => 7,
    }
}

fn decode_capture_kind(tag: u8) -> Result<CaptureKind, DecodeError> {
    match tag {
        0 => Ok(CaptureKind::Fill),
        1 => Ok(CaptureKind::PlotShape),
        2 => Ok(CaptureKind::PlotCandle),
        3 => Ok(CaptureKind::BarColor),
        4 => Ok(CaptureKind::Hline),
        5 => Ok(CaptureKind::Alert),
        6 => Ok(CaptureKind::AlertCondition),
        7 => Ok(CaptureKind::Strategy),
        other => Err(DecodeError::UnknownTag(other)),
    }
}

fn encode_type_arg(out: &mut Vec<u8>, arg: &CallTypeArg, symbols: &SymbolTable) {
    put_symbol(out, symbols, arg.name);
    put_u32(out, arg.args.len() as u32);
    for nested in &arg.args {
        encode_type_arg(out, nested, symbols);
    }
}

fn decode_type_arg(
    reader: &mut Reader<'_>,
    symbols: &mut SymbolTable,
) -> Result<CallTypeArg, DecodeError> {
    let name = reader.symbol(symbols)?;
    let count = reader.u32()? as usize;
    let mut args = Vec::with_capacity(count.min(8));
    for _ in 0..count {
        args.push(decode_type_arg(reader, symbols)?);
    }
    Ok(CallTypeArg { name, args })
}

fn encode_fallback(out: &mut Vec<u8>, reason: &FallbackReason, symbols: &SymbolTable) {
    macro_rules! named {
        ($id:expr, $name:expr) => {{
            out.push($id);
            put_symbol(out, symbols, *$name);
        }};
    }
    match reason {
        FallbackReason::FunctionBodyBinding { name } => named!(0, name),
        FallbackReason::ReassignUndeclared { name } => named!(1, name),
        FallbackReason::HistoryUndeclared { name } => named!(2, name),
        FallbackReason::DottedName { name } => named!(3, name),
        FallbackReason::BareCall { name } => named!(4, name),
        FallbackReason::UnknownCallee { name } => named!(5, name),
        FallbackReason::OverloadedCallee => out.push(6),
        FallbackReason::MissingDefault => out.push(7),
        FallbackReason::TooManyPositionals => out.push(8),
        FallbackReason::UnknownArgument { name } => named!(9, name),
        FallbackReason::CastArity { name } => named!(10, name),
        FallbackReason::CastShape { name } => named!(11, name),
        FallbackReason::NaArity => out.push(12),
        FallbackReason::NzArity => out.push(13),
        FallbackReason::InputMissingDefault => out.push(14),
        FallbackReason::ComputedInputDefault => out.push(15),
        FallbackReason::DottedHome => out.push(16),
        FallbackReason::Loops => out.push(17),
        FallbackReason::Break => out.push(18),
        // Tags 19, 20, 24, 25 stay retired: statement/expression `switch`
        // plus array literals and indexing lower fully, so no block
        // emitted by any current compiler carries them. Tags are never
        // reused — an old block naming one fails closed as unknown.
        FallbackReason::TupleUnpack => out.push(21),
        FallbackReason::FieldRead => out.push(22),
        FallbackReason::InstanceCall => out.push(23),
        FallbackReason::CaptureValue { kind } => {
            out.push(26);
            out.push(encode_capture_kind(*kind));
        }
        FallbackReason::CaptureStatement { kind } => {
            out.push(27);
            out.push(encode_capture_kind(*kind));
        }
        FallbackReason::PlotUntitled => out.push(28),
        FallbackReason::BuiltinStatement { namespace, method } => {
            out.push(41);
            put_symbol(out, symbols, *namespace);
            put_symbol(out, symbols, *method);
        }
        FallbackReason::CaptureShape { kind } => {
            out.push(40);
            out.push(encode_capture_kind(*kind));
        }
        FallbackReason::OutsideBuiltin { namespace, method } => {
            out.push(29);
            put_symbol(out, symbols, *namespace);
            put_symbol(out, symbols, *method);
        }
        FallbackReason::UndeclaredRead => out.push(30),
        FallbackReason::EnumMember => out.push(31),
        FallbackReason::ComputedSeriesHistory => out.push(32),
        FallbackReason::StatementCall { name } => named!(33, name),
        FallbackReason::CallArgument { inner } => {
            out.push(34);
            encode_fallback(out, inner, symbols);
        }
        FallbackReason::NaArgument { inner } => {
            out.push(35);
            encode_fallback(out, inner, symbols);
        }
        FallbackReason::NzArgument { inner } => {
            out.push(36);
            encode_fallback(out, inner, symbols);
        }
        FallbackReason::IfCondition { inner } => {
            out.push(37);
            encode_fallback(out, inner, symbols);
        }
        FallbackReason::TernaryCondition { inner } => {
            out.push(38);
            encode_fallback(out, inner, symbols);
        }
        FallbackReason::TernaryArm { inner } => {
            out.push(39);
            encode_fallback(out, inner, symbols);
        }
    }
}

fn decode_fallback(
    reader: &mut Reader<'_>,
    symbols: &mut SymbolTable,
) -> Result<FallbackReason, DecodeError> {
    match reader.byte()? {
        0 => Ok(FallbackReason::FunctionBodyBinding {
            name: reader.symbol(symbols)?,
        }),
        1 => Ok(FallbackReason::ReassignUndeclared {
            name: reader.symbol(symbols)?,
        }),
        2 => Ok(FallbackReason::HistoryUndeclared {
            name: reader.symbol(symbols)?,
        }),
        3 => Ok(FallbackReason::DottedName {
            name: reader.symbol(symbols)?,
        }),
        4 => Ok(FallbackReason::BareCall {
            name: reader.symbol(symbols)?,
        }),
        5 => Ok(FallbackReason::UnknownCallee {
            name: reader.symbol(symbols)?,
        }),
        6 => Ok(FallbackReason::OverloadedCallee),
        7 => Ok(FallbackReason::MissingDefault),
        8 => Ok(FallbackReason::TooManyPositionals),
        9 => Ok(FallbackReason::UnknownArgument {
            name: reader.symbol(symbols)?,
        }),
        10 => Ok(FallbackReason::CastArity {
            name: reader.symbol(symbols)?,
        }),
        11 => Ok(FallbackReason::CastShape {
            name: reader.symbol(symbols)?,
        }),
        12 => Ok(FallbackReason::NaArity),
        13 => Ok(FallbackReason::NzArity),
        14 => Ok(FallbackReason::InputMissingDefault),
        15 => Ok(FallbackReason::ComputedInputDefault),
        16 => Ok(FallbackReason::DottedHome),
        17 => Ok(FallbackReason::Loops),
        18 => Ok(FallbackReason::Break),
        // Tags 19, 20, 24, 25 retired with their variants (see encode):
        // no current block carries them; old blocks naming one fail
        // closed as unknown tags, never miscompile.
        21 => Ok(FallbackReason::TupleUnpack),
        22 => Ok(FallbackReason::FieldRead),
        23 => Ok(FallbackReason::InstanceCall),
        26 => Ok(FallbackReason::CaptureValue {
            kind: decode_capture_kind(reader.byte()?)?,
        }),
        27 => Ok(FallbackReason::CaptureStatement {
            kind: decode_capture_kind(reader.byte()?)?,
        }),
        28 => Ok(FallbackReason::PlotUntitled),
        41 => Ok(FallbackReason::BuiltinStatement {
            namespace: reader.symbol(symbols)?,
            method: reader.symbol(symbols)?,
        }),
        40 => Ok(FallbackReason::CaptureShape {
            kind: decode_capture_kind(reader.byte()?)?,
        }),
        29 => Ok(FallbackReason::OutsideBuiltin {
            namespace: reader.symbol(symbols)?,
            method: reader.symbol(symbols)?,
        }),
        30 => Ok(FallbackReason::UndeclaredRead),
        31 => Ok(FallbackReason::EnumMember),
        32 => Ok(FallbackReason::ComputedSeriesHistory),
        33 => Ok(FallbackReason::StatementCall {
            name: reader.symbol(symbols)?,
        }),
        34 => Ok(FallbackReason::CallArgument {
            inner: Box::new(decode_fallback(reader, symbols)?),
        }),
        35 => Ok(FallbackReason::NaArgument {
            inner: Box::new(decode_fallback(reader, symbols)?),
        }),
        36 => Ok(FallbackReason::NzArgument {
            inner: Box::new(decode_fallback(reader, symbols)?),
        }),
        37 => Ok(FallbackReason::IfCondition {
            inner: Box::new(decode_fallback(reader, symbols)?),
        }),
        38 => Ok(FallbackReason::TernaryCondition {
            inner: Box::new(decode_fallback(reader, symbols)?),
        }),
        39 => Ok(FallbackReason::TernaryArm {
            inner: Box::new(decode_fallback(reader, symbols)?),
        }),
        other => Err(DecodeError::UnknownTag(other)),
    }
}

/// Encode one block to the wire form: tag bytes, little-endian operands,
/// length-prefixed text. Open names resolve through `symbols` and encode
/// inline, so the bytes stay self-describing; the fallback AST linkage
/// rides link-time side tables, never these bytes.
#[must_use]
pub fn encode_block(block: &CompiledBlock, symbols: &SymbolTable) -> Vec<u8> {
    let mut out = Vec::new();
    put_u32(&mut out, block.instrs.len() as u32);
    for instr in &block.instrs {
        match instr {
            Instruction::Const { dst, value } => {
                out.push(0x01);
                put_u32(&mut out, dst.0);
                put_u32(&mut out, value.0);
            }
            Instruction::Copy { dst, src } => {
                out.push(0x02);
                put_u32(&mut out, dst.0);
                put_u32(&mut out, src.0);
            }
            Instruction::BinOp { dst, op, lhs, rhs } => {
                out.push(0x03);
                put_u32(&mut out, dst.0);
                out.push(*op as u8);
                put_u32(&mut out, lhs.0);
                put_u32(&mut out, rhs.0);
            }
            Instruction::UnOp { dst, op, src } => {
                out.push(0x04);
                put_u32(&mut out, dst.0);
                out.push(*op as u8);
                put_u32(&mut out, src.0);
            }
            Instruction::Builtin { dst, var } => {
                out.push(0x05);
                put_u32(&mut out, dst.0);
                out.push(*var as u8);
            }
            Instruction::History { dst, src, offset } => {
                out.push(0x06);
                put_u32(&mut out, dst.0);
                put_u32(&mut out, src.0);
                put_i32(&mut out, *offset);
            }
            Instruction::GLoad { dst, slot } => {
                out.push(0x0F);
                put_u32(&mut out, dst.0);
                put_u32(&mut out, slot.0);
            }
            Instruction::GStore {
                slot,
                src,
                plot_tag,
            } => {
                out.push(0x10);
                put_u32(&mut out, slot.0);
                put_u32(&mut out, src.0);
                put_opt_const(&mut out, *plot_tag);
            }
            Instruction::GReassign {
                slot,
                src,
                plot_tag,
            } => {
                out.push(0x37);
                put_u32(&mut out, slot.0);
                put_u32(&mut out, src.0);
                put_opt_const(&mut out, *plot_tag);
            }
            Instruction::GStoreLoop { slot, src } => {
                out.push(0x1C);
                put_u32(&mut out, slot.0);
                put_u32(&mut out, src.0);
            }
            Instruction::GStoreInit { slot, src } => {
                out.push(0x11);
                put_u32(&mut out, slot.0);
                put_u32(&mut out, src.0);
            }
            Instruction::Call {
                dst,
                target,
                args,
                homes,
                type_args,
                site,
            } => {
                out.push(0x07);
                match dst {
                    Some(slot) => {
                        out.push(1);
                        put_u32(&mut out, slot.0);
                    }
                    None => out.push(0),
                }
                match target {
                    CallTarget::Block(index) => {
                        out.push(0);
                        put_u32(&mut out, *index as u32);
                    }
                    CallTarget::Named { module, name } => {
                        out.push(1);
                        match module {
                            Some(key) => {
                                out.push(1);
                                put_symbol(&mut out, symbols, *key);
                            }
                            None => out.push(0),
                        }
                        put_symbol(&mut out, symbols, *name);
                    }
                    CallTarget::Resolved { module, block } => {
                        out.push(3);
                        put_u32(&mut out, *module);
                        put_u32(&mut out, *block);
                    }
                    CallTarget::Builtin { namespace, method } => {
                        out.push(2);
                        out.push(encode_namespace(*namespace));
                        put_symbol(&mut out, symbols, *method);
                    }
                }
                put_call_args(&mut out, args, homes, symbols);
                put_u32(&mut out, type_args.len() as u32);
                for arg in type_args {
                    encode_type_arg(&mut out, arg, symbols);
                }
                put_symbol(&mut out, symbols, *site);
            }
            Instruction::Jump { target } => {
                out.push(0x08);
                put_u32(&mut out, *target as u32);
            }
            Instruction::JumpIfFalse { cond, target, ctx } => {
                out.push(0x09);
                put_u32(&mut out, cond.0);
                put_u32(&mut out, *target as u32);
                out.push(match ctx {
                    CondContext::If => 0,
                    CondContext::Ternary => 1,
                    CondContext::While => 2,
                    CondContext::SwitchPattern => 3,
                });
            }
            Instruction::JumpIfNa { cond, target } => {
                out.push(0x0E);
                put_u32(&mut out, cond.0);
                put_u32(&mut out, *target as u32);
            }
            Instruction::Return { src } => {
                out.push(0x0A);
                match src {
                    Some(slot) => {
                        out.push(1);
                        put_u32(&mut out, slot.0);
                    }
                    None => out.push(0),
                }
            }
            Instruction::Plot {
                title,
                src,
                site,
                display,
            } => {
                match title {
                    PlotTitle::Const(id) => {
                        out.push(0x0B);
                        put_u32(&mut out, id.0);
                    }
                    PlotTitle::Slot(slot) => {
                        out.push(0x35);
                        put_u32(&mut out, slot.0);
                    }
                }
                put_u32(&mut out, src.0);
                put_u32(&mut out, site.0);
                put_opt_slot(&mut out, *display);
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
                out.push(0x12);
                put_u32(&mut out, open.0);
                put_u32(&mut out, high.0);
                put_u32(&mut out, low.0);
                put_u32(&mut out, close.0);
                put_opt_const(&mut out, *title);
                put_u32(&mut out, site.0);
                put_opt_slot(&mut out, *display);
                put_opt_slot(&mut out, *color);
                put_opt_slot(&mut out, *wickcolor);
                put_opt_slot(&mut out, *bordercolor);
            }
            Instruction::BarColor { src, title, site } => {
                out.push(0x13);
                put_u32(&mut out, src.0);
                put_opt_const(&mut out, *title);
                put_u32(&mut out, site.0);
            }
            Instruction::PlotShape { src, title, site } => {
                out.push(0x14);
                put_u32(&mut out, src.0);
                put_opt_const(&mut out, *title);
                put_u32(&mut out, site.0);
            }
            Instruction::Hline { src, title, site } => {
                out.push(0x15);
                put_u32(&mut out, src.0);
                put_opt_const(&mut out, *title);
                put_u32(&mut out, site.0);
            }
            Instruction::Fill {
                first,
                second,
                title,
                site,
                color,
            } => {
                out.push(0x16);
                match first {
                    FillLeg::PlotSite(id) => {
                        out.push(0);
                        put_u32(&mut out, id.0);
                    }
                    FillLeg::Variable(id) => {
                        out.push(1);
                        put_symbol(&mut out, symbols, *id);
                    }
                }
                match second {
                    FillLeg::PlotSite(id) => {
                        out.push(0);
                        put_u32(&mut out, id.0);
                    }
                    FillLeg::Variable(id) => {
                        out.push(1);
                        put_symbol(&mut out, symbols, *id);
                    }
                }
                put_opt_const(&mut out, *title);
                put_u32(&mut out, site.0);
                put_opt_slot(&mut out, *color);
            }
            Instruction::Coalesce { dst, options } => {
                out.push(0x17);
                put_u32(&mut out, dst.0);
                put_u32(&mut out, options.len() as u32);
                for option in options {
                    put_u32(&mut out, option.0);
                }
            }
            Instruction::ForInit {
                first,
                last,
                counter,
                step,
                ascending,
                stride,
            } => {
                out.push(0x18);
                put_u32(&mut out, first.0);
                put_u32(&mut out, last.0);
                put_u32(&mut out, counter.0);
                put_u32(&mut out, step.0);
                put_u32(&mut out, ascending.0);
                put_u32(&mut out, stride.0);
            }
            Instruction::WhileTick { trip } => {
                out.push(0x19);
                put_u32(&mut out, trip.0);
            }
            Instruction::ForInInit { arr, len } => {
                out.push(0x1A);
                put_u32(&mut out, arr.0);
                put_u32(&mut out, len.0);
            }
            Instruction::ArrayGet {
                dst,
                arr,
                idx,
                hist,
            } => {
                out.push(0x1B);
                put_u32(&mut out, dst.0);
                put_u32(&mut out, arr.0);
                put_u32(&mut out, idx.0);
                put_opt_slot(&mut out, *hist);
            }
            Instruction::ArrayNew { dst, options } => {
                out.push(0x1E);
                put_u32(&mut out, dst.0);
                put_u32(&mut out, options.len() as u32);
                for option in options {
                    put_u32(&mut out, option.0);
                }
            }
            Instruction::Destructure { dsts, arr } => {
                out.push(0x1F);
                put_u32(&mut out, arr.0);
                put_u32(&mut out, dsts.len() as u32);
                for dst in dsts {
                    put_u32(&mut out, dst.0);
                }
            }
            Instruction::JournalWrite {
                home,
                src,
                mirror,
                strict,
            } => {
                out.push(0x20);
                put_symbol(&mut out, symbols, *home);
                put_u32(&mut out, src.0);
                put_u32(&mut out, mirror.0);
                out.push(u8::from(*strict));
            }
            Instruction::PlotTag { name, tag } => {
                out.push(0x21);
                put_symbol(&mut out, symbols, *name);
                put_opt_const(&mut out, *tag);
            }
            Instruction::VarDecl {
                ordinal,
                is_varip,
                src,
                name,
            } => {
                out.push(0x22);
                put_u32(&mut out, *ordinal);
                out.push(u8::from(*is_varip));
                put_u32(&mut out, src.0);
                put_symbol(&mut out, symbols, *name);
            }
            Instruction::VarStore {
                ordinal,
                is_varip,
                src,
                name,
                strict,
            } => {
                out.push(0x23);
                put_u32(&mut out, *ordinal);
                out.push(u8::from(*is_varip));
                put_u32(&mut out, src.0);
                put_symbol(&mut out, symbols, *name);
                out.push(u8::from(*strict));
            }
            Instruction::VarRead {
                dst,
                ordinal,
                name,
                hist,
            } => {
                out.push(0x24);
                put_u32(&mut out, dst.0);
                put_u32(&mut out, *ordinal);
                put_symbol(&mut out, symbols, *name);
                match hist {
                    Some(back) => {
                        out.push(1);
                        put_i32(&mut out, *back);
                    }
                    None => out.push(0),
                }
            }
            Instruction::FieldGet { dst, obj, field } => {
                out.push(0x25);
                put_u32(&mut out, dst.0);
                put_u32(&mut out, obj.0);
                put_symbol(&mut out, symbols, *field);
            }
            Instruction::ObjNew {
                dst,
                type_name,
                fields,
            } => {
                out.push(0x26);
                put_u32(&mut out, dst.0);
                put_symbol(&mut out, symbols, *type_name);
                put_u32(&mut out, fields.len() as u32);
                for (field, slot) in fields {
                    put_symbol(&mut out, symbols, *field);
                    put_u32(&mut out, slot.0);
                }
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
                out.push(0x27);
                put_opt_slot(&mut out, *dst);
                put_u32(&mut out, recv.0);
                put_symbol(&mut out, symbols, *method);
                put_call_args(&mut out, args, homes, symbols);
                put_u32(&mut out, args_debug.0);
                match home {
                    None => out.push(0),
                    Some(home) => {
                        out.push(1);
                        put_call_home(&mut out, home, symbols);
                    }
                }
                out.push(u8::from(*stmt));
            }
            Instruction::ThisRead { dst } => {
                out.push(0x28);
                put_u32(&mut out, dst.0);
            }
            Instruction::ThisFieldGet { dst, field } => {
                out.push(0x29);
                put_u32(&mut out, dst.0);
                put_symbol(&mut out, symbols, *field);
            }
            Instruction::ThisFieldSet { field, src } => {
                out.push(0x2A);
                put_symbol(&mut out, symbols, *field);
                put_u32(&mut out, src.0);
            }
            Instruction::AliasRead {
                dst,
                name,
                var_ordinal,
            } => {
                out.push(0x2B);
                put_u32(&mut out, dst.0);
                put_symbol(&mut out, symbols, *name);
                match var_ordinal {
                    Some(ordinal) => {
                        out.push(1);
                        put_u32(&mut out, *ordinal);
                    }
                    None => out.push(0),
                }
            }
            Instruction::AliasFieldGet {
                dst,
                name,
                field,
                var_ordinal,
            } => {
                out.push(0x2D);
                put_u32(&mut out, dst.0);
                put_symbol(&mut out, symbols, *name);
                put_symbol(&mut out, symbols, *field);
                match var_ordinal {
                    Some(ordinal) => {
                        out.push(1);
                        put_u32(&mut out, *ordinal);
                    }
                    None => out.push(0),
                }
            }
            Instruction::FieldPut { home, field, src } => {
                out.push(0x2C);
                put_call_home(&mut out, home, symbols);
                put_symbol(&mut out, symbols, *field);
                put_u32(&mut out, src.0);
            }
            Instruction::DottedGet {
                dst,
                root,
                name,
                field,
                flat,
            } => {
                // The flat-first shape rides its own tag so legacy
                // field-only bytes still decode to `None`.
                out.push(if flat.is_some() { 0x38 } else { 0x2F });
                put_u32(&mut out, dst.0);
                put_u32(&mut out, root.0);
                put_symbol(&mut out, symbols, *name);
                put_symbol(&mut out, symbols, *field);
                if let Some(flat) = flat {
                    put_u32(&mut out, flat.0);
                }
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
                site,
            } => {
                out.push(0x32);
                put_opt_slot(&mut out, *dst);
                put_u32(&mut out, symbol.0);
                put_u32(&mut out, timeframe.0);
                put_opt_slot(&mut out, *gaps);
                put_opt_slot(&mut out, *lookahead);
                put_opt_slot(&mut out, *calc_bars);
                out.push(u8::from(*single));
                put_security_items(&mut out, items, symbols);
                put_symbol(&mut out, symbols, *site);
            }
            Instruction::RequestLowerTf {
                dst,
                symbol,
                timeframe,
                legs,
                calc_bars,
                site,
            } => {
                out.push(0x33);
                put_opt_slot(&mut out, *dst);
                put_u32(&mut out, symbol.0);
                put_u32(&mut out, timeframe.0);
                put_u32(&mut out, legs.len() as u32);
                for leg in legs {
                    put_symbol(&mut out, symbols, *leg);
                }
                put_opt_slot(&mut out, *calc_bars);
                put_symbol(&mut out, symbols, *site);
            }
            Instruction::StrategyRead { dst, member } => {
                out.push(0x31);
                put_u32(&mut out, dst.0);
                put_symbol(&mut out, symbols, *member);
            }
            Instruction::SetMapProv { name, tag } => {
                out.push(0x30);
                put_symbol(&mut out, symbols, *name);
                match tag {
                    Some((map, key)) => {
                        out.push(1);
                        put_symbol(&mut out, symbols, *map);
                        put_u32(&mut out, key.0);
                    }
                    None => out.push(0),
                }
            }
            Instruction::SwitchEq { dst, target, probe } => {
                out.push(0x1D);
                put_u32(&mut out, dst.0);
                put_u32(&mut out, target.0);
                put_u32(&mut out, probe.0);
            }
            Instruction::Fallback { reason, node } => {
                out.push(0x0C);
                encode_fallback(&mut out, reason, symbols);
                put_u32(&mut out, node.get());
            }
            Instruction::LocalHistory {
                dst,
                slot,
                name,
                offset,
                bare,
            } => {
                // The qualified-only shape keeps its legacy tag; anything
                // with a bare column or a computed offset rides the
                // extended tag so old bytes still decode.
                if let (HistOffset::Const(offset), None) = (offset, bare) {
                    out.push(0x2E);
                    put_u32(&mut out, dst.0);
                    put_u32(&mut out, slot.0);
                    put_symbol(&mut out, symbols, *name);
                    put_i32(&mut out, *offset);
                } else {
                    out.push(0x3C);
                    put_u32(&mut out, dst.0);
                    put_u32(&mut out, slot.0);
                    put_symbol(&mut out, symbols, *name);
                    match offset {
                        HistOffset::Const(offset) => {
                            out.push(0);
                            put_i32(&mut out, *offset);
                        }
                        HistOffset::Slot(offset) => {
                            out.push(1);
                            put_u32(&mut out, offset.0);
                        }
                    }
                    put_opt_slot(&mut out, *bare);
                }
            }
            Instruction::Refuse { reason } => {
                out.push(0x34);
                put_u32(&mut out, reason.0);
            }
            Instruction::RuntimeRefuse { message } => {
                out.push(0x36);
                put_u32(&mut out, message.0);
            }
            Instruction::EnumMark { enm, members } => {
                out.push(0x39);
                put_symbol(&mut out, symbols, *enm);
                put_u32(&mut out, members.len() as u32);
                for member in members {
                    put_symbol(&mut out, symbols, *member);
                }
            }
            Instruction::EnumRead {
                dst,
                enm,
                member,
                name,
                flat,
            } => {
                out.push(0x3A);
                put_u32(&mut out, dst.0);
                put_symbol(&mut out, symbols, *enm);
                put_symbol(&mut out, symbols, *member);
                put_symbol(&mut out, symbols, *name);
                put_u32(&mut out, flat.0);
            }
            Instruction::AliasFlatRead { dst, flat, src } => {
                out.push(0x3B);
                put_u32(&mut out, dst.0);
                put_u32(&mut out, flat.0);
                put_u32(&mut out, src.0);
            }
            Instruction::Noop => out.push(0x0D),
        }
    }
    out
}

fn decode_binop(tag: u8) -> Result<BinOp, DecodeError> {
    match tag {
        0 => Ok(BinOp::Add),
        1 => Ok(BinOp::Subtract),
        2 => Ok(BinOp::Multiply),
        3 => Ok(BinOp::Divide),
        4 => Ok(BinOp::Modulo),
        5 => Ok(BinOp::Equal),
        6 => Ok(BinOp::NotEqual),
        7 => Ok(BinOp::Less),
        8 => Ok(BinOp::LessEqual),
        9 => Ok(BinOp::Greater),
        10 => Ok(BinOp::GreaterEqual),
        11 => Ok(BinOp::And),
        12 => Ok(BinOp::Or),
        other => Err(DecodeError::UnknownTag(other)),
    }
}

fn decode_unop(tag: u8) -> Result<UnOp, DecodeError> {
    match tag {
        0 => Ok(UnOp::Negate),
        1 => Ok(UnOp::Not),
        other => Err(DecodeError::UnknownTag(other)),
    }
}

fn decode_builtin(tag: u8) -> Result<BuiltinVarKind, DecodeError> {
    match tag {
        0 => Ok(BuiltinVarKind::Open),
        1 => Ok(BuiltinVarKind::High),
        2 => Ok(BuiltinVarKind::Low),
        3 => Ok(BuiltinVarKind::Close),
        4 => Ok(BuiltinVarKind::Volume),
        5 => Ok(BuiltinVarKind::Time),
        6 => Ok(BuiltinVarKind::BarIndex),
        7 => Ok(BuiltinVarKind::ChartSymbol),
        8 => Ok(BuiltinVarKind::ChartPeriod),
        9 => Ok(BuiltinVarKind::ChartIsIntraday),
        10 => Ok(BuiltinVarKind::ChartTimezone),
        11 => Ok(BuiltinVarKind::ChartPointValue),
        12 => Ok(BuiltinVarKind::ChartMinContract),
        13 => Ok(BuiltinVarKind::BarstateIsFirst),
        14 => Ok(BuiltinVarKind::BarstateIsNew),
        15 => Ok(BuiltinVarKind::BarstateIsConfirmed),
        16 => Ok(BuiltinVarKind::BarstateIsLast),
        17 => Ok(BuiltinVarKind::BarstateIsHistory),
        18 => Ok(BuiltinVarKind::BarstateIsRealtime),
        19 => Ok(BuiltinVarKind::Timenow),
        20 => Ok(BuiltinVarKind::Tr),
        21 => Ok(BuiltinVarKind::ChartPeriodSeconds),
        other => Err(DecodeError::UnknownTag(other)),
    }
}

/// Decode one block: the full slice must be exactly one block, so
/// trailing bytes refuse instead of silently decoding a prefix. Open
/// names intern into `symbols` as they decode.
pub fn decode_block(bytes: &[u8], symbols: &mut SymbolTable) -> Result<CompiledBlock, DecodeError> {
    let mut reader = Reader { bytes, position: 0 };
    let count = reader.u32()? as usize;
    let mut block = CompiledBlock::new();
    for _ in 0..count {
        let tag = reader.byte()?;
        block.instrs.push(match tag {
            0x01 => Instruction::Const {
                dst: reader.slot()?,
                value: reader.const_id()?,
            },
            0x02 => Instruction::Copy {
                dst: reader.slot()?,
                src: reader.slot()?,
            },
            0x03 => {
                let dst = reader.slot()?;
                let op = decode_binop(reader.byte()?)?;
                Instruction::BinOp {
                    dst,
                    op,
                    lhs: reader.slot()?,
                    rhs: reader.slot()?,
                }
            }
            0x04 => {
                let dst = reader.slot()?;
                let op = decode_unop(reader.byte()?)?;
                Instruction::UnOp {
                    dst,
                    op,
                    src: reader.slot()?,
                }
            }
            0x05 => {
                let dst = reader.slot()?;
                let var = decode_builtin(reader.byte()?)?;
                Instruction::Builtin { dst, var }
            }
            0x06 => Instruction::History {
                dst: reader.slot()?,
                src: reader.slot()?,
                offset: reader.i32()?,
            },
            0x0F => Instruction::GLoad {
                dst: reader.slot()?,
                slot: reader.slot()?,
            },
            0x10 => Instruction::GStore {
                slot: reader.slot()?,
                src: reader.slot()?,
                plot_tag: reader.opt_const()?,
            },
            0x37 => Instruction::GReassign {
                slot: reader.slot()?,
                src: reader.slot()?,
                plot_tag: reader.opt_const()?,
            },
            0x1C => Instruction::GStoreLoop {
                slot: reader.slot()?,
                src: reader.slot()?,
            },
            0x11 => Instruction::GStoreInit {
                slot: reader.slot()?,
                src: reader.slot()?,
            },
            0x07 => {
                let dst = match reader.byte()? {
                    0 => None,
                    1 => Some(reader.slot()?),
                    other => return Err(DecodeError::UnknownTag(other)),
                };
                let target = match reader.byte()? {
                    0 => CallTarget::Block(reader.u32()? as usize),
                    1 => {
                        let module = match reader.byte()? {
                            0 => None,
                            1 => Some(reader.symbol(symbols)?),
                            other => return Err(DecodeError::UnknownTag(other)),
                        };
                        CallTarget::Named {
                            module,
                            name: reader.symbol(symbols)?,
                        }
                    }
                    2 => CallTarget::Builtin {
                        namespace: decode_namespace(reader.byte()?)?,
                        method: reader.symbol(symbols)?,
                    },
                    3 => CallTarget::Resolved {
                        module: reader.u32()?,
                        block: reader.u32()?,
                    },
                    other => return Err(DecodeError::UnknownTag(other)),
                };
                let (args, homes) = reader.call_args(symbols)?;
                let typec = reader.u32()? as usize;
                let mut type_args = Vec::with_capacity(typec.min(8));
                for _ in 0..typec {
                    type_args.push(decode_type_arg(&mut reader, symbols)?);
                }
                let site = reader.symbol(symbols)?;
                Instruction::Call {
                    dst,
                    target,
                    args,
                    homes,
                    type_args,
                    site,
                }
            }
            0x08 => Instruction::Jump {
                target: reader.u32()? as usize,
            },
            0x09 => Instruction::JumpIfFalse {
                cond: reader.slot()?,
                target: reader.u32()? as usize,
                ctx: match reader.byte()? {
                    0 => CondContext::If,
                    1 => CondContext::Ternary,
                    2 => CondContext::While,
                    3 => CondContext::SwitchPattern,
                    other => return Err(DecodeError::UnknownTag(other)),
                },
            },
            0x0E => Instruction::JumpIfNa {
                cond: reader.slot()?,
                target: reader.u32()? as usize,
            },
            0x0A => Instruction::Return {
                src: match reader.byte()? {
                    0 => None,
                    1 => Some(reader.slot()?),
                    other => return Err(DecodeError::UnknownTag(other)),
                },
            },
            0x0B => Instruction::Plot {
                title: PlotTitle::Const(reader.const_id()?),
                src: reader.slot()?,
                site: reader.const_id()?,
                display: reader.opt_slot()?,
            },
            0x35 => Instruction::Plot {
                title: PlotTitle::Slot(reader.slot()?),
                src: reader.slot()?,
                site: reader.const_id()?,
                display: reader.opt_slot()?,
            },
            0x12 => Instruction::PlotCandle {
                open: reader.slot()?,
                high: reader.slot()?,
                low: reader.slot()?,
                close: reader.slot()?,
                title: reader.opt_const()?,
                site: reader.const_id()?,
                display: reader.opt_slot()?,
                color: reader.opt_slot()?,
                wickcolor: reader.opt_slot()?,
                bordercolor: reader.opt_slot()?,
            },
            0x13 => Instruction::BarColor {
                src: reader.slot()?,
                title: reader.opt_const()?,
                site: reader.const_id()?,
            },
            0x14 => Instruction::PlotShape {
                src: reader.slot()?,
                title: reader.opt_const()?,
                site: reader.const_id()?,
            },
            0x15 => Instruction::Hline {
                src: reader.slot()?,
                title: reader.opt_const()?,
                site: reader.const_id()?,
            },
            0x16 => {
                let first = match reader.byte()? {
                    0 => FillLeg::PlotSite(reader.const_id()?),
                    1 => FillLeg::Variable(reader.symbol(symbols)?),
                    other => return Err(DecodeError::UnknownTag(other)),
                };
                let second = match reader.byte()? {
                    0 => FillLeg::PlotSite(reader.const_id()?),
                    1 => FillLeg::Variable(reader.symbol(symbols)?),
                    other => return Err(DecodeError::UnknownTag(other)),
                };
                Instruction::Fill {
                    first,
                    second,
                    title: reader.opt_const()?,
                    site: reader.const_id()?,
                    color: reader.opt_slot()?,
                }
            }
            0x17 => {
                let dst = reader.slot()?;
                let count = reader.u32()? as usize;
                let mut options = Vec::with_capacity(count.min(8));
                for _ in 0..count {
                    options.push(reader.slot()?);
                }
                Instruction::Coalesce { dst, options }
            }
            0x18 => Instruction::ForInit {
                first: reader.slot()?,
                last: reader.slot()?,
                counter: reader.slot()?,
                step: reader.slot()?,
                ascending: reader.slot()?,
                stride: reader.slot()?,
            },
            0x19 => Instruction::WhileTick {
                trip: reader.slot()?,
            },
            0x1A => Instruction::ForInInit {
                arr: reader.slot()?,
                len: reader.slot()?,
            },
            0x1B => Instruction::ArrayGet {
                dst: reader.slot()?,
                arr: reader.slot()?,
                idx: reader.slot()?,
                hist: reader.opt_slot()?,
            },
            0x1E => {
                let dst = reader.slot()?;
                let count = reader.u32()? as usize;
                let mut options = Vec::with_capacity(count.min(64));
                for _ in 0..count {
                    options.push(reader.slot()?);
                }
                Instruction::ArrayNew { dst, options }
            }
            0x1F => {
                let arr = reader.slot()?;
                let count = reader.u32()? as usize;
                let mut dsts = Vec::with_capacity(count.min(64));
                for _ in 0..count {
                    dsts.push(reader.slot()?);
                }
                Instruction::Destructure { dsts, arr }
            }
            0x20 => Instruction::JournalWrite {
                home: reader.symbol(symbols)?,
                src: reader.slot()?,
                mirror: reader.slot()?,
                strict: reader.byte()? != 0,
            },
            0x21 => Instruction::PlotTag {
                name: reader.symbol(symbols)?,
                tag: reader.opt_const()?,
            },
            0x22 => Instruction::VarDecl {
                ordinal: reader.u32()?,
                is_varip: reader.byte()? != 0,
                src: reader.slot()?,
                name: reader.symbol(symbols)?,
            },
            0x23 => Instruction::VarStore {
                ordinal: reader.u32()?,
                is_varip: reader.byte()? != 0,
                src: reader.slot()?,
                name: reader.symbol(symbols)?,
                strict: reader.byte()? != 0,
            },
            0x24 => {
                let dst = reader.slot()?;
                let ordinal = reader.u32()?;
                let name = reader.symbol(symbols)?;
                let hist = match reader.byte()? {
                    0 => None,
                    _ => Some(reader.i32()?),
                };
                Instruction::VarRead {
                    dst,
                    ordinal,
                    name,
                    hist,
                }
            }
            0x25 => Instruction::FieldGet {
                dst: reader.slot()?,
                obj: reader.slot()?,
                field: reader.symbol(symbols)?,
            },
            0x26 => {
                let dst = reader.slot()?;
                let type_name = reader.symbol(symbols)?;
                let count = reader.u32()? as usize;
                let mut fields = Vec::with_capacity(count.min(64));
                for _ in 0..count {
                    let field = reader.symbol(symbols)?;
                    let slot = reader.slot()?;
                    fields.push((field, slot));
                }
                Instruction::ObjNew {
                    dst,
                    type_name,
                    fields,
                }
            }
            0x27 => {
                let dst = reader.opt_slot()?;
                let recv = reader.slot()?;
                let method = reader.symbol(symbols)?;
                let (args, homes) = reader.call_args(symbols)?;
                let args_debug = ConstId(reader.u32()?);
                let home = match reader.byte()? {
                    0 => None,
                    1 => Some(reader.call_home(symbols)?),
                    other => return Err(DecodeError::UnknownTag(other)),
                };
                let stmt = reader.byte()? != 0;
                Instruction::InstanceCall {
                    dst,
                    recv,
                    method,
                    args,
                    homes,
                    args_debug,
                    home,
                    stmt,
                }
            }
            0x28 => Instruction::ThisRead {
                dst: reader.slot()?,
            },
            0x29 => Instruction::ThisFieldGet {
                dst: reader.slot()?,
                field: reader.symbol(symbols)?,
            },
            0x2A => Instruction::ThisFieldSet {
                field: reader.symbol(symbols)?,
                src: reader.slot()?,
            },
            0x2B => {
                let dst = reader.slot()?;
                let name = reader.symbol(symbols)?;
                let var_ordinal = match reader.byte()? {
                    0 => None,
                    _ => Some(reader.u32()?),
                };
                Instruction::AliasRead {
                    dst,
                    name,
                    var_ordinal,
                }
            }
            0x2D => {
                let dst = reader.slot()?;
                let name = reader.symbol(symbols)?;
                let field = reader.symbol(symbols)?;
                let var_ordinal = match reader.byte()? {
                    0 => None,
                    _ => Some(reader.u32()?),
                };
                Instruction::AliasFieldGet {
                    dst,
                    name,
                    field,
                    var_ordinal,
                }
            }
            0x2C => Instruction::FieldPut {
                home: reader.call_home(symbols)?,
                field: reader.symbol(symbols)?,
                src: reader.slot()?,
            },
            0x2F => Instruction::DottedGet {
                dst: reader.slot()?,
                root: reader.slot()?,
                name: reader.symbol(symbols)?,
                field: reader.symbol(symbols)?,
                flat: None,
            },
            0x38 => Instruction::DottedGet {
                dst: reader.slot()?,
                root: reader.slot()?,
                name: reader.symbol(symbols)?,
                field: reader.symbol(symbols)?,
                flat: Some(reader.slot()?),
            },
            0x32 => Instruction::RequestSecurity {
                dst: reader.opt_slot()?,
                symbol: reader.slot()?,
                timeframe: reader.slot()?,
                gaps: reader.opt_slot()?,
                lookahead: reader.opt_slot()?,
                calc_bars: reader.opt_slot()?,
                single: match reader.byte()? {
                    0 => false,
                    1 => true,
                    other => return Err(DecodeError::UnknownTag(other)),
                },
                items: reader.security_items(symbols)?,
                site: reader.symbol(symbols)?,
            },
            0x33 => Instruction::RequestLowerTf {
                dst: reader.opt_slot()?,
                symbol: reader.slot()?,
                timeframe: reader.slot()?,
                legs: {
                    let count = reader.u32()? as usize;
                    let mut legs = Vec::with_capacity(count);
                    for _ in 0..count {
                        legs.push(reader.symbol(symbols)?);
                    }
                    legs
                },
                calc_bars: reader.opt_slot()?,
                site: reader.symbol(symbols)?,
            },
            0x31 => Instruction::StrategyRead {
                dst: reader.slot()?,
                member: reader.symbol(symbols)?,
            },
            0x30 => Instruction::SetMapProv {
                name: reader.symbol(symbols)?,
                tag: match reader.byte()? {
                    0 => None,
                    1 => Some((reader.symbol(symbols)?, reader.slot()?)),
                    other => return Err(DecodeError::UnknownTag(other)),
                },
            },
            0x1D => Instruction::SwitchEq {
                dst: reader.slot()?,
                target: reader.slot()?,
                probe: reader.slot()?,
            },
            0x0C => Instruction::Fallback {
                reason: decode_fallback(&mut reader, symbols)?,
                node: NodeId::new(reader.u32()?),
            },
            0x34 => Instruction::Refuse {
                reason: reader.const_id()?,
            },
            0x36 => Instruction::RuntimeRefuse {
                message: reader.slot()?,
            },
            0x39 => {
                let enm = reader.symbol(symbols)?;
                let count = reader.u32()? as usize;
                let mut members = Vec::with_capacity(count);
                for _ in 0..count {
                    members.push(reader.symbol(symbols)?);
                }
                Instruction::EnumMark { enm, members }
            }
            0x3A => Instruction::EnumRead {
                dst: reader.slot()?,
                enm: reader.symbol(symbols)?,
                member: reader.symbol(symbols)?,
                name: reader.symbol(symbols)?,
                flat: reader.slot()?,
            },
            0x3B => Instruction::AliasFlatRead {
                dst: reader.slot()?,
                flat: reader.slot()?,
                src: reader.slot()?,
            },
            0x0D => Instruction::Noop,
            0x2E => Instruction::LocalHistory {
                dst: reader.slot()?,
                slot: reader.slot()?,
                name: reader.symbol(symbols)?,
                offset: HistOffset::Const(reader.i32()?),
                bare: None,
            },
            0x3C => Instruction::LocalHistory {
                dst: reader.slot()?,
                slot: reader.slot()?,
                name: reader.symbol(symbols)?,
                offset: match reader.byte()? {
                    0 => HistOffset::Const(reader.i32()?),
                    1 => HistOffset::Slot(reader.slot()?),
                    other => return Err(DecodeError::UnknownTag(other)),
                },
                bare: reader.opt_slot()?,
            },
            other => return Err(DecodeError::UnknownTag(other)),
        });
    }
    if reader.position != bytes.len() {
        return Err(DecodeError::TrailingBytes(bytes.len() - reader.position));
    }
    Ok(block)
}

/// Encode one constant pool (tag + payload per entry, count-prefixed).
/// Open names resolve through `symbols`.
#[must_use]
pub fn encode_consts(pool: &ConstPool, symbols: &SymbolTable) -> Vec<u8> {
    let mut out = Vec::new();
    put_u32(&mut out, pool.values.len() as u32);
    for value in &pool.values {
        encode_const(&mut out, value, symbols);
    }
    out
}

/// Decode one constant pool; the full slice must be exactly one pool.
/// Open names intern into `symbols` as they decode.
pub fn decode_consts(bytes: &[u8], symbols: &mut SymbolTable) -> Result<ConstPool, DecodeError> {
    let mut reader = Reader { bytes, position: 0 };
    let count = reader.u32()? as usize;
    let mut pool = ConstPool::new();
    for _ in 0..count {
        pool.values.push(decode_const(&mut reader, symbols)?);
    }
    if reader.position != bytes.len() {
        return Err(DecodeError::TrailingBytes(bytes.len() - reader.position));
    }
    Ok(pool)
}

/// Encode one symbol table (count-prefixed length-framed names).
#[must_use]
pub fn encode_symbols(symbols: &SymbolTable) -> Vec<u8> {
    let mut out = Vec::new();
    put_u32(&mut out, symbols.names.len() as u32);
    for name in &symbols.names {
        put_str(&mut out, name);
    }
    out
}

/// Decode one symbol table; the full slice must be exactly one table.
pub fn decode_symbols(bytes: &[u8]) -> Result<SymbolTable, DecodeError> {
    let mut reader = Reader { bytes, position: 0 };
    let count = reader.u32()? as usize;
    let mut symbols = SymbolTable::new();
    for _ in 0..count {
        let text = reader.text()?;
        symbols.intern(&text);
    }
    if reader.position != bytes.len() {
        return Err(DecodeError::TrailingBytes(bytes.len() - reader.position));
    }
    Ok(symbols)
}

/// Test-only disassembler: one line per instruction with resolved
/// constants, for reviewed output shape (never a runtime path).
///
/// Display helpers shared by every capture arm below (hoisted from
/// per-arm closures so all families spell the same shapes).
#[cfg(test)]
fn show_const(consts: &ConstPool, id: ConstId) -> String {
    match consts.get(id) {
        Some(ConstValue::Str(text)) => format!("{text:?}"),
        other => format!("{other:?}"),
    }
}

/// Test-only optional-constant display: the resolved text, or the
/// untitled default both engines fall back to.
#[cfg(test)]
fn show_opt_const(consts: &ConstPool, id: Option<ConstId>) -> String {
    match id {
        Some(id) => show_const(consts, id),
        None => "<default>".to_owned(),
    }
}

/// Test-only optional-slot display: the slot, or nothing when absent.
#[cfg(test)]
fn show_opt_slot(slot: Option<SlotId>) -> String {
    match slot {
        Some(slot) => format!(" s{}", slot.0),
        None => String::new(),
    }
}

/// Test-only symbol display: the interned name, or `?` when unknown.
#[cfg(test)]
fn show_symbol<'a>(symbols: &'a SymbolTable, id: &'a SymbolId) -> &'a str {
    symbols.resolve(*id).unwrap_or("?")
}

/// Render one call home for the disassembler: the same spelling for both
/// instructions that carry a home.
#[cfg(test)]
fn show_home(symbols: &SymbolTable, home: &CallHome) -> String {
    match home {
        CallHome::Var { name, .. } => show_symbol(symbols, name).to_owned(),
        CallHome::Innermost => "this".to_owned(),
        CallHome::VarPath { name, path, .. } => {
            let mut text = show_symbol(symbols, name).to_owned();
            for field in path {
                text.push('.');
                text.push_str(show_symbol(symbols, field));
            }
            text
        }
        CallHome::ThisField { field } => format!("this.{}", show_symbol(symbols, field)),
    }
}

#[cfg(test)]
pub fn disassemble(block: &CompiledBlock, consts: &ConstPool, symbols: &SymbolTable) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for (index, instr) in block.instrs.iter().enumerate() {
        let _ = match instr {
            Instruction::Const { dst, value } => {
                writeln!(
                    out,
                    "{index:04} const s{} <- {:?}",
                    dst.0,
                    consts.get(*value)
                )
            }
            Instruction::Copy { dst, src } => {
                writeln!(out, "{index:04} copy s{} <- s{}", dst.0, src.0)
            }
            Instruction::BinOp { dst, op, lhs, rhs } => {
                writeln!(
                    out,
                    "{index:04} binop s{} <- s{} {op:?} s{}",
                    dst.0, lhs.0, rhs.0
                )
            }
            Instruction::UnOp { dst, op, src } => {
                writeln!(out, "{index:04} unop s{} <- {op:?} s{}", dst.0, src.0)
            }
            Instruction::Builtin { dst, var } => {
                writeln!(out, "{index:04} builtin s{} <- {var:?}", dst.0)
            }
            Instruction::History { dst, src, offset } => {
                writeln!(out, "{index:04} history s{} <- s{}[{offset}]", dst.0, src.0)
            }
            Instruction::Call {
                dst,
                target,
                args,
                homes: _,
                type_args,
                site,
            } => {
                let args: Vec<String> = args
                    .iter()
                    .map(|arg| {
                        let name = arg.name.as_ref().map_or("", |id| show_symbol(symbols, id));
                        format!("{name}=s{}", arg.slot.0)
                    })
                    .collect();
                let target_text = match target {
                    CallTarget::Block(index) => format!("block{index}"),
                    CallTarget::Named { module, name } => match module {
                        Some(key) => format!(
                            "{}/{}",
                            show_symbol(symbols, key),
                            show_symbol(symbols, name)
                        ),
                        None => show_symbol(symbols, name).to_owned(),
                    },
                    CallTarget::Resolved { module, block } => {
                        format!("resolved[{module}:{block}]")
                    }
                    CallTarget::Builtin { namespace, method } => {
                        format!("{}.{}", namespace.as_str(), show_symbol(symbols, method))
                    }
                };
                let site_text = show_symbol(symbols, site);
                fn show_type(
                    arg: &super::bytecode::CallTypeArg,
                    symbols: &super::bytecode::SymbolTable,
                ) -> String {
                    let nested: Vec<String> = arg
                        .args
                        .iter()
                        .map(|item| show_type(item, symbols))
                        .collect();
                    if nested.is_empty() {
                        show_symbol(symbols, &arg.name).to_owned()
                    } else {
                        format!("{}<{}>", show_symbol(symbols, &arg.name), nested.join(", "))
                    }
                }
                let generics: Vec<String> = type_args
                    .iter()
                    .map(|arg| show_type(arg, symbols))
                    .collect();
                let generics = if generics.is_empty() {
                    String::new()
                } else {
                    format!("<{}>", generics.join(", "))
                };
                match dst {
                    Some(slot) => writeln!(
                        out,
                        "{index:04} call s{} <- {target_text}{generics}({}) @{site_text}",
                        slot.0,
                        args.join(", ")
                    ),
                    None => writeln!(
                        out,
                        "{index:04} call _ <- {target_text}{generics}({}) @{site_text}",
                        args.join(", ")
                    ),
                }
            }
            Instruction::Jump { target } => {
                writeln!(out, "{index:04} jump -> {target:04}")
            }
            Instruction::JumpIfFalse { cond, target, ctx } => {
                let ctx = match ctx {
                    CondContext::If => "if",
                    CondContext::Ternary => "?:",
                    CondContext::While => "while",
                    CondContext::SwitchPattern => "switch?",
                };
                writeln!(
                    out,
                    "{index:04} jump_false[{ctx}] s{0} -> {target:04}",
                    cond.0
                )
            }
            Instruction::JumpIfNa { cond, target } => {
                writeln!(out, "{index:04} jump_na s{} -> {target:04}", cond.0)
            }
            Instruction::Return { src } => match src {
                Some(slot) => writeln!(out, "{index:04} return s{}", slot.0),
                None => writeln!(out, "{index:04} return"),
            },
            Instruction::Plot {
                title,
                src,
                site,
                display,
            } => {
                let title = match title {
                    PlotTitle::Const(id) => show_const(consts, *id),
                    PlotTitle::Slot(slot) => format!("s{}", slot.0),
                };
                let display = show_opt_slot(*display);
                writeln!(
                    out,
                    "{index:04} plot {title} <- s{} @ {}{display}",
                    src.0,
                    show_const(consts, *site)
                )
            }
            Instruction::GLoad { dst, slot } => {
                writeln!(out, "{index:04} gload s{} <- g{}", dst.0, slot.0)
            }
            Instruction::GStore {
                slot,
                src,
                plot_tag,
            } => match plot_tag {
                Some(tag) => writeln!(
                    out,
                    "{index:04} gstore g{} <- s{} @tag{}",
                    slot.0, src.0, tag.0
                ),
                None => writeln!(out, "{index:04} gstore g{} <- s{}", slot.0, src.0),
            },
            Instruction::GReassign {
                slot,
                src,
                plot_tag,
            } => match plot_tag {
                Some(tag) => writeln!(
                    out,
                    "{index:04} greassign g{} <- s{} @tag{}",
                    slot.0, src.0, tag.0
                ),
                None => writeln!(out, "{index:04} greassign g{} <- s{}", slot.0, src.0),
            },
            Instruction::GStoreLoop { slot, src } => {
                writeln!(out, "{index:04} gstoreloop g{} <- s{}", slot.0, src.0)
            }
            Instruction::GStoreInit { slot, src } => {
                writeln!(out, "{index:04} gstore.init g{} <- s{}", slot.0, src.0)
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
                writeln!(
                    out,
                    "{index:04} plotcandle s{}/{}/{}/{} {} <- {}{}{}{}",
                    open.0,
                    high.0,
                    low.0,
                    close.0,
                    show_opt_const(consts, *title),
                    show_const(consts, *site),
                    show_opt_slot(*display),
                    show_opt_slot(*color),
                    format_args!(
                        "{}{}",
                        show_opt_slot(*wickcolor),
                        show_opt_slot(*bordercolor)
                    ),
                )
            }
            Instruction::BarColor { src, title, site } => {
                let title = show_opt_const(consts, *title);
                writeln!(
                    out,
                    "{index:04} barcolor s{} {} @ {}",
                    src.0,
                    title,
                    show_const(consts, *site)
                )
            }
            Instruction::PlotShape { src, title, site } => {
                let title = show_opt_const(consts, *title);
                writeln!(
                    out,
                    "{index:04} plotshape s{} {} @ {}",
                    src.0,
                    title,
                    show_const(consts, *site)
                )
            }
            Instruction::Hline { src, title, site } => {
                let title = show_opt_const(consts, *title);
                writeln!(
                    out,
                    "{index:04} hline s{} {} @ {}",
                    src.0,
                    title,
                    show_const(consts, *site)
                )
            }
            Instruction::Fill {
                first,
                second,
                title,
                site,
                color,
            } => {
                let show_leg = |leg: &FillLeg| match leg {
                    FillLeg::PlotSite(id) => format!("plot#{}", id.0),
                    FillLeg::Variable(id) => {
                        format!("var:{}", symbols.resolve(*id).unwrap_or("?"))
                    }
                };
                let title = show_opt_const(consts, *title);
                let color = show_opt_slot(*color);
                writeln!(
                    out,
                    "{index:04} fill {}/{} {} @ {}{}",
                    show_leg(first),
                    show_leg(second),
                    title,
                    show_const(consts, *site),
                    color
                )
            }
            Instruction::Coalesce { dst, options } => {
                let options: Vec<String> =
                    options.iter().map(|slot| format!("s{}", slot.0)).collect();
                writeln!(
                    out,
                    "{index:04} coalesce s{} <- {}",
                    dst.0,
                    options.join(", ")
                )
            }
            Instruction::ForInit {
                first,
                last,
                counter,
                step,
                ascending,
                stride,
            } => {
                writeln!(
                    out,
                    "{index:04} forinit s{}/s{} c{} step{} asc{} stride{}",
                    first.0, last.0, counter.0, step.0, ascending.0, stride.0
                )
            }
            Instruction::WhileTick { trip } => {
                writeln!(out, "{index:04} whiletick s{}", trip.0)
            }
            Instruction::ForInInit { arr, len } => {
                writeln!(out, "{index:04} forininit s{} len{}", arr.0, len.0)
            }
            Instruction::ArrayGet {
                dst,
                arr,
                idx,
                hist,
            } => {
                let hist = match hist {
                    Some(slot) => format!(" hist:{}", slot.0),
                    None => String::new(),
                };
                writeln!(
                    out,
                    "{index:04} arrayget s{} <- s{}[s{}]{hist}",
                    dst.0, arr.0, idx.0
                )
            }
            Instruction::ArrayNew { dst, options } => {
                let options: Vec<String> =
                    options.iter().map(|slot| format!("s{}", slot.0)).collect();
                writeln!(
                    out,
                    "{index:04} arraynew s{} <- [{}]",
                    dst.0,
                    options.join(", ")
                )
            }
            Instruction::Destructure { dsts, arr } => {
                let dsts: Vec<String> = dsts.iter().map(|slot| format!("s{}", slot.0)).collect();
                writeln!(
                    out,
                    "{index:04} destructure [{}] <- s{}",
                    dsts.join(", "),
                    arr.0
                )
            }
            Instruction::JournalWrite {
                home,
                src,
                mirror,
                strict,
            } => {
                writeln!(
                    out,
                    "{index:04} journal s{} <- s{} as {}{}",
                    mirror.0,
                    src.0,
                    show_symbol(symbols, home),
                    if *strict { " !" } else { "" },
                )
            }
            Instruction::PlotTag { name, tag } => match tag {
                Some(id) => writeln!(
                    out,
                    "{index:04} plottag {} as {}",
                    show_symbol(symbols, name),
                    show_const(consts, *id),
                ),
                None => writeln!(
                    out,
                    "{index:04} plottag {} clear",
                    show_symbol(symbols, name),
                ),
            },
            Instruction::VarDecl {
                ordinal,
                is_varip,
                src,
                name,
            } => {
                writeln!(
                    out,
                    "{index:04} vardecl #{} s{} as {}{}",
                    ordinal,
                    src.0,
                    show_symbol(symbols, name),
                    if *is_varip { " varip" } else { "" },
                )
            }
            Instruction::VarStore {
                ordinal,
                is_varip,
                src,
                name,
                strict,
            } => {
                writeln!(
                    out,
                    "{index:04} varstore #{} s{} as {}{}{}",
                    ordinal,
                    src.0,
                    show_symbol(symbols, name),
                    if *is_varip { " varip" } else { "" },
                    if *strict { " !" } else { "" },
                )
            }
            Instruction::VarRead {
                dst,
                ordinal,
                name,
                hist,
            } => match hist {
                Some(back) => writeln!(
                    out,
                    "{index:04} varread s{} <- #{}[{}] as {}",
                    dst.0,
                    ordinal,
                    back,
                    show_symbol(symbols, name),
                ),
                None => writeln!(
                    out,
                    "{index:04} varread s{} <- #{} as {}",
                    dst.0,
                    ordinal,
                    show_symbol(symbols, name),
                ),
            },
            Instruction::FieldGet { dst, obj, field } => {
                writeln!(
                    out,
                    "{index:04} fieldget s{} <- s{}.{}",
                    dst.0,
                    obj.0,
                    show_symbol(symbols, field),
                )
            }
            Instruction::ObjNew {
                dst,
                type_name,
                fields,
            } => {
                let fields: Vec<String> = fields
                    .iter()
                    .map(|(field, slot)| format!("{}=s{}", show_symbol(symbols, field), slot.0))
                    .collect();
                writeln!(
                    out,
                    "{index:04} objnew s{} <- {}({})",
                    dst.0,
                    show_symbol(symbols, type_name),
                    fields.join(", "),
                )
            }
            Instruction::InstanceCall {
                dst,
                recv,
                method,
                args,
                args_debug,
                home,
                stmt,
                ..
            } => {
                let args: Vec<String> = args.iter().map(|arg| format!("s{}", arg.slot.0)).collect();
                let home = match home {
                    None => "drop".to_owned(),
                    Some(home) => show_home(symbols, home),
                };
                let rendered = match consts.get(*args_debug) {
                    Some(ConstValue::Str(text)) => text.clone(),
                    _ => "?".to_owned(),
                };
                let tail = if *stmt { " stmt" } else { "" };
                match dst {
                    Some(dst) => writeln!(
                        out,
                        "{index:04} methodcall s{} <- s{}.{}({}) via {} @{}{}",
                        dst.0,
                        recv.0,
                        show_symbol(symbols, method),
                        args.join(", "),
                        home,
                        rendered,
                        tail,
                    ),
                    None => writeln!(
                        out,
                        "{index:04} methodcall s{}.{}({}) via {} @{}{}",
                        recv.0,
                        show_symbol(symbols, method),
                        args.join(", "),
                        home,
                        rendered,
                        tail,
                    ),
                }
            }
            Instruction::ThisRead { dst } => {
                writeln!(out, "{index:04} thisread s{}", dst.0)
            }
            Instruction::ThisFieldGet { dst, field } => {
                writeln!(
                    out,
                    "{index:04} thisget s{} <- this.{}",
                    dst.0,
                    show_symbol(symbols, field),
                )
            }
            Instruction::ThisFieldSet { field, src } => {
                writeln!(
                    out,
                    "{index:04} thisset this.{} <- s{}",
                    show_symbol(symbols, field),
                    src.0,
                )
            }
            Instruction::AliasRead {
                dst,
                name,
                var_ordinal,
            } => match var_ordinal {
                Some(ordinal) => writeln!(
                    out,
                    "{index:04} aliasread s{} <- {} #{}",
                    dst.0,
                    show_symbol(symbols, name),
                    ordinal,
                ),
                None => writeln!(
                    out,
                    "{index:04} aliasread s{} <- {}",
                    dst.0,
                    show_symbol(symbols, name),
                ),
            },
            Instruction::AliasFieldGet {
                dst,
                name,
                field,
                var_ordinal,
            } => match var_ordinal {
                Some(ordinal) => writeln!(
                    out,
                    "{index:04} aliasget s{} <- {}.{} #{}",
                    dst.0,
                    show_symbol(symbols, name),
                    show_symbol(symbols, field),
                    ordinal,
                ),
                None => writeln!(
                    out,
                    "{index:04} aliasget s{} <- {}.{}",
                    dst.0,
                    show_symbol(symbols, name),
                    show_symbol(symbols, field),
                ),
            },
            Instruction::FieldPut { home, field, src } => {
                let home = show_home(symbols, home);
                writeln!(
                    out,
                    "{index:04} fieldput {}.{} <- s{}",
                    home,
                    show_symbol(symbols, field),
                    src.0,
                )
            }
            Instruction::SwitchEq { dst, target, probe } => {
                writeln!(
                    out,
                    "{index:04} switcheq s{} <- s{} == s{}",
                    dst.0, target.0, probe.0
                )
            }
            Instruction::Fallback { reason, node } => {
                writeln!(
                    out,
                    "{index:04} fallback {} @node{}",
                    reason.display(symbols),
                    node.get()
                )
            }
            Instruction::LocalHistory {
                dst,
                slot,
                name,
                offset,
                bare,
            } => {
                let offset = match offset {
                    HistOffset::Const(offset) => format!("{offset}"),
                    HistOffset::Slot(offset) => format!("s{}", offset.0),
                };
                let bare = bare
                    .map(|bare| format!(" bare=g{}", bare.0))
                    .unwrap_or_default();
                writeln!(
                    out,
                    "{index:04} local_history s{} <- s{} ({name}[{offset}]){bare}",
                    dst.0,
                    slot.0,
                    name = show_symbol(symbols, name),
                )
            }
            Instruction::DottedGet {
                dst,
                root,
                name,
                field,
                flat,
            } => {
                let flat = flat
                    .map(|flat| format!(" flat=g{}", flat.0))
                    .unwrap_or_default();
                writeln!(
                    out,
                    "{index:04} dotted s{} <- s{} ({} . {}){flat}",
                    dst.0,
                    root.0,
                    show_symbol(symbols, name),
                    show_symbol(symbols, field)
                )
            }
            Instruction::RequestSecurity { dst, items, .. } => writeln!(
                out,
                "{index:04} request.security{} ({} item(s))",
                show_opt_slot(*dst),
                items.len()
            ),
            Instruction::RequestLowerTf { dst, legs, .. } => writeln!(
                out,
                "{index:04} request.security_lower_tf{} ({} leg(s))",
                show_opt_slot(*dst),
                legs.len()
            ),
            Instruction::StrategyRead { dst, member } => writeln!(
                out,
                "{index:04} strategy s{} <- {}",
                dst.0,
                show_symbol(symbols, member)
            ),
            Instruction::SetMapProv { name, tag } => match tag {
                Some((map, key)) => writeln!(
                    out,
                    "{index:04} map_prov {} follows {} via s{}",
                    show_symbol(symbols, name),
                    show_symbol(symbols, map),
                    key.0
                ),
                None => writeln!(
                    out,
                    "{index:04} map_prov {} cleared",
                    show_symbol(symbols, name)
                ),
            },
            Instruction::Refuse { reason } => writeln!(
                out,
                "{index:04} refuse {}",
                match consts.get(*reason) {
                    Some(ConstValue::Str(text)) => text.as_str(),
                    _ => "?",
                }
            ),
            Instruction::RuntimeRefuse { message } => {
                writeln!(out, "{index:04} runtime_refuse s{}", message.0)
            }
            Instruction::EnumMark { enm, members } => {
                writeln!(
                    out,
                    "{index:04} enum_mark {} ({})",
                    show_symbol(symbols, enm),
                    members
                        .iter()
                        .map(|member| show_symbol(symbols, member))
                        .collect::<Vec<_>>()
                        .join(", "),
                )
            }
            Instruction::EnumRead {
                dst,
                enm,
                member,
                name,
                flat,
            } => {
                writeln!(
                    out,
                    "{index:04} enum_read s{} <- {}.{} ({}) flat=g{}",
                    dst.0,
                    show_symbol(symbols, enm),
                    show_symbol(symbols, member),
                    show_symbol(symbols, name),
                    flat.0,
                )
            }
            Instruction::AliasFlatRead { dst, flat, src } => {
                writeln!(
                    out,
                    "{index:04} alias_read s{} <- s{} flat=g{}",
                    dst.0, src.0, flat.0,
                )
            }
            Instruction::Noop => writeln!(out, "{index:04} noop"),
        };
    }
    out
}

#[cfg(test)]
#[path = "../../tests/pine_bytecode_repr.rs"]
mod repr_tests;

/// Closeout audit: fallback dispositions, the coverage ledger, the
/// no-weakening floors, and the documentation scan.
#[cfg(test)]
#[path = "../../tests/pine_bytecode_closeout.rs"]
mod closeout_tests;
