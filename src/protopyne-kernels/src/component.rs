//! Indicator-as-`Component`: native kernels as supervised units.
//!
//! [`IndicatorComponent`] binds one [`Indicator`] with its [`ParamSet`] and
//! window ([`OwnedOhlcv`]) behind the `Component` contract: `name` is the
//! spec id, and `depends`/`provides` are wired from the spec's dependency
//! list (interned at construction) — the same derivation as the batch-only
//! [`KernelUnit`](crate::timed::KernelUnit) — so a `SupervisedBatch` orders
//! indicators through the same channel as every other supervised stage. Timing still composes
//! `Instrumented::with_metrics` at the registration site (see
//! [`instrumented_kernel`](crate::timed::instrumented_kernel)).
//!
//! [`ComponentRegistry`] is the shared-handle counterpart to the
//! instance-owned [`IndicatorRegistry`](crate::indicator::IndicatorRegistry):
//! setup-time registration with duplicate-refusal over a
//! [`ConcurrentRegistry`], concurrent lock-free reads, and ordering through
//! the canonical [`DependencyGraph`](fluent_dag::dep_graph::DependencyGraph).
//! Doubles compose [`ComponentAdapter`] and runtime-assembled handles compose
//! [`DynamicComponent`] — both from `fluent-wvr`, never re-implemented here.
//!
//! [`ComponentAdapter`]: fluent_wvr::wrapper::ComponentAdapter
//! [`DynamicComponent`]: fluent_wvr::dynamic::DynamicComponent
//! [`ConcurrentRegistry`]: common_core::registry::ConcurrentRegistry

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use common_core::registry::ConcurrentRegistry;
use fluent_dag::dep_graph::GraphError;
use fluent_wvr::prelude::*;

use crate::error::KernelError;
use crate::indicator::{Indicator, IndicatorId, IndicatorResult, OwnedOhlcv, ParamSet};

/// One bound kernel evaluation as a `Component`: indicator + parameters +
/// window, assembled via [`builder`](Self::builder).
pub struct IndicatorComponent {
    indicator: Arc<dyn Indicator>,
    params: ParamSet,
    input: Arc<OwnedOhlcv>,
    depends: Vec<ArcIntern<str>>,
    provides: Vec<ArcIntern<str>>,
}

/// Builder for [`IndicatorComponent`]: parameters accumulate through the
/// `param_*` setters (the `ParamSet` builder shape), the window attaches
/// with [`input`](Self::input), and [`build`](Self::build) binds the unit.
pub struct IndicatorComponentBuilder {
    indicator: Arc<dyn Indicator>,
    params: ParamSet,
    input: Option<Arc<OwnedOhlcv>>,
}

impl IndicatorComponentBuilder {
    /// Add an int parameter.
    #[must_use]
    pub fn param_int(mut self, name: impl Into<String>, value: i64) -> Self {
        self.params = std::mem::take(&mut self.params).with_int(name, value);
        self
    }

    /// Add a float parameter.
    #[must_use]
    pub fn param_float(mut self, name: impl Into<String>, value: f64) -> Self {
        self.params = std::mem::take(&mut self.params).with_float(name, value);
        self
    }

    /// Add a bool parameter.
    #[must_use]
    pub fn param_bool(mut self, name: impl Into<String>, value: bool) -> Self {
        self.params = std::mem::take(&mut self.params).with_bool(name, value);
        self
    }

    /// Attach the window under evaluation.
    #[must_use]
    pub fn input(mut self, input: Arc<OwnedOhlcv>) -> Self {
        self.input = Some(input);
        self
    }

    /// Bind the component (an absent window evaluates over no bars).
    #[must_use]
    pub fn build(self) -> IndicatorComponent {
        IndicatorComponent::new(self.indicator, self.params, self.input.unwrap_or_default())
    }
}

impl IndicatorComponent {
    /// Bind an indicator with parameters and a window. `depends`/`provides`
    /// are interned from the spec once, here, so per-`execute` dispatch pays
    /// no allocation.
    #[must_use]
    pub fn new(indicator: Arc<dyn Indicator>, params: ParamSet, input: Arc<OwnedOhlcv>) -> Self {
        let depends = crate::kernel_eval::kernel_depends(&indicator);
        let provides = crate::kernel_eval::kernel_provides(&indicator);
        Self {
            indicator,
            params,
            input,
            depends,
            provides,
        }
    }

