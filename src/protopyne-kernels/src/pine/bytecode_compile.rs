//! Bytecode compiler: pure lowering from the parsed AST to
//! [`CompiledModule`](super::bytecode::CompiledModule).
//!
//! Design notes the execution loop depends on:
//!
//! * **Totality over refusal.** `compile_module` never fails: anything
//!   without a lowering becomes `Instruction::Fallback` with a named
//!   reason. A compile-time refusal would surface earlier than the
//!   tree-walk's run-time refusal and break differential identity, so
//!   the worst case is a fully tree-walked module — never a compile
//!   error. Reasons propagate from the deepest unlowerable node, so the closeout
//!   works down a named list instead of re-auditing.
//! * **Whole-run granularity.** A module containing any
//!   `Fallback` runs entirely on the tree-walk; only fully lowered
//!   modules run the VM. There is therefore no mixed-execution state to
//!   synchronize in M4 (no shared variables/history between engines),
//!   while still-unlowered constructs keep their exact tree-walk
//!   behavior. A follow-up may refine this to statement granularity with an
//!   explicit state-sync design; the `Fallback` markers are its
//!   instrumentation.
//! * **Names die here.** Variables resolve to global slots, call sites
//!   to interned ids, aliases to module keys. The execution loop never
//!   hashes a name per bar (Tier 1: concrete slots on the hot path).
//! * **Copy, don't re-derive.** Site strings reuse the tree-walk formats
//!   (`ta.{method}@{args:?}`, `input@{args:?}`,
//!   `{module}::{name}@{args:?}`, `plot@{args:?}`); offset and `na`
//!   semantics are the tree-walk's (`read_variable`, `binary`), reused
//!   in M4 rather than reimplemented.
//! * **Conservative by default.** Undeclared reads, dotted homes,
//!   missing defaults, overloads, UDT dispatch, drawings, broker
//!   dialogue, loops, switches, and destructuring all fall back with
//!   named reasons instead of approximating.

use super::builtins_core::resolve_constant;
use super::bytecode::{
    BinOp, BuiltinNamespace, BuiltinVarKind, CallArgSlot, CallHome, CallTarget, CallTypeArg,
    CaptureKind, CompiledBlock, CompiledModule, CondContext, ConstId, ConstPool, ConstValue,
    FallbackReason, FillLeg, Instruction, LegKind, MethodEntry, SecurityExpr, SlotId, SymbolId,
    SymbolTable, UnOp, LEG_COUNT,
};
use super::eval::{walk_stmts, PartitionVisit, WalkVisit};
use crate::pine::syntax::{
    site_string, AssignOp, BinaryOp, BuiltinVar, CallArg, PineExpr, PineProgram, PineStmt,
    PineType, UnaryOp,
};
use crate::value::{DrawingKind, PineValue};
use std::collections::{HashMap, HashSet};

/// Compile one linked module to bytecode. Total: see the module docs.
/// Callers pass the linked module's key, program, alias table, the
/// cross-module enum registry, and the cross-module declaration
/// registry; enum members resolve and value bindings gate through the
/// registries exactly like the tree-walk's execution-order registration
/// does.
#[must_use]
#[allow(clippy::implicit_hasher)]
pub fn compile_module(
    key: &str,
    program: &PineProgram,
    aliases: &HashMap<String, String>,
    enums: &EnumRegistry,
    decls: &DeclRegistry,
    exports: &ExportRegistry,
) -> CompiledModule {
    let mut compiler = Compiler::new(key, aliases, enums, decls, exports);
    compiler.compile(program);
    compiler.finish()
}

/// Which names one module exposes to importers: the `export`ed function,
/// method, constant, enum, and type names in source order, mirroring
/// `modules::exported_names`. Plain assignments never export, so an alias
/// read of an unexported spelling keeps its fallback and the tree-walk
/// decides it at run time.
/// Exported type shapes: owning module key and type name → declared
/// fields with defaults.
#[allow(clippy::implicit_hasher)]
type ExportedTypes = HashMap<String, HashMap<String, Vec<(String, Option<PineExpr>)>>>;

/// Exported function signatures: owning module key → function name →
/// ordered (param, default) pairs, mirroring the callee's own index.
/// Cross-module calls fill omitted defaults caller-side from this table,
/// exactly like same-module calls fill from their own index.
#[allow(clippy::implicit_hasher)]
type ExportedFuncs = HashMap<String, HashMap<String, Vec<(String, Option<PineExpr>)>>>;

#[derive(Debug, Default)]
pub struct ExportRegistry {
    /// Owning module key → exported names.
    #[allow(clippy::implicit_hasher)]
    table: HashMap<String, HashSet<String>>,
    /// Owning module key → exported type name → declared fields with
    /// defaults: alias-qualified construction (`ALIAS.Type.new()`)
    /// evaluates these per construction at the call site, exactly like
    /// same-module construction evaluates its own table.
    types: ExportedTypes,
    /// Owning module key → function name → ordered params with defaults.
    funcs: ExportedFuncs,
    /// (Module, function) pairs declared more than once: cross-module
    /// calls to these keep the object-model fallback, exactly like the
    /// same-module overloaded veto.
    #[allow(clippy::implicit_hasher)]
    func_overloaded: HashSet<(String, String)>,
}

impl ExportRegistry {
    /// Collect `export`ed names across linked modules in link order.
    /// The trailing alias tables ride along so all three registries
    /// share one module list; only the enum table reads them.
    #[allow(clippy::implicit_hasher)]
    pub fn build(modules: RegistrySources<'_>) -> Self {
        let mut registry = Self {
            table: HashMap::new(),
            types: HashMap::new(),
            funcs: HashMap::new(),
            func_overloaded: HashSet::new(),
        };
        for (key, program, _) in modules {
            let mut names = HashSet::new();
            for stmt in &program.statements {
                // Function signatures ride the export registry so
                // cross-module calls fill defaults caller-side: the tree
                // serves alias calls from the target module's function
                // table regardless of export, so every top-level
                // declaration lands here (plain and exported alike).
                let (decl, _was_export) = match stmt {
                    PineStmt::ExportDecl(inner) => (inner.as_ref(), true),
                    other => (other, false),
                };
                if let PineStmt::FuncDecl { name, params, .. } = decl {
                    let signature = params
                        .iter()
                        .map(|param| (param.name.clone(), param.default.clone()))
                        .collect();
                    let entry = registry.funcs.entry((*key).to_owned()).or_default();
                    if entry.insert(name.clone(), signature).is_some() {
                        registry
                            .func_overloaded
                            .insert(((*key).to_owned(), name.clone()));
                    }
                }
                let PineStmt::ExportDecl(inner) = stmt else {
                    continue;
                };
                let name = match inner.as_ref() {
                    PineStmt::FuncDecl { name, .. }
                    | PineStmt::MethodDecl { name, .. }
                    | PineStmt::ConstDecl { name, .. }
                    | PineStmt::EnumDecl { name, .. }
                    | PineStmt::TypeDecl { name, .. } => Some(name.clone()),
                    _ => None,
                };
                if let Some(name) = name {
                    names.insert(name);
                }
                if let PineStmt::TypeDecl { name, fields } = inner.as_ref() {
                    registry
                        .types
                        .entry((*key).to_owned())
                        .or_default()
                        .insert(
                            name.clone(),
                            fields
                                .iter()
                                .map(|field| (field.name.clone(), field.default.clone()))
                                .collect(),
                        );
                }
            }
            // Alias reads resolve through the target namespace with no
            // export gate on the tree, and alias calls dispatch the same
            // way: the signature table above holds every declaration so
            // both stay lowered.
            registry.table.insert((*key).to_owned(), names);
        }
        registry
    }

    /// Whether `module` exports `item`.
    pub fn contains(&self, module: &str, item: &str) -> bool {
        self.table
            .get(module)
            .is_some_and(|names| names.contains(item))
    }

    /// Declared fields with defaults for an exported type, if the owning
    /// module exports it. `None` keeps the caller's fallback and the
    /// tree-walk decides (unknown or unexported type).
    pub fn type_fields(
        &self,
        module: &str,
        type_name: &str,
    ) -> Option<Vec<(String, Option<PineExpr>)>> {
        self.types
            .get(module)?
            .get(type_name)
            .cloned()
    }

    /// Ordered (param, default) pairs for one module's function, if the
    /// target declares it exactly once. `None` keeps the caller's
    /// fallback: unknown spellings refuse through the shared unknown
    /// path, overloaded ones through the overload veto — both exactly
    /// like the tree-walk.
    pub fn func_params(
        &self,
        module: &str,
        func: &str,
    ) -> Option<Vec<(String, Option<PineExpr>)>> {
        if self
            .func_overloaded
            .contains(&(module.to_owned(), func.to_owned()))
        {
            return None;
        }
        self.funcs.get(module)?.get(func).cloned()
    }

    /// Whether one module's function is overloaded (declared more than
    /// once): callers keep the fallback and the tree refuses, exactly
    /// like the same-module overloaded veto.
    pub fn func_is_overloaded(&self, module: &str, func: &str) -> bool {
        self.func_overloaded
            .contains(&(module.to_owned(), func.to_owned()))
    }
}

/// One declared enum: where it was declared and which members it names.
/// Member values never flow — handles carry identity only, exactly like
/// the tree-walk's `enum_lookup`, which ignores the value table.
#[derive(Debug)]
struct EnumEntry {
    /// Link-order position of the declaring module.
    module: usize,
    /// Top-level statement index of the declaration.
    stmt: usize,
    /// Declared member names.
    members: HashSet<String>,
}

/// Top-level enum declarations visible to later uses, built once per run
/// across all linked modules in link order. First declaration wins per
/// name, mirroring the tree-walk's execution-order registration (which
/// registers on first execution, bar zero onward).
#[derive(Debug, Default)]
pub struct EnumRegistry {
    /// Enum name → declaring position + member names. Positions index
    /// the build-time module list, shared with [`DeclRegistry`].
    #[allow(clippy::implicit_hasher)]
    table: HashMap<String, EnumEntry>,
    /// Module keys in link order (positions index this list).
    order: Vec<String>,
    /// Imported module key → importer keys, from the modules' own
    /// `import` lines. Bodies are reachable from their importers, so a
    /// body use resolves only when no importer could arrive before the
    /// enum's declaration executed.
    #[allow(clippy::implicit_hasher)]
    importers: HashMap<String, HashSet<String>>,
}

impl EnumRegistry {
    /// Collect top-level enum declarations (unwrapping one `export`
    /// layer, exactly like the tree-walk's declare path) across linked
    /// modules in link order. Conditional and nested declarations are
    /// skipped: their registration timing is unknowable statically, so
    /// uses keep falling back and the tree-walk decides. Import edges
    /// come from both the modules' `import` lines and their linked alias
    /// tables (hand-built closures may carry either): a body is reachable
    /// from every module that imports its own (transitively), so body
    /// uses resolve only when none of those could arrive first. Extra
    /// edges only ever block resolution, so the union fails safe.
    #[allow(clippy::implicit_hasher)]
    pub fn build(modules: RegistrySources<'_>) -> Self {
        let mut registry = Self {
            table: HashMap::new(),
            order: modules
                .iter()
                .map(|(key, _, _)| (*key).to_owned())
                .collect(),
            importers: HashMap::new(),
        };
        for (position, (key, program, aliases)) in modules.iter().enumerate() {
            for (index, stmt) in program.statements.iter().enumerate() {
                let decl = match stmt {
                    PineStmt::ExportDecl(inner) => inner.as_ref(),
                    other => other,
                };
                if let PineStmt::EnumDecl { name, members } = decl {
                    registry
                        .table
                        .entry(name.clone())
                        .or_insert_with(|| EnumEntry {
                            module: position,
                            stmt: index,
                            members: members.iter().map(|member| member.name.clone()).collect(),
                        });
                }
                if let PineStmt::Import(import) = stmt {
                    let target = format!("{}/{}", import.library.publisher, import.library.name);
                    if modules.iter().any(|(known, _, _)| *known == target) {
                        registry
                            .importers
                            .entry(target)
                            .or_default()
                            .insert((*key).to_owned());
                    }
                }
            }
            for target in aliases.values() {
                if modules.iter().any(|(known, _, _)| *known == target) {
                    registry
                        .importers
                        .entry(target.clone())
                        .or_default()
                        .insert((*key).to_owned());
                }
            }
        }
        registry
    }

    /// Link-order position of one module (`usize::MAX` when absent).
    fn order_position(&self, key: &str) -> usize {
        self.order
            .iter()
            .position(|k| k == key)
            .unwrap_or(usize::MAX)
    }

    /// Whether a dotted spelling has the shape of a declared enum member
    /// (trailing pair present in the table), ignoring declaration timing.
    /// Reads that shape-match take the registered read and let run time
    /// decide; anything else keeps its existing route.
    pub(crate) fn shape_matches(&self, dotted: &str) -> Option<(String, String)> {
        let (enum_name, member) = super::builtins_core::split_enum_member(dotted)?;
        self.table
            .get(enum_name)
            .filter(|entry| entry.members.contains(member))
            .map(|_| (enum_name.to_owned(), member.to_owned()))
    }

    /// Whether a body in `use_key` can observe an enum declared at
    /// `entry_module`: every transitive importer of the body module runs
    /// after the declaration. Direct and chained importers count (a call
    /// chain crosses one edge per call); method dispatch needs no edge
    /// (a receiver's home module always ran before any holder could call
    /// through it). Unknown when the order is adversarial — and then
    /// false, failing safe toward the fallback.
    fn body_observes(&self, use_key: &str, entry_module: usize) -> bool {
        let use_module = self.order_position(use_key);
        if entry_module >= use_module {
            return false;
        }
        let mut seen = HashSet::new();
        let mut stack: Vec<String> = self
            .importers
            .get(use_key)
            .map(|importers| importers.iter().cloned().collect())
            .unwrap_or_default();
        while let Some(importer) = stack.pop() {
            if !seen.insert(importer.clone()) {
                continue;
            }
            if self.order_position(&importer) <= entry_module {
                return false;
            }
            if let Some(next) = self.importers.get(&importer) {
                stack.extend(next.iter().cloned());
            }
        }
        true
    }

    /// Resolve a dotted name to an enum handle exactly when the tree-walk
    /// would: trailing `Enum.Member` pair (any module prefix), member
    /// declared, declaration top-level and already executed — a strictly
    /// earlier module, or an earlier statement in the same module.
    /// Function and method bodies resolve only when no caller could
    /// arrive before the declaration executed (every transitive importer
    /// of the body module runs after it); same-module bodies and
    /// backward-called bodies never do, because call timing is unknowable
    /// there — a body reached before the declaration observes an
    /// unregistered table (the tree-walk refuses) while a static handle
    /// would return a value. Anything else is `None`: the caller falls
    /// back, and the tree-walk decides na-vs-refusal at run time.
    fn lookup(
        &self,
        dotted: &str,
        use_key: &str,
        use_module: usize,
        use_stmt: Option<usize>,
        in_function: bool,
    ) -> Option<crate::EnumHandle> {
        let (enum_name, member) = super::builtins_core::split_enum_member(dotted)?;
        let entry = self.table.get(enum_name)?;
        if !entry.members.contains(member) {
            return None;
        }
        if in_function {
            // The importer check makes the handle exact (a receiver's home
            // module always ran before any holder could dispatch through
            // it, so method calls need no edge); anything uncertain keeps
            // the fallback.
            if !self.body_observes(use_key, entry.module) {
                return None;
            }
            return Some(crate::EnumHandle::new(enum_name, member));
        }
        let executed = match use_stmt {
            Some(stmt) => {
                entry.module < use_module || (entry.module == use_module && entry.stmt < stmt)
            }
            None => entry.module < use_module,
        };
        executed.then(|| crate::EnumHandle::new(enum_name, member))
    }
}

/// Unconditional top-level value declarations visible to later uses:
/// plain `=`/`const`/`var` bindings and `for`/`for-in` loop targets.
/// Built from the same pre-pass as [`EnumRegistry`], with the same
/// first-declaration-wins rule — mirroring the tree-walk, where a name
/// binds the first time its declaration executes. Conditional and nested
/// declarations are skipped: their binding timing is unknowable
/// statically, so uses keep falling back and the tree-walk decides
/// na-vs-refusal at run time.
#[derive(Debug, Default)]
pub struct DeclRegistry {
    /// Module keys in link order.
    order: Vec<String>,
    /// Declared name → (declaring module, top-level statement index).
    #[allow(clippy::implicit_hasher)]
    table: HashMap<String, DeclEntry>,
}

/// One declaration site: where the name first binds.
#[derive(Debug)]
struct DeclEntry {
    /// Link-order position of the declaring module.
    module: usize,
    /// Top-level statement index of the declaration.
    stmt: usize,
    /// Whether any declaration of the name carries persistence
    /// (`var`/`varip`): rebinding such a name inside a function stays
    /// tree-walk until per-instance persistence lowers.
    persistent: bool,
}

impl DeclRegistry {
    /// Collect unconditional top-level declarations (unwrapping one
    /// `export` layer, exactly like the tree-walk's declare path) across
    /// linked modules in link order.
    #[allow(clippy::implicit_hasher)]
    pub fn build(modules: RegistrySources<'_>) -> Self {
        let mut registry = Self {
            order: modules
                .iter()
                .map(|(key, _, _)| (*key).to_owned())
                .collect(),
            table: HashMap::new(),
        };
        for (position, (_, program, _)) in modules.iter().enumerate() {
            for (index, stmt) in program.statements.iter().enumerate() {
                let decl = match stmt {
                    PineStmt::ExportDecl(inner) => inner.as_ref(),
                    other => other,
                };
                let mut record = |name: &str, persistent: bool| {
                    let entry = registry.table.entry(name.to_owned()).or_insert(DeclEntry {
                        module: position,
                        stmt: index,
                        persistent,
                    });
                    entry.persistent |= persistent;
                };
                match decl {
                    PineStmt::Declaration { name, .. }
                    | PineStmt::ConstDecl { name, .. }
                    | PineStmt::Assignment { name, .. } => record(name, false),
                    PineStmt::VarDeclaration { name, .. } => record(name, true),
                    // A top-level multi-assign binds every item
                    // unconditionally, exactly like the plain
                    // assignments it desugars to — later reads and
                    // rebinds resolve the same names.
                    PineStmt::MultiAssign { items } => {
                        for item in items {
                            record(&item.name, false);
                        }
                    }
                    // A top-level multi-declaration binds every item
                    // unconditionally, exactly like the plain
                    // assignments it desugars to below.
                    PineStmt::MultiDeclaration { decls } => {
                        for decl in decls {
                            record(&decl.name, false);
                        }
                    }
                    // A top-level destructure binds every target
                    // unconditionally: the tree-walk assigns each
                    // element in order with no declared check, so
                    // later reads resolve the same names.
                    PineStmt::DestructureAssign { targets, .. } => {
                        for name in targets {
                            record(name, false);
                        }
                    }
                    // Numeric `for` always iterates at least once
                    // (`|last - first| + 1`), so its counter definitely
                    // binds. A `for-in` over an empty iterable binds
                    // nothing, so its targets stay unregistered (their
                    // bodies resolve through the loop-target stack).
                    PineStmt::For { variable, .. } => record(variable, false),
                    _ => {}
                }
            }
        }
        registry
    }

    /// Link-order position of one module (`usize::MAX` when absent, which
    /// only resolves declarations from strictly earlier modules).
    fn order_position(&self, key: &str) -> usize {
        self.order
            .iter()
            .position(|k| k == key)
            .unwrap_or(usize::MAX)
    }

    /// Whether a top-level use may treat the name as bound: an
    /// unconditional declaration already executed — a strictly earlier
    /// module, or an earlier statement in the same module. Conditional
    /// declarations never register, so their uses fall back and the
    /// tree-walk decides; function and method bodies never consult the
    /// table here (call timing is unknowable — the Reassign path owns
    /// the in-function rule).
    fn declared(&self, name: &str, use_module: usize, use_stmt: Option<usize>) -> bool {
        let Some(entry) = self.table.get(name) else {
            return false;
        };
        match use_stmt {
            Some(stmt) => {
                entry.module < use_module || (entry.module == use_module && entry.stmt < stmt)
            }
            None => entry.module < use_module,
        }
    }

    /// Owning module position of one unconditional declaration, if any.
    /// Mixed execution partitions on this: a read of a name owned by a
    /// differently-engined module needs cross-engine state the split
    /// does not mirror, so the plan stays whole-run tree-walk instead.
    pub(crate) fn owner(&self, name: &str) -> Option<usize> {
        self.table.get(name).map(|entry| entry.module)
    }

    /// Whether any unconditional top-level declaration of this name
    /// carries persistence (`var`/`varip`): rebinding it inside a
    /// function stays tree-walk until per-instance persistence lowers.
    /// First declaration wins for order; persistence sticks once seen.
    pub(crate) fn is_persistent(&self, name: &str) -> bool {
        self.table.get(name).is_some_and(|entry| entry.persistent)
    }
}

/// Builtin namespaces lowered to delegated calls. Everything else
/// (`array`, `matrix`, `map` with write-back homes, drawings, broker
/// dialogue, sessions) falls back with a named reason.
/// Broker dialogue and drawing captures: recorded by the tree-walk,
/// never lowered.
fn capture_kind(name: &str) -> Option<CaptureKind> {
    CaptureKind::parse(name)
}

/// Map one syntax binary operator onto its bytecode operand.
fn bin_op(op: BinaryOp) -> BinOp {
    match op {
        BinaryOp::Add => BinOp::Add,
        BinaryOp::Subtract => BinOp::Subtract,
        BinaryOp::Multiply => BinOp::Multiply,
        BinaryOp::Divide => BinOp::Divide,
        BinaryOp::Modulo => BinOp::Modulo,
        BinaryOp::Equal => BinOp::Equal,
        BinaryOp::NotEqual => BinOp::NotEqual,
        BinaryOp::Less => BinOp::Less,
        BinaryOp::LessEqual => BinOp::LessEqual,
        BinaryOp::Greater => BinOp::Greater,
        BinaryOp::GreaterEqual => BinOp::GreaterEqual,
        BinaryOp::And => BinOp::And,
        BinaryOp::Or => BinOp::Or,
    }
}

/// One compile-time literal as its run-time value, for constant folding.
/// Only side-effect-free literals qualify (numbers, bools, strings,
/// colors); everything else lowers normally.
fn literal_value(expr: &PineExpr) -> Option<PineValue> {
    match expr {
        PineExpr::Number(value) => Some(PineValue::Float(*value)),
        PineExpr::Boolean(flag) => Some(PineValue::Bool(*flag)),
        PineExpr::StringLiteral(text) => Some(PineValue::Str(text.clone())),
        PineExpr::ColorLiteral(packed) => Some(PineValue::ColorHandle(crate::ColorHandle::new(
            *packed,
        ))),
        _ => None,
    }
}

/// One folded value back into its constant form. Structured values never
/// arise from literal inputs, so they decline the fold (return `None`)
/// and the caller's normal lowering runs instead.
fn folded_const(value: Option<&PineValue>) -> Option<ConstValue> {
    match value {
        None => Some(ConstValue::Na),
        Some(PineValue::Float(value)) => Some(ConstValue::Float(*value)),
        Some(PineValue::Int(value)) => Some(ConstValue::Int(*value)),
        Some(PineValue::Bool(value)) => Some(ConstValue::Bool(*value)),
        Some(PineValue::Str(text)) => Some(ConstValue::Str(text.clone())),
        Some(PineValue::ColorHandle(handle)) => Some(ConstValue::Color(handle.raw())),
        _ => None,
    }
}

/// One bare chart-leg name from a request expression item: the only shapes
/// the injected lower-tf path accepts.
fn security_leg_name(expr: &PineExpr) -> Option<&str> {
    match expr {
        PineExpr::BuiltinVariable(var) => Some(super::eval::builtin_name(*var)),
        PineExpr::Variable(name) => Some(name),
        _ => None,
    }
}

/// Bare casts lowered to delegated `core.*` calls.
fn core_cast(name: &str) -> bool {
    matches!(name, "int" | "float" | "color")
}

fn home_is_dotted(home: Option<&str>) -> bool {
    home.is_some_and(|path| path.contains('.'))
}

/// Reserved global column for a chart leg by name, mirroring
/// `Legs::named` (user bindings shadow these; see `lower_variable`).
fn leg_slot(name: &str) -> Option<LegKind> {
    LegKind::parse(name)
}

/// Indexed function bodies: block plus ordered (name, default) params.
type FuncIndex = HashMap<String, (usize, Vec<(String, Option<PineExpr>)>)>;

/// One linked module as the registries see it: key, program, and alias
/// table. All three registries share one list so positions agree.
#[allow(clippy::implicit_hasher)]
type RegistrySources<'a> = &'a [(
    &'a str,
    &'a PineProgram,
    &'a HashMap<String, String>,
)];

/// One indexed method body: block plus ordered (name, default) params
/// (receiver first) and the receiver parameter callers bind `this`
/// through.
struct MethodSig {
    block: usize,
    params: Vec<(String, Option<PineExpr>)>,
    receiver: String,
}

/// Indexed method bodies keyed by (receiver type, method).
type MethodIndex = HashMap<(String, String), MethodSig>;

/// One lowered `plot()` call: the captured series, title, site,
/// and display handle, without the emit.
struct PlotParts {
    src: SlotId,
    title: SlotId,
    site: ConstId,
    display: Option<SlotId>,
}

/// Which plain names one statement list assigns, plus whether it
/// rebinds, loops, or declares persistence: the static escape probe one
/// function body runs before lowering. Anything the tree-walk's journal
/// would observe across the call boundary forces journaled lowering;
/// anything else keeps the locals-only fast path.
#[derive(Default)]
struct EscapeSet {
    /// Plain (non-dotted) names assigned anywhere in the body.
    written: HashSet<String>,
    /// Rebinds, loops, persistence, or dotted/`this` writes: shapes the
    /// loop must journal rather than prove local.
    escapes: bool,
}

