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
//! (warmup-aware); `var x = na` holds absence until assigned; division by
//! zero refuses instead of erroring late. Unbridged namespaces and unknown
//! functions/methods still refuse with a reason.
//! The per-bar hot path stays on concrete `&[f64]` legs; no `dyn` per bar.

use std::collections::{HashMap, HashSet};

use protopyne_types::Bar;

use super::syntax::{
    BinaryOp, BuiltinVar, CallArg, FieldDecl, Param, PineExpr, PineProgram, PineStmt, SwitchStmt,
    UnaryOp,
};
use crate::context::RunCtx;
use crate::error::KernelError;
use crate::inputs::InputSet;
use crate::runtime::{PineRuntime, PineRuntimeConfig};
use crate::value::{EnumHandle, PineValue, UserInstance, nz};

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
/// kernel states never cross runs), the input registry seeded from the run's
/// inputs (so `input.*` declarations and caller overrides share one store),
/// the v6 declaration tables (enums, consts, types, functions, methods,
/// exports), the owning module of the statements under execution plus every
/// module's alias table (so `ALIAS.fn` binds to the exporting module while
/// bare calls stay module-local), the method-receiver stack, the nested-call
/// site stack (which qualifies stateful bridge sites per call path), per-bar
/// function-local values for history reads, snapshots, plot captures, and
/// the first error.
struct EngineState {
    variables: HashMap<String, Option<PineValue>>,
    history: HashMap<String, Vec<Option<PineValue>>>,
    var_kind: HashMap<String, bool>,
    fixnan_memory: HashMap<String, Option<PineValue>>,
    ta_bridge: Option<super::builtins_ta::TaBridge>,
    core_inputs: InputSet,
    enums: HashMap<String, HashMap<String, Option<PineValue>>>,
    consts: HashSet<String>,
    types: HashMap<String, Vec<FieldDecl>>,
    functions: HashMap<(String, String), (Vec<Param>, Vec<PineStmt>)>,
    methods: HashMap<(String, String), (Vec<Param>, Vec<PineStmt>)>,
    exported: HashSet<String>,
    current_module: String,
    module_aliases: HashMap<(String, String), String>,
    receiver: Vec<UserInstance>,
    call_stack: Vec<String>,
    frame_history: HashMap<String, Option<PineValue>>,
    plots: Vec<PlotCapture>,
    snapshots: Vec<BarSnapshot>,
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
            plots: Vec::new(),
            snapshots: Vec::new(),
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

/// The executor. Owns its runtime config; all per-run state resets on
/// [`run`](Self::run), so one engine serves sequential runs safely.
pub struct PineEngine {
    runtime: PineRuntime,
    state: EngineState,
}

impl PineEngine {
    /// Bind an engine to a runtime config (warmup enforced by the driver).
    #[must_use]
    pub fn new(config: PineRuntimeConfig) -> Self {
        Self {
            runtime: PineRuntime::new(config),
            state: EngineState::fresh(),
        }
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
        self.state = EngineState::fresh();
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
        let legs = Legs::of(bars);
        let PineEngine { runtime, state } = &mut *self;
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
        })
    }

    /// Names bound by `export` in the last run, for import-graph consumers.
    #[must_use]
    pub fn exported_names(&self) -> Vec<String> {
        self.state.exported.iter().cloned().collect()
    }
}

