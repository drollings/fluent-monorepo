//! AUDIT-H6: wrapper delegation completeness.

use fluent_wvr::prelude::*;
use std::sync::Arc;

#[allow(dead_code)]
struct TimeoutUnit {
    timeout: u64,
}
impl WorkUnit for TimeoutUnit {
    fn name(&self) -> &str {
        "timeout_unit"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        Ok(WorkOutput::ok("done"))
    }
    fn default_timeout_ms(&self) -> u64 {
        5_000
    }
    fn type_name(&self) -> &'static str {
        "CustomTimeoutUnit"
    }
}
impl FieldAccess for TimeoutUnit {
    fn set_field(&mut self, _: &str, _: &str) -> Result<(), FieldError> {
        Err(FieldError::NotFound("none".into()))
    }
    fn get_field(&self, _: &str) -> Result<String, FieldError> {
        Err(FieldError::NotFound("none".into()))
    }
    fn field_names(&self) -> &'static [&'static str] {
        &[]
    }
}
impl Describable for TimeoutUnit {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({})
    }
}
fluent_wvr::impl_component!(TimeoutUnit);

#[test]
fn h6_instrumented_delegates_timeout_and_typename() {
    let wrapped = Instrumented::new(TimeoutUnit { timeout: 0 }, "lbl");
    assert_eq!(wrapped.default_timeout_ms(), 5_000);
    assert_eq!(wrapped.type_name(), "CustomTimeoutUnit", "type_name delegates correctly");
}

#[test]
fn h6_adapter_delegates_timeout() {
    let inner: Arc<dyn Component> = Arc::new(TimeoutUnit { timeout: 0 });
    let adapter = ComponentAdapter::new(inner);
    assert_eq!(adapter.default_timeout_ms(), 5_000);
}

#[test]
fn h6_suffixed_delegates_both() {
    let inner: Arc<dyn Component> = Arc::new(TimeoutUnit { timeout: 0 });
    let ctx = WorkContext::default();
    let s = SuffixedComponent::new(inner, "shard", ctx);
    assert_eq!(s.default_timeout_ms(), 5_000);
    assert_eq!(s.type_name(), "CustomTimeoutUnit");
}

#[test]
fn h6_stacked_wrappers_delegate_timeout() {
    // Instrumented(Adapter(TimeoutUnit)) -> both layers delegate.
    let inner: Arc<dyn Component> = Arc::new(TimeoutUnit { timeout: 0 });
    let adapter = ComponentAdapter::new(inner);
    // Adapter is a Component; wrap in Instrumented generically.
    let stacked = Instrumented::new(adapter, "outer");
    assert_eq!(stacked.default_timeout_ms(), 5_000);
}

#[test]
fn h6_downcast_through_wrapper_fails_characterization() {
    let inner = TimeoutUnit { timeout: 0 };
    let wrapped = Instrumented::new(inner, "lbl");
    let arc: Arc<dyn Component> = Arc::new(wrapped);
    assert!(fluent_wvr::component_downcast_ref::<Instrumented<TimeoutUnit>>(&*arc).is_some());
    assert!(fluent_wvr::component_downcast_ref::<TimeoutUnit>(&*arc).is_none());
}