/// One walk over the shared fold: assignment targets and loop counters
/// count as writes wherever they nest; bodies reached from anywhere run
/// after their own stores, so conditional writes still force journaling.
struct EscapeProbe;

impl PartitionVisit for EscapeProbe {
    type State = EscapeSet;

    fn stmt(&mut self, stmt: &PineStmt, state: &mut Self::State) -> WalkVisit {
        let plain = |state: &mut EscapeSet, name: &str| {
            if !name.contains('.') && name != "this" {
                state.written.insert(name.to_owned());
            }
        };
        match stmt {
            PineStmt::Assignment { name, .. } => plain(state, name),
            PineStmt::Reassign { name, .. } | PineStmt::VarDeclaration { name, .. } => {
                state.escapes = true;
                plain(state, name);
            }
            PineStmt::MultiAssign { items } => {
                for item in items {
                    match item.op {
                        AssignOp::Assign => plain(state, &item.name),
                        AssignOp::Reassign => {
                            state.escapes = true;
                            plain(state, &item.name);
                        }
                    }
                }
            }
            PineStmt::MultiDeclaration { decls } => {
                for decl in decls {
                    plain(state, &decl.name);
                }
            }
            PineStmt::DestructureAssign { targets, .. } => {
                for name in targets {
                    plain(state, name);
                }
            }
            PineStmt::For { variable, .. } => {
                state.escapes = true;
                plain(state, variable);
            }
            PineStmt::ForIn { targets, .. } => {
                state.escapes = true;
                for name in targets {
                    plain(state, name);
                }
            }
            _ => {}
        }
        WalkVisit::Recurse
    }

    fn expr(&mut self, _expr: &PineExpr, _state: &mut Self::State) -> WalkVisit {
        WalkVisit::Recurse
    }
}

/// Wrap an unlowerable call argument: the tree-walk evaluates arguments
/// left-to-right, so the report names the failing position.
fn call_arg_ctx(inner: FallbackReason) -> FallbackReason {
    FallbackReason::CallArgument {
        inner: Box::new(inner),
    }
}

/// Syntactic `plot()` call arguments for an in-function binding
/// right-hand side: `Some` exactly when the tree-walk would tag the
/// name, so the loop tags the same shape through the same site.
fn plot_call_args(value: &PineExpr) -> Option<&[CallArg]> {
    match value {
        PineExpr::FunctionCall { name, args, .. } if name == "plot" => Some(args),
        _ => None,
    }
}

/// Which positional index a capture consumed as its title (if any),
/// so the effect loop skips exactly the literal already folded into a
/// constant. Named titles skip by name instead.
struct Compiler<'a> {
    key: String,
    // Standard hasher throughout: every map here mirrors a tree-walk
    // map keyed the same way (`LinkedModule.aliases` is a std map).
    #[allow(clippy::implicit_hasher)]
    aliases: &'a HashMap<String, String>,
    consts: ConstPool,
    blocks: Vec<CompiledBlock>,
    /// Function name → body block, with parameter names (recorded in the
    /// index pass so forward references resolve). Methods live in their
    /// own table keyed by receiver type: same-named methods on different
    /// types coexist, and bare calls to a method name refuse exactly like
    /// the tree-walk instead of borrowing a method body.
    func_blocks: FuncIndex,
    /// Method (receiver type, name) → body block with parameters
    /// (receiver first). Indexed in the same pass so forward references
    /// resolve; dispatch itself stays a runtime type test.
    method_blocks: MethodIndex,
    /// Types declared in this module (method receivers resolve here).
    own_types: HashSet<String>,
    /// Declared fields per own type, with defaults: construction evaluates
    /// them per call at the call site.
    own_type_fields: HashMap<String, Vec<(String, Option<PineExpr>)>>,
    /// Receiver parameter names of this module's methods: reads of such
    /// names hit the live receiver slot first, exactly like the
    /// tree-walk's top-down stack search.
    any_method_alias: HashSet<String>,
    /// Overloaded names: calls to these fall back (no static dispatch).
    overloaded: HashSet<String>,
    /// Module-level series columns.
    globals: HashMap<String, SlotId>,
    /// Global columns with intrabar persistence.
    varip: HashSet<SlotId>,
    next_global: u32,
    /// Current block under construction.
    current: usize,
    /// Frame layout of the current block.
    frame_slots: u32,
    /// Local names (params + function locals) of the current block.
    locals: HashMap<String, SlotId>,
    /// Inside a function body (locals active).
    in_function: bool,
    /// Cross-module enum registry (member identity only).
    enums: &'a EnumRegistry,
    /// Cross-module declaration registry (unconditional top-level
    /// bindings only).
    decls: &'a DeclRegistry,
    /// Cross-module export table (which names each module exposes).
    exports: &'a ExportRegistry,
    /// This module's link-order position (for declaration-order checks).
    module_pos: usize,
    /// Current top-level statement index (`None` while compiling called
    /// bodies, whose execution timing is unknowable statically).
    top_level: Option<usize>,
    /// Loop targets whose bodies are currently lowering: a use inside
    /// the body executes only after the loop seeded the target, so the
    /// read is bound no matter how conditionally the loop itself runs.
    /// Pushed when a loop body is entered, popped when it ends; function
    /// bodies start with an empty stack (call timing is unknowable).
    /// Shadowing nests naturally — any match means bound.
    loop_targets: Vec<String>,
    /// Interned open names (sites, callee names, fallback payloads).
    symbols: SymbolTable,
    /// Enclosing loop jump lists for `break`: each level collects the
    /// jump positions to patch to that loop's end. A `break` with no
    /// enclosing loop in the same body falls back — the tree-walk
    /// refuses it instead of unwinding, and call boundaries reset the
    /// depth there exactly as this stack resets here.
    break_stack: Vec<Vec<usize>>,
    /// Names bound earlier in the current top-level statement by `=`
    /// items already lowered (multi-assign, multi-declaration): later
    /// items read them exactly like the tree-walk's left-to-right
    /// evaluation, while the declaration registry still owns
    /// cross-statement order. Cleared per statement; never consulted
    /// inside function bodies (call timing is unknowable — locals own
    /// the in-function rule).
    stmt_bound: HashSet<String>,
    /// Count of history-over-parenthesized-expression sites lowered in
    /// this module: each spill site takes a fresh engine-internal
    /// column, so two sites never share one.
    spills: u32,
    /// Whether the body under lowering may touch caller-visible state.
    /// Set per function body from the static escape probe before
    /// lowering it; journaled bodies route named writes through the
    /// loop-side journal while locals-only bodies keep the fast path.
    journaled: bool,
    /// Plain names the current function body may write (body-wide, from
    /// the same escape probe): in-function history reads of these names
    /// stay tree-walk even when the read precedes the write in source
    /// order, because the tree serves them from per-call qualified
    /// history while the loop only holds committed global columns.
    journaled_written: HashSet<String>,
    /// In-function `var`/`varip` declarations of the body under lowering:
    /// name maps to its declaration ordinal and intrabar flag. Reads and
    /// writes route to per-instance cells; the tree-walk's scope walk
    /// puts these before plain frame bindings on writes and after them
    /// on reads, and the loop mirrors exactly that order.
    frame_vars: HashMap<String, (u32, bool)>,
    /// Every name declared as in-function `var`/`varip` anywhere in this
    /// module: reads of such names from bodies that do not declare them
    /// stay tree-walk, since the tree's scope walk would serve an
    /// ancestor instance the loop cannot see.
    any_frame_var: HashSet<String>,
    /// Every name assigned from `map.get(...)` anywhere in this module:
    /// the loop emits provenance tags for these names so receiver
    /// mutations persist into the map entry exactly like the tree-walk.
    map_prov_names: HashSet<String>,
    /// Every non-`this` dotted name passed as a call argument anywhere in
    /// this module. A write-back may create a flat variable with that
    /// spelling at run time, which the tree-walk reads before field
    /// lookup; reads of these names therefore stay tree-walk.
    dotted_homes: HashSet<String>,
    /// Names whose per-call past the body being lowered reads; resolved
    /// to `(name, slot)` at body end and frozen onto the block so the loop
    /// records each call's final value.
    history_locals: Vec<SymbolId>,
    /// Plain names read while bound nowhere: the unknown-read rule
    /// allocated their column, and the loop refuses them at run time
    /// unless a real binding lands later. Dotted reads consult this set
    /// so an unbound head keeps the dotted fallback (which names the full
    /// path, like the tree-walk) instead of the head-naming refusal.
    speculative_reads: HashSet<String>,
}

impl<'a> Compiler<'a> {
    fn new(
        key: &str,
        aliases: &'a HashMap<String, String>,
        enums: &'a EnumRegistry,
        decls: &'a DeclRegistry,
        exports: &'a ExportRegistry,
    ) -> Self {
        Self {
            key: key.to_owned(),
            aliases,
            enums,
            decls,
            exports,
            module_pos: decls.order_position(key),
            top_level: None,
            loop_targets: Vec::new(),
            consts: ConstPool::new(),
            blocks: vec![CompiledBlock::new()],
            func_blocks: HashMap::new(),
            method_blocks: HashMap::new(),
            own_types: HashSet::new(),
            own_type_fields: HashMap::new(),
            any_method_alias: HashSet::new(),
            overloaded: HashSet::new(),
            globals: HashMap::new(),
            varip: HashSet::new(),
            next_global: LEG_COUNT,
            current: 0,
            frame_slots: LEG_COUNT,
            locals: HashMap::new(),
            in_function: false,
            symbols: SymbolTable::new(),
            break_stack: Vec::new(),
            spills: 0,
            stmt_bound: HashSet::new(),
            journaled: false,
            journaled_written: HashSet::new(),
            frame_vars: HashMap::new(),
            any_frame_var: HashSet::new(),
            map_prov_names: HashSet::new(),
            dotted_homes: HashSet::new(),
            history_locals: Vec::new(),
            speculative_reads: HashSet::new(),
        }
    }

    fn finish(mut self) -> CompiledModule {
        let functions = self
            .func_blocks
            .iter()
            .map(|(name, (index, _))| (name.clone(), *index))
            .collect();
        let methods = self.method_table();
        CompiledModule {
            key: self.key,
            entry: 0,
            blocks: self.blocks,
            globals: self.globals,
            varip: self.varip,
            functions,
            methods,
            overloaded: self.overloaded,
            consts: std::sync::Arc::new(self.consts),
            symbols: std::sync::Arc::new(self.symbols),
        }
    }

    fn emit(&mut self, instr: Instruction) -> usize {
        let block = &mut self.blocks[self.current];
        block.instrs.push(instr);
        if self.frame_slots > block.slots {
            block.slots = self.frame_slots;
        }
        block.instrs.len() - 1
    }

    fn patch(&mut self, position: usize, target: usize, block: usize) {
        let slot = &mut self.blocks[block].instrs[position];
        match slot {
            Instruction::Jump { target: t }
            | Instruction::JumpIfFalse { target: t, .. }
            | Instruction::JumpIfNa { target: t, .. } => *t = target,
            other => panic!("patch target is not a jump: {other:?}"),
        }
    }

    fn here(&self) -> usize {
        self.blocks[self.current].instrs.len()
    }

    /// Fresh frame temp. Temps are never reused within a block, so an
    /// unwritten temp reads `na` — the slots-reset-per-bar contract the
    /// Execution loop implements.
    fn temp(&mut self) -> SlotId {
        let slot = SlotId(self.frame_slots);
        self.frame_slots += 1;
        let block = &mut self.blocks[self.current];
        if self.frame_slots > block.slots {
            block.slots = self.frame_slots;
        }
        slot
    }

    /// Module-level series column, allocating on first use.
    fn global(&mut self, name: &str) -> SlotId {
        if let Some(slot) = self.globals.get(name) {
            return *slot;
        }
        let slot = SlotId(self.next_global);
        self.next_global += 1;
        self.globals.insert(name.to_owned(), slot);
        slot
    }

    /// Whether an enum-shaped read may resolve through the registered
    /// table at run time: the head is never declared unconditionally
    /// anywhere (no declaration for a later object binding to shadow the
    /// table). The checked read serves a recorded flat first and the
    /// handle otherwise, so journaled or pre-allocated flats ride along
    /// instead of taking the bare column — which would refuse where the
    /// tree serves the handle.
    fn enum_runtime_ok(&self, head: &str) -> bool {
        self.decls.owner(head).is_none()
    }

    /// Whether a speculatively-read name gained a real binding after the
    /// read: same-statement binds and loop targets (no emitted store
    /// needed), unconditional declarations, an emitted declaring store to
    /// its column, or a journal home resolving to it. Anything else is
    /// still unbound, so dotted reads keep the full-path fallback.
    fn speculative_binding(&self, name: &str) -> bool {
        if self.loop_bound(name)
            || self.stmt_bound.contains(name)
            || self.decls.declared(name, self.module_pos, self.top_level)
        {
            return true;
        }
        let Some(slot) = self.globals.get(name).copied() else {
            return false;
        };
        self.blocks.iter().any(|block| {
            block.instrs.iter().any(|instr| match instr {
                Instruction::GStore { slot: target, .. }
                | Instruction::GStoreInit { slot: target, .. }
                | Instruction::GStoreLoop { slot: target, .. } => *target == slot,
                Instruction::JournalWrite { home, .. } => {
                    self.symbols.resolve(*home).is_some_and(|home| home == name)
                }
                _ => false,
            })
        })
    }

    fn const_value(&mut self, value: ConstValue) -> ConstId {
        self.consts.intern(value)
    }

    fn intern(&mut self, name: &str) -> SymbolId {
        self.symbols.intern(name)
    }

    /// Intern one explicit generic argument, mirroring the syntax shape.
    fn intern_type_arg(&mut self, ty: &PineType) -> CallTypeArg {
        CallTypeArg {
            name: self.intern(&ty.name),
            args: ty
                .args
                .iter()
                .map(|arg| self.intern_type_arg(arg))
                .collect(),
        }
    }

    /// One literal default as a link-time constant: numbers, strings,    /// booleans, and colors inline exactly like plain user calls;
    /// anything computed stays tree-walk when a call leaves it unfilled.
    fn literal_const(expr: &PineExpr) -> Option<ConstValue> {
        match expr {
            PineExpr::Number(v) => Some(ConstValue::Float(*v)),
            PineExpr::StringLiteral(text) => Some(ConstValue::Str(text.clone())),
            PineExpr::Boolean(flag) => Some(ConstValue::Bool(*flag)),
            PineExpr::ColorLiteral(packed) => Some(ConstValue::Color(*packed)),
            _ => None,
        }
    }

    /// Fold one static default to a pool constant in defining scope: a
    /// bare `na`, an enum member under the importer-aware order check,
    /// then a display constant — the same order as reads. Anything
    /// caller-dependent is `None`, keeping the computed mark for the
    /// same-module unanimity path or the honest fallback.
    fn fold_static_default(&mut self, expr: &PineExpr) -> Option<ConstValue> {
        let PineExpr::Variable(dotted) = expr else {
            return None;
        };
        if dotted == "na" {
            return Some(ConstValue::Na);
        }
        if let Some(handle) = self
            .enums
            .lookup(dotted, &self.key, self.module_pos, None, false)
        {
            let namespace = self.intern(&handle.enum_name);
            let member = self.intern(&handle.member);
            return Some(ConstValue::Enum { namespace, member });
        }
        let value = super::builtins_core::resolve_constant(dotted)?;
        match value {
            PineValue::Int(v) => Some(ConstValue::Int(v)),
            PineValue::ColorHandle(handle) => Some(ConstValue::Color(handle.raw())),
            PineValue::EnumHandle(handle) => {
                let namespace = self.intern(&handle.enum_name);
                let member = self.intern(&handle.member);
                Some(ConstValue::Enum { namespace, member })
            }
            _ => None,
        }
    }

    /// Link-time method table: per (type, method) the body block plus
    /// bindable parameters with literal defaults interned. Computed
    /// defaults mark the entry so underfilled calls keep their fallback.
    fn method_table(&mut self) -> HashMap<(String, String), MethodEntry> {
        let mut methods = HashMap::new();
        let sigs: Vec<((String, String), MethodSig)> = std::mem::take(&mut self.method_blocks)
            .into_iter()
            .collect();
        for ((type_name, method), sig) in sigs {
            let mut params = Vec::with_capacity(sig.params.len().saturating_sub(1));
            let mut has_computed = false;
            for (name, default) in sig.params.iter().skip(1) {
                match default {
                    None => params.push((name.clone(), None)),
                    Some(expr) => match Self::literal_const(expr) {
                        Some(value) => {
                            let id = self.const_value(value);
                            params.push((name.clone(), Some(id)));
                        }
                        None => {
                            // Static members fold in defining scope: enum
                            // members (importer-aware order check), then
                            // display constants — the same order as reads.
                            // Cross-module calls fill them as values with
                            // no receiver tracking, exactly like literals.
                            // A bare `na` fills absence the same way.
                            // Anything caller-dependent keeps the computed
                            // mark for the same-module unanimity path or
                            // the honest fallback.
                            if let Some(value) = Self::fold_static_default(self, expr) {
                                let id = self.const_value(value);
                                params.push((name.clone(), Some(id)));
                            } else {
                                has_computed = true;
                                params.push((name.clone(), None));
                            }
                        }
                    },
                }
            }
            methods.insert(
                (type_name, method),
                MethodEntry {
                    block: sig.block,
                    params,
                    has_computed,
                    receiver: sig.receiver,
                },
            );
        }
        methods
    }

    /// Index one plain function: block plus ordered params, with
    /// overloads falling back exactly like the tree-walk's refusal.
    fn index_func(&mut self, name: &str, params: &[super::syntax::Param]) {
        self.blocks.push(CompiledBlock::new());
        let index = self.blocks.len() - 1;
        let signature = params
            .iter()
            .map(|param| (param.name.clone(), param.default.clone()))
            .collect();
        if self
            .func_blocks
            .insert(name.to_owned(), (index, signature))
            .is_some()
        {
            self.overloaded.insert(name.to_owned());
        }
    }

    /// Index one method under its receiver type: block, ordered params,
    /// and the receiver alias method bodies spell `this` through. A
    /// missing or untyped receiver never dispatches — the tree-walk
    /// refuses registration, so the body keeps a fallback up front and
    /// the tree-walk decides it identically.
    fn index_method(&mut self, name: &str, params: &[super::syntax::Param]) {
        self.blocks.push(CompiledBlock::new());
        let index = self.blocks.len() - 1;
        let valid = match params.first() {
            Some(head) => match head.type_name.as_ref() {
                Some(type_name) => !super::eval::trailing_type(&type_name.name).is_empty(),
                None => false,
            },
            None => false,
        };
        if !valid {
            let saved = self.current;
            self.current = index;
            self.fallback(FallbackReason::InstanceCall);
            self.current = saved;
            return;
        }
        let head = params.first().expect("checked above");
        let receiver =
            super::eval::trailing_type(&head.type_name.as_ref().expect("checked above").name)
                .to_owned();
        self.any_method_alias.insert(head.name.clone());
        let signature = params
            .iter()
            .map(|param| (param.name.clone(), param.default.clone()))
            .collect();
        self.method_blocks.insert(
            (receiver, name.to_owned()),
            MethodSig {
                block: index,
                params: signature,
                receiver: head.name.clone(),
            },
        );
    }

    /// Receiver type of one method declaration, if it can ever dispatch.
    fn method_key(name: &str, params: &[super::syntax::Param]) -> Option<(String, String)> {
        let head = params.first()?;
        let type_name = head.type_name.as_ref()?;
        let receiver = super::eval::trailing_type(&type_name.name).to_owned();
        if receiver.is_empty() {
            return None;
        }
        Some((receiver, name.to_owned()))
    }

    fn fallback(&mut self, reason: FallbackReason) {
        self.emit(Instruction::Fallback {
            reason,
            node: crate::vars::NodeId::ROOT,
        });
    }

