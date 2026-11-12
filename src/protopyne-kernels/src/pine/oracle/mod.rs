//! Tree-walk interpreter over the runtime driver.
//!
//! [`EngineState`] re-walks the parsed AST on every bar: hash-map name
//! resolution, string-built call-site keys, a merged snapshot per bar. It is
//! the correctness oracle the differential suites compare against.

use std::collections::{HashMap, HashSet};

use super::builtins_state::RunBuiltins;
use super::home::HomeSlot;

use protopyne_types::Bar;
use protopyne_types::Timeframe;

use super::engine::{
    module_key, BarColorCapture, BarSnapshot, FillCapture, HlineCapture, LinkedModule,
    PlotCandleCapture, PlotCapture, PlotShapeCapture, MAX_CALL_DEPTH, MAX_LOOP_ITERATIONS,
};
use super::semantics::{array_index_out_of_bounds, array_negative_index, barstate_flag, binary,
    branch_of, builtin_name, call_depth_over_limit, call_wall_part, cannot_rebind_this,
    capture_fallback_title, chart_ms, destructure_arity_mismatch, destructure_non_array,
    field_needs_object, field_read_on_na, fill_plot_site_without_capture,
    fill_variable_without_plot_tag, float_cast, for_in_absent, for_in_not_array, for_in_over_limit,
    for_range_over_limit, for_step_non_positive, history_negative_offset, int_cast, is_absent, is_hidden_series,
    leg_or_nan, method_arg_missing, method_called_on_na, method_needs_object,
    method_too_many_positionals, method_unknown_argument, read_leg, read_leg_slice,
    reassign_undeclared, refused, security_column, security_leg, serve_security, session_time_of,
    switch_eq, this_unbound, trailing_type, unary, undefined_variable, unknown_dotted_field,
    unknown_field_on, unknown_function, unknown_method_on, unknown_this_field, while_over_limit,
    whole_number, writes_back_collection, Legs, SecurityTail, INPUT_MISSING_DEFAULT_MESSAGE,
    LIVE_REQUEST_TAIL, NA_ARITY_MESSAGE, NZ_ARITY_MESSAGE, PLOT_MISSING_SERIES_MESSAGE,
};

use super::syntax::{
    site_string, AssignOp, CallArg, FieldDecl, Param, PineExpr, PineStmt, SwitchStmt,
};
use crate::context::RunCtx;
use crate::error::KernelError;
use crate::market_context::MarketContextKey;
use crate::mtf::MtfStore;
use crate::native_libs::{
    BarContextServe, FetchCounters, LeaseKey, LeasedRing, MethodCtx, NativeOptions, TickLegs,
};
use crate::value::{
    nz, ColorHandle, DrawingKind, EnumHandle, FieldTable, NativeCall, PineValue, UserInstance,
};
use crate::vars::{CallSite, ModuleKey, NodeId, ScopeKey};

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
pub(crate) type MethodDef = (String, Vec<Param>, std::sync::Arc<Vec<PineStmt>>);

/// One standalone-function overload: params and body. Same-named functions
/// stack in source order; dispatch picks the first arity-compatible
/// overload whose declared parameter types match the call values. Bodies
/// ride `Arc` so per-call dispatch clones a pointer, never the AST.
pub(crate) type FuncDef = (Vec<Param>, std::sync::Arc<Vec<PineStmt>>);

/// One spelling in one body instance: the module whose body declares it, the
/// call expressions enclosing that body, and the declared name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// One spelling in one body instance: the module whose body declares it, the
/// call expressions enclosing that body, and the declared name.
pub(crate) struct VarScope {
    module: Option<ModuleKey>,
    call_sites: Vec<CallSite>,
    name: String,
}

/// Where a `var`/`varip` spelling is bound here: the declaration node that
/// owns its cell, and the lifetime flag.
#[derive(Debug, Clone, Copy)]
/// Where a `var`/`varip` spelling is bound here: the declaration node that
/// owns its cell, and the lifetime flag.
pub(crate) struct VarBinding {
    decl: NodeId,
    is_varip: bool,
}

/// site stack (which qualifies stateful bridge sites and persistent
/// declarations per call path), per-bar
/// function-local values for history reads, snapshots, plot captures, and
/// the first error.
pub(crate) struct EngineState {
    pub(crate) variables: HashMap<String, Option<PineValue>>,
    pub(crate) history: HashMap<String, Vec<Option<PineValue>>>,
    /// Persistent `var`/`varip` declarations by the body instance that
    /// declares them: reads and writes resolve a spelling to the innermost
    /// declaring body, so two declarations never share a cell.
    pub(crate) var_decls: HashMap<VarScope, VarBinding>,
    /// Call expressions enclosing the statement under execution, outermost
    /// first: the body instance a declaration belongs to.
    pub(crate) sites: Vec<CallSite>,

    pub(crate) chart_symbol: String,
    pub(crate) chart_period: String,
    pub(crate) enums: HashMap<String, HashMap<String, Option<PineValue>>>,
    pub(crate) consts: HashSet<String>,
    pub(crate) types: HashMap<String, Vec<FieldDecl>>,
    /// Standalone functions by `(owning module, name)`: each name holds
    /// its overloads in source order for signature dispatch.
    pub(crate) functions: HashMap<(String, String), Vec<FuncDef>>,
    /// Methods by `(receiver type, method)`: the owning module travels with
    /// the body (see [`MethodDef`]).
    pub(crate) methods: HashMap<(String, String), MethodDef>,
    pub(crate) exported: HashSet<String>,
    pub(crate) current_module: String,
    pub(crate) module_aliases: HashMap<(String, String), String>,
    pub(crate) receiver: Vec<ReceiverSlot>,
    pub(crate) call_stack: Vec<String>,
    /// Per-bar values of every persistent cell, keyed by the declaration
    /// instance that owns it: `x[1]` inside one call instance reads that
    /// instance's past, never a same-named cell's.
    pub(crate) var_history: HashMap<ScopeKey, Vec<Option<PineValue>>>,
    /// Per-bar function-local values for history reads, keyed by call site
    /// (`site#name`) inside a call and by bare name at the top level, so a
    /// helper's locals never read the caller's past for a same-named
    /// binding (and vice versa).
    pub(crate) frame_history: HashMap<String, Option<PineValue>>,
    /// Loop nesting depth for `break`: incremented by `for`/`while`, reset
    /// across function/method call boundaries so a `break` binds lexically.
    pub(crate) loop_depth: usize,
    /// Set by `break`, consumed by the innermost enclosing loop; statement
    /// lists stop through [`stopped`](Self::stopped) while it is set.
    pub(crate) break_flag: bool,
    /// Per-update tick timestamps (epoch milliseconds), parallel to the
    /// run's bars. Non-empty selects tick/realtime emulation: `timenow`
    /// reads these instead of the `time` leg, and the cursor flags follow
    /// bar-time changes rather than positions.
    pub(crate) tick_times: Vec<i64>,
    /// Native-execution switch copied from the engine at run start (default
    /// native where providers exist): consulted once per instance-method
    /// call, never mutated mid-run.
    pub(crate) native: NativeOptions,
    /// Ring-lease versions per leased object slot: bumped on every
    /// Pine-side write to the slot (the single bump rule — see
    /// [`EngineState::bump_lease`]), so a cached resident ring whose
    /// version skews re-hydrates instead of serving stale state.
    pub(crate) lease_versions: HashMap<LeaseKey, u64>,
    /// Cached resident rings per leased slot with the table version they
    /// were synced at. A version match serves with zero hydrate; any other
    /// outcome drops the entry.
    pub(crate) leased_rings: HashMap<LeaseKey, LeasedRing>,
    /// Commit-vs-fallback telemetry for the leased `fetch` seam, reset per
    /// run with the rest of the state. Read through
    /// [`PineEngine::fetch_counters`]; observed, never asserted.
    pub(crate) fetch_counters: FetchCounters,
    /// Chart bars of the current run, cloned only when the run links the
    /// context library (the provider's resident tape — per-run state reset
    /// by [`EngineState::fresh`], never shared across runs).
    pub(crate) run_bars: Vec<Bar>,
    /// Resident shared store behind natively served context reads: fed
    /// incrementally with chart bars through the cursor, so steady-state
    /// reads extend the tail instead of rebuilding history.
    pub(crate) apex_store: MtfStore,
    /// Chart bars loaded into `apex_store`.
    pub(crate) apex_loaded: usize,
    /// Unconditional top-level declaration sites pre-registered before the
    /// first bar, as `(module index, statement index)`: the bar loop skips
    /// them, since re-execution would only re-collapse identical shapes.
    pub(crate) decl_skip: HashSet<(usize, usize)>,
    /// Plain frame writes journaled per call as `(name, entry value)`
    /// with absence for unbound names: the teardown reverts exactly what
    /// the body touched, without cloning the whole frame up front.
    /// The outer `None` means unbound, distinct from bound-to-`na` —
    /// hence the nested option.
    #[allow(clippy::option_option)]
    pub(crate) journal: Vec<(String, Option<Option<PineValue>>)>,
    /// Journal suffix owned by the innermost call (`None` outside calls):
    /// only writes at or after the base journal, so nested calls revert
    /// their own suffix and leave the caller's entries alone.
    pub(crate) journal_base: Option<usize>,
    /// Suppress journaling while parameter binding and write-backs run
    /// (those restore through the shadow list and caller homes instead).
    pub(crate) journal_hold: bool,
    /// Variable-to-map-entry provenance for `map.get` handles: `var ->
    /// (map variable, key encoding, key value)`. A method call through a
    /// tagged variable writes the mutated instance back to the map entry
    /// as well as the variable, giving Pine reference behavior. Rebinding
    /// the variable from anything but `map.get` clears its tag.
    pub(crate) map_prov: HashMap<String, (String, String, PineValue)>,
    /// Variable-to-plot-site tags: `var -> plot@args` for variables bound
    /// from a `plot()` call, so `fill(plot_var, ...)` pairs the same sites
    /// as the inline form. `plot()` itself evaluates to absence (its value
    /// is captured, not returned), so the tag is the only pairing record.
    /// Rebinding from anything but `plot()` clears the tag.
    pub(crate) plot_vars: HashMap<String, String>,
    pub(crate) plots: Vec<PlotCapture>,
    pub(crate) snapshots: Vec<BarSnapshot>,
    pub(crate) plotcandles: Vec<PlotCandleCapture>,
    pub(crate) barcolors: Vec<BarColorCapture>,
    pub(crate) fills: Vec<FillCapture>,
    pub(crate) plotshapes: Vec<PlotShapeCapture>,
    pub(crate) hlines: Vec<HlineCapture>,
    pub(crate) error: Option<KernelError>,
}

impl EngineState {
    pub(crate) fn fresh() -> Self {
        Self {
            variables: HashMap::new(),
            history: HashMap::new(),
            var_decls: HashMap::new(),
            var_history: HashMap::new(),
            sites: Vec::new(),
            chart_symbol: crate::runtime::default_chart_symbol(),
            chart_period: crate::runtime::default_chart_period(),
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
            native: NativeOptions {
                enabled: true,
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
            error: None,
        }
    }
}

impl EngineState {
    /// Tick/realtime emulation is on when the run carries per-update tick
    /// timestamps: cursor flags follow bar-time changes and `timenow`
    /// reads tick times instead of the `time` leg.
    pub(crate) fn tick_mode(&self) -> bool {
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
        if !slot.iter().any(|(known_params, known_body)| {
            known_params == params && known_body.as_slice() == body
        }) {
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
    pub(crate) fn predeclare(
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
                    skip.insert((position, index));
                }
                // Export names are static: record them up front so the
                // compiled path reports the same set without executing
                // the declaration statements. Re-insertion during the
                // bar loop is idempotent, so tree runs observe nothing.
                if was_export {
                    if let Some(name) = export_label(decl) {
                        self.exported.insert(name);
                    }
                }
            }
        }
        Ok(skip)
    }

