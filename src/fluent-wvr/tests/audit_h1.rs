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

/// Characterization: adapter over shared inner stores override WITHOUT validation.
/// Desired: should return Err and NOT store (see ignored test below).
#[test]
fn h1b_adapter_shared_stores_invalid_without_validation_characterization() {
    let inner: Arc<dyn Component> = Arc::new(AuditPort { port: 8079 });
    let _clone = Arc::clone(&inner); // strong count >= 2 -> Arc::get_mut fails
    let mut adapter = ComponentAdapter::new(inner);
    // "0" is below min=1 -> inner would reject, but adapter currently stores anyway.
    let res = adapter.set_field("port", "0");
    // PIN current behavior:
    assert!(res.is_ok(), "characterization: currently Ok, desired is Err");
    assert_eq!(adapter.get_field("port").unwrap(), "0");
    // DIVERGENCE: inner still holds old value; execute sees old value.
    assert_eq!(adapter.inner().get_field("port").unwrap(), "8079");
    let out = adapter.execute(&WorkContext::default()).unwrap();
    assert!(
        out.message.contains("8079"),
        "execute sees inner value, not override: {}",
        out.message
    );
}

/// Desired behavior for H1b: adapter must validate even when shared.
/// Currently fails -> marked ignore until fix.
#[test]
#[ignore = "AUDIT-H1: adapter stores unvalidated override when shared; want Err + no store"]
fn h1b_adapter_shared_invalid_must_err_desired() {
    let inner: Arc<dyn Component> = Arc::new(AuditPort { port: 8079 });
    let _clone = Arc::clone(&inner);
    let mut adapter = ComponentAdapter::new(inner);
    let err = adapter.set_field("port", "0").unwrap_err();
    assert!(matches!(
        err,
        FieldError::Constraint(_) | FieldError::Parse(_)
    ));
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