    fn compile(&mut self, program: &PineProgram) {
        // Index pass: every function body gets a block (with parameter
        // names) before any call can reference it — forward references
        // included. Methods index separately by receiver type: same-named
        // methods on different types coexist, and bare calls to a method
        // name refuse exactly like the tree-walk instead of borrowing a
        // method body. Types index by name for construction.
        for stmt in &program.statements {
            match stmt {
                PineStmt::FuncDecl { name, params, .. } => {
                    self.index_func(name, params);
                }
                PineStmt::MethodDecl { name, params, .. } => {
                    self.index_method(name, params);
                }
                PineStmt::TypeDecl { name, fields, .. } => {
                    self.own_types.insert(name.clone());
                    self.own_type_fields.insert(
                        name.clone(),
                        fields
                            .iter()
                            .map(|field| (field.name.clone(), field.default.clone()))
                            .collect(),
                    );
                }
                PineStmt::ExportDecl(inner) => match inner.as_ref() {
                    PineStmt::FuncDecl { name, params, .. } => {
                        self.index_func(name, params);
                    }
                    PineStmt::MethodDecl { name, params, .. } => {
                        self.index_method(name, params);
                    }
                    PineStmt::TypeDecl { name, fields, .. } => {
                        self.own_types.insert(name.clone());
                        self.own_type_fields.insert(
                            name.clone(),
                            fields
                                .iter()
                                .map(|field| (field.name.clone(), field.default.clone()))
                                .collect(),
                        );
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        // Static pre-passes run before any lowering: top-level statements
        // consult the same registries the bodies do, so a name later in
        // the module must be visible here exactly as it is there.
        // Map-held names emit provenance tags so receiver mutations
        // persist into the map entry, exactly like the tree-walk.
        Self::collect_map_prov(&program.statements, &mut self.map_prov_names);
        let mut dotted_homes = std::mem::take(&mut self.dotted_homes);
        self.collect_dotted_homes(&program.statements, &mut dotted_homes);
        self.dotted_homes = dotted_homes;
        Self::collect_frame_var_names(&program.statements, &mut self.any_frame_var);
        // Entry pass: the module top level runs into block 0.
        let mut bodies = Vec::new();
        let mut method_bodies = Vec::new();
        for (index, stmt) in program.statements.iter().enumerate() {
            // Declaration order matters for enum uses below: a use only
            // resolves when its declaration already executed, so the
            // current top-level index travels with every lowering. The
            // same-statement bound set resets here: only earlier items
            // of this statement count as bound.
            self.top_level = Some(index);
            self.stmt_bound.clear();
            match stmt {
                PineStmt::FuncDecl { name, body, .. } => {
                    bodies.push((name.clone(), body.clone()));
                }
                PineStmt::MethodDecl {
                    name, body, params, ..
                } => {
                    method_bodies.push((Self::method_key(name, params), body.clone()));
                }
                PineStmt::ExportDecl(inner) => match inner.as_ref() {
                    PineStmt::FuncDecl { name, body, .. } => {
                        bodies.push((name.clone(), body.clone()));
                    }
                    PineStmt::MethodDecl {
                        name, body, params, ..
                    } => {
                        method_bodies.push((Self::method_key(name, params), body.clone()));
                    }
                    _ => self.lower_stmt(inner),
                },
                _ => self.lower_stmt(stmt),
            }
        }
        for (name, body) in bodies {
            self.compile_function(&name, &body);
        }
        for (key, body) in method_bodies {
            self.compile_method(key.as_ref(), &body);
        }
    }

    /// Every name bound from `map.get(...)` anywhere in scope: reads and
    /// calls on such variables keep their fallback, since the tree-walk
    /// routes mutated results back through the map entry.
    /// Collect every non-`this` dotted name passed to a call that can
    /// write a collection back. A write-back through such a home creates a
    /// flat variable with the dotted spelling (the tree-walk's assign
    /// path), and later reads prefer that variable over a field lookup —
    /// so those reads must stay on the tree-walk, where the runtime
    /// presence decides. Capture and builtin calls never write back, so
    /// their arguments do not poison the name.
    fn collect_dotted_homes(&self, stmts: &[PineStmt], into: &mut HashSet<String>) {
        struct Collector<'a> {
            user_calls: &'a FuncIndex,
        }
        fn is_user_call(call: &PineExpr, user_calls: &FuncIndex) -> bool {
            match call {
                PineExpr::FunctionCall { name, .. } => user_calls.contains_key(name),
                PineExpr::MethodCall { namespace, .. } => {
                    !matches!(
                        namespace.as_str(),
                        "strategy" | "runtime" | "request" | "label" | "table" | "box" | "line"
                    ) && BuiltinNamespace::parse(namespace).is_none()
                }
                PineExpr::InstanceCall { .. } => true,
                _ => false,
            }
        }
        impl PartitionVisit for Collector<'_> {
            type State = HashSet<String>;
            fn stmt(&mut self, _stmt: &PineStmt, _state: &mut Self::State) -> WalkVisit {
                WalkVisit::Recurse
            }
            fn expr(&mut self, expr: &PineExpr, state: &mut Self::State) -> WalkVisit {
                let args: &[CallArg] = match expr {
                    PineExpr::FunctionCall { args, .. }
                    | PineExpr::MethodCall { args, .. }
                    | PineExpr::InstanceCall { args, .. } => args,
                    _ => return WalkVisit::Recurse,
                };
                if is_user_call(expr, self.user_calls) {
                    for arg in args {
                        if let PineExpr::Variable(name) = &arg.value {
                            if name.contains('.') && name != "this" && !name.starts_with("this.") {
                                state.insert(name.clone());
                            }
                        }
                    }
                }
                WalkVisit::Recurse
            }
        }
        let mut collector = Collector {
            user_calls: &self.func_blocks,
        };
        walk_stmts(stmts, &mut collector, into);
    }

    fn collect_map_prov(stmts: &[PineStmt], into: &mut HashSet<String>) {
        struct Collector;
        fn record_value(value: &PineExpr, into: &mut HashSet<String>, name: &str) {
            if let PineExpr::MethodCall {
                namespace, method, ..
            } = value
            {
                if namespace == "map" && method == "get" {
                    into.insert(name.to_owned());
                }
            }
        }
        impl PartitionVisit for Collector {
            type State = HashSet<String>;
            fn stmt(&mut self, stmt: &PineStmt, state: &mut Self::State) -> WalkVisit {
                match stmt {
                    PineStmt::Declaration { name, value, .. }
                    | PineStmt::Assignment { name, value }
                    | PineStmt::Reassign { name, value } => {
                        record_value(value, state, name);
                    }
                    PineStmt::MultiAssign { items } => {
                        for item in items {
                            record_value(&item.value, state, &item.name);
                        }
                    }
                    PineStmt::MultiDeclaration { decls } => {
                        for decl in decls {
                            if let Some(value) = &decl.value {
                                record_value(value, state, &decl.name);
                            }
                        }
                    }
                    _ => {}
                }
                WalkVisit::Recurse
            }
            fn expr(&mut self, _expr: &PineExpr, _state: &mut Self::State) -> WalkVisit {
                WalkVisit::Recurse
            }
        }
        let mut seen = Collector;
        walk_stmts(stmts, &mut seen, into);
    }

    /// One body’s in-function `var`/`varip` names into a set: nested
    /// function bodies prune, so each body only sees its own instances —
    /// exactly like the tree-walk’s per-body scope walk.
    fn collect_frame_var_names(body: &[PineStmt], into: &mut HashSet<String>) {
        struct Collector;
        impl PartitionVisit for Collector {
            type State = HashSet<String>;
            fn stmt(&mut self, stmt: &PineStmt, state: &mut Self::State) -> WalkVisit {
                match stmt {
                    PineStmt::FuncDecl { .. } | PineStmt::MethodDecl { .. } => WalkVisit::Prune,
                    PineStmt::VarDeclaration { name, .. } => {
                        state.insert(name.clone());
                        WalkVisit::Recurse
                    }
                    _ => WalkVisit::Recurse,
                }
            }
            fn expr(&mut self, _expr: &PineExpr, _state: &mut Self::State) -> WalkVisit {
                WalkVisit::Recurse
            }
        }
        let mut seen = Collector;
        walk_stmts(body, &mut seen, into);
    }

    /// One body’s in-function `var`/`varip` declarations with ordinals:
    /// first declaration wins, mirroring the tree-walk’s once-per-scope
    /// registration.
    fn frame_var_table(body: &[PineStmt]) -> HashMap<String, (u32, bool)> {
        struct Collector;
        impl PartitionVisit for Collector {
            type State = HashMap<String, (u32, bool)>;
            fn stmt(&mut self, stmt: &PineStmt, state: &mut Self::State) -> WalkVisit {
                match stmt {
                    PineStmt::FuncDecl { .. } | PineStmt::MethodDecl { .. } => WalkVisit::Prune,
                    PineStmt::VarDeclaration { name, is_varip, .. } => {
                        let ordinal = state.len() as u32;
                        state.entry(name.clone()).or_insert((ordinal, *is_varip));
                        WalkVisit::Recurse
                    }
                    _ => WalkVisit::Recurse,
                }
            }
            fn expr(&mut self, _expr: &PineExpr, _state: &mut Self::State) -> WalkVisit {
                WalkVisit::Recurse
            }
        }
        let mut table = HashMap::new();
        let mut seen = Collector;
        walk_stmts(body, &mut seen, &mut table);
        table
    }

    fn compile_function(&mut self, name: &str, body: &[PineStmt]) {
        let (index, params) = match self.func_blocks.get(name) {
            Some(entry) => entry.clone(),
            None => return,
        };
        self.lower_body(index, &params, body);
    }

    /// Compile one method body under its type key: untyped or missing
    /// receivers never dispatch (the tree-walk refuses registration), so
    /// the body keeps a fallback and the tree-walk decides it identically.
    fn compile_method(&mut self, key: Option<&(String, String)>, body: &[PineStmt]) {
        let Some(key) = key else {
            return;
        };
        let Some(entry) = self.method_blocks.get(key) else {
            return;
        };
        let (index, params) = (entry.block, entry.params.clone());
        if params.is_empty() {
            self.current = index;
            self.fallback(FallbackReason::InstanceCall);
            return;
        }
        self.lower_body(index, &params, body);
    }

    /// Lower one indexed body (function or method) into its block: the
    /// shared frame, journal, and per-instance-cell protocol. Method
    /// bodies ride the same path — `this` and alias reads resolve at run
    /// time through the receiver stack, never through these locals.
    fn lower_body(
        &mut self,
        index: usize,
        params: &[(String, Option<PineExpr>)],
        body: &[PineStmt],
    ) {
        // Static escape probe before lowering: a body that may write
        // caller-visible state lowers with the loop-side journal, while
        // locals-only bodies keep the values-only fast path. The probe
        // runs over the already-indexed body and consults only the
        // completed top-level namespace — never a runtime guess. The
        // body-wide written set travels alongside: history reads of
        // written names stay tree-walk even when the read precedes the
        // write, because the tree serves per-call qualified history.
        let probe = Self::escape_set(body);
        let journaled = Self::escapes(&probe, &self.globals);
        let saved_block = self.current;
        let saved_frame = self.frame_slots;
        let saved_locals = std::mem::take(&mut self.locals);
        let saved_function = self.in_function;
        // Loop targets never cross a call boundary: a body reached from
        // anywhere runs after its own loops seed, never after the
        // definition site's. Restore unconditionally below.
        let saved_loop_targets = std::mem::take(&mut self.loop_targets);
        // Same-statement bounds never cross a call boundary either:
        // bodies resolve their own locals, never the definition
        // site's trailing items.
        let saved_stmt_bound = std::mem::take(&mut self.stmt_bound);
        let saved_journaled = self.journaled;
        let saved_written = std::mem::take(&mut self.journaled_written);
        let saved_frame_vars = std::mem::take(&mut self.frame_vars);
        let saved_history_locals = std::mem::take(&mut self.history_locals);
        self.journaled = journaled;
        self.journaled_written = probe.written;
        self.frame_vars = Self::frame_var_table(body);
        self.current = index;
        self.frame_slots = 0;
        self.in_function = true;
        self.blocks[index].params = params
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>()
            .into();
        for (param, _) in params {
            let slot = SlotId(self.frame_slots);
            self.frame_slots += 1;
            self.locals.insert(param.clone(), slot);
        }
        if let Some(block) = self.blocks.get_mut(index) {
            block.slots = self.frame_slots;
        }
        let mut trailing = None;
        for stmt in body {
            match self.lower_stmt_value(stmt) {
                Ok(slot) => trailing = slot,
                Err(reason) => {
                    self.fallback(reason);
                    trailing = None;
                }
            }
        }
        self.emit(Instruction::Return { src: trailing });
        // Freeze the frame's named slots for nested home resolution:
        // params plus body-bound locals, exactly what the tree-walk's
        // shared namespace would expose to a nested call.
        let frame_names: HashMap<String, u32> = self
            .locals
            .iter()
            .map(|(name, slot)| (name.clone(), slot.0))
            .collect();
        self.blocks[index].frame_names = std::sync::Arc::new(frame_names);
        // Only slots that still exist at body end can be recorded: a name
        // read before its journaled mirror is created resolves here.
        let history_locals: Vec<(SymbolId, u32)> = self
            .history_locals
            .iter()
            .filter_map(|name| {
                let text = self.symbols.resolve(*name)?.to_owned();
                let slot = self.locals.get(&text)?;
                Some((*name, slot.0))
            })
            .collect();
        self.blocks[index].history_locals = history_locals.into();
        self.current = saved_block;
        self.frame_slots = saved_frame;
        self.locals = saved_locals;
        self.in_function = saved_function;
        self.loop_targets = saved_loop_targets;
        self.stmt_bound = saved_stmt_bound;
        self.journaled = saved_journaled;
        self.journaled_written = saved_written;
        self.frame_vars = saved_frame_vars;
        self.history_locals = saved_history_locals;
    }

    /// Shared escape probe over one function body: which plain names it
    /// may write and whether it escapes (rebinds, loops, persistence).
    /// Both the journal decision and the history-read fallback compose
    /// this single walk, so the two never disagree on what the body
    /// touches.
    fn escape_set(body: &[PineStmt]) -> EscapeSet {
        let mut probe = EscapeSet::default();
        walk_stmts(body, &mut EscapeProbe, &mut probe);
        probe
    }

    /// Whether one probed body escapes (needs the journal): rebinds,
    /// loops, persistence, or an assignment to a name the shared
    /// namespace already binds. Composed from the shared probe so the
    /// journal decision and the history fallback never disagree.
    fn escapes(probe: &EscapeSet, globals: &HashMap<String, SlotId>) -> bool {
        probe.escapes || probe.written.iter().any(|name| globals.contains_key(name))
    }

    /// Lower one statement for effect; returns the trailing value slot
    /// (function-body tail protocol) or `None`. A `Err` reason is emitted
    /// as exactly one `Fallback` by the caller contract below: statement
    /// handlers own fallback emission, expression lowerers only report.
    fn lower_stmt_value(&mut self, stmt: &PineStmt) -> Result<Option<SlotId>, FallbackReason> {
        match stmt {
            // Trailing call expressions keep statement semantics (mutating
            // collection calls persist), exactly like the tree-walk's
            // statement-then-value path; nested calls stay functional.
            PineStmt::Expression(PineExpr::MethodCall {
                namespace,
                method,
                args,
                type_args,
                ..
            }) => Ok(Some(
                self.lower_method_call(namespace, method, args, type_args, true)?,
            )),
            PineStmt::Expression(expr) => Ok(Some(self.lower_expr_slot(expr)?)),
            PineStmt::If {
                condition,
                then_branch,
                else_branch,
            } => self.lower_if(condition, then_branch, else_branch.as_deref()),
            // A trailing statement-switch returns its arm value exactly
            // like the tree-walk's body-value protocol (and like `if`
            // above): non-trailing positions discard the merge slot, so
            // effect-only switches keep their shape with one extra temp.
            PineStmt::Switch(switch) => self.lower_switch_value(switch).map(Some),
            _ => {
                self.lower_stmt(stmt);
                Ok(None)
            }
        }
    }

    /// Lower one statement for effect. Errors become exactly one
    /// `Fallback` with the deepest named reason (statement handlers own
    /// fallback emission; expression lowerers only report).
    fn lower_stmt(&mut self, stmt: &PineStmt) {
        let result = self.lower_stmt_inner(stmt);
        if let Err(reason) = result {
            self.fallback(reason);
            return;
        }
        // Provenance tags follow every binding of a map-held name: a
        // `map.get` right-hand side tags it, anything else clears it. The
        // tree-walk re-evaluates the key after the assignment, so the tag
        // observes exactly the same value.
        match stmt {
            PineStmt::Assignment { name, value }
            | PineStmt::Reassign { name, value }
            | PineStmt::Declaration { name, value, .. }
            | PineStmt::VarDeclaration { name, value, .. } => {
                self.lower_map_prov(name, value);
            }
            PineStmt::MultiAssign { items } => {
                for item in items {
                    self.lower_map_prov(&item.name, &item.value);
                }
            }
            PineStmt::MultiDeclaration { decls } => {
                for decl in decls {
                    if let Some(value) = &decl.value {
                        self.lower_map_prov(&decl.name, value);
                    }
                }
            }
            _ => {}
        }
    }

    /// Emit one provenance update for a name the module ever tags with
    /// `map.get`. A non-tagging binding clears the tag, exactly like the
    /// tree-walk's `refresh_prov`; names never tagged pay nothing.
    fn lower_map_prov(&mut self, name: &str, value: &PineExpr) {
        if !self.map_prov_names.contains(name) {
            return;
        }
        let tag = match value {
            PineExpr::MethodCall {
                namespace,
                method,
                args,
                ..
            } if namespace == "map" && method == "get" && args.len() == 2 => match &args[0].value {
                PineExpr::Variable(map_var) => {
                    let map = self.intern(map_var);
                    match self.lower_expr_slot(&args[1].value) {
                        Ok(key) => Some((map, key)),
                        Err(_) => None,
                    }
                }
                _ => None,
            },
            _ => None,
        };
        let name = self.intern(name);
        self.emit(Instruction::SetMapProv { name, tag });
    }

    fn lower_stmt_inner(&mut self, stmt: &PineStmt) -> Result<(), FallbackReason> {
        match stmt {
            PineStmt::Import(_)
            | PineStmt::LibraryDecl { .. }
            | PineStmt::IndicatorDecl { .. }
            | PineStmt::TypeDecl { .. } => Ok(()),
            PineStmt::EnumDecl { name, members } => {
                // Registration mark: the run registers the member set on
                // first execution (conditional, duplicate, and per-call
                // declarations find it occupied and keep it), exactly like
                // the tree-walk's declare-once table — so member reads
                // that static order cannot prove take the checked read.
                let enm = self.intern(name);
                let members = members
                    .iter()
                    .map(|member| self.intern(&member.name))
                    .collect();
                self.emit(Instruction::EnumMark { enm, members });
                Ok(())
            }
            PineStmt::ConstDecl { name, value, .. } => {
                let slot = self.lower_expr_slot(value)?;
                if self.in_function {
                    // Compile-time bindings evaluate once per call without
                    // touching plot tags on either engine.
                    let dst = self.temp();
                    self.emit(Instruction::Copy { dst, src: slot });
                    self.locals.insert(name.clone(), dst);
                    return Ok(());
                }
                let plot_tag = self.plot_tag_of(value);
                let global = self.global(name);
                self.emit(Instruction::GStore {
                    slot: global,
                    src: slot,
                    plot_tag,
                });
                Ok(())
            }
            PineStmt::Declaration { name, value, .. } => {
                if self.in_function {
                    // Locals-only discipline: the binding lives in the call
                    // frame and discards on return — exactly the tree-walk's
                    // journal teardown, with no caller state touched. In-function
                    // `var` cells route before plain frame bindings,
                    // mirroring the tree-walk's scope-walk order on writes.
                    // A `plot()` value tags the run-global site while the
                    // value itself stays frame-local.
                    if let Some((ordinal, is_varip)) = self.frame_vars.get(name).copied() {
                        if let Some(args) = plot_call_args(value) {
                            let (na, site) = self.lower_plot_value(args)?;
                            let tag = self.intern(name);
                            self.emit(Instruction::VarStore {
                                ordinal,
                                is_varip,
                                src: na,
                                name: tag,
                                strict: false,
                            });
                            self.emit(Instruction::PlotTag {
                                name: tag,
                                tag: Some(site),
                            });
                            return Ok(());
                        }
                        let slot = self.lower_expr_slot(value)?;
                        let tag = self.intern(name);
                        self.emit(Instruction::VarStore {
                            ordinal,
                            is_varip,
                            src: slot,
                            name: tag,
                            strict: false,
                        });
                        self.emit(Instruction::PlotTag {
                            name: tag,
                            tag: None,
                        });
                        return Ok(());
                    }
                    if let Some(args) = plot_call_args(value) {
                        return self.lower_plot_assign(name, args, false);
                    }
                    let slot = self.lower_expr_slot(value)?;
                    let dst = self.temp();
                    self.emit(Instruction::Copy { dst, src: slot });
                    self.locals.insert(name.clone(), dst);
                    let tag = self.intern(name);
                    self.emit(Instruction::PlotTag {
                        name: tag,
                        tag: None,
                    });
                    return Ok(());
                }
                let slot = self.lower_expr_slot(value)?;
                let plot_tag = self.plot_tag_of(value);
                let global = self.global(name);
                self.emit(Instruction::GStore {
                    slot: global,
                    src: slot,
                    plot_tag,
                });
                Ok(())
            }
            PineStmt::VarDeclaration {
                name,
                value,
                is_varip,
                ..
            } => {
                if self.in_function {
                    // Per-instance cells keyed by declaration-site
                    // identity: seed on first evaluation per instance,
                    // never again — exactly the tree-walk's once-per-run
                    // initializer under the same identity.
                    if let Some((ordinal, cell_varip)) = self.frame_vars.get(name).copied() {
                        // First declaration wins, mirroring the tree-walk's
                        // once-per-scope registration under one key.
                        let slot = self.lower_expr_slot(value)?;
                        let tag = self.intern(name);
                        self.emit(Instruction::VarDecl {
                            ordinal,
                            is_varip: cell_varip,
                            src: slot,
                            name: tag,
                        });
                        return Ok(());
                    }
                    return Err(FallbackReason::FunctionBodyBinding {
                        name: self.intern(name),
                    });
                }
                let slot = self.lower_expr_slot(value)?;
                let global = self.global(name);
                if *is_varip {
                    self.varip.insert(global);
                }
                // Initializers run once, on the first evaluated bar —
                // exactly like the tree-walk's declaration gate.
                self.emit(Instruction::GStoreInit {
                    slot: global,
                    src: slot,
                });
                Ok(())
            }
            PineStmt::Assignment { name, value } => {
                if self.in_function {
                    // In-function `var` cells first: the tree-walk routes
                    // writes via its scope walk before plain frame
                    // bindings, and the loop mirrors exactly that order.
                    if let Some((ordinal, is_varip)) = self.frame_vars.get(name).copied() {
                        if let Some(args) = plot_call_args(value) {
                            let (na, site) = self.lower_plot_value(args)?;
                            let tag = self.intern(name);
                            self.emit(Instruction::VarStore {
                                ordinal,
                                is_varip,
                                src: na,
                                name: tag,
                                strict: false,
                            });
                            self.emit(Instruction::PlotTag {
                                name: tag,
                                tag: Some(site),
                            });
                            return Ok(());
                        }
                        let slot = self.lower_expr_slot(value)?;
                        let tag = self.intern(name);
                        self.emit(Instruction::VarStore {
                            ordinal,
                            is_varip,
                            src: slot,
                            name: tag,
                            strict: false,
                        });
                        self.emit(Instruction::PlotTag {
                            name: tag,
                            tag: None,
                        });
                        return Ok(());
                    }
                    // A `plot()` value tags the run-global site while the
                    // value itself stays frame-local — never a global
                    // store, mirroring the tree-walk's tag-on-assign split.
                    if let Some(args) = plot_call_args(value) {
                        let dotted =
                            name == "this" || name.starts_with("this.") || name.contains('.');
                        if dotted {
                            return Err(FallbackReason::FunctionBodyBinding {
                                name: self.intern(name),
                            });
                        }
                        return self.lower_plot_assign(name, args, false);
                    }
                    let slot = self.lower_expr_slot(value)?;
                    if self.journaled {
                        // Escaping bodies route caller-visible writes
                        // through the loop-side journal (logged once per
                        // call, reverted on return); persistent cells
                        // stay tree-walk until per-instance persistence
                        // lowers. Dotted/`this` homes keep the exact
                        // pre-journal behavior below for the object model
                        // to lower.
                        let dotted =
                            name == "this" || name.starts_with("this.") || name.contains('.');
                        if !dotted {
                            if self.decls.is_persistent(name) {
                                return Err(FallbackReason::FunctionBodyBinding {
                                    name: self.intern(name),
                                });
                            }
                            self.emit_journaled(name, slot, false);
                            let tag = self.intern(name);
                            self.emit(Instruction::PlotTag {
                                name: tag,
                                tag: None,
                            });
                            return Ok(());
                        }
                    }
                    if let Some(local) = self.locals.get(name) {
                        let dst = *local;
                        self.emit(Instruction::Copy { dst, src: slot });
                        let tag = self.intern(name);
                        self.emit(Instruction::PlotTag {
                            name: tag,
                            tag: None,
                        });
                        return Ok(());
                    }
                    // Fresh frame local: `=` both declares and assigns in
                    // Pine, and the tree-walk journals the frame binding
                    // away on return — the same discard the frame gives.
                    let dst = self.temp();
                    self.emit(Instruction::Copy { dst, src: slot });
                    self.locals.insert(name.clone(), dst);
                    let tag = self.intern(name);
                    self.emit(Instruction::PlotTag {
                        name: tag,
                        tag: None,
                    });
                    return Ok(());
                }
                let slot = self.lower_expr_slot(value)?;
                let plot_tag = self.plot_tag_of(value);
                let global = self.global(name);
                self.emit(Instruction::GStore {
                    slot: global,
                    src: slot,
                    plot_tag,
                });
                Ok(())
            }
            PineStmt::Reassign { name, value } => {
                // Top-level rebinding needs an unconditional declaration
                // that already executed — same module earlier, or an
                // earlier module (the shared columns the linker interns
                // stay synchronized). A statically unknown plain name
                // lowers to the checked store instead of falling back:
                // the loop refuses at run time exactly when the column
                // was never recorded (conditional declarations that ran
                // bind normally), mirroring the tree-walk's rebind rule.
                // Dotted takes still resolve at run time through the
                // object model, so they keep the fallback.
                // In-function `var` cells skip the static check: the
                // seeded set decides at run time, exactly like the
                // tree-walk's registration check.
                let known = if self.in_function {
                    // Journaled bodies resolve caller homes at run time
                    // (caller frame plus shared globals), so the static
                    // declaration check does not apply: unresolvable
                    // homes refuse through the shared constructor below.
                    // Dotted takes resolve at run time the same way.
                    self.journaled
                        || self.locals.contains_key(name)
                        || self.frame_vars.contains_key(name)
                        || self.field_put_home(name).is_some()
                } else {
                    self.loop_bound(name)
                        || self.decls.declared(name, self.module_pos, self.top_level)
                        || self.field_put_home(name).is_some()
                };
                if !known {
                    // Dotted takes resolve at run time through the object
                    // model, so they keep the fallback; in-function
                    // plain names are unreachable here (any body holding
                    // a rebind journals, taking the `known` path above)
                    // and stay fallback as defense in depth.
                    if self.in_function || name.contains('.') || name == "this" {
                        return Err(FallbackReason::ReassignUndeclared {
                            name: self.intern(name),
                        });
                    }
                    let slot = self.lower_expr_slot(value)?;
                    let plot_tag = self.plot_tag_of(value);
                    let global = self.global(name);
                    // A checked store binds nothing by itself: until a
                    // real declaration lands, dotted reads of this name
                    // keep the full-path fallback.
                    self.speculative_reads.insert(name.clone());
                    self.emit(Instruction::GReassign {
                        slot: global,
                        src: slot,
                        plot_tag,
                    });
                    return Ok(());
                }
                if self.in_function {
                    // In-function `var` cells first, mirroring the
                    // tree-walk's scope-walk order on writes.
                    if let Some((ordinal, is_varip)) = self.frame_vars.get(name).copied() {
                        if let Some(args) = plot_call_args(value) {
                            let (na, site) = self.lower_plot_value(args)?;
                            let tag = self.intern(name);
                            self.emit(Instruction::VarStore {
                                ordinal,
                                is_varip,
                                src: na,
                                name: tag,
                                strict: true,
                            });
                            self.emit(Instruction::PlotTag {
                                name: tag,
                                tag: Some(site),
                            });
                            return Ok(());
                        }
                        let slot = self.lower_expr_slot(value)?;
                        let tag = self.intern(name);
                        self.emit(Instruction::VarStore {
                            ordinal,
                            is_varip,
                            src: slot,
                            name: tag,
                            strict: true,
                        });
                        self.emit(Instruction::PlotTag {
                            name: tag,
                            tag: None,
                        });
                        return Ok(());
                    }
                    if let Some(args) = plot_call_args(value) {
                        let dotted =
                            name == "this" || name.starts_with("this.") || name.contains('.');
                        if dotted {
                            return Err(FallbackReason::ReassignUndeclared {
                                name: self.intern(name),
                            });
                        }
                        let strict = !self.locals.contains_key(name);
                        return self.lower_plot_assign(name, args, strict);
                    }
                    let slot = self.lower_expr_slot(value)?;
                    if self.journaled {
                        // Escaping rebinds resolve their caller home at
                        // run time and refuse unresolvable ones through
                        // the shared constructor; persistent cells stay
                        // tree-walk until per-instance persistence lowers.
                        // Names already bound in this body (parameters or
                        // earlier journaled writes) never refuse: the
                        // tree-walk finds them bound too. Receiver
                        // field writes hit the live slot in place.
                        // Single-dot variable paths take the instance out
                        // of its home, mutate it, and write it back; other
                        // dotted targets keep the tree-walk's exact refusal
                        // for the object model to lower.
                        if let Some(field) = name.strip_prefix("this.") {
                            let field = self.intern(field);
                            self.emit(Instruction::ThisFieldSet { field, src: slot });
                            return Ok(());
                        }
                        if let Some(home) = self.field_put_home(name) {
                            let field = name.split_once('.').expect("dotted").1;
                            let field = self.intern(field);
                            self.emit(Instruction::FieldPut {
                                home,
                                field,
                                src: slot,
                            });
                            return Ok(());
                        }
                        if name == "this" || name.contains('.') {
                            return Err(FallbackReason::ReassignUndeclared {
                                name: self.intern(name),
                            });
                        }
                        if self.decls.is_persistent(name) {
                            return Err(FallbackReason::FunctionBodyBinding {
                                name: self.intern(name),
                            });
                        }
                        let strict = !self.locals.contains_key(name);
                        self.emit_journaled(name, slot, strict);
                        let tag = self.intern(name);
                        self.emit(Instruction::PlotTag {
                            name: tag,
                            tag: None,
                        });
                        return Ok(());
                    }
                    if let Some(field) = name.strip_prefix("this.") {
                        let field = self.intern(field);
                        self.emit(Instruction::ThisFieldSet { field, src: slot });
                        return Ok(());
                    }
                    if let Some(home) = self.field_put_home(name) {
                        let field = name.split_once('.').expect("dotted").1;
                        let field = self.intern(field);
                        self.emit(Instruction::FieldPut {
                            home,
                            field,
                            src: slot,
                        });
                        return Ok(());
                    }
                    if let Some(local) = self.locals.get(name) {
                        let dst = *local;
                        self.emit(Instruction::Copy { dst, src: slot });
                        let tag = self.intern(name);
                        self.emit(Instruction::PlotTag {
                            name: tag,
                            tag: None,
                        });
                        return Ok(());
                    }
                    return Err(FallbackReason::FunctionBodyBinding {
                        name: self.intern(name),
                    });
                }
                if let Some(field) = name.strip_prefix("this.") {
                    let slot = self.lower_expr_slot(value)?;
                    let field = self.intern(field);
                    self.emit(Instruction::ThisFieldSet { field, src: slot });
                    return Ok(());
                }
                let slot = self.lower_expr_slot(value)?;
                // Single-dot variable paths take the instance out of its
                // home, mutate it, and write it back; anything else keeps
                // the tag-preserving global store below.
                if let Some(home) = self.field_put_home(name) {
                    let field = name.split_once('.').expect("dotted").1;
                    let field = self.intern(field);
                    self.emit(Instruction::FieldPut {
                        home,
                        field,
                        src: slot,
                    });
                    return Ok(());
                }
                let plot_tag = self.plot_tag_of(value);
                let global = self.global(name);
                self.emit(Instruction::GStore {
                    slot: global,
                    src: slot,
                    plot_tag,
                });
                Ok(())
            }
            PineStmt::Expression(expr) => self.lower_expr_discard(expr),
            PineStmt::If {
                condition,
                then_branch,
                else_branch,
            } => self
                .lower_if(condition, then_branch, else_branch.as_deref())
                .map(|_| ()),
            PineStmt::FuncDecl { .. } | PineStmt::MethodDecl { .. } => {
                // Indexed in the first pass; bodies compile separately.
                Ok(())
            }
            PineStmt::ExportDecl(inner) => {
                self.lower_stmt(inner);
                Ok(())
            }
            PineStmt::For {
                variable,
                start,
                body,
                end,
                step,
            } => self.lower_for(variable, start, end, step.as_deref(), body),
            PineStmt::ForIn {
                targets,
                iterable,
                body,
            } => self.lower_for_in(targets, iterable, body),
            PineStmt::While { condition, body } => self.lower_while(condition, body),
            PineStmt::Break => self.lower_break(),
            PineStmt::Switch(switch) => self.lower_switch_statement(switch),
            PineStmt::MultiDeclaration { decls } => self.lower_multi_declaration(decls),
            PineStmt::DestructureAssign { targets, value } => {
                self.lower_destructure(targets, value)
            }
            PineStmt::MultiAssign { items } => self.lower_multi_assign(items),
        }
    }

    /// Lower a comma-separated assignment item by item, in order: each
    /// item runs its single-assignment rule (`=` declares and stores,
    /// `:=` rebinds a declared name with the same target checks), so a
    /// multi-assign emits exactly the stores the tree-walk executes.
    /// The first unlowerable item reports its own reason, like any
    /// single statement would.
    fn lower_multi_assign(
        &mut self,
        items: &[crate::pine::syntax::AssignItem],
    ) -> Result<(), FallbackReason> {
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
            self.lower_stmt_inner(&single)?;
            // `=` items bind for later items of the same statement,
            // exactly like the tree-walk's left-to-right evaluation;
            // `:=` items never bind.
            if matches!(item.op, AssignOp::Assign) {
                self.stmt_bound.insert(item.name.clone());
            }
        }
        Ok(())
    }

    /// Lower a comma-separated declaration item by item, in order: each
    /// item runs its single-assignment rule (declare plus init, absent
    /// initializers seed absence), so a multi-declaration emits exactly
    /// the stores the tree-walk executes. The first unlowerable item
    /// reports, like any single statement would; the statement arm
    /// masks the family to the tuple shape.
    fn lower_multi_declaration(
        &mut self,
        decls: &[crate::pine::syntax::MultiDecl],
    ) -> Result<(), FallbackReason> {
        for decl in decls {
            let value = match &decl.value {
                Some(value) => value.clone(),
                None => PineExpr::Variable("na".to_owned()),
            };
            let single = PineStmt::Assignment {
                name: decl.name.clone(),
                value: Box::new(value),
            };
            self.lower_stmt_inner(&single)?;
            // Items bind for later items of the same statement, exactly
            // like the tree-walk's left-to-right evaluation.
            self.stmt_bound.insert(decl.name.clone());
        }
        Ok(())
    }

    /// Lower a tuple destructure as length-check plus per-element
    /// extract plus per-item assign in order. The length check and
    /// extract share one `Destructure` instruction whose run-time
    /// refusals name the tree-walk's exact reasons; each element then
    /// stores through the single-assignment path (`Copy` into frame
    /// locals inside functions, tag-preserving global stores at top
    /// level — the destructure never refreshes tags, exactly like the
    /// tree-walk's plain assign path). Dotted/`this` targets keep the
    /// statement fallback for the object-model milestone.
    fn lower_destructure(
        &mut self,
        targets: &[String],
        value: &PineExpr,
    ) -> Result<(), FallbackReason> {
        let arr = self.lower_expr_slot(value)?;
        if targets
            .iter()
            .any(|name| name == "this" || name.starts_with("this.") || name.contains('.'))
        {
            return Err(FallbackReason::TupleUnpack);
        }
        let mut dsts = Vec::with_capacity(targets.len());
        for _ in targets {
            dsts.push(self.temp());
        }
        self.emit(Instruction::Destructure {
            dsts: dsts.clone(),
            arr,
        });
        for (name, dst) in targets.iter().zip(dsts) {
            if self.in_function {
                // In-function `var` targets store per-instance, mirroring
                // the tree-walk's scope-walk routing under `assign_path`.
                // Destructuring never refreshes plot tags on either engine.
                if let Some((ordinal, is_varip)) = self.frame_vars.get(name).copied() {
                    let tag = self.intern(name);
                    self.emit(Instruction::VarStore {
                        ordinal,
                        is_varip,
                        src: dst,
                        name: tag,
                        strict: false,
                    });
                    continue;
                }
                if self.journaled {
                    if self.decls.is_persistent(name) {
                        return Err(FallbackReason::TupleUnpack);
                    }
                    self.emit_journaled(name, dst, false);
                    continue;
                }
                if let Some(local) = self.locals.get(name).copied() {
                    self.emit(Instruction::Copy {
                        dst: local,
                        src: dst,
                    });
                } else {
                    // Fresh frame local, mirroring single-assignment
                    // lowering: the tree-walk journals the binding away
                    // on return, which the frame discard reproduces.
                    let slot = self.temp();
                    self.emit(Instruction::Copy {
                        dst: slot,
                        src: dst,
                    });
                    self.locals.insert(name.clone(), slot);
                }
            } else {
                // Tag-preserving store: destructuring assigns without
                // refreshing plot tags, exactly like the tree-walk's
                // plain assign path.
                let global = self.global(name);
                self.emit(Instruction::GStoreLoop {
                    slot: global,
                    src: dst,
                });
            }
        }
        Ok(())
    }

    /// Lower an `if`: `na` conditions read absence (never the else
    /// branch), so the na-jump precedes the false-jump. Returns the merge
    /// temp when both arms produce values.
    fn lower_if(
        &mut self,
        condition: &PineExpr,
        then_branch: &[PineStmt],
        else_branch: Option<&[PineStmt]>,
    ) -> Result<Option<SlotId>, FallbackReason> {
        let cond =
            self.lower_expr_slot(condition)
                .map_err(|inner| FallbackReason::IfCondition {
                    inner: Box::new(inner),
                })?;
        let merge = self.temp();
        let na_jump = self.emit(Instruction::JumpIfNa {
            cond,
            target: usize::MAX,
        });
        let else_jump = self.emit(Instruction::JumpIfFalse {
            cond,
            target: usize::MAX,
            ctx: CondContext::If,
        });
        let mut then_value = None;
        for stmt in then_branch {
            if let Some(slot) = self.lower_stmt_value(stmt)? {
                then_value = Some(slot);
            }
        }
        if let Some(slot) = then_value {
            self.emit(Instruction::Copy {
                dst: merge,
                src: slot,
            });
        }
        let end_jump = self.emit(Instruction::Jump { target: usize::MAX });
        let else_at = self.here();
        if let Some(branch) = else_branch {
            let mut else_value = None;
            for stmt in branch {
                if let Some(slot) = self.lower_stmt_value(stmt)? {
                    else_value = Some(slot);
                }
            }
            if let Some(slot) = else_value {
                self.emit(Instruction::Copy {
                    dst: merge,
                    src: slot,
                });
            }
        }
        let end_at = self.here();
        let current = self.current;
        self.patch(na_jump, end_at, current);
        self.patch(else_jump, else_at, current);
        self.patch(end_jump, end_at, current);
        Ok(Some(merge))
    }

    /// Lower an expression to a value slot. `Err` carries the deepest
    /// unlowerable construct family, named for the closeout work list.
    fn lower_expr_slot(&mut self, expr: &PineExpr) -> Result<SlotId, FallbackReason> {
        match expr {
            PineExpr::Number(v) => {
                let dst = self.temp();
                let id = self.const_value(ConstValue::Float(*v));
                self.emit(Instruction::Const { dst, value: id });
                Ok(dst)
            }
            PineExpr::StringLiteral(text) => {
                let dst = self.temp();
                let id = self.const_value(ConstValue::Str(text.clone()));
                self.emit(Instruction::Const { dst, value: id });
                Ok(dst)
            }
            PineExpr::Boolean(flag) => {
                let dst = self.temp();
                let id = self.const_value(ConstValue::Bool(*flag));
                self.emit(Instruction::Const { dst, value: id });
                Ok(dst)
            }
            PineExpr::ColorLiteral(packed) => {
                let dst = self.temp();
                let id = self.const_value(ConstValue::Color(*packed));
                self.emit(Instruction::Const { dst, value: id });
                Ok(dst)
            }
            PineExpr::Variable(name) => self.lower_variable(name),
            PineExpr::BuiltinVariable(var) => {
                let dst = self.temp();
                self.emit(Instruction::Builtin {
                    dst,
                    var: match var {
                        BuiltinVar::Open => BuiltinVarKind::Open,
                        BuiltinVar::High => BuiltinVarKind::High,
                        BuiltinVar::Low => BuiltinVarKind::Low,
                        BuiltinVar::Close => BuiltinVarKind::Close,
                        BuiltinVar::Volume => BuiltinVarKind::Volume,
                        BuiltinVar::Time => BuiltinVarKind::Time,
                    },
                });
                Ok(dst)
            }
            PineExpr::Binary { left, op, right } => {
                // Constant folding: literal/literal pairs evaluate once at
                // compile time through the shared leaf, so the run loop
                // executes one `Const` instead of two plus a `BinOp`. Only
                // successful folds lower (a refusing pair keeps its `BinOp`
                // so the refusal fires at run time, never at link).
                if let (Some(first), Some(second)) = (literal_value(left), literal_value(right))
                {
                    if let Ok(folded) = super::eval::binary(*op, Some(&first), Some(&second)) {
                        if let Some(constant) = folded_const(folded.as_ref()) {
                            let dst = self.temp();
                            let id = self.const_value(constant);
                            self.emit(Instruction::Const { dst, value: id });
                            return Ok(dst);
                        }
                    }
                }
                let lhs = self.lower_expr_slot(left)?;
                let rhs = self.lower_expr_slot(right)?;
                let dst = self.temp();
                self.emit(Instruction::BinOp {
                    dst,
                    op: bin_op(*op),
                    lhs,
                    rhs,
                });
                Ok(dst)
            }
            PineExpr::Unary { op, expr } => {
                // Same fold as binary: a literal operand evaluates once at
                // compile time; refusals stay run-time.
                if let Some(operand) = literal_value(expr) {
                    if let Ok(folded) = super::eval::unary(*op, Some(&operand)) {
                        if let Some(constant) = folded_const(folded.as_ref()) {
                            let dst = self.temp();
                            let id = self.const_value(constant);
                            self.emit(Instruction::Const { dst, value: id });
                            return Ok(dst);
                        }
                    }
                }
                let src = self.lower_expr_slot(expr)?;
                let dst = self.temp();
                self.emit(Instruction::UnOp {
                    dst,
                    op: match op {
                        UnaryOp::Negate => UnOp::Negate,
                        UnaryOp::Not => UnOp::Not,
                    },
                    src,
                });
                Ok(dst)
            }
            PineExpr::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                let cond = self.lower_expr_slot(condition).map_err(|inner| {
                    FallbackReason::TernaryCondition {
                        inner: Box::new(inner),
                    }
                })?;
                let merge = self.temp();
                let na_jump = self.emit(Instruction::JumpIfNa {
                    cond,
                    target: usize::MAX,
                });
                let else_jump = self.emit(Instruction::JumpIfFalse {
                    cond,
                    target: usize::MAX,
                    ctx: CondContext::Ternary,
                });
                let then_slot = self.lower_expr_slot(then_expr).map_err(|inner| {
                    FallbackReason::TernaryArm {
                        inner: Box::new(inner),
                    }
                })?;
                self.emit(Instruction::Copy {
                    dst: merge,
                    src: then_slot,
                });
                let end_jump = self.emit(Instruction::Jump { target: usize::MAX });
                let else_at = self.here();
                let else_slot = self.lower_expr_slot(else_expr).map_err(|inner| {
                    FallbackReason::TernaryArm {
                        inner: Box::new(inner),
                    }
                })?;
                self.emit(Instruction::Copy {
                    dst: merge,
                    src: else_slot,
                });
                let end_at = self.here();
                let current = self.current;
                self.patch(na_jump, end_at, current);
                self.patch(else_jump, else_at, current);
                self.patch(end_jump, end_at, current);
                Ok(merge)
            }
            PineExpr::SeriesOffset { series, offset } => {
                // Cursor history (`bar_index[n]`): the cursor is a
                // computed series, never recorded — spill it into a
                // hidden column once per bar and read history through
                // it, exactly like parenthesized-expression histories
                // below (pre-window reads stay `na`, negatives refuse
                // in the shared history read). A user binding shadows
                // the cursor on the tree, so shadowed spellings take
                // the normal variable path and its binding history.
                if let PineExpr::Variable(name) = series.as_ref() {
                    if name == "bar_index"
                        && !self.frame_vars.contains_key(name)
                        && !self.locals.contains_key(name)
                        && !self.any_method_alias.contains(name)
                        && !self.loop_bound(name)
                        && !self.globals.contains_key(name)
                        && !self.stmt_bound.contains(name)
                        && !self
                            .decls
                            .declared(name, self.module_pos, self.top_level)
                    {
                        let cursor = self.temp();
                        self.emit(Instruction::Builtin {
                            dst: cursor,
                            var: BuiltinVarKind::BarIndex,
                        });
                        let hidden = format!("\0spill:{}#{}", self.key, self.spills);
                        self.spills += 1;
                        let column = self.global(&hidden);
                        self.emit(Instruction::GStore {
                            slot: column,
                            src: cursor,
                            plot_tag: None,
                        });
                        let dst = self.temp();
                        self.emit(Instruction::History {
                            dst,
                            src: column,
                            offset: *offset,
                        });
                        return Ok(dst);
                    }
                }
                let src = match series.as_ref() {
                    PineExpr::BuiltinVariable(var) => SlotId(match var {
                        BuiltinVar::Open => LegKind::Open.index(),
                        BuiltinVar::High => LegKind::High.index(),
                        BuiltinVar::Low => LegKind::Low.index(),
                        BuiltinVar::Close => LegKind::Close.index(),
                        BuiltinVar::Volume => LegKind::Volume.index(),
                        BuiltinVar::Time => LegKind::Time.index(),
                    }),
                    PineExpr::Variable(name) => {
                        if self.in_function {
                            // Per-instance `var` cells read their own past,
                            // but only when no shared global shadows them:
                            // the tree-walk serves its single shared
                            // namespace before per-instance cells.
                            let own_var = self.frame_vars.get(name).copied();
                            if let Some((ordinal, _)) = own_var {
                                if !self.globals.contains_key(name) {
                                    let dst = self.temp();
                                    let tag = self.intern(name);
                                    self.emit(Instruction::VarRead {
                                        dst,
                                        ordinal,
                                        name: tag,
                                        hist: Some(*offset),
                                    });
                                    return Ok(dst);
                                }
                            }
                            // Plain frame locals and parameters read their
                            // own per-call past (below): the loop records
                            // each call's final slot value and keys the
                            // history by call path, exactly the tree-walk's
                            // qualified frame history, with the bare column
                            // behind it.
                            // Per-call history reads the call's qualified past
                            // on the tree, falling back to the bare column
                            // when the qualified key never recorded — the
                            // checked local-history read mirrors both, so
                            // bound locals take it even when a shared
                            // global shadows the name. Ancestor `var`
                            // instances stay tree-walk, and so does a read
                            // that precedes its own binding: the loop
                            // cannot see either the same way.
                            if self.any_frame_var.contains(name) && own_var.is_none() {
                                return Err(FallbackReason::FunctionBodyBinding {
                                    name: self.intern(name),
                                });
                            }
                            if let Some(slot) = self.locals.get(name).copied() {
                                if *offset == 0 {
                                    return Ok(slot);
                                }
                                let tag = self.intern(name);
                                if !self.history_locals.contains(&tag) {
                                    self.history_locals.push(tag);
                                }
                                let bare = self.globals.get(name).copied();
                                let dst = self.temp();
                                self.emit(Instruction::LocalHistory {
                                    dst,
                                    slot,
                                    name: tag,
                                    offset: super::bytecode::HistOffset::Const(*offset),
                                    bare,
                                });
                                return Ok(dst);
                            }
                            if self.journaled && self.journaled_written.contains(name) {
                                return Err(FallbackReason::FunctionBodyBinding {
                                    name: self.intern(name),
                                });
                            }
                            if !self.globals.contains_key(name) {
                                self.speculative_reads.insert(name.clone());
                            }
                            self.global(name)
                        } else {
                            if !self.decls.declared(name, self.module_pos, self.top_level)
                                && !self.stmt_bound.contains(name)
                            {
                                // Bound nowhere: mark speculative and read
                                // the shared column, whose history rule
                                // refuses at run time exactly like the
                                // tree-walk.
                                self.speculative_reads.insert(name.clone());
                            }
                            self.global(name)
                        }
                    }
                    _ => {
                        // History over a parenthesized expression: evaluate
                        // the expression once per bar into an
                        // engine-internal column, then read history through
                        // it. The column name carries a `\0` prefix no Pine
                        // identifier can spell, plus the owning module and a
                        // per-module site counter, so spills never collide
                        // with user state or each other. Whatever the series
                        // refuses with stays a fallback for the tree-walk
                        // to decide; only cleanly lowered series spill.
                        let Ok(value) = self.lower_expr_slot(series) else {
                            return Err(FallbackReason::ComputedSeriesHistory);
                        };
                        let hidden = format!("\0spill:{}#{}", self.key, self.spills);
                        self.spills += 1;
                        let column = self.global(&hidden);
                        self.emit(Instruction::GStore {
                            slot: column,
                            src: value,
                            plot_tag: None,
                        });
                        column
                    }
                };
                let dst = self.temp();
                self.emit(Instruction::History {
                    dst,
                    src,
                    offset: *offset,
                });
                Ok(dst)
            }
            PineExpr::FunctionCall { name, args, .. } => self.lower_call(name, args),
            PineExpr::MethodCall {
                namespace,
                method,
                args,
                type_args,
                ..
            } => self.lower_method_call(namespace, method, args, type_args, false),
            PineExpr::FieldRead { receiver, field } => {
                if let PineExpr::Variable(head) = receiver.as_ref() {
                    let dotted = format!("{head}.{field}");
                    if let Some(value) = resolve_constant(&dotted) {
                        return self.lower_resolved_const(&dotted, &value);
                    }
                    if let Some(handle) = self.enums.lookup(
                        &dotted,
                        &self.key,
                        self.module_pos,
                        self.top_level,
                        self.in_function,
                    ) {
                        return self.lower_resolved_const(&dotted, &PineValue::EnumHandle(handle));
                    }
                }
                // Shared-table read: the receiver evaluates once, then the
                // loop reads its table with the tree-walk's exact refusal
                // edges through the shared constructors. Unlowerable
                // receivers keep the construct fallback (masked, like the
                // computed-series and destructure families) so the census
                // names the field family, never the hidden inner gap.
                let obj = self
                    .lower_expr_slot(receiver)
                    .map_err(|_| FallbackReason::FieldRead)?;
                let dst = self.temp();
                let field = self.intern(field);
                self.emit(Instruction::FieldGet { dst, obj, field });
                Ok(dst)
            }
            PineExpr::InstanceCall {
                receiver,
                method,
                args,
                ..
            } => self.lower_rvalue_call(receiver, method, args),
            PineExpr::ArrayLiteral(items) => {
                let mut options = Vec::with_capacity(items.len());
                for item in items {
                    options.push(self.lower_expr_slot(item).map_err(call_arg_ctx)?);
                }
                let dst = self.temp();
                self.emit(Instruction::ArrayNew { dst, options });
                Ok(dst)
            }
            PineExpr::ArrayAccess { array, index } => {
                // Index first, then the array — the tree-walk's exact
                // evaluation order. An undeclared name falls back (the
                // tree-walk decides na-vs-refusal at run time); a named
                // series that is not an array at run time reads history
                // through the resolved column, exactly like offset reads.
                let idx = self.lower_expr_slot(index).map_err(call_arg_ctx)?;
                let arr = self.lower_expr_slot(array).map_err(call_arg_ctx)?;
                let hist = match array.as_ref() {
                    PineExpr::Variable(name) => Some({
                        if self.in_function {
                            // The array lowering above already bound (or
                            // speculatively allocated) the column, so the
                            // declaredness rule rides the shared history
                            // column below.
                            // Bound locals read per-call qualified history
                            // (bare column behind), exactly like offset
                            // reads; ancestor `var` instances stay
                            // tree-walk, and so does a read that precedes
                            // its own binding.
                            if self.any_frame_var.contains(name)
                                && !self.frame_vars.contains_key(name)
                            {
                                return Err(FallbackReason::FunctionBodyBinding {
                                    name: self.intern(name),
                                });
                            }
                            if let Some(slot) = self.locals.get(name).copied() {
                                let tag = self.intern(name);
                                if !self.history_locals.contains(&tag) {
                                    self.history_locals.push(tag);
                                }
                                let bare = self.globals.get(name).copied();
                                let offset = match index.as_ref() {
                                    PineExpr::Number(v)
                                        if v.fract() == 0.0
                                            && *v >= f64::from(i32::MIN)
                                            && *v <= f64::from(i32::MAX) =>
                                    {
                                        super::bytecode::HistOffset::Const(*v as i32)
                                    }
                                    _ => super::bytecode::HistOffset::Slot(idx),
                                };
                                let dst = self.temp();
                                self.emit(Instruction::LocalHistory {
                                    dst,
                                    slot,
                                    name: tag,
                                    offset,
                                    bare,
                                });
                                return Ok(dst);
                            }
                            if self.journaled && self.journaled_written.contains(name) {
                                return Err(FallbackReason::FunctionBodyBinding {
                                    name: self.intern(name),
                                });
                            }
                            self.global(name)
                        } else {
                            // Same shared column the array lowering above
                            // already resolved; unrecorded names refuse
                            // through the history rule at run time.
                            self.global(name)
                        }
                    }),
                    PineExpr::BuiltinVariable(var) => Some(SlotId(match var {
                        BuiltinVar::Open => LegKind::Open.index(),
                        BuiltinVar::High => LegKind::High.index(),
                        BuiltinVar::Low => LegKind::Low.index(),
                        BuiltinVar::Close => LegKind::Close.index(),
                        BuiltinVar::Volume => LegKind::Volume.index(),
                        BuiltinVar::Time => LegKind::Time.index(),
                    })),
                    _ => None,
                };
                let dst = self.temp();
                self.emit(Instruction::ArrayGet {
                    dst,
                    arr,
                    idx,
                    hist,
                });
                Ok(dst)
            }
            PineExpr::Switch(switch) => self.lower_switch_value(switch),
        }
    }