    /// Start builder assembly for `indicator`.
    #[must_use]
    pub fn builder(indicator: Arc<dyn Indicator>) -> IndicatorComponentBuilder {
        IndicatorComponentBuilder {
            indicator,
            params: ParamSet::new(),
            input: None,
        }
    }
}

impl WorkUnit for IndicatorComponent {
    fn name(&self) -> &str {
        &self.indicator.spec().id
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &self.depends
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &self.provides
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let result = self
            .indicator
            .batch(&self.input.views(), &self.params)
            .map_err(WorkError::execution_display)?;
        let warmed = crate::kernel_eval::warmed_count(&result);
        Ok(WorkOutput::ok_with_data(
            crate::kernel_eval::kernel_message(&self.indicator.spec().id, result.len(), warmed),
            serde_json::to_value(&result).unwrap_or(serde_json::Value::Null),
        ))
    }
}

// Parameters live in a dynamic `ParamSet` map (Pine-typed values by name),
// not in static struct fields, so the derive macro has no fields to read:
// the `FieldAccess` surface mirrors the batch adapter (the `indicator` id
// reads back; everything else is not a static field).
impl FieldAccess for IndicatorComponent {
    fn set_field(&mut self, name: &str, _value: &str) -> Result<(), FieldError> {
        Err(FieldError::NotFound(name.into()))
    }

    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        match name {
            "indicator" => Ok(self.indicator.spec().id.clone()),
            _ => Err(FieldError::NotFound(name.into())),
        }
    }

    fn field_names(&self) -> &'static [&'static str] {
        &["indicator"]
    }
}

impl Describable for IndicatorComponent {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "indicator": {
                    "type": "string",
                    "description": "Registry id of the evaluated kernel"
                }
            },
            "required": ["indicator"]
        })
    }
}

impl_component!(IndicatorComponent);

/// Shared-handle indicator registry with duplicate-refusal.
///
/// Setup-time `register` refuses a second indicator under one id (the same
/// [`KernelError::DuplicateIndicator`] the instance registry reports);
/// `get`/`ids` are concurrent-reader safe through the shared handle.
/// Ordering composes the canonical dependency graph over the registered
/// specs — the same `topo_sort_from` discipline as
/// [`execution_order`](crate::indicator::execution_order), over shared
/// handles instead of an owned map.
///
/// Values are shared as `Arc`, so `get` is one atomic bump — the same cost
/// as the map it replaces. `ConcurrentRegistry` stores `Arc<V>` itself, so
/// the value type is a thin `SharedIndicator` newtype rather than a
/// doubly-wrapped `Arc<Arc<…>>`.
pub struct ComponentRegistry {
    inner: ConcurrentRegistry<IndicatorId, SharedIndicator>,
}

/// One registry-held indicator behind the shared handle.
struct SharedIndicator(Arc<dyn Indicator>);