impl EngineState {
    /// Execute one bar across every linked module in order, then record its
    /// snapshot and extend histories. Declaration statements re-run
    /// idempotently (declare-once tables), so dependency modules need no
    /// separate registration pass.
    fn exec_bar(&mut self, modules: &[LinkedModule], ctx: &mut RunCtx<'_>, legs: &Legs) {
        // Function-local values never survive the call, so last bar's frame
        // history must not leak into this bar's extension.
        self.frame_history.clear();
        if self.error.is_none() {
            for module in modules {
                self.current_module.clone_from(&module.key);
                for stmt in &module.program.statements {
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
                self.assign(ctx, name, assigned);
                Ok(())
            }
            PineStmt::Reassign { name, value } => {
                if name == "this" || name.starts_with("this.") {
                    let assigned = self.eval_expr(value, ctx, legs)?;
                    return self.assign_path(ctx, name, assigned);
                }
                if self.var_kind.contains_key(name) || self.variables.contains_key(name) {
                    let assigned = self.eval_expr(value, ctx, legs)?;
                    self.assign(ctx, name, assigned);
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
                // Collection calls in statement position write their result
                // back to a variable target, giving statement calls
                // reference-type behavior (`map.put(m, ...)` mutates the
                // binding); nested calls stay functional.
                if let PineExpr::MethodCall {
                    namespace, args, ..
                } = expr
                {
                    if matches!(namespace.as_str(), "map" | "array") {
                        if let Some(PineExpr::Variable(target)) =
                            args.first().map(|arg| &arg.value)
                        {
                            let written = self.eval_expr(expr, ctx, legs)?;
                            return self.assign_path(ctx, target, written);
                        }
                    }
                }
                self.eval_expr(expr, ctx, legs)?;
                Ok(())
            }
            PineStmt::If {
                condition,
                then_branch,
                else_branch,
            } => {
                // An `na` condition takes no branch (Pine never executes on
                // `na`); numbers still refuse — only `bool` branches.
                let taken = branch_of("if condition", self.eval_expr(condition, ctx, legs)?.as_ref())?
                    .unwrap_or(false);
                let block = if taken {
                    then_branch
                } else {
                    else_branch.as_ref().map_or(&[][..], Vec::as_slice)
                };
                for stmt in block {
                    self.exec_stmt(stmt, ctx, legs);
                    if self.error.is_some() {
                        break;
                    }
                }
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
                let count = i128::from(last) - i128::from(first) + 1;
                if count > MAX_LOOP_ITERATIONS as i128 {
                    return Err(refused(format!(
                        "'for' range {first}..={last} exceeds {MAX_LOOP_ITERATIONS} iterations"
                    )));
                }
                for i in first..=last {
                    self.assign(ctx, variable, Some(PineValue::Int(i)));
                    for stmt in body {
                        self.exec_stmt(stmt, ctx, legs);
                        if self.error.is_some() {
                            break;
                        }
                    }
                    if self.error.is_some() {
                        break;
                    }
                }
                Ok(())
            }
            PineStmt::While { condition, body } => {
                let mut iterations: usize = 0;
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
                        return Err(refused(format!(
                            "'while' exceeded {MAX_LOOP_ITERATIONS} iterations"
                        )));
                    }
                    iterations += 1;
                    for stmt in body {
                        self.exec_stmt(stmt, ctx, legs);
                        if self.error.is_some() {
                            break;
                        }
                    }
                    if self.error.is_some() {
                        break;
                    }
                }
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
                self.functions.insert(
                    (self.current_module.clone(), name.clone()),
                    (params.clone(), body.clone()),
                );
                Ok(())
            }
            PineStmt::MethodDecl { name, params, body } => {
                let receiver_type = params.first().and_then(|param| {
                    param.type_name.as_ref().map(|decl| decl.name.clone())
                });
                match receiver_type {
                    Some(type_name) => {
                        self.methods.insert(
                            (type_name, name.clone()),
                            (params.clone(), body.clone()),
                        );
                        Ok(())
                    }
                    None => Err(refused(format!(
                        "method '{name}' needs a typed receiver ('Type this')"
                    ))),
                }
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
                self.types.insert(name.clone(), fields.clone());
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
    /// it (the initialization mark stays, so it is not re-seeded).
    fn assign(&mut self, ctx: &mut RunCtx<'_>, name: &str, value: Option<PineValue>) {
        match self.var_kind.get(name) {
            Some(&is_varip) => match value {
                Some(present) => {
                    if is_varip {
                        ctx.vars_mut().set_varip(name, present);
                    } else {
                        ctx.vars_mut().set_var(name, present);
                    }
                }
                None => {
                    if is_varip {
                        ctx.vars_mut().clear_varip(name);
                    } else {
                        ctx.vars_mut().clear_var(name);
                    }
                }
            },
            None => {
                self.variables.insert(name.to_owned(), value);
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
            let receiver = self
                .receiver
                .last_mut()
                .ok_or_else(|| refused("'this' is only bound inside a method"))?;
            if !receiver.fields.contains_key(field) {
                return Err(refused(format!("unknown field 'this.{field}'")));
            }
            receiver.fields.insert(field.to_owned(), value);
            return Ok(());
        }
        if name == "this" {
            return Err(refused("cannot rebind 'this'"));
        }
        self.assign(ctx, name, value);
        Ok(())
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
            let receiver = self
                .receiver
                .last_mut()
                .ok_or_else(|| refused("'this' is only bound inside a method"))?;
            let slot = receiver
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
                    Ok(Some((instance, ObjectHome::Variables(name.to_owned()))))
                }
                other => {
                    self.variables.insert(name.to_owned(), other);
                    Ok(None)
                }
            };
        }
        if let Some(PineValue::Object(instance)) = ctx.vars().get_var(name) {
            ctx.vars_mut().clear_var(name);
            return Ok(Some((instance, ObjectHome::VarStore(name.to_owned(), false))));
        }
        if let Some(PineValue::Object(instance)) = ctx.vars().get_varip(name) {
            ctx.vars_mut().clear_varip(name);
            return Ok(Some((instance, ObjectHome::VarStore(name.to_owned(), true))));
        }
        Ok(None)
    }

    /// Write a (possibly mutated) instance back to the home it came from.
    fn put_object(
        &mut self,
        ctx: &mut RunCtx<'_>,
        home: ObjectHome,
        instance: UserInstance,
    ) -> Result<(), KernelError> {
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
                let receiver = self
                    .receiver
                    .last_mut()
                    .ok_or_else(|| refused("'this' is only bound inside a method"))?;
                receiver.fields.insert(field, Some(value));
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
                .map(|instance| {
                    Some(PineValue::Object(instance.clone()))
                })
                .ok_or_else(|| refused("'this' is only bound inside a method"));
        }
        if let Some(field) = name.strip_prefix("this.") {
            let receiver = self
                .receiver
                .last()
                .ok_or_else(|| refused("'this' is only bound inside a method"))?;
            return match receiver.fields.get(field) {
                Some(bound) => Ok(bound.clone()),
                None => Err(refused(format!("unknown field 'this.{field}'"))),
            };
        }
        if name == "bar_index" {
            let index = i64::try_from(ctx.cursor())
                .map_err(|_| refused("bar index does not fit this run"))?;
            return Ok(Some(PineValue::Int(index)));
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
        if name == "na" {
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
        if bound.is_none() && !self.history.contains_key(name) {
            return Err(refused(format!("undefined variable '{name}'")));
        }
        if back == 0 {
            return Ok(bound.flatten());
        }
        let position = self.snapshots.len().checked_sub(back);
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
            "plot" => {
                if values.is_empty() {
                    return Err(refused("'plot' takes a series to draw"));
                }
                let site = format!("plot@{args:?}");
                let series = values[0].clone();
                let title = args
                    .iter()
                    .zip(values.iter())
                    .find(|(arg, _)| arg.name.as_deref() == Some("title"))
                    .and_then(|(_, value)| match value {
                        Some(PineValue::Str(text)) => Some(text.clone()),
                        _ => None,
                    })
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
            _ if self
                .functions
                .contains_key(&(self.current_module.clone(), name.to_owned())) =>
            {
                let key = (self.current_module.clone(), name.to_owned());
                let (params, body) = self
                    .functions
                    .get(&key)
                    .cloned()
                    .expect("function checked above");
                let site = format!("{}::{name}@{args:?}", self.current_module);
                let module = self.current_module.clone();
                self.exec_call(site, &module, &params, args, &values, &body, ctx, legs)
            }
            _ => Err(refused(format!("unknown function '{name}'"))),
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
        if namespace != "ta"
            && !matches!(namespace, "math" | "str" | "input" | "array" | "map" | "color" | "timeframe")
        {
            if namespace == "this"
                || (method == "new" && self.types.contains_key(namespace))
            {
                return self.call_instance_method(namespace, method, args, ctx, legs);
            }
            // Instance-held receivers (`sched.register(...)`) dispatch to
            // their type's methods; anything else falls through to the
            // import-alias dispatch below.
            let held = self.peek_object(ctx, namespace);
            if held {
                return self.call_instance_method(namespace, method, args, ctx, legs);
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
                if let Some((params, body)) = self.functions.get(&key).cloned() {
                    let mut values = Vec::with_capacity(args.len());
                    for arg in args {
                        values.push(self.eval_expr(&arg.value, ctx, legs)?);
                    }
                    let site = format!("{target}::{method}@{args:?}");
                    return self.exec_call(site, &target, &params, args, &values, &body, ctx, legs);
                }
                return Err(refused(format!(
                    "unknown function '{namespace}.{method}'"
                )));
            }
            return Err(refused(format!("unknown method '{namespace}.{method}'")));
        }
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.eval_expr(&arg.value, ctx, legs)?);
        }
        if namespace == "ta" {
            if self.ta_bridge.is_none() {
                match super::builtins_ta::TaBridge::new() {
                    Ok(bridge) => self.ta_bridge = Some(bridge),
                    Err(error) => {
                        return Err(refused(format!("ta bridge unavailable: {error}")));
                    }
                }
            }
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

    /// Whether `name` currently holds a user-type instance (series variable,
    /// persistent binding, or receiver field), without moving it.
    fn peek_object(&self, ctx: &RunCtx<'_>, name: &str) -> bool {
        if let Some(field) = name.strip_prefix("this.") {
            return self
                .receiver
                .last()
                .and_then(|receiver| receiver.fields.get(field))
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
            let receiver = self
                .receiver
                .pop()
                .ok_or_else(|| refused("'this' is only bound inside a method"))?;
            let (back, value) = self.run_method(receiver, method, args, ctx, legs)?;
            self.receiver.push(back);
            return value;
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
        match self.take_object(ctx, namespace)? {
            Some((instance, home)) => {
                let (back, value) = self.run_method(instance, method, args, ctx, legs)?;
                self.put_object(ctx, home, back)?;
                value
            }
            None => Err(refused(format!(
                "unknown method '{namespace}.{method}'"
            ))),
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

    /// Run one registered method body against a receiver: the receiver binds
    /// as `this`, remaining parameters bind positionally/named (defaults
    /// fill gaps), and the body's trailing value returns. The receiver —
    /// mutated or not — returns alongside for the caller to store back.
    fn run_method(
        &mut self,
        receiver: UserInstance,
        method: &str,
        args: &[CallArg],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<(UserInstance, Result<Option<PineValue>, KernelError>), KernelError> {
        let key = (receiver.type_name.clone(), method.to_owned());
        let Some((params, body)) = self.methods.get(&key).cloned() else {
            return Err(refused(format!(
                "unknown method '{}.{method}'",
                receiver.type_name
            )));
        };
        let Some(head) = params.first() else {
            return Err(refused(format!("method '{method}' needs a receiver")));
        };
        if let Some(decl) = head.type_name.as_ref() {
            if decl.name != receiver.type_name {
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
        let site = format!("{}.{method}@{args:?}", receiver.type_name);
        self.receiver.push(receiver);
        let module = self.current_module.clone();
        let value = self.exec_call(site, &module, &params[1..], args, &values, &body, ctx, legs);
        let back = self
            .receiver
            .pop()
            .expect("receiver pushed above");
        Ok((back, value))
    }

    /// Run one user body with bound parameters: parameters bind into a fresh
    /// frame over the caller's namespace, defaults evaluate in the caller,
    /// and the body's trailing value returns. The body executes under
    /// `module`, so its bare calls resolve to the owning module's helpers;
    /// the caller's module restores on return. Locals never leak — the frame
    /// restores on return — but per-bar local values feed history reads
    /// through the frame table, so series offsets inside helpers observe
    /// prior bars exactly like top-level series do.
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
        let mut bindings: Vec<(String, Option<PineValue>)> = Vec::with_capacity(params.len());
        let mut positional = call_args
            .iter()
            .zip(values.iter())
            .filter(|(arg, _)| arg.name.is_none())
            .map(|(_, value)| value.clone());
        for param in params {
            if let Some(hit) = call_args
                .iter()
                .zip(values.iter())
                .find(|(arg, _)| arg.name.as_deref() == Some(param.name.as_str()))
            {
                bindings.push((param.name.clone(), hit.1.clone()));
            } else if let Some(next) = positional.next() {
                bindings.push((param.name.clone(), next));
            } else if let Some(default) = param.default.as_ref() {
                bindings.push((param.name.clone(), self.eval_expr(default, ctx, legs)?));
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
        let snapshot = self.variables.clone();
        for (name, value) in bindings {
            self.assign(ctx, &name, value);
        }
        let caller = std::mem::replace(&mut self.current_module, module.to_owned());
        self.call_stack.push(site);
        let value = self.exec_body_value(body, ctx, legs);
        self.call_stack.pop();
        self.current_module = caller;
        for (name, current) in &self.variables {
            if snapshot.get(name) != Some(current) {
                self.frame_history.insert(name.clone(), current.clone());
            }
        }
        self.variables = snapshot;
        value
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
            if self.error.is_some() {
                break;
            }
        }
        self.check_slot()?;
        Ok(value)
    }

    /// Execute one body statement for its value: bare expressions (and
    /// nested switches) yield; everything else runs for effect and yields
    /// nothing — an `if` body never becomes a value, matching the language.
    fn exec_stmt_value(
        &mut self,
        stmt: &PineStmt,
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
    ) -> Result<Option<PineValue>, KernelError> {
        let value = match stmt {
            PineStmt::Expression(expr) => self.eval_expr(expr, ctx, legs)?,
            PineStmt::Switch(switch) => self.eval_switch(switch, ctx, legs, true)?,
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
        let scrutinee = self.eval_expr(&switch.scrutinee, ctx, legs)?;
        for arm in &switch.arms {
            let matched = match &arm.pattern {
                None => true,
                Some(pattern) => {
                    let probe = self.eval_expr(pattern, ctx, legs)?;
                    switch_eq(scrutinee.as_ref(), probe.as_ref())
                }
            };
            if matched {
                if want_value {
                    return self.exec_body_value(&arm.body, ctx, legs);
                }
                for stmt in &arm.body {
                    self.exec_stmt(stmt, ctx, legs);
                    if self.error.is_some() {
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

/// Where a method-call receiver lives while the call runs, so the
/// (possibly mutated) instance returns to the same home afterwards.
#[derive(Debug)]
enum ObjectHome {
    /// Series variable in engine state.
    Variables(String),
    /// Persistent `var` (`false`) or `varip` (`true`) binding.
    VarStore(String, bool),
    /// Field of the innermost receiver on the stack.
    ThisField(String),
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

/// Binary evaluation: `na` propagates through every operator; mixed kinds
/// outside the reference table refuse instead of coercing.
#[allow(clippy::too_many_lines)]
fn binary(
    op: BinaryOp,
    left: Option<&PineValue>,
    right: Option<&PineValue>,
) -> Result<Option<PineValue>, KernelError> {
    if left.is_none() || right.is_none() {
        // Three-valued connectives decide on one absent side; every other
        // operator propagates `na`.
        return match op {
            BinaryOp::And | BinaryOp::Or => kleene_absent(op, left, right),
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
        BinaryOp::Divide => {
            if b == 0.0 {
                return Err(refused("division by zero"));
            }
            PineValue::Float(a / b)
        }
        BinaryOp::Modulo => {
            if b == 0.0 {
                return Err(refused("division by zero"));
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
                return Err(refused("division by zero"));
            }
            PineValue::Float(a as f64 / b as f64)
        }
        BinaryOp::Modulo => {
            if b == 0 {
                return Err(refused("division by zero"));
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