    /// Loop-target binding visible here: a target of an enclosing loop
    /// whose body is currently lowering. Seeding precedes the body, so
    /// any execution reaching here observed the bind — unlike a registry
    /// entry, which additionally needs the declaration-order check.
    fn loop_bound(&self, name: &str) -> bool {
        self.loop_targets.iter().any(|target| target == name)
    }

    fn lower_variable(&mut self, name: &str) -> Result<SlotId, FallbackReason> {
        if name == "na" {
            // Absence is the unwritten temp: slots reset per bar, so no
            // write reads back as `na` exactly like the tree-walk.
            return Ok(self.temp());
        }
        if name == "bar_index" {
            let dst = self.temp();
            self.emit(Instruction::Builtin {
                dst,
                var: BuiltinVarKind::BarIndex,
            });
            return Ok(dst);
        }
        if name == "timenow" {
            // Chart clock: tick stamps in tick runs, the time leg
            // otherwise — exactly like the tree-walk. A user binding
            // shadows the clock on the tree (variables and persistent
            // cells are checked first), so a shadowed spelling falls
            // through to the normal variable path below.
            let shadowed = self.frame_vars.contains_key(name)
                || self.loop_bound(name)
                || self.globals.contains_key(name);
            if !shadowed {
                let dst = self.temp();
                self.emit(Instruction::Builtin {
                    dst,
                    var: BuiltinVarKind::Timenow,
                });
                return Ok(dst);
            }
        }
        if name == "tr" {
            // Legacy bare true range — exactly like the clock above: a
            // user binding shadows the builtin on the tree, so a shadowed
            // spelling falls through to the normal variable path below.
            let shadowed = self.frame_vars.contains_key(name)
                || self.loop_bound(name)
                || self.globals.contains_key(name);
            if !shadowed {
                let dst = self.temp();
                self.emit(Instruction::Builtin {
                    dst,
                    var: BuiltinVarKind::Tr,
                });
                return Ok(dst);
            }
        }
        // Function locals shadow everything: parameters and body bindings
        // live in the call frame. In-function `var` cells come next, but
        // only when no shared global shadows them: the tree-walk serves
        // plain frame bindings and its single shared namespace before
        // per-instance cells. Enclosing loop targets bind before their
        // bodies run, so they read bound in either position (a counter
        // spelling an in-function `var` stores per-instance through the
        // scope walk). Reads of names declared as in-function `var`
        // elsewhere stay tree-walk, since the tree would serve an
        // ancestor instance the loop cannot see.
        if self.in_function {
            // Method aliases read the live receiver slot first, exactly
            // like the tree-walk's top-down stack search; with no live
            // slot the frame, the shared column, and the per-instance
            // cell answer in tree order.
            if self.any_method_alias.contains(name) {
                let dst = self.temp();
                let tag = self.intern(name);
                let var_ordinal = self.frame_vars.get(name).map(|(ordinal, _)| *ordinal);
                self.emit(Instruction::AliasRead {
                    dst,
                    name: tag,
                    var_ordinal,
                });
                return Ok(dst);
            }
            if name == "this" {
                if let Some(src) = self.locals.get(name).copied() {
                    let dst = self.temp();
                    self.emit(Instruction::Copy { dst, src });
                    return Ok(dst);
                }
                if self.loop_bound(name) || self.globals.contains_key(name) {
                    let slot = self.global(name);
                    let dst = self.temp();
                    self.emit(Instruction::GLoad { dst, slot });
                    return Ok(dst);
                }
                let dst = self.temp();
                self.emit(Instruction::ThisRead { dst });
                return Ok(dst);
            }
            if let Some(field) = name.strip_prefix("this.") {
                // `this.*` reads bind the innermost enclosing method; with
                // no live receiver the loop refuses exactly like the
                // tree-walk instead of loading a column the tree would
                // never read.
                let dst = self.temp();
                let field = self.intern(field);
                self.emit(Instruction::ThisFieldGet { dst, field });
                return Ok(dst);
            }
            if let Some(src) = self.locals.get(name).copied() {
                let dst = self.temp();
                self.emit(Instruction::Copy { dst, src });
                return Ok(dst);
            }
            let own_var = self.frame_vars.get(name).copied();
            if let Some((ordinal, _)) = own_var {
                if self.loop_bound(name) || !self.globals.contains_key(name) {
                    let dst = self.temp();
                    let tag = self.intern(name);
                    self.emit(Instruction::VarRead {
                        dst,
                        ordinal,
                        name: tag,
                        hist: None,
                    });
                    return Ok(dst);
                }
            }
            if self.any_frame_var.contains(name) && own_var.is_none() {
                return Err(FallbackReason::FunctionBodyBinding {
                    name: self.intern(name),
                });
            }
            if self.loop_bound(name) || self.globals.contains_key(name) {
                // Enum-shaped spellings never take the bare-column shortcut:
                // a pre-allocated flat (a call-arg home elsewhere, or an
                // earlier speculative read) must not shadow the registered
                // table — the checked member read below serves a recorded
                // flat first and the handle otherwise, exactly like the
                // tree-walk. Chart-scope spellings (`syminfo.*`,
                // `timeframe.*`) skip it too: they are run configuration,
                // served by the shared member table below exactly like the
                // top-level route. Anything else reads its column directly.
                // Loop-bound names keep the shortcut (a loop target is
                // always bound); otherwise fall through to the dotted
                // logic, which routes shape-matches to the flat-first
                // registered read and everything else back here.
                let enum_shaped = match name.split_once('.') {
                    Some((head, _)) => {
                        !head.is_empty()
                            && head != "this"
                            && head != "na"
                            && self.enums.shape_matches(name).is_some()
                            && self.enum_runtime_ok(head)
                    }
                    None => false,
                };
                if !enum_shaped
                    && super::builtins_core::ChartScope::parse(name).is_none()
                {
                    let slot = self.global(name);
                    let dst = self.temp();
                    self.emit(Instruction::GLoad { dst, slot });
                    return Ok(dst);
                }
            }
        // User bindings shadow legs (tree-walk reads `variables` first),
        // legs precede compile-time constants (which never collide with
        // assignable names). Only unconditional declarations that already
        // executed count as bound — a conditional declaration still
        // unexecuted reads `na`-vs-refusal in the tree-walk, so the
        // compiler falls back instead of loading a column the tree-walk
        // would refuse. Enclosing loop targets bind before their bodies
        // run. Cross-module columns allocate on demand; the linker
        // interns the shared global namespace by name.
        } else if self.loop_bound(name)
            || self.stmt_bound.contains(name)
            || self.decls.declared(name, self.module_pos, self.top_level)
        {
            let slot = self.global(name);
            let dst = self.temp();
            self.emit(Instruction::GLoad { dst, slot });
            return Ok(dst);
        }
        if let Some(kind) = leg_slot(name) {
            let dst = self.temp();
            self.emit(Instruction::GLoad {
                dst,
                slot: SlotId(kind.index()),
            });
            return Ok(dst);
        }
        if let Some(value) = resolve_constant(name) {
            // Compile-time names (`display.all`, `color.red`, …): resolved
            // once, never hashed per bar. Builtin enum handles lower here
            // too — they resolve unconditionally, with no declaration to
            // order against.
            return self.lower_resolved_const(name, &value);
        }
        if let Some(handle) = self.enums.lookup(
            name,
            &self.key,
            self.module_pos,
            self.top_level,
            self.in_function,
        ) {
            // Registered enum members lower to identity handles exactly
            // when the declaration already executed (see `lookup`); the
            // tree-walk decides every other dotted name at run time.
            return self.lower_resolved_const(name, &PineValue::EnumHandle(handle));
        }
        // Deterministic chart scope: `syminfo.*` and the pure
        // `timeframe.*` members are run configuration, so the loop reads
        // the same shared member table the tree-walk reads.
        if let Some(scope) = super::builtins_core::ChartScope::parse(name) {
            let dst = self.temp();
            self.emit(Instruction::Builtin {
                dst,
                var: BuiltinVarKind::from_chart_scope(scope),
            });
            return Ok(dst);
        }

        // Dotted reads resolve in the tree-walk's order: a flat spelling a
        // write-back may have created stays tree-walk (the runtime presence
        // decides), a compile-time enum member or constant folds, an
        // alias-qualified export reads through its target, and a plain
        // `head.field` lowers to the shared dotted-get instruction with the
        // tree's exact field-or-undefined edges.
        if let Some((head, rest)) = name.split_once('.') {
            if rest.is_empty() || head == "this" || head == "na" || rest.contains('.') {
                // Trailing enum pairs resolve without checking the prefix,
                // exactly like the tree-walk: a static hit folds to a
                // constant, otherwise the registered read decides at run
                // time (flat first). Anything else keeps the fallback.
                if rest.contains('.')
                    && self.enums.shape_matches(name).is_some()
                    && resolve_constant(name).is_none()
                {
                    // A static hit folds to a constant — unless a call home
                    // may have recorded the flat spelling, in which case
                    // the tree serves the flat first and the registered
                    // read must decide at run time.
                    let home_may_shadow = self.dotted_homes.contains(name);
                    if let Some(handle) = self.enums.lookup(
                        name,
                        &self.key,
                        self.module_pos,
                        self.top_level,
                        self.in_function,
                    ) {
                        if !home_may_shadow {
                            return self.lower_resolved_const(name, &PineValue::EnumHandle(handle));
                        }
                    }
                    let (enm, member) = super::builtins_core::split_enum_member(name)
                        .expect("shape-matched spelling splits");
                    let flat = self.global(name);
                    let dst = self.temp();
                    let enm = self.intern(enm);
                    let member = self.intern(member);
                    let full = self.intern(name);
                    self.emit(Instruction::EnumRead {
                        dst,
                        enm,
                        member,
                        name: full,
                        flat,
                    });
                    return Ok(dst);
                }
                return Err(FallbackReason::DottedName {
                    name: self.intern(name),
                });
            }
            // Flat-first reads: a dotted spelling a call home may have
            // recorded reads the flat column before the field lookup,
            // exactly like the tree-walk — unless the spelling resolves
            // through a dedicated route (strategy, enum, constant, live
            // receiver alias), which keeps its existing rule below and
            // the fallback when possibly shadowed. Alias spellings flow
            // through too: their route reads flat-aware as well.
            let flat_first = self.dotted_homes.contains(name)
                && name.strip_prefix("strategy.").is_none()
                && super::builtins_core::ChartScope::parse(name).is_none()
                && self
                    .enums
                    .lookup(
                        name,
                        &self.key,
                        self.module_pos,
                        self.top_level,
                        self.in_function,
                    )
                    .is_none()
                && resolve_constant(name).is_none()
                && !(self.in_function && self.any_method_alias.contains(head));
            if self.dotted_homes.contains(name) && !flat_first {
                // In-set enum shapes resolve through the registered table
                // at run time (flat first): the static order cannot prove
                // them, but the marks observe every declaration execution
                // exactly like the tree-walk. Anything else keeps the
                // fallback, where runtime presence decides.
                if self.enums.shape_matches(name).is_some()
                    && name.strip_prefix("strategy.").is_none()
                    && super::builtins_core::ChartScope::parse(name).is_none()
                    && resolve_constant(name).is_none()
                    && !(self.in_function && self.any_method_alias.contains(head))
                    && !self.aliases.contains_key(head)
                    && self.enum_runtime_ok(head)
                {
                    let (enm, member) = super::builtins_core::split_enum_member(name)
                        .expect("shape-matched spelling splits");
                    let flat = self.global(name);
                    let dst = self.temp();
                    let enm = self.intern(enm);
                    let member = self.intern(member);
                    let full = self.intern(name);
                    self.emit(Instruction::EnumRead {
                        dst,
                        enm,
                        member,
                        name: full,
                        flat,
                    });
                    return Ok(dst);
                }
                return Err(FallbackReason::DottedName {
                    name: self.intern(name),
                });
            }
            // Per-bar flags serve from the run cursor, flat-first when a
            // home may have recorded the spelling (the tree checks the
            // flat before the flag); otherwise the flag lowers to one
            // builtin read. Unknown members keep the fallback — the
            // tree-walk refuses them, so it decides.
            if let Some(member) = name.strip_prefix("barstate.") {
                if !flat_first {
                    match super::bytecode::BuiltinVarKind::from_barstate_member(member) {
                        Some(var) => {
                            let dst = self.temp();
                            self.emit(Instruction::Builtin { dst, var });
                            return Ok(dst);
                        }
                        None => {
                            return Err(FallbackReason::DottedName {
                                name: self.intern(name),
                            });
                        }
                    }
                }
            }
            // Headless harness reads (`strategy.long`, `strategy.equity`):
            // the loop reads the run's shared strategy state through one
            // instruction, so values and refusals match the tree-walk.
            if let Some(member) = name.strip_prefix("strategy.") {
                let dst = self.temp();
                let member = self.intern(member);
                self.emit(Instruction::StrategyRead { dst, member });
                return Ok(dst);
            }
            // Deterministic chart scope in bodies (`syminfo.*`,
            // `timeframe.*`): run configuration, served through the same
            // shared member table as the top-level route, so in-function
            // reads agree with top-level reads and the tree-walk.
            if let Some(scope) = super::builtins_core::ChartScope::parse(name) {
                let dst = self.temp();
                self.emit(Instruction::Builtin {
                    dst,
                    var: BuiltinVarKind::from_chart_scope(scope),
                });
                return Ok(dst);
            }
            if let Some(handle) = self.enums.lookup(
                name,
                &self.key,
                self.module_pos,
                self.top_level,
                self.in_function,
            ) {
                // A static hit folds to a constant — unless a call home
                // may have recorded the flat spelling (see the multi-dot
                // arm above for why the registered read must decide).
                if self.dotted_homes.contains(name) {
                    let (enm, member) = super::builtins_core::split_enum_member(name)
                        .expect("resolved spelling splits");
                    let flat = self.global(name);
                    let dst = self.temp();
                    let enm = self.intern(enm);
                    let member = self.intern(member);
                    let full = self.intern(name);
                    self.emit(Instruction::EnumRead {
                        dst,
                        enm,
                        member,
                        name: full,
                        flat,
                    });
                    return Ok(dst);
                }
                return self.lower_resolved_const(name, &PineValue::EnumHandle(handle));
            }
            if let Some(value) = resolve_constant(name) {
                // Constants have no registered table behind them, so a
                // recorded flat always wins on the tree: keep the fallback
                // and let runtime presence decide.
                if self.dotted_homes.contains(name) {
                    return Err(FallbackReason::DottedName {
                        name: self.intern(name),
                    });
                }
                return self.lower_resolved_const(name, &value);
            }
            if self.aliases.contains_key(head) {
                let ambiguous = if self.in_function {
                    self.locals.contains_key(head) || self.globals.contains_key(head)
                } else {
                    self.loop_bound(head)
                        || self.decls.declared(head, self.module_pos, self.top_level)
                        || leg_slot(head).is_some()
                };
                if !ambiguous {
                    // A declared-enum shape keeps the full-path fallback:
                    // the tree serves a registered enum before the export,
                    // and no single instruction serves both — the fallback
                    // runs the tree and agrees either way.
                    if self.enums.shape_matches(name).is_some() {
                        return Err(FallbackReason::DottedName {
                            name: self.intern(name),
                        });
                    }
                    // Alias-qualified reads resolve through the target's
                    // namespace exactly like the tree-walk, which applies
                    // no export gate — with one flat-aware instruction so
                    // a recorded write-back column wins first, exactly
                    // like the tree-walk. Declaration timing rides the
                    // shared bare-name rules and unbound spellings refuse
                    // naming the remainder on both engines.
                    let src = self.lower_variable(rest)?;
                    let flat = self.global(name);
                    let dst = self.temp();
                    self.emit(Instruction::AliasFlatRead { dst, flat, src });
                    return Ok(dst);
                }
            }
            // Receiver-alias field reads (`level.aged`) resolve through the
            // live slot first, exactly like the tree-walk's top-down
            // receiver search.
            if self.in_function && self.any_method_alias.contains(head) {
                let dst = self.temp();
                let head_id = self.intern(head);
                let field = self.intern(rest);
                let var_ordinal = self.frame_vars.get(head).map(|(ordinal, _)| *ordinal);
                self.emit(Instruction::AliasFieldGet {
                    dst,
                    name: head_id,
                    field,
                    var_ordinal,
                });
                return Ok(dst);
            }
            // A plain field read: the head must lower to a value; anything
            // the head cannot bind keeps the fallback for the tree-walk.
            // A head with no binding anywhere (freshly allocated by the
            // read itself, or still speculative with no later binding)
            // also keeps the fallback: the loop's declaredness rule would
            // name the head, while the tree-walk names the full path.
            let head_had_column = self.globals.contains_key(head);
            let head_speculative = self.speculative_reads.contains(head);
            let saved_instrs = self.blocks[self.current].instrs.len();
            let saved_frame_slots = self.frame_slots;
            let saved_next_global = self.next_global;
            let root = self.lower_variable(head);
            // Freshly allocated means bound nowhere (every binding rule
            // resolves without allocating); a previously speculative head
            // needs binding evidence since.
            let allocated = !head_had_column && self.globals.contains_key(head);
            let mut bound = root.is_ok() && !allocated;
            if bound && head_speculative {
                bound = self.speculative_binding(head);
            }
            if bound {
                if let Ok(root) = root {
                    if head_speculative {
                        self.speculative_reads.remove(head);
                    }
                    // Flat-first reads serve a recorded write-back column
                    // before the field lookup, exactly like the tree-walk;
                    // the column allocates on demand and stays silent
                    // until a home records it.
                    let flat = flat_first.then(|| self.global(name));
                    let dst = self.temp();
                    let full = self.intern(name);
                    let field = self.intern(rest);
                    self.emit(Instruction::DottedGet {
                        dst,
                        root,
                        name: full,
                        field,
                        flat,
                    });
                    return Ok(dst);
                }
            }
            // Unbound non-alias heads with a declared-enum shape resolve
            // through the registered table at run time when no binding
            // could shadow it; anything else keeps the full-path
            // fallback, where the tree-walk names the whole spelling.
            // The speculative head read unwinds first either way: a dead
            // load would refuse naming the head before the real read.
            if allocated {
                self.globals.remove(head);
            }
            self.blocks[self.current].instrs.truncate(saved_instrs);
            self.frame_slots = saved_frame_slots;
            self.blocks[self.current].slots =
                self.blocks[self.current].slots.min(saved_frame_slots);
            self.next_global = saved_next_global;
            if root.is_ok()
                && !self.aliases.contains_key(head)
                && self.enums.shape_matches(name).is_some()
                && self.enum_runtime_ok(head)
            {                let (enm, member) = super::builtins_core::split_enum_member(name)
                    .expect("shape-matched spelling splits");
                let flat = self.global(name);
                let dst = self.temp();
                let enm = self.intern(enm);
                let member = self.intern(member);
                let full = self.intern(name);
                self.emit(Instruction::EnumRead {
                    dst,
                    enm,
                    member,
                    name: full,
                    flat,
                });
                return Ok(dst);
            }
            return Err(FallbackReason::DottedName {
                name: self.intern(name),
            });
        }
        if name.contains('.') {
            return Err(FallbackReason::DottedName {
                name: self.intern(name),
            });
        }
        if name == "this" {
            // Unbound `this` outside any method refuses at run time with
            // the tree-walk's exact wording instead of falling back.
            let dst = self.temp();
            self.emit(Instruction::ThisRead { dst });
            return Ok(dst);
        }
        // Plain names with no binding anywhere lower to a column read:
        // the loop's declaredness rule refuses at run time exactly when
        // the column was never recorded (persistent cells read as
        // absence), mirroring the tree-walk's undeclared read — so dead
        // branches over unknown names stay silent until reached.
        self.speculative_reads.insert(name.to_owned());
        let slot = self.global(name);
        let dst = self.temp();
        self.emit(Instruction::GLoad { dst, slot });
        Ok(dst)
    }