    /// Execute one bar across every linked module in order, then record its
    /// snapshot and extend histories. Declaration statements re-run
    /// idempotently (declare-once tables), so dependency modules need no
    /// separate registration pass.
    pub(crate) fn exec_bar(
        &mut self,
        modules: &[LinkedModule],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,

        builtins: &mut RunBuiltins,
    ) {
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
                    self.exec_stmt(stmt, ctx, legs, builtins);
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
        // Persistent cells also advance a series of their own, so a `var`
        // read with an offset inside one call instance observes that
        // instance's past even when another declaration spells the name the
        // same way.
        for (scope, value) in ctx.vars().occupied() {
            let slot = self.var_history.entry(scope).or_default();
            while slot.len() < completed {
                slot.push(None);
            }
            slot.push(value);
        }
        self.snapshots.push(BarSnapshot {
            bar: ctx.cursor(),
            values: merged
                .into_iter()
                .filter(|(name, _)| !is_hidden_series(name))
                .collect(),
        });
    }

    /// Execute one statement; the first refusal sticks and halts the run.
    pub(crate) fn exec_stmt(
        &mut self,
        stmt: &PineStmt,
        ctx: &mut RunCtx<'_>,
        legs: &Legs,

        builtins: &mut RunBuiltins,
    ) {
        if self.error.is_some() {
            return;
        }
        let result = self.exec_stmt_inner(stmt, ctx, legs, builtins);
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

        builtins: &mut RunBuiltins,
    ) -> bool {
        self.assign(ctx, variable, Some(PineValue::Int(at)));
        for stmt in body {
            self.exec_stmt(stmt, ctx, legs, builtins);
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

        builtins: &mut RunBuiltins,
    ) -> Result<(), KernelError> {
        match stmt {
            PineStmt::VarDeclaration {
                id,
                name,
                value,
                is_varip,
                ..
            } => {
                // Initializer runs exactly once per run, on the first
                // evaluated bar — warmup bars never seed it. The
                // declaration's own body instance is the scope: a second
                // declaration of the same spelling elsewhere is a second
                // cell with its own initializer.
                let module = self.scope_module();
                let call_sites = self.sites.clone();
                let key = VarScope {
                    module: module.clone(),
                    call_sites: call_sites.clone(),
                    name: name.clone(),
                };
                if !self.var_decls.contains_key(&key) {
                    let init = self.eval_expr(value, ctx, legs, builtins)?;
                    self.var_decls.insert(
                        key,
                        VarBinding {
                            decl: *id,
                            is_varip: *is_varip,
                        },
                    );
                    if let Some(present) = init {
                        let scope = ScopeKey::new(module, call_sites, *id);
                        // A fresh object at this home invalidates any lease
                        // the slot may hold (defensive: tables start empty
                        // per run, but declarations also evaluate in the
                        // predeclare pass).
                        if matches!(present, PineValue::Object(_)) {
                            self.bump_lease_key(LeaseKey::var_store(scope.clone()));
                        }
                        if *is_varip {
                            ctx.vars_mut().set_varip(scope, name, present);
                        } else {
                            ctx.vars_mut().set_var(scope, name, present);
                        }
                    }
                }
                Ok(())
            }
            PineStmt::Declaration { name, value, .. } | PineStmt::Assignment { name, value } => {
                let assigned = self.eval_expr(value, ctx, legs, builtins)?;
                self.assign(ctx, name, assigned.clone());
                self.refresh_prov(ctx, legs, name, value, assigned.as_ref(), builtins)?;
                self.refresh_plot_var(name, value);
                Ok(())
            }
            PineStmt::MultiDeclaration { decls } => {
                for decl in decls {
                    let assigned = match &decl.value {
                        Some(value) => self.eval_expr(value, ctx, legs, builtins)?,
                        None => None,
                    };
                    self.assign(ctx, &decl.name, assigned.clone());
                    if let Some(value) = &decl.value {
                        self.refresh_prov(
                            ctx,
                            legs,
                            &decl.name,
                            value,
                            assigned.as_ref(),
                            builtins,
                        )?;
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
                    self.exec_stmt(&single, ctx, legs, builtins);
                    if self.error.is_some() {
                        break;
                    }
                }
                Ok(())
            }
            PineStmt::Reassign { name, value } => {
                if name == "this" || name.starts_with("this.") {
                    let assigned = self.eval_expr(value, ctx, legs, builtins)?;
                    return self.assign_path(ctx, name, assigned, builtins);
                }
                // Receiver-alias field writes (`level.aged := 0`, and the
                // `+=` family desugared onto them) hit the live slot; the
                // right-hand side already reads through the same alias.
                if let Some((head, field)) = name.split_once('.') {
                    if !field.contains('.') && self.receiver_slot(head).is_some() {
                        let assigned = self.eval_expr(value, ctx, legs, builtins)?;
                        if self.write_alias_field(head, field, assigned) {
                            return Ok(());
                        }
                        return Err(unknown_dotted_field(head, field));
                    }
                    // Variable-held instance field writes (`context.regime
                    // := ...`): the instance takes out of its home, gains
                    // the field, and writes back — the same take/put
                    // discipline as method receivers. Unknown fields
                    // refuse after restoring the instance.
                    if !field.contains('.') && !head.is_empty() {
                        let assigned = self.eval_expr(value, ctx, legs, builtins)?;
                        if let Some((mut instance, home)) = self.take_object(ctx, head)? {
                            if !instance.fields.contains_key(field) {
                                self.put_object(ctx, &home, instance, false)?;
                                return Err(unknown_dotted_field(head, field));
                            }
                            instance.fields.insert(field.to_owned(), assigned);
                            self.put_object(ctx, &home, instance, true)?;
                            return Ok(());
                        }
                    }
                }
                if self.var_binding(name).is_some() || self.variables.contains_key(name) {
                    let assigned = self.eval_expr(value, ctx, legs, builtins)?;
                    self.assign(ctx, name, assigned.clone());
                    self.refresh_prov(ctx, legs, name, value, assigned.as_ref(), builtins)?;
                    self.refresh_plot_var(name, value);
                    Ok(())
                } else {
                    Err(reassign_undeclared(name))
                }
            }
            PineStmt::DestructureAssign { targets, value } => {
                let assigned = self.eval_expr(value, ctx, legs, builtins)?;
                match assigned {
                    Some(PineValue::Array(items)) => {
                        if items.len() != targets.len() {
                            return Err(destructure_arity_mismatch(items.len(), targets.len()));
                        }
                        for (name, slot) in targets.iter().zip(items) {
                            self.assign_path(ctx, name, slot, builtins)?;
                        }
                        Ok(())
                    }
                    other => Err(destructure_non_array(other.as_ref())),
                }
            }
            PineStmt::Expression(expr) => {
                self.exec_expr_stmt(expr, ctx, legs, builtins)?;
                Ok(())
            }
            PineStmt::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let block = self.taken_block(
                    condition,
                    then_branch,
                    else_branch.as_deref(),
                    ctx,
                    legs,
                    builtins,
                )?;
                for stmt in block {
                    self.exec_stmt(stmt, ctx, legs, builtins);
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
                step,
                body,
            } => {
                let first = whole_number(
                    "for start",
                    self.eval_expr(start, ctx, legs, builtins)?.as_ref(),
                )?;
                let last = whole_number(
                    "for end",
                    self.eval_expr(end, ctx, legs, builtins)?.as_ref(),
                )?;
                let stride = match step {
                    Some(step) => {
                        let value = whole_number(
                            "for step",
                            self.eval_expr(step, ctx, legs, builtins)?.as_ref(),
                        )?;
                        if value <= 0 {
                            return Err(for_step_non_positive(value));
                        }
                        value
                    }
                    None => 1,
                };
                // The range limit counts strided visits, exactly like the
                // compiled loop: unit strides keep the historical count.
                let count =
                    (i128::from(last) - i128::from(first)).abs() / i128::from(stride) + 1;
                if count > MAX_LOOP_ITERATIONS as i128 {
                    return Err(for_range_over_limit(first, last));
                }
                self.loop_depth += 1;
                // The step adapts to the direction: a start past the end
                // counts down, so reverse scans (`size - 1 to 0`) iterate
                // instead of silently running zero times. The stride
                // multiplies the unit direction; bounds checks run on the
                // strided counter exactly like the compiled loop's.
                // `i128` counters cannot overflow past the checked range.
                if first <= last {
                    let mut at = i128::from(first);
                    while at <= i128::from(last) {
                        // Exact: the counter never leaves the i64 range
                        // spanned by the checked endpoints.
                        #[allow(clippy::cast_possible_wrap)]
                        let bounded = at as i64;
                        if self.for_step(variable, bounded, body, ctx, legs, builtins) {
                            break;
                        }
                        at += i128::from(stride);
                    }
                } else {
                    let mut at = i128::from(first);
                    while at >= i128::from(last) {
                        // Exact: the counter never leaves the i64 range
                        // spanned by the checked endpoints.
                        #[allow(clippy::cast_possible_wrap)]
                        let bounded = at as i64;
                        if self.for_step(variable, bounded, body, ctx, legs, builtins) {
                            break;
                        }
                        at -= i128::from(stride);
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
                let collection = self.eval_expr(iterable, ctx, legs, builtins)?;
                let items = match collection {
                    Some(PineValue::Array(items)) => items,
                    Some(other) => {
                        return Err(for_in_not_array(&other));
                    }
                    None => {
                        return Err(for_in_absent());
                    }
                };
                if items.len() > MAX_LOOP_ITERATIONS {
                    return Err(for_in_over_limit(items.len()));
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
                        self.exec_stmt(stmt, ctx, legs, builtins);
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
                    let keep = branch_of(
                        "while condition",
                        self.eval_expr(condition, ctx, legs, builtins)?.as_ref(),
                    )?
                    .unwrap_or(false);
                    if !keep {
                        break;
                    }
                    if iterations >= MAX_LOOP_ITERATIONS {
                        self.loop_depth -= 1;
                        self.break_flag = false;
                        return Err(while_over_limit());
                    }
                    iterations += 1;
                    for stmt in body {
                        self.exec_stmt(stmt, ctx, legs, builtins);
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
            PineStmt::Import(_) | PineStmt::LibraryDecl { .. } | PineStmt::IndicatorDecl { .. } => {
                // Headers declare metadata for the loader and the import
                // graph; the executor ignores them. A linked run carries the
                // graph separately, so executing a program that still holds
                // its header lines is not a refusal.
                Ok(())
            }
            PineStmt::ExportDecl(inner) => {
                let label = export_label(inner);
                self.exec_stmt_inner(inner, ctx, legs, builtins)?;
                if let Some(name) = label {
                    self.exported.insert(name);
                }
                Ok(())
            }
            PineStmt::FuncDecl { name, params, body } => {
                self.declare_func(name, params, body);
                Ok(())
            }
            PineStmt::MethodDecl { name, params, body } => self.declare_method(name, params, body),
            PineStmt::ConstDecl { name, value, .. } => {
                // Compile-time bindings evaluate once, on the first
                // evaluated bar that declares them.
                if !self.consts.contains(name) {
                    let assigned = self.eval_expr(value, ctx, legs, builtins)?;
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
                            self.eval_expr(&member.value, ctx, legs, builtins)?,
                        );
                    }
                    builtins.register_enum(
                        name.clone(),
                        table.keys().cloned().collect(),
                    );
                    self.enums.insert(name.clone(), table);
                }
                Ok(())
            }
            PineStmt::TypeDecl { name, fields } => {
                self.declare_type(name, fields);
                Ok(())
            }
            PineStmt::Switch(switch) => {
                self.eval_switch(switch, ctx, legs, false, builtins)?;
                Ok(())
            }
        }
    }

    /// The module owning the body under execution, as a scope key. `None`
    /// means an unlinked program (no module identity to qualify cells).
    fn scope_module(&self) -> Option<ModuleKey> {
        module_key(&self.current_module)
    }

    /// Resolve the persistent declaration a spelling names here: the body
    /// under execution first, then each caller's body, then the module's top
    /// level, then the caller's top level. The returned key is the cell's
    /// identity, so two declarations of one spelling in different bodies
    /// never share it, while a callee still reads an enclosing body's binding
    /// exactly as before.
    fn var_binding(&self, name: &str) -> Option<(ScopeKey, bool)> {
        if self.var_decls.is_empty() {
            return None;
        }
        let module = self.scope_module();
        let outer = self.sites.len();
        for depth in (1..=outer).rev() {
            let body = if depth == outer {
                module.clone()
            } else {
                self.sites[depth].module.clone()
            };
            if let Some(found) = self.lookup_var(body.as_ref(), &self.sites[..depth], name) {
                return Some(found);
            }
        }
        // The running module's top level is lexically visible inside its own
        // bodies even when the call arrived from another module.
        if let Some(found) = self.lookup_var(module.as_ref(), &[], name) {
            return Some(found);
        }
        // The outer body stays live under the call, so its top level still
        // resolves names the callee does not declare.
        if outer > 0 {
            if let Some(found) = self.lookup_var(self.sites[0].module.as_ref(), &[], name) {
                return Some(found);
            }
        }
        None
    }

    /// One scope lookup: the body instance named by `module` plus the calls
    /// that reached it, resolved to the declaration a spelling binds.
    fn lookup_var(
        &self,
        module: Option<&ModuleKey>,
        call_sites: &[CallSite],
        name: &str,
    ) -> Option<(ScopeKey, bool)> {
        let slot = VarScope {
            module: module.cloned(),
            call_sites: call_sites.to_vec(),
            name: name.to_owned(),
        };
        let binding = self.var_decls.get(&slot)?;
        Some((
            ScopeKey::new(slot.module, slot.call_sites, binding.decl),
            binding.is_varip,
        ))
    }

    /// The live value of a spelling's persistent cell, resolved in the
    /// running scope. The outer `None` means the spelling is not `var`/
    /// `varip` here; the inner one is Pine `na`.
    #[allow(clippy::option_option)]
    fn persistent_binding(&self, name: &str, ctx: &RunCtx<'_>) -> Option<Option<PineValue>> {
        let (scope, is_varip) = self.var_binding(name)?;
        Some(if is_varip {
            ctx.vars().get_varip(&scope)
        } else {
            ctx.vars().get_var(&scope)
        })
    }

    /// Route an assignment: persistent stores for `var`/`varip`, series
    /// variables otherwise. Assigning `na` to a persistent binding clears
    /// it (the initialization mark stays, so it is not re-seeded). Plain
    /// frame writes inside a call journal their entry value, so the call
    /// teardown reverts exactly what the body touched instead of cloning
    /// the whole frame up front.
    fn assign(&mut self, ctx: &mut RunCtx<'_>, name: &str, value: Option<PineValue>) {
        let binding = self.var_binding(name);
        // Any object write (or any write to a leased name, including
        // clearing it) invalidates the slot's ring lease — see bump_lease.
        let key = match &binding {
            Some((scope, _)) => LeaseKey::var_store(scope.clone()),
            None => LeaseKey::variables(name),
        };
        if matches!(value, Some(PineValue::Object(_))) || self.lease_versions.contains_key(&key) {
            self.bump_lease_key(key);
        }
        if let Some((scope, is_varip)) = binding {
            if let Some(present) = value {
                if is_varip {
                    ctx.vars_mut().set_varip(scope, name, present);
                } else {
                    ctx.vars_mut().set_var(scope, name, present);
                }
            } else if is_varip {
                ctx.vars_mut().clear_varip(&scope);
            } else {
                ctx.vars_mut().clear_var(&scope);
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
        _builtins: &mut RunBuiltins,
    ) -> Result<(), KernelError> {
        if let Some(field) = name.strip_prefix("this.") {
            let slot = self.receiver.last_mut().ok_or_else(this_unbound)?;
            if !slot.instance.fields.contains_key(field) {
                return Err(unknown_this_field(field));
            }
            slot.instance.fields.insert(field.to_owned(), value);
            return Ok(());
        }
        if name == "this" {
            return Err(cannot_rebind_this());
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
        self.receiver.iter().rposition(|slot| slot.alias == alias)
    }

    /// Write one value through a dotted object chain (`w.items.push`
    /// writes back the shrunken array): take the root like any instance
    /// call, walk the fields, set the leaf, and put the root back.
    /// Returns `false` when the root is not a takable object (the caller
    /// keeps its refusal), so only served chains divert. Unjournaled
    /// like every take/put pair — holder persistence (var store, top
    /// level, frame discard) decides, exactly like object chains.
    fn assign_nested(
        &mut self,
        ctx: &mut RunCtx<'_>,
        path: &str,
        value: Option<PineValue>,
    ) -> Result<bool, KernelError> {
        let Some((root, rest)) = path.split_once('.') else {
            return Ok(false);
        };
        if root.is_empty() || rest.is_empty() {
            return Ok(false);
        }
        if root == "this" {
            let Some(slot) = self.receiver.last_mut() else {
                return Ok(false);
            };
            let mut current = &mut slot.instance;
            let mut fields = rest.split('.').peekable();
            while let Some(field) = fields.next() {
                if fields.peek().is_none() {
                    if !current.fields.contains_key(field) {
                        return Ok(false);
                    }
                    current.fields.insert(field.to_owned(), value);
                    return Ok(true);
                }
                let Some(PineValue::Object(next)) = current
                    .fields
                    .get_mut(field)
                    .and_then(|slot| slot.as_mut())
                else {
                    return Ok(false);
                };
                current = next;
            }
            return Ok(false);
        }
        // Alias-held roots write through the live slot for single-level
        // paths, exactly like dotted rebinding.
        if !rest.contains('.') && self.write_alias_field(root, rest, value.clone()) {
            return Ok(true);
        }
        let Some((instance, home)) = self.take_object(ctx, root)? else {
            return Ok(false);
        };
        let mut instance = instance;
        let mut current = &mut instance;
        let mut fields = rest.split('.').peekable();
        while let Some(field) = fields.next() {
            if fields.peek().is_none() {
                if !current.fields.contains_key(field) {
                    self.put_object(ctx, &home, instance, true)?;
                    return Ok(false);
                }
                current.fields.insert(field.to_owned(), value);
                self.put_object(ctx, &home, instance, true)?;
                return Ok(true);
            }
            let Some(PineValue::Object(next)) = current
                .fields
                .get_mut(field)
                .and_then(|slot| slot.as_mut())
            else {
                self.put_object(ctx, &home, instance, true)?;
                return Ok(false);
            };
            current = next;
        }
        self.put_object(ctx, &home, instance, true)?;
        Ok(false)
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
    /// entry as well as the variable.
    fn prov_home(&self, name: &str, inner: ObjectHome) -> ObjectHome {
        match self.map_prov.get(name) {
            Some((map_var, key, key_value)) => super::home::Home::MapEntry {
                inner: Box::new(inner),
                map_var: map_var.clone(),
                key: key.clone(),
                key_value: key_value.clone(),
            },
            None => inner,
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

    /// Take an instance out of its home for a method call, remembering where
    /// to write it back. `None` means the name holds no instance (the caller
    /// falls through to the unknown-method refusal).
    fn take_object(
        &mut self,
        ctx: &mut RunCtx<'_>,
        name: &str,
    ) -> Result<Option<(UserInstance, ObjectHome)>, KernelError> {
        if let Some(field) = name.strip_prefix("this.") {
            let slot = self.receiver.last_mut().ok_or_else(this_unbound)?;
            let slot = slot
                .instance
                .fields
                .get_mut(field)
                .ok_or_else(|| unknown_this_field(field))?;
            return match slot.take() {
                Some(PineValue::Object(instance)) => Ok(Some((
                    instance,
                    super::home::Home::ThisField(field.to_owned()),
                ))),
                other => {
                    *slot = other;
                    Ok(None)
                }
            };
        }
        if let Some(slot) = self.variables.remove(name) {
            return match slot {
                Some(PineValue::Object(instance)) => {
                    let home = self.prov_home(
                        name,
                        super::home::Home::Slot(TreeSlot::Variable(name.to_owned())),
                    );
                    Ok(Some((instance, home)))
                }
                other => {
                    self.variables.insert(name.to_owned(), other);
                    Ok(None)
                }
            };
        }
        if let Some((scope, is_varip)) = self.var_binding(name) {
            let cell = if is_varip {
                ctx.vars().get_varip(&scope)
            } else {
                ctx.vars().get_var(&scope)
            };
            if let Some(PineValue::Object(instance)) = cell {
                if is_varip {
                    ctx.vars_mut().clear_varip(&scope);
                } else {
                    ctx.vars_mut().clear_var(&scope);
                }
                let home = self.prov_home(
                    name,
                    super::home::Home::Slot(TreeSlot::Cell {
                        scope,
                        name: name.to_owned(),
                        is_varip,
                    }),
                );
                return Ok(Some((instance, home)));
            }
        }
        Ok(None)
    }

    /// Take a nested field instance (`bars.hist_l`) out of its root object
    /// for a method call, remembering the parent chain for write-back. The
    /// walk and its restore-on-miss logic are the shared home model's, so
    /// the loop rebuilds the same chain. `None` means the path holds no
    /// object (unknown root, unknown field, or non-object field) and taken
    /// pieces are restored before declining.
    fn take_nested_object(
        &mut self,
        ctx: &mut RunCtx<'_>,
        path: &str,
    ) -> Result<Option<(UserInstance, ObjectHome)>, KernelError> {
        let Some((root, rest)) = path.split_once('.') else {
            return Ok(None);
        };
        let Some((instance, home)) = self.take_object(ctx, root)? else {
            return Ok(None);
        };
        let fields: Vec<String> = rest.split('.').map(str::to_owned).collect();
        let mut store = TreeStore {
            engine: self,
            ctx,
            bump: false,
        };
        super::home::take_nested(&mut store, home, instance, &fields)
    }

    /// Write a (possibly mutated) instance back to the home it came from.
    /// `bump` invalidates the slot's ring lease for Pine-side writes; native
    /// serves pass `false` (they sync the lease entry instead), as do
    /// identical restores that wrote nothing.
    fn put_object(
        &mut self,
        ctx: &mut RunCtx<'_>,
        home: &ObjectHome,
        instance: UserInstance,
        bump: bool,
    ) -> Result<(), KernelError> {
        let mut store = TreeStore {
            engine: self,
            ctx,
            bump,
        };
        home.put(&mut store, instance)
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
            .or_else(|| self.persistent_binding(map_var, ctx).flatten());
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
            PineExpr::FunctionCall {
                name: callee, args, ..
            } if callee == "plot" => {
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

        builtins: &mut RunBuiltins,
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
                        if let Some(key_value) =
                            self.eval_expr(&key_arg.value, ctx, legs, builtins)?
                        {
                            if let Some(key) = crate::value::map_key_of(&key_value) {
                                self.map_prov
                                    .insert(name.to_owned(), (map_var.clone(), key, key_value));
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

        builtins: &mut RunBuiltins,
    ) -> Result<Option<PineValue>, KernelError> {
        match expr {
            PineExpr::Number(v) => Ok(Some(PineValue::Float(*v))),
            PineExpr::StringLiteral(text) => Ok(Some(PineValue::Str(text.clone()))),
            PineExpr::Boolean(flag) => Ok(Some(PineValue::Bool(*flag))),
            PineExpr::Variable(name) => self.read_variable(name, ctx, legs, builtins),
            PineExpr::BuiltinVariable(var) => Ok(read_leg(legs, builtin_name(*var), ctx.cursor())),
            PineExpr::Binary { left, op, right } => {
                let first = self.eval_expr(left, ctx, legs, builtins)?;
                let second = self.eval_expr(right, ctx, legs, builtins)?;
                binary(*op, first.as_ref(), second.as_ref())
            }
            PineExpr::Unary { op, expr } => {
                let value = self.eval_expr(expr, ctx, legs, builtins)?;
                unary(*op, value.as_ref())
            }
            PineExpr::FunctionCall { id, name, args, .. } => {
                self.call(*id, name, args, ctx, legs, builtins)
            }
            PineExpr::MethodCall {
                id,
                namespace,
                method,
                args,
                type_args,
            } => self.call_method(*id, namespace, method, args, type_args, ctx, legs, builtins),
            PineExpr::InstanceCall {
                id,
                receiver,
                method,
                args,
                ..
            } => {
                // Chained construction runs against the evaluated instance;
                // an rvalue has no home, so the mutated copy is dropped and
                // only the returned value flows on.
                match self.eval_expr(receiver, ctx, legs, builtins)? {
                    Some(PineValue::Object(instance)) => {
                        let outcome = self
                            .run_method(*id, instance, method, args, ctx, legs, None, builtins)?;
                        outcome.value
                    }
                    Some(other) => Err(method_needs_object(method, &other)),
                    None => Err(method_called_on_na(method)),
                }
            }
            PineExpr::FieldRead { receiver, field } => {
                let value = self.eval_expr(receiver, ctx, legs, builtins)?;
                match value {
                    Some(PineValue::Object(instance)) => match instance.fields.get(field) {
                        Some(bound) => Ok(bound.clone()),
                        None => Err(unknown_field_on(field, &instance.type_name)),
                    },
                    Some(other) => Err(field_needs_object(field, &other)),
                    None => Err(field_read_on_na(field)),
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
                    self.eval_expr(condition, ctx, legs, builtins)?.as_ref(),
                )? {
                    Some(true) => self.eval_expr(then_expr, ctx, legs, builtins),
                    Some(false) => self.eval_expr(else_expr, ctx, legs, builtins),
                    None => Ok(None),
                }
            }
            PineExpr::ArrayAccess { array, index } => {
                let position = self.eval_expr(index, ctx, legs, builtins)?;
                let Some(at) = position else {
                    return Ok(None);
                };
                let at = whole_number("array index", Some(&at))?;
                let target = self.eval_expr(array, ctx, legs, builtins)?;
                if let Some(PineValue::Array(items)) = target {
                    let slot = usize::try_from(at).map_err(|_| array_negative_index(at))?;
                    match items.get(slot).cloned() {
                        Some(value) => Ok(value),
                        None => Err(array_index_out_of_bounds(slot)),
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
                    bound.push(self.eval_expr(item, ctx, legs, builtins)?);
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
                        // History over a parenthesized expression evaluates
                        // the expression once per bar into an
                        // engine-internal name, then reads history through
                        // the shared offset machinery below — same edges
                        // as a named series. Whatever the expression
                        // evaluates to (including its refusal) surfaces
                        // exactly as if parenthesized inline.
                        let value = self.eval_expr(series, ctx, legs, builtins)?;
                        let key = format!("\0spill:{}:{:?}", self.current_module, series);
                        self.variables.insert(key.clone(), value);
                        return self.offset_name(&key, i64::from(*offset), ctx, legs);
                    }
                };
                self.offset_name(name, i64::from(*offset), ctx, legs)
            }
            PineExpr::Switch(switch) => self.eval_switch(switch, ctx, legs, true, builtins),
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
        builtins: &RunBuiltins,
    ) -> Result<Option<PineValue>, KernelError> {
        // Receiver aliases shadow every outer binding: a body spells its
        // receiver by the declared parameter name, and nested calls may
        // reuse the same spelling for their own receiver.
        if let Some((head, rest)) = name.split_once('.') {
            if !rest.contains('.') {
                if let Some(depth) = self.receiver_slot(head) {
                    return match self.receiver[depth].instance.fields.get(rest) {
                        Some(bound) => Ok(bound.clone()),
                        None => Err(unknown_dotted_field(head, rest)),
                    };
                }
            }
        } else if let Some(depth) = self.receiver_slot(name) {
            return Ok(Some(PineValue::Object(
                self.receiver[depth].instance.clone(),
            )));
        }
        if let Some(bound) = self.variables.get(name) {
            return Ok(bound.clone());
        }
        if let Some(present) = self.persistent_binding(name, ctx) {
            return Ok(present);
        }
        if name == "this" {
            return self
                .receiver
                .last()
                .map(|slot| Some(PineValue::Object(slot.instance.clone())))
                .ok_or_else(this_unbound);
        }
        if let Some(field) = name.strip_prefix("this.") {
            let slot = self.receiver.last().ok_or_else(this_unbound)?;
            return match slot.instance.fields.get(field) {
                Some(bound) => Ok(bound.clone()),
                None => Err(unknown_this_field(field)),
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
        if let Some(member) = name.strip_prefix("strategy.") {
            // One shared harness read: the loop answers from the same
            // state through its own instruction, so values and refusals
            // cannot drift between the engines.
            return builtins.strategy().read(member);
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
        if name == "tr" {
            // Legacy bare true range: the shared kernel helper over the
            // legs at the cursor. User bindings shadow it (variables are
            // checked first), so this only serves unbound reads — exactly
            // like the compiled loop's instruction below.
            return Ok(super::eval::true_range_at(legs, ctx.cursor()));
        }
        // Deterministic chart scope: the same resolver the compiled
        // loop's chart-scope instruction reads, so both engines answer
        // one member identically (and unknown members keep refusing).
        if let Some(scope) = super::builtins_core::ChartScope::parse(name) {
            return Ok(Some(scope.read(&self.chart_symbol, &self.chart_period)));
        }
        if name == "chart.fg_color" {
            // Harness constant: headless runs have no theme, so the chart
            // foreground reads as opaque white. Display-only (plot colors
            // evaluate and drop, never reaching pixels), so the neutral
            // stands in for any theme without moving a number.
            return Ok(Some(PineValue::ColorHandle(ColorHandle::new(
                0xFF_FF_FF_00,
            ))));
        }
        if name == "chart.bg_color" {
            // Harness constant, same contract as the foreground: opaque
            // black, display-only, never numeric.
            return Ok(Some(PineValue::ColorHandle(ColorHandle::new(
                0x00_00_00_00,
            ))));
        }
        if name == "last_bar_index" {
            // Index of the last bar of the evaluated window.
            let last = legs
                .named("close")
                .map_or(0, |leg| leg.len().saturating_sub(1));
            return Ok(Some(PineValue::Int(i64::try_from(last).unwrap_or(0))));
        }
        if name == "time_open" {
            return Ok(read_leg(legs, "time", ctx.cursor()));
        }
        if name == "time_close" {
            let span = super::builtins_core::chart_period_seconds(&self.chart_period) as f64;
            return Ok(read_leg(legs, "time", ctx.cursor()).map(|v| match v {
                PineValue::Float(f) => PineValue::Float(f + span),
                PineValue::Int(i) => PineValue::Int(i + span as i64),
                other => other,
            }));
        }
        if let Some(leg) = legs.named(name) {
            return Ok(read_leg_slice(leg, ctx.cursor()));
        }
        if let Some(member) = self.enum_lookup(name, builtins) {
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
                    None => self.persistent_binding(head, ctx).flatten(),
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
                    return self.read_variable(rest, ctx, legs, builtins);
                }
            }
        }
        if name == "na" {
            return Ok(None);
        }
        // A declared persistent (`var`/`varip`) reads as absence until its
        // first assignment: `var float eit = na` binds `na`, it does not
        // leave the name undeclared. Only truly undeclared names refuse.
        if self.var_binding(name).is_some() {
            return Ok(None);
        }
        Err(undefined_variable(name))
    }

    /// Resolve a trailing `Enum.Member` pair against the registered enums.
    /// Longer dotted names (module aliases like `UTIL.Smoothings.WMA`)
    /// resolve on their last two segments.
    fn enum_lookup(&self, name: &str, builtins: &RunBuiltins) -> Option<PineValue> {
        let (enum_name, member) = super::builtins_core::split_enum_member(name)?;
        if let Some(table) = self.enums.get(enum_name) {
            return table
                .contains_key(member)
                .then(|| PineValue::EnumHandle(EnumHandle::new(enum_name, member)));
        }
        // Mixed runs declare enums on whichever engine owns the module:
        // the shared table observes every declaration execution, so a
        // tree read of a loop-declared enum serves the same handle.
        builtins
            .enum_member_registered(enum_name, member)
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
            return Err(history_negative_offset(bars_back));
        }
        let back = usize::try_from(bars_back)
            .map_err(|_| refused(format!("history offset {bars_back} does not fit this run")))?;
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
                None => Err(array_index_out_of_bounds(back)),
            };
        }
        let bound = self.current_binding(name, ctx);
        let recorded = self.history.contains_key(name)
            || self.var_binding(name).is_some()
            || self
                .call_stack
                .last()
                .is_some_and(|top| self.history.contains_key(&format!("{top}#{name}")));
        if bound.is_none() && !recorded {
            return Err(undefined_variable(name));
        }
        if back == 0 {
            return Ok(bound.flatten());
        }
        let position = self.snapshots.len().checked_sub(back);
        // A persistent cell reads its own past: the same declaration reached
        // by a different call instance is a different series, so the offset
        // never crosses instances.
        if !self.variables.contains_key(name) {
            if let Some((scope, _)) = self.var_binding(name) {
                if let Some(slot) = self.var_history.get(&scope) {
                    return Ok(position
                        .and_then(|index| slot.get(index).cloned())
                        .flatten());
                }
            }
        }
        // The innermost scope's past wins; the bare name stays as the
        // fallback for top-level series and frame bindings. A scope that
        // never recorded the name contributes no value rather than another
        // scope's past.
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
            .and_then(|index| {
                self.history
                    .get(name)
                    .and_then(|slot| slot.get(index).cloned())
            })
            .flatten())
    }

    /// The in-progress bar's binding: series variables, then persistent
    /// stores. The outer `None` means unbound here and now, which is
    /// distinct from bound-to-`na` — hence the nested option.
    #[allow(clippy::option_option)]
    fn current_binding(&self, name: &str, ctx: &RunCtx<'_>) -> Option<Option<PineValue>> {
        if let Some(bound) = self.variables.get(name) {
            return Some(bound.clone());
        }
        self.persistent_binding(name, ctx)
    }

    /// Calls: the `na`/`nz`/`fixnan` specials, `plot` capture, registered
    /// user functions, else a refusal naming the callee.
    /// One shared `ta` dispatch: the stacked site key plus the chart tick
    /// feed the bridge identically for `ta.*` and legacy bare names, so
    /// one logical call path keys one history however it is spelled.
    fn call_ta(
        &mut self,
        method: &str,
        args: &[CallArg],
        values: &[Option<PineValue>],
        ctx: &RunCtx<'_>,
        legs: &Legs,
        builtins: &mut RunBuiltins,
    ) -> Result<Option<PineValue>, KernelError> {
        let own = site_string(&format!("ta.{method}"), args);
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
        builtins.ta_mut()?.call(&site, method, values, tick)
    }

    fn call(
        &mut self,
        id: NodeId,
        name: &str,
        args: &[CallArg],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,

        builtins: &mut RunBuiltins,
    ) -> Result<Option<PineValue>, KernelError> {
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.eval_expr(&arg.value, ctx, legs, builtins)?);
        }
        match name {
            "na" => match values.len() {
                0 => Ok(None),
                1 => Ok(Some(PineValue::Bool(is_absent(values[0].as_ref())))),
                _ => Err(refused(NA_ARITY_MESSAGE)),
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
                _ => Err(refused(NZ_ARITY_MESSAGE)),
            },
            "fixnan" => {
                if args.is_empty() {
                    return Err(refused("'fixnan' takes exactly one argument"));
                }
                let site = format!("{:?}", args[0].value);
                builtins.fixnan(&site, &values)
            }
            // Legacy three-way choice: all three arguments evaluate first
            // (it is a call, not an operator), then the shared leaf picks
            // the taken branch exactly like the compiled loop does.
            "iff" => super::eval::iff_of(&values),
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
                    return Err(refused(PLOT_MISSING_SERIES_MESSAGE));
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
                    .unwrap_or_else(|| capture_fallback_title("plot", self.plots.len()));
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
                let site = site_string("hline", args);
                let series = values[0].clone();
                let title = call_title(
                    args,
                    &values,
                    &capture_fallback_title("hline", self.hlines.len()),
                );
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
                let site = site_string("plotshape", args);
                let series = values[0].clone();
                let title = call_title(
                    args,
                    &values,
                    &capture_fallback_title("plotshape", self.plotshapes.len()),
                );
                match self.plotshapes.iter_mut().find(|shape| shape.site == site) {
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
                let Some(first) = args.first() else {
                    return Err(refused(INPUT_MISSING_DEFAULT_MESSAGE));
                };
                let method = bare_input_method(Some(&first.value));
                let core_args: Vec<super::builtins_core::CoreArg> = args
                    .iter()
                    .zip(values)
                    .map(|(arg, value)| super::builtins_core::CoreArg {
                        name: arg.name.clone(),
                        value,
                    })
                    .collect();
                let site = site_string("input", args);
                match method {
                    Some(method) => super::builtins_core::call(
                        "input",
                        method,
                        &site,
                        &core_args,
                        &[],
                        ctx.inputs_mut(),
                    ),
                    // A computed default names its input from its run-time
                    // value through the one shared inference table.
                    None => super::builtins_core::input_auto(&site, &core_args, ctx.inputs_mut()),
                }
            }
            "time" => self.call_session_time(&values, ctx, legs),
            // Clock reads for session math: `hour(t)`/`minute(t)` in UTC,
            // or in ET with an explicit `"America/New_York"` zone (the only
            // zone the headless harness maps — histories are US equities).
            // Anything else refuses naming the row instead of guessing an
            // offset.
            "hour" => call_wall_part(&values, 0),
            "minute" => call_wall_part(&values, 1),
            // The strategy declaration carries the broker contract the
            // harness observes but never runs: pyramiding, commission,
            // slippage, margin, and the initial capital the day-P&L
            // baseline reads back. Arguments evaluate (errors refuse
            // above); only a declared initial capital is retained,
            // everything else is a documented harness boundary.
            "strategy" => {
                let core_args: Vec<super::builtins_core::CoreArg> = args
                    .iter()
                    .zip(values)
                    .map(|(arg, value)| super::builtins_core::CoreArg {
                        name: arg.name.clone(),
                        value,
                    })
                    .collect();
                builtins.strategy_mut().declare(&core_args)?;
                Ok(None)
            }
            // Alert declarations evaluate their condition and record
            // nothing: alerts cannot fire headlessly, and they draw
            // nothing, so there is no capture vector for them. Argument
            // evaluation errors still refuse above. Bare `alert(...)`
            // calls from strategy scripts share the rule.
            "alertcondition" | "alert" => Ok(None),
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
                let site = site_string(&format!("{}::{name}", self.current_module), args);
                let module = self.current_module.clone();
                self.exec_call(
                    site, id, &module, &params, args, &values, &body, ctx, legs, builtins,
                )
            }
            // Legacy bare technical-analysis names (`sma(...)`): user
            // definitions shadow the bridge (checked above), so an
            // unclaimed name serves through the shared dispatch with the
            // identical site key — one keyed history either way.
            _ if super::builtins_ta::TaBridge::supported_methods().contains(&name) => {
                self.call_ta(name, args, &values, ctx, legs, builtins)
            }
            // Legacy bare math names (`pow(...)`): same shadow rule, then
            // the shared `math` namespace with the qualified spelling's
            // site key.
            _ if super::builtins_core::legacy_bare_math_names().contains(&name) => {
                let core_args: Vec<super::builtins_core::CoreArg> = args
                    .iter()
                    .zip(values.iter())
                    .map(|(arg, value)| super::builtins_core::CoreArg {
                        name: arg.name.clone(),
                        value: value.clone(),
                    })
                    .collect();
                let site = site_string(&format!("math.{name}"), args);
                super::builtins_core::call("math", name, &site, &core_args, &[], ctx.inputs_mut())
            }
            // Legacy bare string names (`tostring(...)`): same shadow rule,
            // then the shared `str` namespace with the qualified key.
            _ if super::builtins_core::legacy_bare_str_names().contains(&name) => {
                let core_args: Vec<super::builtins_core::CoreArg> = args
                    .iter()
                    .zip(values.iter())
                    .map(|(arg, value)| super::builtins_core::CoreArg {
                        name: arg.name.clone(),
                        value: value.clone(),
                    })
                    .collect();
                let site = site_string(&format!("str.{name}"), args);
                super::builtins_core::call("str", name, &site, &core_args, &[], ctx.inputs_mut())
            }
            _ => Err(unknown_function(name)),
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
        let [open, high, low, close] = legs.try_into().expect("four legs checked above");
        let site = site_string("plotcandle", args);
        let title = named_title(
            args,
            values,
            &capture_fallback_title("plotcandle", self.plotcandles.len()),
        );
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
        let site = site_string("barcolor", args);
        let title = named_title(
            args,
            values,
            &capture_fallback_title("barcolor", self.barcolors.len()),
        );
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
        if positional.len() != 2 && positional.len() != 4 && positional.len() != 6 {
            return Err(refused(format!(
                "'fill' takes two plot calls (or two plus top/bottom value/color bounds), got {} positional arguments",
                positional.len()
            )));
        }
        let mut keys = Vec::with_capacity(2);
        for (arg, _) in positional.iter().take(2) {
            match &arg.value {
                PineExpr::FunctionCall {
                    name, args: inner, ..
                } if name == "plot" => {
                    keys.push(plot_site_key(inner));
                }
                PineExpr::Variable(var) => match self.plot_vars.get(var) {
                    Some(site) => keys.push(site.clone()),
                    None => {
                        return Err(fill_variable_without_plot_tag(var));
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
                return Err(fill_plot_site_without_capture(key));
            };
            titles.push(plot.title.clone());
            legs.push(plot.values.last().cloned().unwrap_or(None));
        }
        let region = match (&legs[0], &legs[1]) {
            (None, None) => None,
            _ => Some(PineValue::Array(legs)),
        };
        let site = site_string("fill", args);
        let title = named_title(
            args,
            values,
            &capture_fallback_title("fill", self.fills.len()),
        );
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
        session_time_of(
            values,
            &self.chart_period,
            read_leg(legs, "time", ctx.cursor()),
        )
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

        builtins: &mut RunBuiltins,
    ) -> Result<Option<PineValue>, KernelError> {
        let usage =
            "request.security_lower_tf takes a symbol, a timeframe and a leg-expression list";
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
                    let given = self.eval_expr(&arg.value, ctx, legs, builtins)?;
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
        let symbol = match self.eval_expr(&symbol_arg.value, ctx, legs, builtins)? {
            Some(PineValue::Str(symbol)) => symbol,
            other => {
                return Err(refused(format!(
                    "request.security_lower_tf takes a string symbol, got {other:?}"
                )));
            }
        };
        let timeframe = match self.eval_expr(&timeframe_arg.value, ctx, legs, builtins)? {
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
        let dataset = builtins
            .request()
            .lower_tf(&symbol, &timeframe)
            .ok_or_else(|| {
                refused(format!(
                    "live request.security_lower_tf {LIVE_REQUEST_TAIL}"
                ))
            })?;
        // The same window rule as `request.security` in both modes: only rows
        // whose own bucket has closed at the current chart instant, then the
        // last `calc_bars_count` of those.
        let count = bar_count.map(|count| usize::try_from(count).unwrap_or(usize::MAX));
        let window = serve_security(
            &dataset,
            chart_ms(legs, ctx.cursor(), self.tick_mode()),
            count,
        );
        let columns = legs_wanted
            .iter()
            .map(|leg| Some(PineValue::Array(security_column(&dataset, leg, window))))
            .collect();
        Ok(Some(PineValue::Array(columns)))
    }

    /// Serve one `request.security(symbol, timeframe, exprs, gaps?,
    /// lookahead?, calc_bars_count?)` call from a registered per-timeframe
    /// dataset: only closed buckets count (each bucket's close at or before
    /// the current chart instant), and the last confirmed value carries
    /// forward — the headless reading of `barmerge.gaps_off` with
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

        builtins: &mut RunBuiltins,
    ) -> Result<Option<PineValue>, KernelError> {
        let usage =
            "request.security takes a symbol, a timeframe and an expression or expression list";
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
                    let given = self.eval_expr(&arg.value, ctx, legs, builtins)?;
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
        let symbol = match self.eval_expr(&symbol_arg.value, ctx, legs, builtins)? {
            Some(PineValue::Str(symbol)) => symbol,
            other => {
                return Err(refused(format!(
                    "request.security takes a string symbol, got {other:?}"
                )));
            }
        };
        let timeframe = match self.eval_expr(&timeframe_arg.value, ctx, legs, builtins)? {
            Some(PineValue::Str(timeframe)) => timeframe,
            other => {
                return Err(refused(format!(
                    "request.security takes a string timeframe, got {other:?}"
                )));
            }
        };
        let gaps = match gaps_arg {
            None => "gaps_off".to_owned(),
            Some(arg) => match self.eval_expr(&arg.value, ctx, legs, builtins)? {
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
            Some(arg) => match self.eval_expr(&arg.value, ctx, legs, builtins)? {
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
        let dataset = builtins
            .request()
            .security(&symbol, &timeframe)
            .ok_or_else(|| refused(format!("live request.security {LIVE_REQUEST_TAIL}")))?;
        let count = bar_count.map(|count| usize::try_from(count).unwrap_or(usize::MAX));
        let window = serve_security(
            &dataset,
            chart_ms(legs, ctx.cursor(), self.tick_mode()),
            count,
        );
        if window.0 == window.1 {
            if single {
                return Ok(None);
            }
            return Ok(Some(PineValue::Array(vec![None; items.len()])));
        }
        let tail = SecurityTail::of(&dataset, window, count);
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            out.push(self.security_expr(item, &tail, ctx, legs, builtins)?);
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

        builtins: &mut RunBuiltins,
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
                        values.push(self.eval_expr(&arg.value, ctx, legs, builtins)?);
                    }
                    let history = tail.history();
                    let bridge = builtins.ta_mut()?;
                    let closes = history.column("close").expect("served window has close");
                    return bridge.batch_security(
                        method,
                        closes,
                        &values,
                        &history.ohlcv(),
                        history.display_count,
                    );
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
                let history = tail.history();
                let column = history.column(&source_name).expect("leg checked above");
                let mut values = Vec::with_capacity(args.len());
                values.push(security_leg(tail, &source_name));
                for arg in &args[1..] {
                    values.push(self.eval_expr(&arg.value, ctx, legs, builtins)?);
                }
                let bridge = builtins.ta_mut()?;
                bridge.batch_security(
                    method,
                    column,
                    &values,
                    &history.ohlcv(),
                    history.display_count,
                )
            }
            PineExpr::Binary { left, op, right } => {
                let first = self.security_expr(left, tail, ctx, legs, builtins)?;
                let second = self.security_expr(right, tail, ctx, legs, builtins)?;
                binary(*op, first.as_ref(), second.as_ref())
            }
            _ => Err(refused(
                "injected security serves bare legs, kernel-backed single-source ta calls and scalar operators only",
            )),
        }
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

        builtins: &mut RunBuiltins,
    ) -> Result<Option<PineValue>, KernelError> {
        let [arg] = args else {
            return Err(refused(format!(
                "timeframe.change takes one period string, got {} argument(s)",
                args.len()
            )));
        };
        let period = self.eval_expr(&arg.value, ctx, legs, builtins)?;
        super::builtins_core::timeframe_change(
            &[period],
            ctx.cursor(),
            legs.named("time").unwrap_or(&[]),
        )
    }

    /// Record one drawing call as data. The shared registry owns creation,
    /// mutation, reads, deletion, and every refusal; this path only
    /// evaluates the arguments in source order (the tree-walk's scope) and
    /// hands them over with the evaluated-bar index. Nothing renders.
    fn call_drawing(
        &mut self,
        kind: DrawingKind,
        method: &str,
        args: &[CallArg],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,

        builtins: &mut RunBuiltins,
    ) -> Result<Option<PineValue>, KernelError> {
        let mut evaluated = Vec::with_capacity(args.len());
        for arg in args {
            evaluated.push(super::builtins_core::CoreArg {
                name: arg.name.clone(),
                value: self.eval_expr(&arg.value, ctx, legs, builtins)?,
            });
        }
        builtins
            .drawing_mut()
            .invoke(kind, method, &evaluated, self.snapshots.len())
    }

    /// Harness recording of the `strategy.*` broker dialogue: entries,
    /// exits, closes, and the empty-book trade-history reads. Every call
    /// lands in the run's shared [`super::strategy::StrategyState`] with the
    /// committing bar; nothing here reaches a venue. The shared dispatcher
    /// owns the event shape and every refusal wording, so the compiled loop
    /// records the identical stream.
    fn call_strategy(
        &mut self,
        namespace: &str,
        method: &str,
        args: &[CallArg],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,

        builtins: &mut RunBuiltins,
    ) -> Result<Option<PineValue>, KernelError> {
        let mut evaluated = Vec::with_capacity(args.len());
        for arg in args {
            evaluated.push(super::builtins_core::CoreArg {
                name: arg.name.clone(),
                value: self.eval_expr(&arg.value, ctx, legs, builtins)?,
            });
        }
        let member = match namespace.strip_prefix("strategy.") {
            Some(tail) => format!("{tail}.{method}"),
            None => method.to_owned(),
        };
        builtins
            .strategy_mut()
            .invoke(ctx.cursor(), &member, &evaluated)
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
        id: NodeId,
        namespace: &str,
        method: &str,
        args: &[CallArg],
        type_args: &[super::syntax::PineType],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,

        builtins: &mut RunBuiltins,
    ) -> Result<Option<PineValue>, KernelError> {
        // `strategy.*` broker calls are harness observations: entries,
        // exits, closes, and the empty-book trade-history reads the
        // strategy file leans on. Nothing here reaches a venue.
        if namespace == "strategy" || namespace.starts_with("strategy.") {
            return self.call_strategy(namespace, method, args, ctx, legs, builtins);
        }
        if namespace == "runtime" && method == "error" {
            let mut values = Vec::with_capacity(args.len());
            for arg in args {
                values.push(self.eval_expr(&arg.value, ctx, legs, builtins)?);
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
                return self.call_security_lower_tf(args, ctx, legs, builtins);
            }
            if method == "security" {
                return self.call_security(args, ctx, legs, builtins);
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
            return self.call_timeframe_change(args, ctx, legs, builtins);
        }
        // Bare `timeframe.in_seconds()` reports the chart period in
        // seconds through the shared resolver. Any arguments take the
        // namespaced bridge below.
        if namespace == "timeframe" && method == "in_seconds" && args.is_empty() {
            return Ok(Some(PineValue::Int(
                super::builtins_core::chart_period_seconds(&self.chart_period),
            )));
        }
        // Drawing namespaces record lifecycle data instead of rendering:
        // `.new` mints a recorded object, setters/`table.cell` mutate it,
        // getters read it back, `.delete` retires it. Anything outside the
        // recorded surface refuses with the unknown-method reason.
        if let Some(kind) = DrawingKind::parse(namespace) {
            return self.call_drawing(kind, method, args, ctx, legs, builtins);
        }
        if namespace != "ta"
            && !matches!(
                namespace,
                "math" | "str" | "input" | "array" | "matrix" | "map" | "color" | "timeframe"
            )
        {
            if namespace == "this" || (method == "new" && self.types.contains_key(namespace)) {
                return self.call_instance_method(id, namespace, method, args, ctx, legs, builtins);
            }
            // Imported-type construction (`ALIAS.Type.new()`) resolves the
            // alias through the caller's table, then constructs the
            // registered type exactly like a bare `Type.new()`.
            if method == "new" {
                if let Some((alias, type_name)) = namespace.split_once('.') {
                    let owner = self.current_module.clone();
                    if self.module_aliases.contains_key(&(owner, alias.to_owned()))
                        && self.types.contains_key(type_name)
                    {
                        return self.call_instance_method(
                            id, type_name, method, args, ctx, legs, builtins,
                        );
                    }
                }
            }
            // Instance-held receivers (`sched.register(...)`) dispatch to
            // their type's methods; anything else falls through to the
            // import-alias dispatch below.
            let held = self.peek_object(ctx, namespace);
            if held {
                return self.call_instance_method(id, namespace, method, args, ctx, legs, builtins);
            }
            // Nested field receivers (`bars.hist_l.get`) dispatch like held
            // instances when their root is held: the instance dispatch
            // takes the field out of its parent chain. Alias and `this.`
            // namespaces keep their existing routes below and above.
            if let Some((root, _)) = namespace.split_once('.') {
                if !root.is_empty() && root != "this" && self.peek_object(ctx, root) {
                    return self
                        .call_instance_method(id, namespace, method, args, ctx, legs, builtins);
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
                        values.push(self.eval_expr(&arg.value, ctx, legs, builtins)?);
                    }
                    let (params, body) = select_function(
                        &format!("{namespace}.{method}"),
                        &overloads,
                        args,
                        &values,
                    )?;
                    let site = site_string(&format!("{target}::{method}"), args);
                    return self.exec_call(
                        site, id, &target, &params, args, &values, &body, ctx, legs, builtins,
                    );
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
                    .find(|((_, name), (owner, _, _))| *name == *method && *owner == target)
                    .map(|((receiver_type, _), found)| (receiver_type.clone(), found.clone()))
                {
                    let mut values = Vec::with_capacity(args.len());
                    for arg in args {
                        values.push(self.eval_expr(&arg.value, ctx, legs, builtins)?);
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
                        let site = site_string(&format!("{}.{method}", instance.type_name), args);
                        self.receiver.push(ReceiverSlot {
                            instance,
                            alias: head.name.clone(),
                        });
                        let value = self.exec_call(
                            site,
                            id,
                            &method_owner,
                            &params[1..],
                            &args[1..],
                            &values[1..],
                            &body,
                            ctx,
                            legs,
                            builtins,
                        );
                        let back = self.receiver.pop().expect("receiver pushed above");
                        if let Some(home) = home {
                            self.assign_path(
                                ctx,
                                &home,
                                Some(PineValue::Object(back.instance)),
                                builtins,
                            )?;
                        }
                        return value;
                    }
                    let site = site_string(&format!("{namespace}.{method}"), args);
                    return self.exec_call(
                        site,
                        id,
                        &method_owner,
                        &params,
                        args,
                        &values,
                        &body,
                        ctx,
                        legs,
                        builtins,
                    );
                }
                return Err(refused(format!("unknown function '{namespace}.{method}'")));
            }
            // Collection method-call syntax (`items.push(x)`): a
            // variable-held array/matrix/map shares its namespace bridge.
            if let Some(value) =
                self.try_collection_call(namespace, method, args, type_args, ctx, legs, builtins)?
            {
                return Ok(value);
            }
            return Err(refused(format!("unknown method '{namespace}.{method}'")));
        }
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.eval_expr(&arg.value, ctx, legs, builtins)?);
        }
        if namespace == "ta" {
            return self.call_ta(method, args, &values, ctx, legs, builtins);
        }
        let core_args: Vec<super::builtins_core::CoreArg> = args
            .iter()
            .zip(values)
            .map(|(arg, value)| super::builtins_core::CoreArg {
                name: arg.name.clone(),
                value,
            })
            .collect();
        let site = site_string(&format!("{namespace}.{method}"), args);
        super::builtins_core::call(
            namespace,
            method,
            &site,
            &core_args,
            type_args,
            ctx.inputs_mut(),
        )
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

        builtins: &mut RunBuiltins,
    ) -> Result<Option<Option<PineValue>>, KernelError> {
        let Some(bridge) = self.peek_collection(ctx, namespace) else {
            return Ok(None);
        };
        let receiver = self.eval_expr(
            &PineExpr::Variable(namespace.to_owned()),
            ctx,
            legs,
            builtins,
        )?;
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.eval_expr(&arg.value, ctx, legs, builtins)?);
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
        let site = site_string(&format!("{bridge}.{method}"), args);
        super::builtins_core::call(
            bridge,
            method,
            &site,
            &core_args,
            type_args,
            ctx.inputs_mut(),
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
        if let Some(Some(present)) = self.persistent_binding(name, ctx) {
            return matches!(present, PineValue::Object(_));
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
        if let Some(value) = self.persistent_binding(name, ctx) {
            if let Some(found) = bridge(value.as_ref()) {
                return Some(found);
            }
        }
        None
    }

    /// Dispatch one method call on a taken receiver: `this.*` reuses the
    /// innermost receiver, `Type.new()` constructs, and every other
    /// namespace takes the instance out of its home first. The mutated
    /// instance returns to its home before the value does.
    fn call_instance_method(
        &mut self,
        id: NodeId,
        namespace: &str,
        method: &str,
        args: &[CallArg],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,

        builtins: &mut RunBuiltins,
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
                .ok_or_else(this_unbound)?;
            let outcome = self.run_method(id, receiver, method, args, ctx, legs, None, builtins)?;
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
                if args.is_empty() {
                    return Ok(Some(PineValue::Object(
                        self.construct(namespace, &fields, ctx, legs, builtins)?,
                    )));
                }
                // Positional field initializers fill declaration order;
                // named ones bind by field name. Every argument evaluates
                // in order first (their refusals stand); the first binding
                // wins on repeats, and unknown names plus surplus
                // positionals refuse — one wording on both engines.
                let mut evaluated = Vec::with_capacity(args.len());
                for arg in args {
                    evaluated.push(self.eval_expr(&arg.value, ctx, legs, builtins)?);
                }
                let mut bound = HashMap::with_capacity(fields.len());
                let mut positional = 0_usize;
                for (arg, value) in args.iter().zip(evaluated) {
                    if let Some(given) = &arg.name {
                        if !fields.iter().any(|field| &field.name == given) {
                            return Err(method_unknown_argument(given));
                        }
                        bound.entry(given.clone()).or_insert(value);
                    } else {
                        let Some(field) = fields.get(positional) else {
                            return Err(method_too_many_positionals());
                        };
                        positional += 1;
                        bound.entry(field.name.clone()).or_insert(value);
                    }
                }
                return Ok(Some(PineValue::Object(self.construct_with(
                    namespace, &fields, &bound, ctx, legs, builtins,
                )?)));
            }
        }
        // Receiver-alias dispatch (`level.updateLevelSuccess(...)`): the
        // call runs against a clone of the live slot and writes the
        // mutated copy back to its slot — the same index-back discipline
        // as the `this.` path above, so nested calls never unbind the
        // receiver their arguments still spell.
        if let Some(depth) = self.receiver_slot(namespace) {
            let receiver = self.receiver[depth].instance.clone();
            let outcome = self.run_method(id, receiver, method, args, ctx, legs, None, builtins)?;
            match self.receiver.get_mut(depth) {
                Some(slot) => {
                    slot.instance = outcome.receiver;
                    return outcome.value;
                }
                None => {
                    return Err(refused("receiver alias slot vanished during a method call"));
                }
            }
        }
        match self.take_object(ctx, namespace)? {
            Some((instance, home)) => {
                let lease = home.lease_key();
                let outcome =
                    self.run_method(id, instance, method, args, ctx, legs, lease, builtins)?;
                // Native serves synced the lease; Pine bodies invalidate it.
                self.put_object(ctx, &home, outcome.receiver, !outcome.served_native)?;
                outcome.value
            }
            None => match self.take_nested_object(ctx, namespace)? {
                Some((instance, home)) => {
                    let lease = home.lease_key();
                    let outcome =
                        self.run_method(id, instance, method, args, ctx, legs, lease, builtins)?;
                    self.put_object(ctx, &home, outcome.receiver, !outcome.served_native)?;
                    outcome.value
                }
                None => {
                    // Collection leaf through an object chain
                    // (`w.items.size()`): the takes above only serve
                    // objects, so a collection leaf dispatches to its
                    // bridge with the chain value as receiver. A failed
                    // chain read keeps the unknown-method refusal, so only
                    // served values divert.
                    match self.eval_expr(
                        &PineExpr::Variable(namespace.to_owned()),
                        ctx,
                        legs,
                        builtins,
                    ) {
                        Ok(receiver) => {
                            let bridge = match &receiver {
                                Some(PineValue::Array(_)) => Some("array"),
                                Some(PineValue::Matrix(_)) => Some("matrix"),
                                Some(PineValue::IntMap(_) | PineValue::Map(_)) => {
                                    Some("map")
                                }
                                _ => None,
                            };
                            let Some(bridge) = bridge else {
                                return Err(refused(format!(
                                    "unknown method '{namespace}.{method}'"
                                )));
                            };
                            let mut values = Vec::with_capacity(args.len());
                            for arg in args {
                                values.push(self.eval_expr(&arg.value, ctx, legs, builtins)?);
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
                            let site = site_string(&format!("{bridge}.{method}"), args);
                            super::builtins_core::call(
                                bridge,
                                method,
                                &site,
                                &core_args,
                                &[],
                                ctx.inputs_mut(),
                            )
                        }
                        Err(_) => Err(refused(format!(
                            "unknown method '{namespace}.{method}'"
                        ))),
                    }
                }
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

        builtins: &mut RunBuiltins,
    ) -> Result<UserInstance, KernelError> {
        self.construct_with(type_name, fields, &HashMap::new(), ctx, legs, builtins)
    }

    /// Build one fresh instance, filling fields from `overrides` where
    /// bound and evaluating defaults elsewhere — the shared shape behind
    /// bare and argument-carrying construction.
    fn construct_with(
        &mut self,
        type_name: &str,
        fields: &[FieldDecl],
        overrides: &HashMap<String, Option<PineValue>>,
        ctx: &mut RunCtx<'_>,
        legs: &Legs,

        builtins: &mut RunBuiltins,
    ) -> Result<UserInstance, KernelError> {
        let mut bound = HashMap::with_capacity(fields.len());
        for field in fields {
            let value = match overrides.get(&field.name) {
                Some(value) => value.clone(),
                None => match &field.default {
                    Some(default) => self.eval_expr(default, ctx, legs, builtins)?,
                    None => None,
                },
            };
            bound.insert(field.name.clone(), value);
        }
        Ok(UserInstance::new(type_name, bound))
    }

    /// Attempt one natively served instance-method call. `Declined` (any
    /// guard closed, any lookup missed, any provider decline) runs the Pine
    /// body unchanged; `Served` reports the taken receiver already updated
    /// in place, alongside the call value. Guards, narrowest first: engine
    /// switch off declines everything (explicit `enabled: false` keeps
    /// Pine-oracle runs); named args decline (positional binding would
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
        builtins: &mut RunBuiltins,
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
        if !self.tick_mode() || builtins.request().has_lower_tf() {
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
            bar_open_ms: chart_ms(legs, cursor, true).get(),
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
    /// first feed validates the whole tape up front and loads the prefix;
    /// later feeds append only the new tail, so steady-state reads extend
    /// incrementally. Bars must arrive evenly at `span_ms`: any gap,
    /// overlap, or unordered stamp declines — no resample guessing, Pine
    /// runs the body. The store is untouched on decline.
    ///
    /// Whole-tape validation (not just the prefix) is load-bearing: a
    /// served bar skips the Pine body's `ta.*` calls, so the run's
    /// `TaBridge` never warms that bar. Serving bar 0 while a later bar
    /// declines would reseed every recursive kernel one bar late and lag
    /// the runforever — the freeze suite pins the lag. A single gap
    /// anywhere therefore poisons the run to Pine (`usize::MAX` sentinel,
    /// checked before the reuse shortcut below).
    fn apex_feed(&mut self, span_ms: i64, through: usize) -> Option<i64> {
        if self.apex_loaded == usize::MAX {
            return None;
        }
        let want = through + 1;
        if want > self.run_bars.len() {
            return None;
        }
        // A second `fetch` on the same bar reuses the fed store.
        if self.apex_loaded >= want {
            return Some(self.run_bars[want - 1].ts.timestamp_millis() + span_ms);
        }
        if self.apex_loaded == 0 {
            let mut prev_ts = None;
            for bar in &self.run_bars {
                let ts = bar.ts.timestamp_millis();
                if prev_ts.is_some_and(|prev| ts - prev != span_ms) {
                    self.apex_loaded = usize::MAX;
                    return None;
                }
                prev_ts = Some(ts);
            }
        }
        let from = self.apex_loaded;
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
            return self.serve_miss(
                type_name,
                method,
                receiver_fields,
                values,
                frame,
                key,
                current,
                provider,
            );
        }
        let tick = crate::native_libs::fetch_tick_from_frame(frame);
        let result = {
            let Some(entry) = self.leased_rings.get_mut(&key) else {
                return self.serve_miss(
                    type_name,
                    method,
                    receiver_fields,
                    values,
                    frame,
                    key,
                    current,
                    provider,
                );
            };
            if entry.version != current {
                return self.serve_miss(
                    type_name,
                    method,
                    receiver_fields,
                    values,
                    frame,
                    key,
                    current,
                    provider,
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
        id: NodeId,
        mut receiver: UserInstance,
        method: &str,
        args: &[CallArg],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,
        lease: Option<LeaseKey>,

        builtins: &mut RunBuiltins,
    ) -> Result<MethodOutcome, KernelError> {
        let key = (receiver.type_name.clone(), method.to_owned());
        let Some((owner, params, body)) = self.methods.get(&key).cloned() else {
            return Err(unknown_method_on(&receiver.type_name, method));
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
            values.push(self.eval_expr(&arg.value, ctx, legs, builtins)?);
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
            builtins,
        )? {
            return Ok(MethodOutcome {
                receiver,
                value: Ok(value),
                served_native: true,
            });
        }
        let site = site_string(&format!("{}.{method}", receiver.type_name), args);
        // The receiver's declared alias (`level` in `updateLevelTouch(
        // MarketLevel level, ...)`) rides the slot: reads, nested calls,
        // and field writes through the alias hit the one live copy, never
        // a stale clone. The slot pops on return like every frame.
        self.receiver.push(ReceiverSlot {
            instance: receiver,
            alias: head.name.clone(),
        });
        let value = self.exec_call(
            site,
            id,
            &owner,
            &params[1..],
            args,
            &values,
            &body,
            ctx,
            legs,
            builtins,
        );
        let back = self.receiver.pop().expect("receiver pushed above");
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
    /// do, never a same-named binding from another scope. The call expression
    /// joins the call-path stack for the same reason: a `var`/`varip` cell
    /// declared in this body belongs to this instance of it.
    /// Bind one call's evaluated arguments to its parameter list: named
    /// arguments first, then positional, then each parameter's default
    /// (evaluated here, in declaration order). Returns `(parameter name,
    /// value, caller home)` in parameter order; the home is the variable the
    /// argument was read from, which is what a collection result writes back
    /// to. One binder serves the tree-walk call and the tree-to-VM call, so
    /// both engines bind, default, and refuse identically.
    fn bind_call_args(
        &mut self,
        params: &[Param],
        call_args: &[CallArg],
        values: &[Option<PineValue>],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,

        builtins: &mut RunBuiltins,
    ) -> Result<Vec<BoundArg>, KernelError> {
        let mut bindings: Vec<BoundArg> = Vec::with_capacity(params.len());
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
                    self.eval_expr(default, ctx, legs, builtins)?,
                    None,
                ));
            } else {
                return Err(method_arg_missing(&param.name));
            }
        }
        if positional.next().is_some() {
            return Err(method_too_many_positionals());
        }
        for arg in call_args {
            if let Some(given) = arg.name.as_ref() {
                if !params.iter().any(|param| &param.name == given) {
                    return Err(method_unknown_argument(given));
                }
            }
        }
        Ok(bindings)
    }

    pub(crate) fn exec_call(
        &mut self,
        site: String,
        site_id: NodeId,
        module: &str,
        params: &[Param],
        call_args: &[CallArg],
        values: &[Option<PineValue>],
        body: &[PineStmt],
        ctx: &mut RunCtx<'_>,
        legs: &Legs,

        builtins: &mut RunBuiltins,
    ) -> Result<Option<PineValue>, KernelError> {
        if self.call_stack.len() >= MAX_CALL_DEPTH {
            return Err(call_depth_over_limit());
        }
        let bindings = self.bind_call_args(params, call_args, values, ctx, legs, builtins)?;
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
        // The callee's persistent declarations belong to this call
        // expression's instance, not to the module's top level.
        self.sites.push(CallSite::new(module_key(&caller), site_id));
        // A `break` binds lexically: a body called from inside a loop still
        // refuses a loop-less `break` instead of unwinding the caller.
        let saved_depth = std::mem::replace(&mut self.loop_depth, 0);
        let saved_break = std::mem::replace(&mut self.break_flag, false);
        let value = self.exec_body_value(body, ctx, legs, builtins);
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
                if current
                    .as_ref()
                    .is_some_and(PineValue::is_collection_writeback)
                {
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
        self.sites.pop();
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
            if let Err(error) = self.assign_path(ctx, &target, written, builtins) {
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

        builtins: &mut RunBuiltins,
    ) -> Result<Option<PineValue>, KernelError> {
        let mut value = None;
        for stmt in stmts {
            value = self.exec_stmt_value(stmt, ctx, legs, builtins)?;
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

        builtins: &mut RunBuiltins,
    ) -> Result<Option<PineValue>, KernelError> {
        if let PineExpr::MethodCall {
            namespace,
            method,
            args,
            ..
        } = expr
        {
            if let Some(PineExpr::Variable(target)) = args.first().map(|arg| &arg.value) {
                if namespace == "array" && method == "shift" {
                    return self.exec_shift_stmt(target, args, ctx, legs, builtins);
                }
                if namespace == "map" && method == "remove" {
                    return self.exec_map_remove_stmt(target, args, ctx, legs, builtins);
                }
                if writes_back_collection(namespace, method) {
                    let written = self.eval_expr(expr, ctx, legs, builtins)?;
                    self.assign_path(ctx, target, written.clone(), builtins)?;
                    return Ok(written);
                }
            }
            // Instance-held collection calls (`items.push(x)`) persist to
            // the receiver variable exactly like the namespace form.
            // Split-return mutators (`shift`, `pop`, `remove`) persist
            // the shrunken collection while yielding the removed
            // element; every other mutator persists its own return.
            if let Some(bridge) = self.peek_collection(ctx, namespace) {
                if writes_back_collection(bridge, method) {
                    let receiver = self.eval_expr(
                        &PineExpr::Variable(namespace.to_owned()),
                        ctx,
                        legs,
                        builtins,
                    )?;
                    let written = self.eval_expr(expr, ctx, legs, builtins)?;
                    let key = if bridge == "map" && method == "remove" {
                        match args.first() {
                            Some(arg) => Some(self.eval_expr(&arg.value, ctx, legs, builtins)?),
                            None => None,
                        }
                    } else {
                        None
                    };
                    let stored = super::builtins_core::split_rest(
                        bridge,
                        method,
                        receiver.as_ref(),
                        key.as_ref().and_then(|slot| slot.as_ref()),
                    )?
                    .unwrap_or(written.clone());
                    self.assign_path(ctx, namespace, stored, builtins)?;
                    return Ok(written);
                }
            }
            // Chained collection receivers (`w.items.push(x)`): the call
            // above dispatched through the chain value, so mutators write
            // back through the chain too. An untakable chain keeps the
            // unknown-method refusal the value path raised.
            if namespace.contains('.') {
                let Ok(receiver) = self.eval_expr(
                    &PineExpr::Variable(namespace.to_owned()),
                    ctx,
                    legs,
                    builtins,
                ) else {
                    return self.eval_expr(expr, ctx, legs, builtins);
                };
                let bridge = match &receiver {
                    Some(PineValue::Array(_)) => Some("array"),
                    Some(PineValue::Matrix(_)) => Some("matrix"),
                    Some(PineValue::IntMap(_) | PineValue::Map(_)) => Some("map"),
                    _ => None,
                };
                if let Some(bridge) = bridge {
                    if writes_back_collection(bridge, method) {
                        let written = self.eval_expr(expr, ctx, legs, builtins)?;
                        let key = if bridge == "map" && method == "remove" {
                            match args.first() {
                                Some(arg) => {
                                    Some(self.eval_expr(&arg.value, ctx, legs, builtins)?)
                                }
                                None => None,
                            }
                        } else {
                            None
                        };
                        let stored = super::builtins_core::split_rest(
                            bridge,
                            method,
                            receiver.as_ref(),
                            key.as_ref().and_then(|slot| slot.as_ref()),
                        )?
                        .unwrap_or(written.clone());
                        if !self.assign_nested(ctx, namespace, stored)? {
                            return Err(refused(format!(
                                "unknown method '{namespace}.{method}'"
                            )));
                        }
                        return Ok(written);
                    }
                }
            }
        }
        self.eval_expr(expr, ctx, legs, builtins)
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

        builtins: &mut RunBuiltins,
    ) -> Result<Option<PineValue>, KernelError> {
        if args.len() != 1 {
            return Err(refused("'array.shift' takes one array"));
        }
        let current = self.eval_expr(&args[0].value, ctx, legs, builtins)?;
        match super::builtins_core::shift_parts("array.shift", current.as_ref())? {
            Some((rest, head)) => {
                self.assign_path(ctx, target, Some(PineValue::Array(rest)), builtins)?;
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

        builtins: &mut RunBuiltins,
    ) -> Result<Option<PineValue>, KernelError> {
        if args.len() != 2 {
            return Err(refused("'map.remove' takes a map and key"));
        }
        let current = self.eval_expr(&args[0].value, ctx, legs, builtins)?;
        let key = self.eval_expr(&args[1].value, ctx, legs, builtins)?;
        match super::builtins_core::remove_parts("map.remove", current.as_ref(), key.as_ref())? {
            Some((rest, old)) => {
                self.assign_path(ctx, target, Some(PineValue::IntMap(rest)), builtins)?;
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

        builtins: &mut RunBuiltins,
    ) -> Result<&'a [PineStmt], KernelError> {
        let taken = branch_of(
            "if condition",
            self.eval_expr(condition, ctx, legs, builtins)?.as_ref(),
        )?
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

        builtins: &mut RunBuiltins,
    ) -> Result<Option<PineValue>, KernelError> {
        let value = match stmt {
            PineStmt::Expression(expr) => self.exec_expr_stmt(expr, ctx, legs, builtins)?,
            PineStmt::Switch(switch) => self.eval_switch(switch, ctx, legs, true, builtins)?,
            PineStmt::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let block = self.taken_block(
                    condition,
                    then_branch,
                    else_branch.as_deref(),
                    ctx,
                    legs,
                    builtins,
                )?;
                self.exec_body_value(block, ctx, legs, builtins)?
            }
            _ => {
                self.exec_stmt(stmt, ctx, legs, builtins);
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

        builtins: &mut RunBuiltins,
    ) -> Result<Option<PineValue>, KernelError> {
        let scrutinee = match &switch.scrutinee {
            Some(target) => Some(self.eval_expr(target, ctx, legs, builtins)?),
            None => None,
        };
        for arm in &switch.arms {
            let matched = match &arm.pattern {
                None => true,
                Some(pattern) => {
                    let probe = self.eval_expr(pattern, ctx, legs, builtins)?;
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
                    return self.exec_body_value(&arm.body, ctx, legs, builtins);
                }
                for stmt in &arm.body {
                    self.exec_stmt(stmt, ctx, legs, builtins);
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

/// Variable home of one call argument, when the argument names an
/// assignable home (bare or dotted): collections bound from such homes
/// write back on return, giving Pine reference-type behavior.
/// One bound call argument: the parameter name, its value, and the caller
/// variable it was read from (the home a collection result writes back to).
type BoundArg = (String, Option<PineValue>, Option<String>);

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
pub(crate) fn select_function(
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
fn overload_matches(params: &[Param], args: &[CallArg], values: &[Option<PineValue>]) -> bool {
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
                instance.type_name == *other || trailing_type(other) == instance.type_name.as_str()
            }
            PineValue::EnumHandle(handle) => {
                handle.enum_name == *other || trailing_type(other) == handle.enum_name.as_str()
            }
            _ => false,
        },
    }
}

/// Classify the bare `input(...)` overload from its *syntactic* default.
/// `Some(method)` names a shape whose kind the syntax fixes (a source
/// series, a literal, or a color constructor); `None` means the kind is
/// only known from the run-time value, so the caller asks the shared
/// [`super::builtins_core::input_auto`] table. `None` first argument is
/// the missing-default refusal the caller raises.
fn bare_input_method(first: Option<&PineExpr>) -> Option<&'static str> {
    match first? {
        PineExpr::BuiltinVariable(_) | PineExpr::Variable(_) | PineExpr::SeriesOffset { .. } => {
            Some("source")
        }
        PineExpr::Number(v) => {
            if v.fract() == 0.0 && (-9_007_199_254_740_992.0..=9_007_199_254_740_992.0).contains(v)
            {
                Some("int")
            } else {
                Some("float")
            }
        }
        PineExpr::Boolean(_) => Some("bool"),
        PineExpr::StringLiteral(_) => Some("string"),
        PineExpr::MethodCall {
            namespace, method, ..
        } if namespace == "color" && matches!(method.as_str(), "rgb" | "new" | "from_gradient") => {
            Some("color")
        }
        _ => None,
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

/// One tree-walk slot address for the shared home model.
#[derive(Debug, Clone)]
/// One tree-walk slot address for the shared home model.
enum TreeSlot {
    /// Shared variable namespace entry.
    Variable(String),
    /// Persistent `var` (`is_varip` false) or `varip` (true) cell.
    Cell {
        /// Cell identity of the declaration this take came from.
        scope: ScopeKey,
        /// Spelling the cell was declared with (used for the write-back).
        name: String,
        /// Intrabar lifetime flag.
        is_varip: bool,
    },
}

impl super::home::HomeSlot for TreeSlot {
    fn lease_key(&self) -> Option<LeaseKey> {
        match self {
            Self::Variable(name) => Some(LeaseKey::variables(name)),
            Self::Cell { scope, .. } => Some(LeaseKey::var_store(scope.clone())),
        }
    }
}

/// Where a method-call receiver lives while the call runs: the shared home
/// model over a tree-walk slot address. The loop uses the same model over
/// its own slot vocabulary, so nested rebuilds and map-entry writes can
/// never drift between engines.
type ObjectHome = super::home::Home<TreeSlot>;

/// Tree-walk storage operations for the shared home model. `bump` carries
/// the native-serve discipline: a Pine body invalidates the lease on the
/// root slot it writes, while a natively served call already synced it.
struct TreeStore<'a, 'c> {
    engine: &'a mut EngineState,
    ctx: &'a mut RunCtx<'c>,
    bump: bool,
}

impl super::home::HomeStore<TreeSlot> for TreeStore<'_, '_> {
    fn put_slot(&mut self, slot: &TreeSlot, instance: UserInstance) -> Result<(), KernelError> {
        if self.bump {
            if let Some(key) = slot.lease_key() {
                self.engine.bump_lease_key(key);
            }
        }
        let value = PineValue::Object(instance);
        match slot {
            TreeSlot::Variable(name) => {
                self.engine.variables.insert(name.clone(), Some(value));
            }
            TreeSlot::Cell {
                scope,
                name,
                is_varip,
            } => {
                if *is_varip {
                    self.ctx.vars_mut().set_varip(scope.clone(), name, value);
                } else {
                    self.ctx.vars_mut().set_var(scope.clone(), name, value);
                }
            }
        }
        Ok(())
    }

    fn put_this_field(&mut self, field: &str, instance: UserInstance) -> Result<(), KernelError> {
        let slot = self.engine.receiver.last_mut().ok_or_else(this_unbound)?;
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
        self.engine.write_map_entry(
            self.ctx,
            map_var,
            key,
            key_value,
            PineValue::Object(instance),
        );
        Ok(())
    }
}

/// One live method receiver: the instance plus the alias the body bound
/// it under (`level` in `updateLevelTouch(MarketLevel level, ...)`).
/// Reads, nested calls, and field writes through the alias hit this slot,
/// so the body always observes one live copy, never a stale clone.
#[derive(Debug)]
/// One live method receiver: the instance plus the alias the body bound
/// it under (`level` in `updateLevelTouch(MarketLevel level, ...)`).
/// Reads, nested calls, and field writes through the alias hit this slot,
/// so the body always observes one live copy, never a stale clone.
pub(crate) struct ReceiverSlot {
    instance: UserInstance,
    alias: String,
}

/// Read one named drawing argument (`title`, `color`, `display`, ...):
/// the evaluated value, whatever it is (absence included).
fn named_value(args: &[CallArg], values: &[Option<PineValue>], name: &str) -> Option<PineValue> {
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
    site_string("plot", args)
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

/// Test-only tree oracle behind the evaluator seam: a facade pinned to the
/// tree-walk selection, so differential suites can hold one engine per
/// hand without naming selections. Production code never names this
/// type (pinned by scan); the build gate in a later step compiles it out
/// of shipping builds.
pub struct OracleEngine {
    engine: super::engine::PineEngine,
}

impl OracleEngine {
    /// Bind a tree-only engine to a runtime config, mirroring the
    /// facade constructor with the tree-walk selection forced.
    #[must_use]
    pub fn new(config: crate::runtime::PineRuntimeConfig) -> Self {
        #[cfg(feature = "bytecode")]
        let mut engine = super::engine::PineEngine::new(config);
        #[cfg(not(feature = "bytecode"))]
        let engine = super::engine::PineEngine::new(config);
        #[cfg(feature = "bytecode")]
        engine.set_bytecode_mode(super::engine::BytecodeMode::TreeWalk);
        Self { engine }
    }
}

impl super::engine::PineEvaluator for OracleEngine {
    fn run_linked(
        &mut self,
        modules: &[LinkedModule],
        bars: &[protopyne_types::Bar],
        inputs: crate::inputs::InputSet,
    ) -> Result<super::engine::RunOutcome, KernelError> {
        self.engine.run_linked(modules, bars, inputs)
    }

    fn run_linked_ticks(
        &mut self,
        modules: &[LinkedModule],
        bars: &[protopyne_types::Bar],
        tick_millis: &[i64],
        inputs: crate::inputs::InputSet,
    ) -> Result<super::engine::RunOutcome, KernelError> {
        self.engine
            .run_linked_ticks(modules, bars, tick_millis, inputs)
    }
}