impl ComponentRegistry {
    /// Create an empty shared registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: ConcurrentRegistry::new(),
        }
    }

    /// Register an indicator; a second registration under one id is refused.
    ///
    /// Setup-time use (registrations race only if two threads register the
    /// same id concurrently — startup code registers before sharing).
    pub fn register(&self, indicator: Arc<dyn Indicator>) -> Result<(), KernelError> {
        let id = indicator.spec().id.clone();
        // First-wins under the write lock: `try_insert` refuses without
        // storing, so two threads racing one id cannot both pass (M8.1).
        // `get` + `insert` could; `insert` would replace — never use it here.
        self.inner
            .try_insert(id.clone(), SharedIndicator(indicator))
            .map_err(|_| KernelError::DuplicateIndicator(id))
    }

    /// Look up an indicator by id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<Arc<dyn Indicator>> {
        self.inner.get(id).map(|shared| Arc::clone(&shared.0))
    }

    /// Registered ids in sorted order.
    #[must_use]
    pub fn ids(&self) -> Vec<IndicatorId> {
        let mut ids = self.inner.keys();
        ids.sort();
        ids
    }

    /// Number of registered indicators.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether no indicators are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Load every builtin kernel (delegates to the canonical builtin list —
    /// no second catalog lives here).
    pub fn from_builtins() -> Result<Self, KernelError> {
        let builtins = crate::indicator::builtin_registry()?;
        let registry = Self::new();
        let mut ids = builtins.ids();
        ids.sort();
        for id in ids {
            if let Some(indicator) = builtins.get(&id) {
                registry.register(Arc::clone(indicator))?;
            }
        }
        Ok(registry)
    }

    /// The process-wide shared component registry: [`Self::from_builtins`]
    /// built once and shared by every reader.
    ///
    /// Same reasoning as
    /// [`shared_registry`](crate::indicator::shared_registry): the entries are
    /// stateless kernels behind concurrent handles, so one registration per
    /// process is enough and two readers cannot tread on each other.
    /// [`Self::from_builtins`] stays the owned constructor for callers that
    /// need their own registration set (a test double, say).
    ///
    /// # Errors
    ///
    /// The same construction refusals [`Self::from_builtins`] can report. A
    /// failed build installs nothing, so a later call retries instead of
    /// inheriting a poisoned registry.
    pub fn shared() -> Result<&'static ComponentRegistry, KernelError> {
        static SHARED: OnceLock<ComponentRegistry> = OnceLock::new();
        if let Some(registry) = SHARED.get() {
            return Ok(registry);
        }
        let built = Self::from_builtins()?;
        // A racing caller may win; either way both read the winner.
        let _ = SHARED.set(built);
        Ok(SHARED
            .get()
            .expect("the shared component registry was installed above"))
    }

    /// Order `requested` indicators so dependencies execute first. Unknown
    /// requested ids and dependencies on unregistered indicators are
    /// [`GraphError::NodeNotFound`]; loops surface as
    /// [`GraphError::CircularDependency`].
    pub fn execution_order(
        &self,
        requested: &[IndicatorId],
    ) -> Result<Vec<IndicatorId>, GraphError> {
        crate::order::execution_order_from(self, requested)
    }
}

impl crate::order::Edged for ComponentRegistry {
    fn edges(&self) -> Vec<crate::order::KernelEdges> {
        self.ids()
            .into_iter()
            .filter_map(|id| {
                let indicator = self.get(&id)?;
                let spec = indicator.spec();
                Some(crate::order::KernelEdges {
                    id,
                    depends: spec.depends.clone(),
                    provides: spec.provides(),
                })
            })
            .collect()
    }

    fn holds(&self, id: &IndicatorId) -> bool {
        self.get(id).is_some()
    }
}

impl Default for ComponentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a test double over `base`: `execute` returns the canned `output`
/// while `name`/`depends`/`provides` still describe the doubled kernel.
/// Canning cannot fail in practice; a serialization failure still reports
/// loudly in the message instead of succeeding with a null payload.
#[must_use]
pub fn indicator_double(base: Arc<dyn Component>, output: &IndicatorResult) -> ComponentAdapter {
    let name = base.name().to_owned();
    let (payload, message) = match serde_json::to_value(output) {
        Ok(payload) => (payload, format!("doubled {name}")),
        Err(e) => (
            serde_json::Value::Null,
            format!("doubled {name} (payload serialization failed: {e})"),
        ),
    };
    ComponentAdapter::new(base).with_execute_override(Arc::new(move |_ctx| {
        Ok(WorkOutput::ok_with_data(message.clone(), payload.clone()))
    }))
}

/// A runtime-assembled handle running a bound kernel evaluation: the
/// [`DynamicComponent`] seam for indicators constructed after startup
/// (config-driven assembly, late-bound kernels). The executor runs the bound
/// batch; the config map records the indicator id for inspection.
#[must_use]
pub fn dynamic_indicator(
    indicator: Arc<dyn Indicator>,
    params: ParamSet,
    input: Arc<OwnedOhlcv>,
) -> DynamicComponent {
    let id = indicator.spec().id.clone();
    let label = id.clone();
    let executor: DynamicExecutor = Arc::new(move |_ctx, _config| {
        let result = indicator
            .batch(&input.views(), &params)
            .map_err(WorkError::execution_display)?;
        WorkOutput::typed(format!("dynamic {label} evaluated"), &result)
    });
    let mut config = HashMap::new();
    config.insert("indicator".to_string(), id.clone());
    DynamicComponent::new(id, executor).with_config(config)
}

#[cfg(test)]
#[path = "../tests/component.rs"]
mod tests;