    fn lower_resolved_const(
        &mut self,
        _name: &str,
        value: &PineValue,
    ) -> Result<SlotId, FallbackReason> {
        let value = match value {
            PineValue::Int(v) => ConstValue::Int(*v),
            PineValue::ColorHandle(handle) => ConstValue::Color(handle.raw()),
            PineValue::EnumHandle(handle) => ConstValue::Enum {
                namespace: self.intern(&handle.enum_name),
                member: self.intern(&handle.member),
            },
            _ => {
                return Err(FallbackReason::EnumMember);
            }
        };
        let dst = self.temp();
        let id = self.const_value(value);
        self.emit(Instruction::Const { dst, value: id });
        Ok(dst)
    }

    /// Lower an expression for effect only (statement position): the value
    /// temp is discarded after evaluation.
    fn lower_expr_discard(&mut self, expr: &PineExpr) -> Result<(), FallbackReason> {
        match expr {
            PineExpr::FunctionCall { name, args, .. } => {
                self.lower_call_statement(name, args);
                Ok(())
            }
            PineExpr::MethodCall {
                namespace,
                method,
                args,
                type_args,
                ..
            } => self.lower_method_statement(namespace, method, args, type_args),
            _ => self.lower_expr_slot(expr).map(|_| ()),
        }
    }

    /// Lower a bare call in value position.
    fn lower_call(&mut self, name: &str, args: &[CallArg]) -> Result<SlotId, FallbackReason> {
        // The harness declaration is a headless effect with its own shared
        // lowering, not one of the capture families.
        if name == "strategy" {
            return self.lower_strategy_declare(args);
        }
        if matches!(name, "alert" | "alertcondition") {
            return self.lower_alert(name, args);
        }
        if name == "plot" {
            let (temp, _) = self.lower_plot_value(args)?;
            return Ok(temp);
        }
        if let Some(kind) = capture_kind(name) {
            return self.lower_capture_value(kind, args);
        }
        if name == "input" {
            return self.lower_bare_input(args);
        }
        if name == "na" {
            if args.is_empty() {
                return Ok(self.temp());
            }
            return self.lower_na_test(args);
        }
        if name == "nz" {
            return self.lower_nz(args);
        }
        if core_cast(name) {
            return self.lower_core_cast(name, args);
        }
        if name == "fixnan"
            || name == "time"
            || name == "hour"
            || name == "minute"
            || name == "iff"
        {
            return self.lower_pure_bare_call(name, args);
        }
        // Legacy bare technical-analysis names (`sma(...)`): a user
        // definition shadows the bridge exactly like the tree-walk (which
        // checks its function table first), so only unclaimed names lower
        // through the shared `ta` namespace — with the identical site key
        // a qualified call builds, since `lower_builtin_call` keys by the
        // same `ta.{method}` spelling.
        if !self.func_blocks.contains_key(name) && !self.overloaded.contains(name) {
            if super::builtins_ta::TaBridge::supported_methods().contains(&name) {
                return self.lower_builtin_call(
                    BuiltinNamespace::Ta,
                    name,
                    args,
                    &[],
                    false,
                );
            }
            // Legacy bare math names (`pow(...)`): same shadow rule, then
            // the shared `math` namespace with the qualified spelling's
            // site key (built by `lower_builtin_call` exactly like a
            // qualified call).
            if super::builtins_core::legacy_bare_math_names().contains(&name) {
                return self.lower_builtin_call(
                    BuiltinNamespace::Math,
                    name,
                    args,
                    &[],
                    false,
                );
            }
            // Legacy bare string names (`tostring(...)`): same shadow
            // rule, then the shared `str` namespace.
            if super::builtins_core::legacy_bare_str_names().contains(&name) {
                return self.lower_builtin_call(
                    BuiltinNamespace::Str,
                    name,
                    args,
                    &[],
                    false,
                );
            }
        }
        // User function: single definition; binding mirrors the
        // tree-walk exactly (named match, then positionals in order,
        // then caller-scope defaults, else run-time refusal, extra
        // positionals and unknown names refuse). Omitted defaults lower
        // at the call site in declaration order, so the loop and the
        // tree-walk evaluate them in the same scope.
        let Some((index, params)) = self.func_blocks.get(name).cloned() else {
            // The tree evaluates every argument before it names the callee:
            // lower them in order (their refusals stand), then refuse here
            // with the same wording instead of evicting the module.
            for arg in args {
                self.lower_expr_slot(&arg.value).map_err(call_arg_ctx)?;
            }
            let message = super::eval::unknown_function_message(name);
            return Ok(self.emit_refuse(message));
        };
        if self.overloaded.contains(name) {
            return Err(FallbackReason::OverloadedCallee);
        }
        // Lower every argument in call order first (tree-walk evaluates
        // before binding), then bind by name and position.
        let mut lowered: Vec<(Option<String>, SlotId, Option<String>)> =
            Vec::with_capacity(args.len());
        for arg in args {
            let slot =
                self.lower_expr_slot(&arg.value)
                    .map_err(|inner| FallbackReason::CallArgument {
                        inner: Box::new(inner),
                    })?;
            let home = match &arg.value {
                PineExpr::Variable(name) => Some(name.clone()),
                _ => None,
            };
            if let Some(dotted) = home.as_deref() {
                if home_is_dotted(Some(dotted)) {
                    // Bare `this` keeps the fallback (rebinding the whole
                    // receiver refuses on both engines). Deeper `this`
                    // paths lower too: their value read refuses the
                    // unknown dotted field on both engines before any
                    // write-back could run, so the home never observes.
                    if dotted == "this" {
                        return Err(FallbackReason::DottedHome);
                    }
                    if !dotted.starts_with("this.") {
                        self.global(dotted);
                    }
                }
            }
            lowered.push((arg.name.clone(), slot, home));
        }
        let positional: Vec<(SlotId, Option<String>)> = lowered
            .iter()
            .filter(|(name, _, _)| name.is_none())
            .map(|(_, slot, home)| (*slot, home.clone()))
            .collect();
        let mut positional_index = 0;
        let mut bound: Vec<(CallArgSlot, Option<SymbolId>)> = Vec::with_capacity(params.len());
        for (param, default) in &params {
            if let Some((_, slot, home)) = lowered
                .iter()
                .find(|(name, _, _)| name.as_deref() == Some(param.as_str()))
            {
                let home = home.as_deref().map(|name| self.intern(name));
                bound.push((
                    CallArgSlot {
                        name: Some(self.intern(param)),
                        slot: *slot,
                    },
                    home,
                ));
                continue;
            }
            if positional_index < positional.len() {
                let (slot, home) = positional[positional_index].clone();
                positional_index += 1;
                let home = home.as_deref().map(|name| self.intern(name));
                bound.push((
                    CallArgSlot {
                        name: Some(self.intern(param)),
                        slot,
                    },
                    home,
                ));
                continue;
            }
            // Omitted default: evaluate at the call site in the caller's
            // scope, in declaration order, exactly like the tree-walk's
            // binder (caller locals visible, sibling parameters not yet
            // bound). An unlowerable default keeps the call's fallback
            // with its own reason, so the tree-walk decides it identically.
            let Some(default) = default else {
                return Err(FallbackReason::MissingDefault);
            };
            let dst =
                self.lower_expr_slot(default)
                    .map_err(|inner| FallbackReason::CallArgument {
                        inner: Box::new(inner),
                    })?;
            bound.push((
                CallArgSlot {
                    name: Some(self.intern(param)),
                    slot: dst,
                },
                None,
            ));
        }
        if positional_index < positional.len() {
            return Err(FallbackReason::TooManyPositionals);
        }
        for (name, _, _) in &lowered {
            if let Some(given) = name {
                if !params.iter().any(|(param, _)| param == given) {
                    return Err(FallbackReason::UnknownArgument {
                        name: self.intern(given),
                    });
                }
            }
        }
        let (slots, homes): (Vec<CallArgSlot>, Vec<Option<SymbolId>>) = bound.into_iter().unzip();
        let key = self.key.clone();
        let site = self.intern(&site_string(&format!("{key}::{name}"), args));
        let dst = self.temp();
        self.emit(Instruction::Call {
            dst: Some(dst),
            target: CallTarget::Block(index),
            args: slots,
            homes,
            type_args: Vec::new(),
            site,
        });
        Ok(dst)
    }

