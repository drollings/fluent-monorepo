//! AUDIT-H1: shared-handle mutation inconsistency.
//! Desired behavior: exclusive vs shared set_field, adapter override validation.
//! Characterization tests pin current behavior; desired tests marked #[ignore].

use fluent_wvr::prelude::*;
use fluent_wvr::{Component, FieldAccess, FieldError};
use std::sync::Arc;

#[derive(fluent_wvr::FieldAccess, fluent_wvr::Describable)]
struct AuditPort {
    #[field(desc = "port", min = 1, max = 65535)]
    port: u16,
}

impl WorkUnit for AuditPort {
    fn name(&self) -> &str {
        "audit_port"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        Ok(WorkOutput::ok(format!("port={}", self.port)))
    }
}
fluent_wvr::impl_component!(AuditPort);

#[test]
fn h1a_exclusive_set_field_validates() {
    let mut c = AuditPort { port: 8079 };
    // valid
    c.set_field("port", "9000").unwrap();
    assert_eq!(c.port, 9000);
    // invalid below min
    let err = c.set_field("port", "0").unwrap_err();
    assert!(matches!(err, FieldError::Constraint(_)), "got {err:?}");
    // value unchanged after rejected write
    assert_eq!(c.port, 9000);
}

#[test]
fn h1a_shared_arc_set_field_is_readonly() {
    let arc: Arc<dyn Component> = Arc::new(AuditPort { port: 8079 });
    let _clone = Arc::clone(&arc);
    let mut arc_mut = arc;
    match arc_mut.set_field("port", "9000") {
        Err(FieldError::ReadOnly(_, _)) => {}
        other => panic!("expected ReadOnly, got {other:?}"),
    }
}

/// Fail-closed H1b: over a shared inner the adapter cannot validate (no
/// exclusive access), so it returns `ReadOnly` without storing — mirroring
/// the `Arc<dyn Component>` blanket impl. Exclusive inners still validate
/// (see the test below).
#[test]
fn h1b_adapter_shared_invalid_must_err_desired() {
    let inner: Arc<dyn Component> = Arc::new(AuditPort { port: 8079 });
    let _clone = Arc::clone(&inner);
    let mut adapter = ComponentAdapter::new(inner);
    let err = adapter.set_field("port", "0").unwrap_err();
    assert!(
        matches!(err, FieldError::ReadOnly(_, _)),
        "shared adapter write must fail closed, got {err:?}"
    );
    // override must NOT be stored
    assert_eq!(adapter.get_field("port").unwrap(), "8079");
}

#[test]
fn h1b_adapter_exclusive_validates_and_stores() {
    // exclusive inner (count==1): validates via inner, then stores
    let inner: Arc<dyn Component> = Arc::new(AuditPort { port: 8079 });
    let mut adapter = ComponentAdapter::new(inner);
    adapter.set_field("port", "9000").unwrap();
    assert_eq!(adapter.get_field("port").unwrap(), "9000");
    let err = adapter.set_field("port", "99999").unwrap_err();
    // 99999 > max 65535 -> Constraint (via f64 bound check); must NOT store
    assert!(matches!(err, FieldError::Constraint(_)), "got {err:?}");
    assert_eq!(adapter.get_field("port").unwrap(), "9000");
}

/// Shared inner, *valid* value: still `Err(ReadOnly)`, nothing stored.
/// A valid value must not fork the source of truth either.
#[test]
fn h1b_adapter_shared_valid_still_readonly_no_store() {
    let inner: Arc<dyn Component> = Arc::new(AuditPort { port: 8079 });
    let _clone = Arc::clone(&inner); // strong count >= 2 -> Arc::get_mut fails
    let mut adapter = ComponentAdapter::new(inner);
    match adapter.set_field("port", "9000") {
        Err(FieldError::ReadOnly(_, _)) => {}
        other => panic!("expected ReadOnly, got {other:?}"),
    }
    // No override stored: get_field falls through to the inner value.
    assert_eq!(adapter.get_field("port").unwrap(), "8079");
}

/// Control: the explicit `with_field_override` builder remains the
/// deliberate unchecked path, even over a shared inner.
#[test]
fn h1b_with_field_override_builder_stays_unchecked_bypass() {
    let inner: Arc<dyn Component> = Arc::new(AuditPort { port: 8079 });
    let _clone = Arc::clone(&inner);
    let adapter = ComponentAdapter::new(inner).with_field_override("port", "0");
    assert_eq!(adapter.get_field("port").unwrap(), "0");
    // The bypass is explicit: execution still sees the inner value.
    let out = adapter.execute(&WorkContext::default()).unwrap();
    assert!(
        out.message.contains("8079"),
        "bypass stores the override; execute reads the inner: {}",
        out.message
    );
}