    /// Lower call arguments left-to-right (tree-walk evaluation order),
    /// recording write-back homes. `Err` names the first unlowerable
    /// argument or dotted home.
    /// Lower one journaled write in an escaping function body: the
    /// loop logs the caller home's entry value once per call, writes
    /// through at run time, and mirrors into a frame temp so same-bar
    /// reads see the written value without another lookup. Strict
    /// rebinds refuse unresolvable homes through the shared
    /// constructor; plain binds fall back to a fresh mirror instead.
    fn emit_journaled(&mut self, name: &str, slot: SlotId, strict: bool) {
        let home = self.intern(name);
        let mirror = if let Some(found) = self.locals.get(name).copied() {
            found
        } else {
            let fresh = self.temp();
            self.locals.insert(name.to_owned(), fresh);
            fresh
        };
        self.emit(Instruction::JournalWrite {
            home,
            src: slot,
            mirror,
            strict,
        });
    }

    fn lower_call_args(
        &mut self,
        args: &[CallArg],
        need_homes: bool,
    ) -> Result<(Vec<CallArgSlot>, Vec<Option<SymbolId>>), FallbackReason> {
        // Homes exist only for write-back: when the caller drops them
        // (read-only calls, casts), no home is computed and dotted
        // arguments lower as pure values instead of refusing.
        let mut slots = Vec::with_capacity(args.len());
        let mut homes = Vec::with_capacity(args.len());
        for arg in args {
            let slot =
                self.lower_expr_slot(&arg.value)
                    .map_err(|inner| FallbackReason::CallArgument {
                        inner: Box::new(inner),
                    })?;
            let home = if need_homes {
                match &arg.value {
                    PineExpr::Variable(name) => {
                        if home_is_dotted(Some(name)) {
                            // Bare `this` keeps the fallback (rebinding
                            // the whole receiver refuses on both engines).
                            // Deeper `this` paths lower too: their value
                            // read refuses the unknown dotted field on
                            // both engines before any write-back could
                            // run, so the home never observes.
                            if name == "this" {
                                return Err(FallbackReason::DottedHome);
                            }
                            if !name.starts_with("this.") {
                                self.global(name);
                            }
                        }
                        Some(self.intern(name))
                    }
                    _ => None,
                }
            } else {
                None
            };
            slots.push(CallArgSlot {
                name: arg.name.as_deref().map(|name| self.intern(name)),
                slot,
            });
            homes.push(home);
        }
        Ok((slots, homes))
    }

    /// Lower the bare `input(...)` overload: the method infers from the
    /// default's static shape exactly like the tree-walk's
    /// `bare_input_method` (legs/variables/offsets read `source`;
    /// integral numbers read `int`, other numbers `float`, booleans
    /// `bool`, strings `string`). Total color constructors
    /// (`color.rgb/new/from_gradient` always yield a color handle or
    /// refuse, mirroring the builtin's own contract) infer `color`.
    /// Other computed defaults need the run-time value and fall back.
    fn lower_bare_input(&mut self, args: &[CallArg]) -> Result<SlotId, FallbackReason> {
        let Some(first) = args.first() else {
            return Ok(self.emit_refuse(super::eval::INPUT_MISSING_DEFAULT_MESSAGE.to_owned()));
        };
        let method = match &first.value {
            PineExpr::BuiltinVariable(_)
            | PineExpr::Variable(_)
            | PineExpr::SeriesOffset { .. } => "source",
            PineExpr::Number(v) => {
                if v.fract() == 0.0
                    && (-9_007_199_254_740_992.0..=9_007_199_254_740_992.0).contains(v)
                {
                    "int"
                } else {
                    "float"
                }
            }
            PineExpr::Boolean(_) => "bool",
            PineExpr::StringLiteral(_) => "string",
            PineExpr::MethodCall {
                namespace, method, ..
            } if namespace == "color"
                && matches!(method.as_str(), "rgb" | "new" | "from_gradient") =>
            {
                "color"
            }
            // A computed default names its input from the run-time value:
            // the shared `input_auto` table infers the kind and owns every
            // declaration, so the loop needs no static guess.
            _ => "auto",
        };
        // Bare `input` never writes back (the tree-walk's value path
        // persists nothing), so dotted defaults lower as pure values
        // instead of refusing homes the call would have dropped.
        let (slots, homes) = self.lower_call_args(args, false)?;
        let site = self.intern(&site_string("input", args));
        let method = self.intern(method);
        let dst = self.temp();
        self.emit(Instruction::Call {
            dst: Some(dst),
            target: CallTarget::Builtin {
                namespace: BuiltinNamespace::Input,
                method,
            },
            args: slots,
            homes,
            type_args: Vec::new(),
            site,
        });
        Ok(dst)
    }

    fn lower_na_test(&mut self, args: &[CallArg]) -> Result<SlotId, FallbackReason> {
        if args.len() != 1 {
            // The tree evaluates every argument, then refuses the arity:
            // lower them in order and refuse at the same point.
            for arg in args {
                self.lower_expr_slot(&arg.value)
                    .map_err(|inner| FallbackReason::NaArgument {
                        inner: Box::new(inner),
                    })?;
            }
            return Ok(self.emit_refuse(super::eval::NA_ARITY_MESSAGE.to_owned()));
        }
        let cond =
            self.lower_expr_slot(&args[0].value)
                .map_err(|inner| FallbackReason::NaArgument {
                    inner: Box::new(inner),
                })?;
        let dst = self.temp();
        let true_jump = self.emit(Instruction::JumpIfNa {
            cond,
            target: usize::MAX,
        });
        let false_id = self.const_value(ConstValue::Bool(false));
        self.emit(Instruction::Const {
            dst,
            value: false_id,
        });
        let end_jump = self.emit(Instruction::Jump { target: usize::MAX });
        let true_at = self.here();
        let true_id = self.const_value(ConstValue::Bool(true));
        self.emit(Instruction::Const {
            dst,
            value: true_id,
        });
        let end_at = self.here();
        let current = self.current;
        self.patch(true_jump, true_at, current);
        self.patch(end_jump, end_at, current);
        Ok(dst)
    }

    fn lower_nz(&mut self, args: &[CallArg]) -> Result<SlotId, FallbackReason> {
        if args.is_empty() || args.len() > 2 {
            for arg in args {
                self.lower_expr_slot(&arg.value)
                    .map_err(|inner| FallbackReason::NzArgument {
                        inner: Box::new(inner),
                    })?;
            }
            return Ok(self.emit_refuse(super::eval::NZ_ARITY_MESSAGE.to_owned()));
        }
        let first =
            self.lower_expr_slot(&args[0].value)
                .map_err(|inner| FallbackReason::NzArgument {
                    inner: Box::new(inner),
                })?;
        let dst = self.temp();
        self.emit(Instruction::Copy { dst, src: first });
        let present_jump = self.emit(Instruction::JumpIfNa {
            cond: dst,
            target: usize::MAX,
        });
        let end_jump = self.emit(Instruction::Jump { target: usize::MAX });
        let else_at = self.here();
        if args.len() == 2 {
            let second = self.lower_expr_slot(&args[1].value).map_err(|inner| {
                FallbackReason::NzArgument {
                    inner: Box::new(inner),
                }
            })?;
            self.emit(Instruction::Copy { dst, src: second });
        } else {
            let zero = self.const_value(ConstValue::Float(0.0));
            self.emit(Instruction::Const { dst, value: zero });
        }
        let end_at = self.here();
        let current = self.current;
        self.patch(present_jump, else_at, current);
        self.patch(end_jump, end_at, current);
        Ok(dst)
    }

    fn lower_core_cast(&mut self, name: &str, args: &[CallArg]) -> Result<SlotId, FallbackReason> {
        if args.len() != 1 {
            return Err(FallbackReason::CastArity {
                name: self.intern(name),
            });
        }
        let (slots, _) = self.lower_call_args(args, false)?;
        if slots.len() != 1 || slots[0].name.is_some() {
            return Err(FallbackReason::CastShape {
                name: self.intern(name),
            });
        }
        let dst = self.temp();
        let site = self.intern(&format!("core.{name}@"));
        let method = self.intern(name);
        self.emit(Instruction::Call {
            dst: Some(dst),
            target: CallTarget::Builtin {
                namespace: BuiltinNamespace::Core,
                method,
            },
            args: slots,
            homes: vec![None; args.len()],
            type_args: Vec::new(),
            site,
        });
        Ok(dst)
    }

    /// Lower a pure bare call (`time`, `hour`, `minute`, `fixnan`) as a
    /// delegated core call: the loop runs the same shared function the
    /// tree-walk runs, so values and refusals agree by construction.
    /// `fixnan` keys its per-site memory on the interned argument shape —
    /// the exact string the tree-walk keys on — carried in the call site.
    fn lower_pure_bare_call(
        &mut self,
        name: &str,
        args: &[CallArg],
    ) -> Result<SlotId, FallbackReason> {
        let (slots, _) = self.lower_call_args(args, false)?;
        let dst = self.temp();
        let site = if name == "fixnan" {
            let shape = args
                .first()
                .map(|arg| format!("{:?}", arg.value))
                .unwrap_or_default();
            self.intern(&shape)
        } else {
            self.intern(&format!("core.{name}@"))
        };
        let method = self.intern(name);
        self.emit(Instruction::Call {
            dst: Some(dst),
            target: CallTarget::Builtin {
                namespace: BuiltinNamespace::Core,
                method,
            },
            args: slots,
            homes: vec![None; args.len()],
            type_args: Vec::new(),
            site,
        });
        Ok(dst)
    }

    /// Fallback reason for a `request.*` call shape the injected path does
    /// not serve: the tree-walk decides it, with its own wording.
    fn request_shape(&mut self, method: &str) -> FallbackReason {
        FallbackReason::OutsideBuiltin {
            namespace: self.intern("request"),
            method: self.intern(method),
        }
    }

    /// Lower `request.security(symbol, timeframe, exprs, gaps?, lookahead?,
    /// calc_bars_count=?)`. Evaluation order matches the tree-walk's
    /// injected path: `calc_bars_count` first, then symbol, timeframe, and
    /// the two merge modes. The expression list compiles to the dataset-tail
    /// subset; any other shape keeps the fallback so the tree refuses.
    fn lower_request_security(&mut self, args: &[CallArg]) -> Result<SlotId, FallbackReason> {
        let positional: Vec<&CallArg> = args.iter().filter(|arg| arg.name.is_none()).collect();
        if positional.len() < 3 || positional.len() > 5 {
            return Err(self.request_shape("security"));
        }
        let mut calc_bars = None;
        for arg in args.iter().filter(|arg| arg.name.is_some()) {
            if arg.name.as_deref() == Some("calc_bars_count") {
                calc_bars = Some(self.lower_expr_slot(&arg.value).map_err(call_arg_ctx)?);
            } else {
                return Err(self.request_shape("security"));
            }
        }
        let symbol = self
            .lower_expr_slot(&positional[0].value)
            .map_err(call_arg_ctx)?;
        let timeframe = self
            .lower_expr_slot(&positional[1].value)
            .map_err(call_arg_ctx)?;
        let gaps = match positional.get(3) {
            Some(arg) => Some(self.lower_expr_slot(&arg.value).map_err(call_arg_ctx)?),
            None => None,
        };
        let lookahead = match positional.get(4) {
            Some(arg) => Some(self.lower_expr_slot(&arg.value).map_err(call_arg_ctx)?),
            None => None,
        };
        let (single, items) = match &positional[2].value {
            PineExpr::ArrayLiteral(items) => (
                false,
                items
                    .iter()
                    .map(|item| self.lower_security_expr(item))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            single => (true, vec![self.lower_security_expr(single)?]),
        };
        let dst = self.temp();
        let site = self.intern(&site_string("request.security", args));
        self.emit(Instruction::RequestSecurity {
            dst: Some(dst),
            symbol,
            timeframe,
            gaps,
            lookahead,
            calc_bars,
            single,
            items,
            site,
        });
        Ok(dst)
    }

    /// Lower `request.security_lower_tf(symbol, timeframe, [legs],
    /// calc_bars_count=?)`: the served value is one array per requested
    /// chart leg, so every item must name a bare leg. Anything else keeps
    /// the fallback.
    fn lower_request_lower_tf(&mut self, args: &[CallArg]) -> Result<SlotId, FallbackReason> {
        let positional: Vec<&CallArg> = args.iter().filter(|arg| arg.name.is_none()).collect();
        if positional.len() != 3 {
            return Err(self.request_shape("security_lower_tf"));
        }
        let mut calc_bars = None;
        for arg in args.iter().filter(|arg| arg.name.is_some()) {
            if arg.name.as_deref() == Some("calc_bars_count") {
                calc_bars = Some(self.lower_expr_slot(&arg.value).map_err(call_arg_ctx)?);
            } else {
                return Err(self.request_shape("security_lower_tf"));
            }
        }
        let symbol = self
            .lower_expr_slot(&positional[0].value)
            .map_err(call_arg_ctx)?;
        let timeframe = self
            .lower_expr_slot(&positional[1].value)
            .map_err(call_arg_ctx)?;
        let PineExpr::ArrayLiteral(items) = &positional[2].value else {
            return Err(self.request_shape("security_lower_tf"));
        };
        let mut legs = Vec::with_capacity(items.len());
        for item in items {
            let Some(name) = security_leg_name(item) else {
                return Err(self.request_shape("security_lower_tf"));
            };
            let name = self.intern(name);
            legs.push(name);
        }
        let dst = self.temp();
        let site = self.intern(&site_string("request.security_lower_tf", args));
        self.emit(Instruction::RequestLowerTf {
            dst: Some(dst),
            symbol,
            timeframe,
            legs,
            calc_bars,
            site,
        });
        Ok(dst)
    }

    /// Compile one `request.security` expression into the dataset-tail
    /// subset: bare legs, literals, kernel-backed `ta.*` calls, and scalar
    /// operators. Every other shape keeps the fallback (the tree refuses
    /// with its own subset wording).
    fn lower_security_expr(&mut self, expr: &PineExpr) -> Result<SecurityExpr, FallbackReason> {
        match expr {
            PineExpr::BuiltinVariable(var) => {
                let name = self.intern(&var.to_string());
                Ok(SecurityExpr::Leg(name))
            }
            PineExpr::Variable(name) => {
                let name = self.intern(name);
                Ok(SecurityExpr::Leg(name))
            }
            PineExpr::Number(value) => Ok(SecurityExpr::Const(
                self.const_value(ConstValue::Float(*value)),
            )),
            PineExpr::StringLiteral(text) => Ok(SecurityExpr::Const(
                self.const_value(ConstValue::Str(text.clone())),
            )),
            PineExpr::Boolean(flag) => Ok(SecurityExpr::Const(
                self.const_value(ConstValue::Bool(*flag)),
            )),
            PineExpr::Binary { left, op, right } => Ok(SecurityExpr::Binary {
                op: bin_op(*op),
                left: Box::new(self.lower_security_expr(left)?),
                right: Box::new(self.lower_security_expr(right)?),
            }),
            PineExpr::MethodCall {
                namespace,
                method,
                args,
                ..
            } if namespace == "ta" => {
                let mut chart_args = Vec::new();
                let source = if method == "atr" {
                    for arg in args {
                        chart_args.push(self.lower_expr_slot(&arg.value).map_err(call_arg_ctx)?);
                    }
                    None
                } else {
                    let Some(first) = args.first() else {
                        return Err(self.request_shape("security"));
                    };
                    let Some(name) = security_leg_name(&first.value) else {
                        return Err(self.request_shape("security"));
                    };
                    let source = self.intern(name);
                    for arg in &args[1..] {
                        chart_args.push(self.lower_expr_slot(&arg.value).map_err(call_arg_ctx)?);
                    }
                    Some(source)
                };
                Ok(SecurityExpr::Call {
                    method: self.intern(method),
                    source,
                    chart_args,
                })
            }
            _ => Err(self.request_shape("security")),
        }
    }

    /// Lower the harness declaration `strategy(...)` as one shared builtin
    /// call: every argument evaluates in source order (its refusals stand),
    /// and the shared dispatcher retains only the declared capital — the
    /// documented harness boundary for the remaining broker settings.
    fn lower_strategy_declare(&mut self, args: &[CallArg]) -> Result<SlotId, FallbackReason> {
        let (slots, _) = self.lower_call_args(args, false)?;
        let dst = self.temp();
        let site = self.intern("strategy.declare@");
        let method = self.intern("declare");
        self.emit(Instruction::Call {
            dst: Some(dst),
            target: CallTarget::Builtin {
                namespace: BuiltinNamespace::Strategy,
                method,
            },
            args: slots,
            homes: vec![None; args.len()],
            type_args: Vec::new(),
            site,
        });
        Ok(dst)
    }

    /// Emit a deterministic refusal at this program point. The module
    /// stays compiled; the run stops here with the tree-walk's exact
    /// message, and the returned slot is a fresh temp no branch reads.
    fn emit_refuse(&mut self, message: String) -> SlotId {
        let reason = self.const_value(ConstValue::Str(message));
        self.emit(Instruction::Refuse { reason });
        self.temp()
    }

    /// Lower `runtime.error(...)`: every argument evaluates in source order
    /// (their refusals stand), and the first argument's slot becomes the
    /// refusal message. No arguments (or an absent first value) raises the
    /// bare wording at run time, exactly like the tree-walk.
    fn lower_runtime_error(&mut self, args: &[CallArg]) -> Result<SlotId, FallbackReason> {
        let (slots, _) = self.lower_call_args(args, false)?;
        let message = slots.first().map_or_else(|| self.temp(), |slot| slot.slot);
        self.emit(Instruction::RuntimeRefuse { message });
        Ok(self.temp())
    }

    /// Lower a headless alert call. Both forms evaluate every argument in
    /// source order (their refusals stand) and return `na`: alerts cannot
    /// fire without a delivery path, and the harness has no capture vector
    /// for them, so the shared handler records nothing.
    fn lower_alert(&mut self, name: &str, args: &[CallArg]) -> Result<SlotId, FallbackReason> {
        let (slots, _) = self.lower_call_args(args, false)?;
        let dst = self.temp();
        let site = self.intern(&format!("{name}@"));
        let method = self.intern(name);
        self.emit(Instruction::Call {
            dst: Some(dst),
            target: CallTarget::Builtin {
                namespace: BuiltinNamespace::Alert,
                method,
            },
            args: slots,
            homes: vec![None; args.len()],
            type_args: Vec::new(),
            site,
        });
        Ok(dst)
    }

    /// Lower a bare call in statement position (`plot(...)`,
    /// `strategy.*`, drawing calls).
    fn lower_call_statement(&mut self, name: &str, args: &[CallArg]) {
        if name == "plot" {
            if let Err(reason) = self.lower_plot(args) {
                self.fallback(reason);
            }
            return;
        }
        if name == "strategy" {
            if let Err(reason) = self.lower_strategy_declare(args) {
                self.fallback(reason);
            }
            return;
        }
        if matches!(name, "alert" | "alertcondition") {
            if let Err(reason) = self.lower_alert(name, args) {
                self.fallback(reason);
            }
            return;
        }
        if let Some(kind) = capture_kind(name) {
            if let Err(reason) = self.lower_capture(kind, args) {
                self.fallback(reason);
            }
            return;
        }
        // Every other bare call is an expression call for effect:
        // evaluate and discard, preserving argument order and side
        // effects. Unlowerable arguments fall the statement back.
        if self.lower_call(name, args).is_err() {
            let name = self.intern(name);
            self.fallback(FallbackReason::StatementCall { name });
        }
    }

    fn lower_method_statement(
        &mut self,
        namespace: &str,
        method: &str,
        args: &[CallArg],
        type_args: &[PineType],
    ) -> Result<(), FallbackReason> {
        // Computed refusal message in statement position: same lowering as
        // the value form, with the (unreached) return dropped.
        if namespace == "runtime" && method == "error" {
            self.lower_runtime_error(args)?;
            return Ok(());
        }
        if let Some(namespace) = BuiltinNamespace::parse(namespace) {
            self.lower_builtin_call(namespace, method, args, type_args, true)?;
            return Ok(());
        }
        if let Some(tail) = namespace.strip_prefix("strategy.") {
            let member = format!("{tail}.{method}");
            self.lower_builtin_call(BuiltinNamespace::Strategy, &member, args, type_args, true)?;
            return Ok(());
        }
        if DrawingKind::parse(namespace).is_some() {
            let member = format!("{namespace}.{method}");
            self.lower_builtin_call(BuiltinNamespace::Drawing, &member, args, type_args, true)?;
            return Ok(());
        }
        if namespace == "request" {
            match method {
                "security" => {
                    self.lower_request_security(args)?;
                    return Ok(());
                }
                "security_lower_tf" => {
                    self.lower_request_lower_tf(args)?;
                    return Ok(());
                }
                _ => {}
            }
        }
        // `this` calls dispatch against the innermost live receiver,
        // exactly like the value-position twin: the mutated copy writes
        // back in place and the (unreached) return drops.
        if namespace == "this" {
            let recv = self.temp();
            self.emit(Instruction::ThisRead { dst: recv });
            self.emit_instance_call(recv, method, args, CallHome::Innermost, true)?;
            return Ok(());
        }
        // Held-receiver calls (`bars.fetch()`) take the instance out of
        // its home and write the mutated copy back, exactly like the
        // value-position twin — only the return drops. An unbound head
        // is not a held receiver: the call keeps the namespace fallback
        // and the tree names the full spelling, exactly like values.
        if !namespace.contains('.')
            && !matches!(
                namespace,
                "strategy" | "runtime" | "request" | "label" | "table" | "box" | "line"
            )
            && !self.aliases.contains_key(namespace)
            && BuiltinNamespace::parse(namespace).is_none()
            && (self.frame_vars.contains_key(namespace)
                || self.locals.contains_key(namespace)
                || self.any_method_alias.contains(namespace)
                || self.loop_bound(namespace)
                || self.globals.contains_key(namespace)
                || self.stmt_bound.contains(namespace)
                || self.decls.declared(namespace, self.module_pos, self.top_level))
        {
            let recv = self.lower_variable(namespace)?;
            let ordinal = self.frame_vars.get(namespace).map(|(ordinal, _)| *ordinal);
            let name = self.intern(namespace);
            self.emit_instance_call(
                recv,
                method,
                args,
                CallHome::Var {
                    name,
                    var_ordinal: ordinal,
                },
                true,
            )?;
            return Ok(());
        }
        // Nested receivers (`w.items.push(x)`) in statement position:
        // the chain home travels with the call exactly like the
        // value-position twin, so the loop writes back through the chain
        // instead of keeping a fallback.
        if let Some((head, rest)) = namespace.split_once('.') {
            if !rest.is_empty()
                && !rest.contains('.')
                && !self.aliases.contains_key(head)
                && head != "this"
            {
                let recv = self.temp();
                let name = self.intern(head);
                let field = self.intern(rest);
                let var_ordinal = self.frame_vars.get(head).map(|(ordinal, _)| *ordinal);
                self.emit_instance_call(
                    recv,
                    method,
                    args,
                    CallHome::VarPath {
                        name,
                        var_ordinal,
                        path: vec![field],
                    },
                    true,
                )?;
                return Ok(());
            }
            if head == "this" && !rest.contains('.') {
                let recv = self.temp();
                let field = self.intern(rest);
                self.emit_instance_call(
                    recv,
                    method,
                    args,
                    CallHome::ThisField { field },
                    true,
                )?;
                return Ok(());
            }
        }
        Err(FallbackReason::OutsideBuiltin {
            namespace: self.intern(namespace),
            method: self.intern(method),
        })
    }

    fn lower_method_call(
        &mut self,
        namespace: &str,
        method: &str,
        args: &[CallArg],
        type_args: &[PineType],
        stmt_position: bool,
    ) -> Result<SlotId, FallbackReason> {
        // Computed refusal message: the call lowers to a run-time refusal
        // instead of a fallback, so a dead-branch error stays silent until
        // reached and a live one names the tree-walk's exact wording.
        if namespace == "runtime" && method == "error" {
            return self.lower_runtime_error(args);
        }
        // `timeframe.in_seconds()` with no arguments reports the chart
        // period in seconds through the shared resolver, so config and
        // value agree on both engines. Any arguments take the namespaced
        // bridge below.
        if namespace == "timeframe" && method == "in_seconds" && args.is_empty() {
            let dst = self.temp();
            self.emit(Instruction::Builtin {
                dst,
                var: BuiltinVarKind::ChartPeriodSeconds,
            });
            return Ok(dst);
        }
        if let Some(namespace) = BuiltinNamespace::parse(namespace) {
            return self.lower_builtin_call(namespace, method, args, type_args, stmt_position);
        }
        // Qualified harness namespaces (`strategy.closedtrades.size`): the
        // tail joins the method so one dispatch member names the whole
        // member, exactly like the tree-walk's `namespace.method` key.
        if let Some(tail) = namespace.strip_prefix("strategy.") {
            let member = format!("{tail}.{method}");
            return self.lower_builtin_call(
                BuiltinNamespace::Strategy,
                &member,
                args,
                type_args,
                stmt_position,
            );
        }
        // Injected request reads: the loop serves exactly the injected
        // tapes; anything the tree's injected path would refuse keeps a
        // fallback so the tree-walk answers with its own wording.
        if namespace == "request" {
            return match method {
                "security" => self.lower_request_security(args),
                "security_lower_tf" => self.lower_request_lower_tf(args),
                _ => Err(FallbackReason::OutsideBuiltin {
                    namespace: self.intern(namespace),
                    method: self.intern(method),
                }),
            };
        }
        // Drawing families: the namespace folds into the dispatch member so
        // one builtin namespace serves `label`, `table`, `box`, and `line`.
        if DrawingKind::parse(namespace).is_some() {
            let member = format!("{namespace}.{method}");
            return self.lower_builtin_call(
                BuiltinNamespace::Drawing,
                &member,
                args,
                type_args,
                stmt_position,
            );
        }
        // `this` calls dispatch against the innermost live receiver.
        if namespace == "this" {
            let recv = self.temp();
            self.emit(Instruction::ThisRead { dst: recv });
            return self.emit_instance_call(recv, method, args, CallHome::Innermost, stmt_position);
        }
        // Construction (`Counter.new()`): field defaults evaluate per
        // call at the call site, exactly like the tree-walk's
        // field-by-field build. Unknown types keep their fallback and
        // the tree-walk decides them.
        if method == "new" && self.own_types.contains(namespace) {
            return self.lower_construct(namespace, args, stmt_position);
        }
        // Method dispatch on a held receiver (`w.bump(...)`): take the
        // instance out of its home, dispatch on its runtime type, and
        // write the mutated copy back — mirroring the tree-walk's
        // take/put lifecycle. Map-held receivers stay tree-walk (their
        // write-back needs the map-entry harvest); nested-field paths,
        // import aliases, and the tree-walk's dialogue namespaces keep
        // their existing routes below. An unbound head is not a held
        // receiver at all: the tree names the full `head.method`
        // spelling in its refusal, so the call keeps the namespace
        // fallback and the tree decides.
        if !namespace.contains('.')
            && !matches!(
                namespace,
                "strategy" | "runtime" | "request" | "label" | "table" | "box" | "line"
            )
            && !self.aliases.contains_key(namespace)
            && (self.frame_vars.contains_key(namespace)
                || self.locals.contains_key(namespace)
                || self.any_method_alias.contains(namespace)
                || self.loop_bound(namespace)
                || self.globals.contains_key(namespace)
                || self.stmt_bound.contains(namespace)
                || self.decls.declared(namespace, self.module_pos, self.top_level))
        {
            let recv = self.lower_variable(namespace)?;
            let ordinal = self.frame_vars.get(namespace).map(|(ordinal, _)| *ordinal);
            let name = self.intern(namespace);
            return self.emit_instance_call(
                recv,
                method,
                args,
                CallHome::Var {
                    name,
                    var_ordinal: ordinal,
                },
                stmt_position,
            );
        }
        // Alias-qualified user call: `ALIAS.fn(...)` resolves through the
        // link-time alias table to the exporting module. Omitted
        // defaults fill caller-side from the target's signature, exactly
        // like same-module calls — otherwise the linker's exact-arity
        // check would fail a call the tree serves (defaults are
        // caller-evaluated on both engines).
        if let Some(key) = self.aliases.get(namespace).cloned() {
            if self.exports.func_is_overloaded(&key, method) {
                return Err(FallbackReason::OverloadedCallee);
            }
            if let Some(params) = self.exports.func_params(&key, method) {
                // Lower every argument in call order first (tree-walk
                // evaluates before binding), then bind by name and
                // position, then caller-scope defaults — the same order
                // as the same-module path above.
                let mut lowered: Vec<(Option<String>, SlotId, Option<String>)> =
                    Vec::with_capacity(args.len());
                for arg in args {
                    let slot = self.lower_expr_slot(&arg.value).map_err(|inner| {
                        FallbackReason::CallArgument {
                            inner: Box::new(inner),
                        }
                    })?;
                    let home = match &arg.value {
                        PineExpr::Variable(name) => Some(name.clone()),
                        _ => None,
                    };
                    if let Some(dotted) = home.as_deref() {
                        if home_is_dotted(Some(dotted)) {
                            if dotted == "this" {
                                return Err(FallbackReason::DottedHome);
                            }
                            if !dotted.starts_with("this.") {
                                self.global(dotted);
                            }
                        }
                    }
                    lowered.push((arg.name.clone(), slot, home));
                }
                let positional: Vec<(SlotId, Option<String>)> = lowered
                    .iter()
                    .filter(|(name, _, _)| name.is_none())
                    .map(|(_, slot, home)| (*slot, home.clone()))
                    .collect();
                let mut positional_index = 0;
                let mut bound: Vec<(CallArgSlot, Option<SymbolId>)> =
                    Vec::with_capacity(params.len());
                for (param, default) in &params {
                    if let Some((_, slot, home)) = lowered
                        .iter()
                        .find(|(name, _, _)| name.as_deref() == Some(param.as_str()))
                    {
                        let home = home.as_deref().map(|name| self.intern(name));
                        bound.push((
                            CallArgSlot {
                                name: Some(self.intern(param)),
                                slot: *slot,
                            },
                            home,
                        ));
                        continue;
                    }
                    if positional_index < positional.len() {
                        let (slot, home) = positional[positional_index].clone();
                        positional_index += 1;
                        let home = home.as_deref().map(|name| self.intern(name));
                        bound.push((
                            CallArgSlot {
                                name: Some(self.intern(param)),
                                slot,
                            },
                            home,
                        ));
                        continue;
                    }
                    let Some(default) = default else {
                        return Err(FallbackReason::MissingDefault);
                    };
                    let dst = self.lower_expr_slot(default).map_err(|inner| {
                        FallbackReason::CallArgument {
                            inner: Box::new(inner),
                        }
                    })?;
                    bound.push((
                        CallArgSlot {
                            name: Some(self.intern(param)),
                            slot: dst,
                        },
                        None,
                    ));
                }
                if positional_index < positional.len() {
                    return Err(FallbackReason::TooManyPositionals);
                }
                for (name, _, _) in &lowered {
                    if let Some(given) = name {
                        if !params.iter().any(|(param, _)| param == given) {
                            return Err(FallbackReason::UnknownArgument {
                                name: self.intern(given),
                            });
                        }
                    }
                }
                let (slots, homes): (Vec<CallArgSlot>, Vec<Option<SymbolId>>) =
                    bound.into_iter().unzip();
                let module = self.intern(&key);
                let name = self.intern(method);
                let site = self.intern(&site_string(&format!("{key}::{method}"), args));
                let dst = self.temp();
                self.emit(Instruction::Call {
                    dst: Some(dst),
                    target: CallTarget::Named {
                        module: Some(module),
                        name,
                    },
                    args: slots,
                    homes,
                    type_args: Vec::new(),
                    site,
                });
                return Ok(dst);
            }
            let (slots, homes) = self.lower_call_args(args, true)?;
            let module = self.intern(&key);
            let name = self.intern(method);
            // Site strings match the tree-walk byte-for-byte (exporting
            // module, method, debug args): a cross-engine call composes
            // the identical stack in either engine, so shared keyed state
            // (ta kernels, call-site vars) partitions identically.
            let site = self.intern(&site_string(&format!("{key}::{method}"), args));
            let dst = self.temp();
            self.emit(Instruction::Call {
                dst: Some(dst),
                target: CallTarget::Named {
                    module: Some(module),
                    name,
                },
                args: slots,
                homes,
                type_args: Vec::new(),
                site,
            });
            return Ok(dst);
        }
        // Nested receiver (`o.box.bump()`, `this.box.bump()`): the loop
        // takes the root and walks the field path through the shared home
        // model, so the mutated chain returns to the same home.
        // Alias-qualified construction (`ALIAS.Type.new(...)`): field
        // defaults evaluate per construction at the call site through the
        // exporting module's field table, exactly like same-module
        // construction. Anything else keeps its existing route below.
        if method == "new" {
            if let Some((head, rest)) = namespace.split_once('.') {
                if !rest.is_empty() && !rest.contains('.') {
                    if let Some(target) = self.aliases.get(head).cloned() {
                        return self.lower_alias_construct(
                            namespace,
                            method,
                            &target,
                            rest,
                            args,
                            stmt_position,
                        );
                    }
                }
            }
        }
        if let Some((head, rest)) = namespace.split_once('.') {
            if !rest.is_empty()
                && !rest.contains('.')
                && !self.aliases.contains_key(head)
                && head != "this"
            {
                let recv = self.temp();
                let name = self.intern(head);
                let field = self.intern(rest);
                let var_ordinal = self.frame_vars.get(head).map(|(ordinal, _)| *ordinal);
                return self.emit_instance_call(
                    recv,
                    method,
                    args,
                    CallHome::VarPath {
                        name,
                        var_ordinal,
                        path: vec![field],
                    },
                    stmt_position,
                );
            }
            if head == "this" && !rest.contains('.') {
                let recv = self.temp();
                let field = self.intern(rest);
                return self.emit_instance_call(
                    recv,
                    method,
                    args,
                    CallHome::ThisField { field },
                    stmt_position,
                );
            }
        }
        Err(FallbackReason::OutsideBuiltin {
            namespace: self.intern(namespace),
            method: self.intern(method),
        })
    }

    /// Lower a held-receiver method call once the receiver value is in a
    /// slot: dispatch on its runtime type and write the mutated copy back
    /// through the caller home — mirroring the tree-walk's take/put
    /// lifecycle. Omitted computed defaults evaluate at the call site in
    /// the caller's scope (unanimous across same-named candidates, so the
    /// dispatch target cannot matter) and ride by name; divergent
    /// candidates keep their fallback.
    fn emit_instance_call(
        &mut self,
        recv: SlotId,
        method: &str,
        args: &[CallArg],
        home: CallHome,
        stmt_position: bool,
    ) -> Result<SlotId, FallbackReason> {
        let computed = self.caller_computed_defaults(method, args)?;
        let (mut slots, mut homes) = self.lower_call_args(args, true)?;
        for (name, expr) in computed {
            let slot =
                self.lower_expr_slot(&expr)
                    .map_err(|inner| FallbackReason::CallArgument {
                        inner: Box::new(inner),
                    })?;
            slots.push(CallArgSlot {
                name: Some(self.intern(&name)),
                slot,
            });
            homes.push(None);
        }
        let method = self.intern(method);
        let args_debug = self.const_value(ConstValue::Str(format!("{args:?}")));
        let dst = self.temp();
        self.emit(Instruction::InstanceCall {
            dst: Some(dst),
            recv,
            method,
            args: slots,
            homes,
            args_debug,
            home: Some(home),
            // Statement tails keep statement semantics (mutating
            // collection calls persist) while the value still flows on —
            // exactly the tree-walk's statement-then-value path. The loop
            // reads this bit (not the `dst` shape) to decide write-back,
            // so value reads never persist and statements always do.
            stmt: stmt_position,
        });
        Ok(dst)
    }

    /// Lower an rvalue method call (`(expr).bump(...)`): no home, so the
    /// mutated copy drops and only the return flows on — exactly like
    /// the tree-walk. Omitted computed defaults ride by name as above.
    fn lower_rvalue_call(
        &mut self,
        receiver: &PineExpr,
        method: &str,
        args: &[CallArg],
    ) -> Result<SlotId, FallbackReason> {
        let computed = self.caller_computed_defaults(method, args)?;
        let recv = self
            .lower_expr_slot(receiver)
            .map_err(|_| FallbackReason::InstanceCall)?;
        let (mut slots, mut homes) = self.lower_call_args(args, true)?;
        for (name, expr) in computed {
            let slot =
                self.lower_expr_slot(&expr)
                    .map_err(|inner| FallbackReason::CallArgument {
                        inner: Box::new(inner),
                    })?;
            slots.push(CallArgSlot {
                name: Some(self.intern(&name)),
                slot,
            });
            homes.push(None);
        }
        let method = self.intern(method);
        let args_debug = self.const_value(ConstValue::Str(format!("{args:?}")));
        let dst = self.temp();
        self.emit(Instruction::InstanceCall {
            dst: Some(dst),
            recv,
            method,
            args: slots,
            homes,
            args_debug,
            home: None,
            stmt: false,
        });
        Ok(dst)
    }

    /// Computed defaults one method call leaves unfilled, in declaration
    /// order: every same-named candidate with an unfilled computed default
    /// must agree on the same (parameter, expression) list, which the
    /// caller then evaluates once in its own scope and passes by name.
    /// Agreement makes the extras exact whichever candidate dispatches at
    /// run time (each resolves them to its own unfilled parameters with
    /// the tree-walk's values); divergent candidates stay ambiguous and
    /// keep the fallback instead of guessing.
    fn caller_computed_defaults(
        &self,
        method: &str,
        args: &[CallArg],
    ) -> Result<Vec<(String, PineExpr)>, FallbackReason> {
        let mut candidate_lists: Vec<Vec<(String, PineExpr)>> = Vec::new();
        for ((_, candidate), sig) in &self.method_blocks {
            if candidate != method {
                continue;
            }
            let mut positional = args.iter().filter(|arg| arg.name.is_none());
            let mut unfilled = Vec::new();
            for (name, default) in sig.params.iter().skip(1) {
                if args
                    .iter()
                    .any(|arg| arg.name.as_deref() == Some(name.as_str()))
                {
                    continue;
                }
                if positional.next().is_some() {
                    continue;
                }
                if let Some(expr) = default {
                    if Self::literal_const(expr).is_none() {
                        unfilled.push((name.clone(), expr.clone()));
                    }
                }
            }
            candidate_lists.push(unfilled);
        }
        let Some(first) = candidate_lists.first() else {
            return Ok(Vec::new());
        };
        // Unanimous lists (including all-empty) evaluate once; anything
        // else stays ambiguous.
        if candidate_lists.iter().all(|list| list == first) {
            return Ok(first.clone());
        }
        Err(FallbackReason::MissingDefault)
    }

    /// Lower construction (`Counter.new()`): field defaults evaluate per
    /// call at the call site, exactly like the tree-walk's field-by-field
    /// build. A non-empty argument list keeps its fallback and the
    /// tree-walk refuses it with the exact wording.
    fn lower_construct(
        &mut self,
        type_name: &str,
        args: &[CallArg],
        stmt_position: bool,
    ) -> Result<SlotId, FallbackReason> {
        let decls = self
            .own_type_fields
            .get(type_name)
            .cloned()
            .unwrap_or_default();
        if args.is_empty() {
            return self.emit_construct(type_name, decls, stmt_position);
        }
        self.lower_construct_args(type_name, &decls, args, stmt_position)
    }

    /// Lower alias-qualified construction through the exporting module's
    /// field table: same per-call field evaluation as [`Self::lower_construct`],
    /// so a fresh cross-module value carries the same defaults. Unknown
    /// or unexported types keep the namespace fallback (the tree-walk
    /// decides), and calls with arguments keep the object-model fallback,
    /// exactly like same-module construction with arguments.
    fn lower_alias_construct(
        &mut self,
        namespace: &str,
        method: &str,
        target: &str,
        type_name: &str,
        args: &[CallArg],
        stmt_position: bool,
    ) -> Result<SlotId, FallbackReason> {
        if !self.exports.contains(target, type_name) {
            return Err(FallbackReason::OutsideBuiltin {
                namespace: self.intern(namespace),
                method: self.intern(method),
            });
        }
        let decls = self
            .exports
            .type_fields(target, type_name)
            .unwrap_or_default();
        if args.is_empty() {
            return self.emit_construct(type_name, decls, stmt_position);
        }
        self.lower_construct_args(type_name, &decls, args, stmt_position)
    }

    /// Lower argument-carrying construction (`Type.new(a, b=2)`): every
    /// argument lowers in order (their refusals stand), positionals fill
    /// declaration order and named ones bind by field — the first binding
    /// wins on repeats, exactly like the tree-walk. Unknown names and
    /// surplus positionals lower to a run-time refusal (never a link
    /// refusal), so dead branches stay silent until reached on both
    /// engines.
    fn lower_construct_args(
        &mut self,
        type_name: &str,
        decls: &[(String, Option<PineExpr>)],
        args: &[CallArg],
        stmt_position: bool,
    ) -> Result<SlotId, FallbackReason> {
        let mut lowered: Vec<(Option<String>, SlotId)> = Vec::with_capacity(args.len());
        for arg in args {
            let slot = self.lower_expr_slot(&arg.value).map_err(|inner| {
                FallbackReason::CallArgument {
                    inner: Box::new(inner),
                }
            })?;
            lowered.push((arg.name.clone(), slot));
        }
        let mut bound: HashMap<String, SlotId> = HashMap::new();
        let mut positional = 0_usize;
        for (name, slot) in lowered {
            if let Some(given) = name {
                if !decls.iter().any(|(field, _)| field == &given) {
                    return Ok(self.emit_refuse(
                        super::eval::method_unknown_argument_message(&given),
                    ));
                }
                bound.entry(given).or_insert(slot);
            } else {
                let Some((field, _)) = decls.get(positional) else {
                    return Ok(self.emit_refuse(
                        super::eval::method_too_many_positionals_message(),
                    ));
                };
                positional += 1;
                bound.entry(field.clone()).or_insert(slot);
            }
        }
        let mut fields = Vec::with_capacity(decls.len());
        for (name, default) in decls {
            let slot = match bound.get(name) {
                Some(slot) => *slot,
                None => match default {
                    Some(expr) => self.lower_expr_slot(expr)?,
                    None => self.temp(),
                },
            };
            fields.push((self.intern(name), slot));
        }
        let _ = stmt_position;
        let dst = self.temp();
        let type_name = self.intern(type_name);
        self.emit(Instruction::ObjNew {
            dst,
            type_name,
            fields,
        });
        Ok(dst)
    }

    /// Emit one construction from evaluated field defaults: each default
    /// lowers at the call site in order (its refusals stand), missing
    /// defaults read absence — exactly the tree-walk's field-by-field
    /// build, shared by same-module and alias-qualified construction.
    fn emit_construct(        &mut self,
        type_name: &str,
        decls: Vec<(String, Option<PineExpr>)>,
        stmt_position: bool,
    ) -> Result<SlotId, FallbackReason> {
        let mut fields = Vec::new();
        for (name, default) in decls {
            let slot = match default {
                Some(expr) => self.lower_expr_slot(&expr)?,
                None => self.temp(),
            };
            fields.push((self.intern(&name), slot));
        }
        let _ = stmt_position;
        let dst = self.temp();
        let type_name = self.intern(type_name);
        self.emit(Instruction::ObjNew {
            dst,
            type_name,
            fields,
        });
        Ok(dst)
    }

    /// Receiver home for one dotted rebinding, if the loop takes it:
    /// single-dot variable paths whose head is neither `this`, an import
    /// alias, nor a map-held name. Anything else keeps its existing
    /// route. The in-function cell ordinal travels when the calling body
    /// declares one, exactly like the journaled write homes.
    fn field_put_home(&mut self, name: &str) -> Option<CallHome> {
        let (head, field) = name.split_once('.')?;
        if head == "this"
            || head.is_empty()
            || field.is_empty()
            || field.contains('.')
            || self.aliases.contains_key(head)
        {
            return None;
        }
        let var_ordinal = self.frame_vars.get(head).map(|(ordinal, _)| *ordinal);
        Some(CallHome::Var {
            name: self.intern(head),
            var_ordinal,
        })
    }

    fn lower_builtin_call(
        &mut self,
        namespace: BuiltinNamespace,
        method: &str,
        args: &[CallArg],
        type_args: &[PineType],
        stmt_position: bool,
    ) -> Result<SlotId, FallbackReason> {
        // `array.shift`/`map.remove` in statement position persist the
        // shrunken collection while the call's own value is dropped: the
        // shared write-back homes below carry them exactly like the other
        // mutators, and the loop serves the collection from the same
        // split steps the tree-walk uses.
        let (slots, homes) = self.lower_call_args(
            args,
            stmt_position && super::eval::writes_back_collection(namespace.as_str(), method),
        )?;
        // Write-back mirrors the tree-walk exactly: only mutating
        // collection calls persist, and only to the first argument's
        // variable home. Every other call drops its homes — persisting
        // e.g. a `size` answer would clobber the collection with one
        // element. Value position never persists (the tree-walk's bridge
        // path has no write-back), so non-statement calls strip too.
        // Homes arrive pre-gated from `lower_call_args`: only the calls
        // that keep one ever computed it, so read-only calls never
        // refuse dotted value arguments they would have dropped.
        let homes =
            if stmt_position && super::eval::writes_back_collection(namespace.as_str(), method) {
                let mut kept = vec![None; homes.len()];
                if let Some(first) = homes.first() {
                    kept[0] = *first;
                }
                kept
            } else {
                vec![None; args.len()]
            };
        let site = self.intern(&site_string(
            &format!("{}.{method}", namespace.as_str()),
            args,
        ));
        let dst = self.temp();
        let method = self.intern(method);
        let type_args = type_args
            .iter()
            .map(|arg| self.intern_type_arg(arg))
            .collect();
        self.emit(Instruction::Call {
            dst: Some(dst),
            target: CallTarget::Builtin { namespace, method },
            args: slots,
            homes,
            type_args,
            site,
        });
        Ok(dst)
    }

    /// Plot tag for a binding right-hand side: the capture site constant
    /// when the value is syntactically a `plot()` call (the tag the
    /// tree-walk's plot-variable discipline records for `fill()`
    /// pairing), else `None` (clears any stale tag).
    fn plot_tag_of(&mut self, value: &PineExpr) -> Option<ConstId> {
        match value {
            PineExpr::FunctionCall { name, args, .. } if name == "plot" => {
                Some(self.const_value(ConstValue::Str(site_string("plot", args))))
            }
            _ => None,
        }
    }

    /// Lower an in-function `plot()` binding: emit the capture (the value
    /// is absence, exactly like the tree-walk), tag the run-global site,
    /// and keep the value frame-local — journaled writes flow through the
    /// caller home so nested observers agree, pure locals bind a temp.
    /// Persistent names stay tree-walk, exactly like plain values.
    fn lower_plot_assign(
        &mut self,
        name: &str,
        args: &[CallArg],
        strict: bool,
    ) -> Result<(), FallbackReason> {
        if self.decls.is_persistent(name) {
            return Err(FallbackReason::FunctionBodyBinding {
                name: self.intern(name),
            });
        }
        let (na, site) = self.lower_plot_value(args)?;
        let tag = self.intern(name);
        if self.journaled && self.globals.contains_key(name) {
            self.emit_journaled(name, na, strict);
        } else if let Some(local) = self.locals.get(name).copied() {
            self.emit(Instruction::Copy {
                dst: local,
                src: na,
            });
        } else {
            let fresh = self.temp();
            self.emit(Instruction::Copy {
                dst: fresh,
                src: na,
            });
            self.locals.insert(name.to_owned(), fresh);
        }
        self.emit(Instruction::PlotTag {
            name: tag,
            tag: Some(site),
        });
        Ok(())
    }

    /// Lowered `plot()` parts: series, title, site, display. Evaluates
    /// every other argument for effect and drops it, exactly like the
    /// tree-walk. Unlowerable shapes report instead of emitting.
    fn lower_plot_parts(&mut self, args: &[CallArg]) -> Result<PlotParts, FallbackReason> {
        // The tree-walk evaluates every argument in source order and takes
        // the first value as the series — named or not. Lower them all in
        // that order so side effects (input declarations) cannot reorder,
        // then assign the roles by index.
        if args.is_empty() {
            return Ok(PlotParts {
                src: self.emit_refuse(super::eval::PLOT_MISSING_SERIES_MESSAGE.to_owned()),
                title: self.temp(),
                site: self.const_value(ConstValue::Str(site_string("plot", args))),
                display: None,
            });
        }
        let mut slots = Vec::with_capacity(args.len());
        for arg in args {
            slots.push(self.lower_expr_slot(&arg.value)?);
        }
        let src = slots[0];
        // A named `title` wins over the second positional, exactly like the
        // tree-walk; when neither exists the title slot stays unwritten and
        // the run-time `plot:{n}` default applies at first sight.
        let named_title = args
            .iter()
            .position(|arg| arg.name.as_deref() == Some("title"));
        let positional_second = args
            .iter()
            .enumerate()
            .filter(|(_, arg)| arg.name.is_none())
            .nth(1)
            .map(|(index, _)| index);
        let title = named_title
            .or(positional_second)
            .map_or_else(|| self.temp(), |index| slots[index]);
        let display = args
            .iter()
            .position(|arg| arg.name.as_deref() == Some("display"))
            .map(|index| slots[index]);
        let site = self.const_value(ConstValue::Str(site_string("plot", args)));
        Ok(PlotParts {
            src,
            title,
            site,
            display,
        })
    }

    /// Lower a `plot()` call in value position (nested inside `fill()`
    /// legs): the capture emits exactly like the statement form, and the
    /// call evaluates to absence — the unwritten temp reads back as `na`,
    /// exactly what the tree-walk's `plot` evaluates to.
    fn lower_plot_value(&mut self, args: &[CallArg]) -> Result<(SlotId, ConstId), FallbackReason> {
        let parts = self.lower_plot_parts(args)?;
        self.emit(Instruction::Plot {
            title: super::bytecode::PlotTitle::Slot(parts.title),
            src: parts.src,
            site: parts.site,
            display: parts.display,
        });
        Ok((self.temp(), parts.site))
    }

    /// Resolve a capture title: a named `title` string literal wins;
    /// with `positional_second`, the second positional string literal
    /// wins next (the `plot`/`plotshape`/`hline` rule); otherwise the
    /// run-time `family:{n}` default applies. Dynamic titles fall back
    /// with the statement, exactly like `plot`.
    fn lower_capture_title(
        &mut self,
        args: &[CallArg],
        kind: CaptureKind,
        positional_second: bool,
    ) -> Result<(Option<ConstId>, Option<usize>), FallbackReason> {
        for arg in args.iter().filter(|arg| arg.name.is_some()) {
            if arg.name.as_deref() == Some("title") {
                if let PineExpr::StringLiteral(text) = &arg.value {
                    let id = self.const_value(ConstValue::Str(text.clone()));
                    return Ok((Some(id), None));
                }
                return Err(FallbackReason::CaptureStatement { kind });
            }
        }
        if positional_second {
            let mut positional = args.iter().filter(|arg| arg.name.is_none());
            positional.next();
            if let Some(second) = positional.next() {
                if let PineExpr::StringLiteral(text) = &second.value {
                    let id = self.const_value(ConstValue::Str(text.clone()));
                    return Ok((Some(id), Some(1)));
                }
                return Err(FallbackReason::CaptureStatement { kind });
            }
        }
        Ok((None, None))
    }

    /// Lower a named drawing handle (`display`, `color`, ...): the
    /// argument's value slot, or `None` when the call names none.
    /// Unlowerable values report with argument context.
    fn lower_named_handle(
        &mut self,
        args: &[CallArg],
        name: &str,
    ) -> Result<Option<SlotId>, FallbackReason> {
        let Some(arg) = args.iter().find(|arg| arg.name.as_deref() == Some(name)) else {
            return Ok(None);
        };
        self.lower_expr_slot(&arg.value)
            .map(Some)
            .map_err(call_arg_ctx)
    }

    /// Evaluate every argument a capture did not consume, for effect
    /// (inputs declare), and drop it — exactly like the tree-walk, which
    /// evaluates the whole argument list. `keep` names consumed named
    /// arguments; the first `used_positional` positionals were consumed.
    /// Unlowerable values report with argument context.
    fn lower_capture_rest(
        &mut self,
        args: &[CallArg],
        keep: &[&str],
        used_positional: usize,
    ) -> Result<(), FallbackReason> {
        let mut seen_positional = 0;
        for arg in args {
            match arg.name.as_deref() {
                Some(name) if keep.contains(&name) => {}
                Some(_) => {
                    self.lower_expr_slot(&arg.value).map_err(call_arg_ctx)?;
                }
                None => {
                    if seen_positional < used_positional {
                        seen_positional += 1;
                    } else {
                        self.lower_expr_slot(&arg.value).map_err(call_arg_ctx)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Capture site key: the callee plus the debug rendering of its
    /// arguments. Identical text to the tree-walk's per-family site keys
    /// (`plot@{args:?}`, `plotcandle@{args:?}`, ...), so both engines
    /// name the same call the same site.
    fn capture_site(&mut self, kind: CaptureKind, args: &[CallArg]) -> ConstId {
        self.const_value(ConstValue::Str(site_string(kind.as_str(), args)))
    }

    /// Lower one visual capture in statement position. Emits the capture
    /// and evaluates to `()`; the sibling `lower_capture_value` emits the
    /// same capture and evaluates to absence for value position.
    fn lower_capture(&mut self, kind: CaptureKind, args: &[CallArg]) -> Result<(), FallbackReason> {
        match kind {
            CaptureKind::PlotCandle => self.lower_plotcandle(args).map(|_| ()),
            CaptureKind::BarColor => self.lower_barcolor(args).map(|_| ()),
            CaptureKind::PlotShape => self.lower_plotshape(args).map(|_| ()),
            CaptureKind::Hline => self.lower_hline(args).map(|_| ()),
            CaptureKind::Fill => self.lower_fill(args).map(|_| ()),
            CaptureKind::Alert | CaptureKind::AlertCondition | CaptureKind::Strategy => {
                Err(FallbackReason::CaptureStatement { kind })
            }
        }
    }

    /// Lower one visual capture in value position: the capture emits
    /// exactly like the statement form, and the call evaluates to
    /// absence — the unwritten temp reads back as `na`, exactly what the
    /// tree-walk's capture calls evaluate to.
    fn lower_capture_value(
        &mut self,
        kind: CaptureKind,
        args: &[CallArg],
    ) -> Result<SlotId, FallbackReason> {
        match kind {
            CaptureKind::PlotCandle => self.lower_plotcandle(args),
            CaptureKind::BarColor => self.lower_barcolor(args),
            CaptureKind::PlotShape => self.lower_plotshape(args),
            CaptureKind::Hline => self.lower_hline(args),
            CaptureKind::Fill => self.lower_fill(args),
            CaptureKind::Alert | CaptureKind::AlertCondition | CaptureKind::Strategy => {
                Err(FallbackReason::CaptureValue { kind })
            }
        }?;
        Ok(self.temp())
    }

    /// Lower `plotcandle(open, high, low, close, ...)`: four positional
    /// series plus optional handles. Any other arity is a static shape
    /// the tree-walk refuses at run time.
    fn lower_plotcandle(&mut self, args: &[CallArg]) -> Result<SlotId, FallbackReason> {
        let kind = CaptureKind::PlotCandle;
        let positional: Vec<&CallArg> = args.iter().filter(|arg| arg.name.is_none()).collect();
        if positional.len() != 4 {
            return Err(FallbackReason::CaptureShape { kind });
        }
        let mut legs = [None, None, None, None];
        for (index, arg) in positional.iter().enumerate() {
            legs[index] = Some(self.lower_expr_slot(&arg.value).map_err(call_arg_ctx)?);
        }
        let [open, high, low, close] = legs.map(|slot| slot.expect("four legs checked above"));
        let (title, _) = self.lower_capture_title(args, kind, false)?;
        let site = self.capture_site(kind, args);
        let display = self.lower_named_handle(args, "display")?;
        let color = self.lower_named_handle(args, "color")?;
        let wickcolor = self.lower_named_handle(args, "wickcolor")?;
        let bordercolor = self.lower_named_handle(args, "bordercolor")?;
        self.lower_capture_rest(
            args,
            &["title", "display", "color", "wickcolor", "bordercolor"],
            4,
        )?;
        self.emit(Instruction::PlotCandle {
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
        });
        Ok(self.temp())
    }

    /// Lower `barcolor(series, ...)`: the first positional series paints
    /// the bar (extra positionals evaluate and drop, like the tree-walk).
    fn lower_barcolor(&mut self, args: &[CallArg]) -> Result<SlotId, FallbackReason> {
        let kind = CaptureKind::BarColor;
        let Some(first) = args.iter().find(|arg| arg.name.is_none()) else {
            return Err(FallbackReason::CaptureShape { kind });
        };
        let src = self.lower_expr_slot(&first.value).map_err(call_arg_ctx)?;
        let (title, _) = self.lower_capture_title(args, kind, false)?;
        let site = self.capture_site(kind, args);
        self.lower_capture_rest(args, &["title"], 1)?;
        self.emit(Instruction::BarColor { src, title, site });
        Ok(self.temp())
    }

    /// Lower `plotshape(series, ...)` / `hline(price, ...)`: the first
    /// argument paints (whatever it is, exactly like the tree-walk);
    /// the title follows the named-then-second-positional rule.
    fn lower_shape_like(
        &mut self,
        kind: CaptureKind,
        args: &[CallArg],
    ) -> Result<SlotId, FallbackReason> {
        let Some(first) = args.first() else {
            return Err(FallbackReason::CaptureShape { kind });
        };
        let src = self.lower_expr_slot(&first.value).map_err(call_arg_ctx)?;
        let (title, skipped_pos) = self.lower_capture_title(args, kind, true)?;
        let site = self.capture_site(kind, args);
        let used = skipped_pos.map_or(1, |_| 2);
        self.lower_capture_rest(args, &["title"], used)?;
        match kind {
            CaptureKind::PlotShape => {
                self.emit(Instruction::PlotShape { src, title, site });
            }
            CaptureKind::Hline => {
                self.emit(Instruction::Hline { src, title, site });
            }
            _ => return Err(FallbackReason::CaptureStatement { kind }),
        }
        Ok(self.temp())
    }

    /// Lower `plotshape(series, ...)`.
    fn lower_plotshape(&mut self, args: &[CallArg]) -> Result<SlotId, FallbackReason> {
        self.lower_shape_like(CaptureKind::PlotShape, args)
    }

    /// Lower `hline(price, ...)`.
    fn lower_hline(&mut self, args: &[CallArg]) -> Result<SlotId, FallbackReason> {
        self.lower_shape_like(CaptureKind::Hline, args)
    }

    /// Lower `fill(plot(a), plot(b), ...)` and its six-positional
    /// gradient form: each leg pairs either by inline plot site or by
    /// the run-time plot tag of a variable bound from a `plot()` call.
    /// Raw series legs and wrong arities are static shapes the tree-walk
    /// refuses at run time. Gradient ink resolves first-present at run
    /// time through an explicit `Coalesce`, mirroring the tree-walk's
    /// named-or-top-or-bottom preference including `na` fallthrough.
    fn lower_fill(&mut self, args: &[CallArg]) -> Result<SlotId, FallbackReason> {
        let kind = CaptureKind::Fill;
        let positional_count = args.iter().filter(|arg| arg.name.is_none()).count();
        if positional_count != 2 && positional_count != 4 && positional_count != 6 {
            return Err(FallbackReason::CaptureShape { kind });
        }
        let mut legs: [Option<FillLeg>; 2] = [None, None];
        let mut pos = 0;
        let mut title = None;
        let mut named_color = None;
        let mut ink: [Option<SlotId>; 2] = [None, None];
        for arg in args {
            match arg.name.as_deref() {
                None => {
                    match pos {
                        0 | 1 => match &arg.value {
                            PineExpr::FunctionCall {
                                name, args: inner, ..
                            } if name == "plot" => {
                                let (_, site) =
                                    self.lower_plot_value(inner).map_err(call_arg_ctx)?;
                                legs[pos] = Some(FillLeg::PlotSite(site));
                            }
                            PineExpr::Variable(var) => {
                                legs[pos] = Some(FillLeg::Variable(self.intern(var)));
                            }
                            _ => return Err(FallbackReason::CaptureShape { kind }),
                        },
                        4 | 5 => {
                            ink[pos - 4] =
                                Some(self.lower_expr_slot(&arg.value).map_err(call_arg_ctx)?);
                        }
                        _ => {
                            self.lower_expr_slot(&arg.value).map_err(call_arg_ctx)?;
                        }
                    }
                    pos += 1;
                }
                Some("title") => {
                    if let PineExpr::StringLiteral(text) = &arg.value {
                        title = Some(self.const_value(ConstValue::Str(text.clone())));
                    } else {
                        return Err(FallbackReason::CaptureStatement { kind });
                    }
                }
                Some("color") => {
                    named_color = Some(self.lower_expr_slot(&arg.value).map_err(call_arg_ctx)?);
                }
                Some(_) => {
                    self.lower_expr_slot(&arg.value).map_err(call_arg_ctx)?;
                }
            }
        }
        let site = self.capture_site(kind, args);
        let mut options = Vec::new();
        if let Some(slot) = named_color {
            options.push(slot);
        }
        for slot in ink.into_iter().flatten() {
            options.push(slot);
        }
        let color = match options.len() {
            0 => None,
            1 => Some(options[0]),
            _ => {
                let dst = self.temp();
                self.emit(Instruction::Coalesce { dst, options });
                Some(dst)
            }
        };
        let [first, second] = legs.map(|leg| leg.expect("two legs checked above"));
        self.emit(Instruction::Fill {
            first,
            second,
            title,
            site,
            color,
        });
        Ok(self.temp())
    }

    /// Push a loop level: `break` jumps recorded from here patch to the
    /// loop's end. Levels nest; each loop pops exactly what it pushed.
    fn push_loop(&mut self) {
        self.break_stack.push(Vec::new());
    }

    /// Pop a loop level, patching every recorded jump (exits and `break`s)
    /// to `end`.
    fn pop_loop(&mut self, end: usize) {
        let current = self.current;
        if let Some(level) = self.break_stack.pop() {
            for position in level {
                self.patch(position, end, current);
            }
        }
    }

    /// Lower `break`: a jump to the innermost enclosing loop's end. With
    /// no enclosing loop in the same body, fall back — the tree-walk
    /// refuses instead of unwinding, and call boundaries reset the depth
    /// there exactly as this stack resets here.
    fn lower_break(&mut self) -> Result<(), FallbackReason> {
        if self.break_stack.is_empty() {
            return Err(FallbackReason::Break);
        }
        let jump = self.emit(Instruction::Jump { target: usize::MAX });
        self.break_stack
            .last_mut()
            .expect("level present above")
            .push(jump);
        Ok(())
    }

    /// Lower a bounded `for` with one shared body: the direction flag
    /// picks the ascending or descending bound check once per iteration
    /// (the flag never changes, so the predictor nails it). Duplicating
    /// the body instead would double the fallback count for unlowerable
    /// bodies without changing behavior — measurement must not mistake
    /// code shape for coverage. Both condition exits and every `break`
    /// patch to the loop end.
    fn lower_for(
        &mut self,
        name: &str,
        start: &PineExpr,
        end: &PineExpr,
        step: Option<&PineExpr>,
        body: &[PineStmt],
    ) -> Result<(), FallbackReason> {
        // Bounds evaluate before the counter binds: a use here sees
        // neither the registry (same statement) nor the body stack
        // (pushed below), so a self-referential bound falls back
        // exactly like the tree-walk's refusal.
        let first = self.lower_expr_slot(start)?;
        let last = self.lower_expr_slot(end)?;
        // Stride (`by`): lowers like a bound (defaulting to one) and
        // rides `ForInit`, which validates whole-and-positive at run
        // time with the tree-walk's wording — never spinning.
        let stride = if let Some(step) = step {
            self.lower_expr_slot(step)?
        } else {
            let one = self.temp();
            let id = self.const_value(ConstValue::Int(1));
            self.emit(Instruction::Const { dst: one, value: id });
            one
        };
        let counter = self.temp();
        let step = self.temp();
        let ascending = self.temp();
        let var = self.global(name);
        self.emit(Instruction::ForInit {
            first,
            last,
            counter,
            step,
            ascending,
            stride,
        });
        self.push_loop();
        let head = self.here();
        let dir_jump = self.emit(Instruction::JumpIfFalse {
            cond: ascending,
            target: usize::MAX,
            ctx: CondContext::If,
        });
        let asc_cond = self.temp();
        self.emit(Instruction::BinOp {
            dst: asc_cond,
            op: BinOp::LessEqual,
            lhs: counter,
            rhs: last,
        });
        let asc_done = self.emit(Instruction::JumpIfFalse {
            cond: asc_cond,
            target: usize::MAX,
            ctx: CondContext::If,
        });
        let to_body = self.emit(Instruction::Jump { target: usize::MAX });
        let desc_at = self.here();
        let desc_cond = self.temp();
        self.emit(Instruction::BinOp {
            dst: desc_cond,
            op: BinOp::GreaterEqual,
            lhs: counter,
            rhs: last,
        });
        let desc_done = self.emit(Instruction::JumpIfFalse {
            cond: desc_cond,
            target: usize::MAX,
            ctx: CondContext::If,
        });
        let body_at = self.here();
        if let Some((ordinal, is_varip)) = self.frame_vars.get(name).copied() {
            // Loop counters over `var` names store per-instance,
            // mirroring the tree-walk's scope-walk routing under `assign`.
            let tag = self.intern(name);
            self.emit(Instruction::VarStore {
                ordinal,
                is_varip,
                src: counter,
                name: tag,
                strict: false,
            });
        } else if self.journaled {
            if self.decls.is_persistent(name) {
                return Err(FallbackReason::FunctionBodyBinding {
                    name: self.intern(name),
                });
            }
            self.emit_journaled(name, counter, false);
        } else {
            self.emit(Instruction::GStoreLoop {
                slot: var,
                src: counter,
            });
        }
        // The counter binds before the body runs: uses inside observe a
        // bound cell no matter how conditionally the loop itself runs.
        // (Bounds above lowered before this push, so a self-referential
        // bound still falls back like the tree-walk's refusal.)
        self.loop_targets.push(name.to_owned());
        for stmt in body {
            self.lower_stmt(stmt);
        }
        self.loop_targets.pop();
        self.emit(Instruction::BinOp {
            dst: counter,
            op: BinOp::Add,
            lhs: counter,
            rhs: step,
        });
        self.emit(Instruction::Jump { target: head });
        // Landing pad: exits target a real index — one-past-the-end
        // would trip the linker's jump validation.
        let end_at = self.here();
        self.emit(Instruction::Noop);
        let current = self.current;
        self.patch(dir_jump, desc_at, current);
        self.patch(to_body, body_at, current);
        if let Some(level) = self.break_stack.last_mut() {
            level.extend([asc_done, desc_done]);
        }
        self.pop_loop(end_at);
        Ok(())
    }

    /// Lower `for x in arr` / `for [i, x] in arr`: the iterable must be
    /// an array (the tree-walk's refusals, including `na`), its length
    /// must fit the trip budget, and one value (plus an integer index for
    /// pairs) binds per iteration.
    fn lower_for_in(
        &mut self,
        targets: &[String],
        iterable: &PineExpr,
        body: &[PineStmt],
    ) -> Result<(), FallbackReason> {
        let (index_name, value_name) = match targets {
            [value] => (None, value),
            [index, value] => (Some(index), value),
            _ => return Err(FallbackReason::Loops),
        };
        // Dotted targets bind receiver fields in the tree-walk, not plain
        // globals — keep them on the tree-walk with the loop family.
        if index_name.is_some_and(|name| name.contains('.')) || value_name.contains('.') {
            return Err(FallbackReason::Loops);
        }
        // The iterable evaluates before any target binds (a self-read
        // refuses in the tree-walk); the body sees bound targets.
        let arr = self.lower_expr_slot(iterable)?;
        let len = self.temp();
        self.emit(Instruction::ForInInit { arr, len });
        let idx = self.temp();
        let zero = self.const_value(ConstValue::Int(0));
        self.emit(Instruction::Const {
            dst: idx,
            value: zero,
        });
        let one = self.const_value(ConstValue::Int(1));
        let value_global = self.global(value_name);
        let index_global = index_name.map(|name| self.global(name));
        self.push_loop();
        let head = self.here();
        let cond = self.temp();
        self.emit(Instruction::BinOp {
            dst: cond,
            op: BinOp::Less,
            lhs: idx,
            rhs: len,
        });
        let end_jump = self.emit(Instruction::JumpIfFalse {
            cond,
            target: usize::MAX,
            ctx: CondContext::If,
        });
        if let Some(level) = self.break_stack.last_mut() {
            level.push(end_jump);
        }
        if let Some(index_slot) = index_global {
            let index = index_name.expect("index global implies index name");
            if let Some((ordinal, is_varip)) = self.frame_vars.get(index).copied() {
                let tag = self.intern(index);
                self.emit(Instruction::VarStore {
                    ordinal,
                    is_varip,
                    src: idx,
                    name: tag,
                    strict: false,
                });
            } else if self.journaled {
                if self.decls.is_persistent(index) {
                    return Err(FallbackReason::FunctionBodyBinding {
                        name: self.intern(index),
                    });
                }
                self.emit_journaled(index, idx, false);
            } else {
                self.emit(Instruction::GStoreLoop {
                    slot: index_slot,
                    src: idx,
                });
            }
        }
        let elem = self.temp();
        self.emit(Instruction::ArrayGet {
            dst: elem,
            arr,
            idx,
            hist: None,
        });
        if let Some((ordinal, is_varip)) = self.frame_vars.get(value_name).copied() {
            let tag = self.intern(value_name);
            self.emit(Instruction::VarStore {
                ordinal,
                is_varip,
                src: elem,
                name: tag,
                strict: false,
            });
        } else if self.journaled {
            if self.decls.is_persistent(value_name) {
                return Err(FallbackReason::FunctionBodyBinding {
                    name: self.intern(value_name),
                });
            }
            self.emit_journaled(value_name, elem, false);
        } else {
            self.emit(Instruction::GStoreLoop {
                slot: value_global,
                src: elem,
            });
        }
        if let Some(index) = index_name {
            self.loop_targets.push(index.clone());
        }
        self.loop_targets.push(value_name.clone());
        for stmt in body {
            self.lower_stmt(stmt);
        }
        // Nested loops push and pop symmetrically, so the top of the
        // stack is ours again here: remove exactly what was pushed.
        self.loop_targets.pop();
        if index_name.is_some() {
            self.loop_targets.pop();
        }
        let one_slot = self.temp();
        self.emit(Instruction::Const {
            dst: one_slot,
            value: one,
        });
        self.emit(Instruction::BinOp {
            dst: idx,
            op: BinOp::Add,
            lhs: idx,
            rhs: one_slot,
        });
        self.emit(Instruction::Jump { target: head });
        let end_at = self.here();
        self.emit(Instruction::Noop);
        self.pop_loop(end_at);
        Ok(())
    }

    /// Lower `while cond`: `na` ends the loop, non-bool refuses naming
    /// the condition, the trip budget refuses before it can overrun, and
    /// `break` jumps to the end — each exactly like the tree-walk.
    fn lower_while(
        &mut self,
        condition: &PineExpr,
        body: &[PineStmt],
    ) -> Result<(), FallbackReason> {
        let trip = self.temp();
        let zero = self.const_value(ConstValue::Int(0));
        self.emit(Instruction::Const {
            dst: trip,
            value: zero,
        });
        let head = self.here();
        let cond = self.lower_expr_slot(condition)?;
        let na_jump = self.emit(Instruction::JumpIfNa {
            cond,
            target: usize::MAX,
        });
        let else_jump = self.emit(Instruction::JumpIfFalse {
            cond,
            target: usize::MAX,
            ctx: CondContext::While,
        });
        self.push_loop();
        if let Some(level) = self.break_stack.last_mut() {
            level.push(na_jump);
            level.push(else_jump);
        }
        self.emit(Instruction::WhileTick { trip });
        for stmt in body {
            self.lower_stmt(stmt);
        }
        self.emit(Instruction::Jump { target: head });
        let end_at = self.here();
        self.emit(Instruction::Noop);
        self.pop_loop(end_at);
        Ok(())
    }

    /// Lower one `switch` arm check: the pattern value against the
    /// scrutinee (first-match wins) or its truthiness for the bare form
    /// (non-`bool` refuses naming the pattern, `na` skips the arm).
    /// Records the skip jumps; the caller patches them to the next arm.
    fn lower_switch_check(
        &mut self,
        scrutinee: Option<SlotId>,
        pattern: &PineExpr,
    ) -> Result<Vec<usize>, FallbackReason> {
        let probe = self.lower_expr_slot(pattern)?;
        let mut nexts = Vec::new();
        if let Some(target) = scrutinee {
            let matched = self.temp();
            self.emit(Instruction::SwitchEq {
                dst: matched,
                target,
                probe,
            });
            nexts.push(self.emit(Instruction::JumpIfFalse {
                cond: matched,
                target: usize::MAX,
                ctx: CondContext::If,
            }));
        } else {
            nexts.push(self.emit(Instruction::JumpIfNa {
                cond: probe,
                target: usize::MAX,
            }));
            nexts.push(self.emit(Instruction::JumpIfFalse {
                cond: probe,
                target: usize::MAX,
                ctx: CondContext::SwitchPattern,
            }));
        }
        Ok(nexts)
    }

    /// Lower a statement `switch`: first match wins, the bare-`=>`
    /// default catches the rest, and no match evaluates to absence.
    /// `break` inside an arm targets the enclosing loop — a switch never
    /// consumes it, exactly like the tree-walk.
    fn lower_switch_statement(
        &mut self,
        switch: &crate::pine::syntax::SwitchStmt,
    ) -> Result<(), FallbackReason> {
        let scrutinee = match &switch.scrutinee {
            Some(target) => Some(self.lower_expr_slot(target)?),
            None => None,
        };
        let mut exits = Vec::new();
        for arm in &switch.arms {
            let mut nexts = Vec::new();
            if let Some(pattern) = &arm.pattern {
                nexts = self.lower_switch_check(scrutinee, pattern)?;
            }
            for stmt in &arm.body {
                self.lower_stmt(stmt);
            }
            exits.push(self.emit(Instruction::Jump { target: usize::MAX }));
            let next_at = self.here();
            let current = self.current;
            for pos in nexts {
                self.patch(pos, next_at, current);
            }
        }
        let end_at = self.here();
        self.emit(Instruction::Noop);
        let current = self.current;
        for pos in exits {
            self.patch(pos, end_at, current);
        }
        Ok(())
    }

    /// Lower an expression `switch`: the matched arm's trailing value, or
    /// absence when no arm matches — the unwritten merge reads back as
    /// `na`, exactly like the tree-walk's `Ok(None)`.
    fn lower_switch_value(
        &mut self,
        switch: &crate::pine::syntax::SwitchStmt,
    ) -> Result<SlotId, FallbackReason> {
        let scrutinee = match &switch.scrutinee {
            Some(target) => Some(self.lower_expr_slot(target)?),
            None => None,
        };
        let merge = self.temp();
        let mut exits = Vec::new();
        for arm in &switch.arms {
            let mut nexts = Vec::new();
            if let Some(pattern) = &arm.pattern {
                nexts = self.lower_switch_check(scrutinee, pattern)?;
            }
            let mut trailing = None;
            for stmt in &arm.body {
                if let Some(slot) = self.lower_stmt_value(stmt)? {
                    trailing = Some(slot);
                }
            }
            if let Some(slot) = trailing {
                self.emit(Instruction::Copy {
                    dst: merge,
                    src: slot,
                });
            }
            exits.push(self.emit(Instruction::Jump { target: usize::MAX }));
            let next_at = self.here();
            let current = self.current;
            for pos in nexts {
                self.patch(pos, next_at, current);
            }
        }
        let end_at = self.here();
        self.emit(Instruction::Noop);
        let current = self.current;
        for pos in exits {
            self.patch(pos, end_at, current);
        }
        Ok(merge)
    }

    /// Lower a `plot()` statement: the series plus an optional display
    /// handle are captured; every other argument is evaluated for its
    /// side effects (inputs declare) and dropped, exactly like the
    /// tree-walk. Anything unlowerable falls the statement back.
    fn lower_plot(&mut self, args: &[CallArg]) -> Result<(), FallbackReason> {
        let parts = self.lower_plot_parts(args)?;
        self.emit(Instruction::Plot {
            title: super::bytecode::PlotTitle::Slot(parts.title),
            src: parts.src,
            site: parts.site,
            display: parts.display,
        });
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../tests/pine_bytecode_compile.rs"]
mod compile_tests;

#[cfg(test)]
#[path = "../../tests/pine_bytecode_audit.rs"]
mod audit_tests;
