//! Shadow wrapper tests (tests-first).

use std::sync::Arc;

use fluent_llm::decision::{
    instrumented_shadow, shadow_adapter, Decision, ShadowLog, ShadowOutcome, ThresholdSet,
};
use fluent_wvr::{Component, WorkContext, WorkOutput, WorkUnit};

struct StubUnit;

impl WorkUnit for StubUnit {
    fn name(&self) -> &str {
        "stub"
    }
    fn depends(&self) -> &[fluent_wvr::ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[fluent_wvr::ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        WorkOutput::typed("stub", &vec!["a".to_string(), "b".to_string()])
    }
}

use fluent_wvr::WorkError;
fluent_wvr::impl_fieldless!(StubUnit);

impl fluent_wvr::Describable for StubUnit {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({"name": "stub"})
    }
}

fluent_wvr::impl_component!(StubUnit);

fn outcome() -> ShadowOutcome {
    let d = Decision::from_distribution(
        "B",
        vec![("B".to_string(), 0.97), ("A".to_string(), 0.03)],
        "m".to_string(),
        1,
        9,
        false,
    )
    .expect("decision");
    ShadowOutcome::record(
        &d,
        &ThresholdSet {
            act: 0.9,
            review: 0.7,
        },
        true,
    )
}

#[test]
fn shadow_never_alters_response() {
    let inner: Arc<dyn Component> = Arc::new(StubUnit);
    let plain = inner.execute(&WorkContext::default()).expect("plain");
    let log = Arc::new(ShadowLog::new());
    let shadowed = shadow_adapter(inner, Arc::clone(&log), outcome());
    let via_shadow = shadowed.execute(&WorkContext::default()).expect("shadow");
    assert_eq!(
        serde_json::to_value(&plain).expect("encode"),
        serde_json::to_value(&via_shadow).expect("encode")
    );
}

#[test]
fn shadow_log_golden_records_would_have_acted() {
    let inner: Arc<dyn Component> = Arc::new(StubUnit);
    let log = Arc::new(ShadowLog::new());
    let shadowed = shadow_adapter(inner, Arc::clone(&log), outcome());
    let _ = shadowed.execute(&WorkContext::default()).expect("shadow");
    let logged = log.outcomes();
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0].label, "B");
    assert!((logged[0].confidence - 0.97).abs() < 1e-9);
    assert!(logged[0].would_have_acted);
    assert_eq!(logged[0].verdict, fluent_llm::decision::Verdict::Act);
}

#[test]
fn instrumented_shadow_times_and_logs() {
    let inner: Arc<dyn Component> = Arc::new(StubUnit);
    let log = Arc::new(ShadowLog::new());
    let hist = Arc::new(common_core::metrics::LatencyHistogram::new());
    let wrapped = instrumented_shadow(
        inner,
        "shadow.test",
        Arc::clone(&hist),
        log.clone(),
        outcome(),
    );
    let out = wrapped.execute(&WorkContext::default()).expect("exec");
    let items: Vec<String> = out.data_as().expect("typed");
    assert_eq!(items, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(hist.count(), 1);
    assert_eq!(log.len(), 1);
}

#[test]
fn inner_error_propagates_without_logging() {
    use fluent_wvr::WorkError;
    struct FailUnit;
    impl WorkUnit for FailUnit {
        fn name(&self) -> &str {
            "fail"
        }
        fn depends(&self) -> &[fluent_wvr::ArcIntern<str>] {
            &[]
        }
        fn provides(&self) -> &[fluent_wvr::ArcIntern<str>] {
            &[]
        }
        fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
            Err(WorkError::Execution("boom".to_string()))
        }
    }
    fluent_wvr::impl_fieldless!(FailUnit);
    impl fluent_wvr::Describable for FailUnit {
        fn describe(&self) -> serde_json::Value {
            serde_json::json!({"name": "fail"})
        }
    }
    fluent_wvr::impl_component!(FailUnit);

    let inner: Arc<dyn Component> = Arc::new(FailUnit);
    let log = Arc::new(ShadowLog::new());
    let shadowed = shadow_adapter(inner, Arc::clone(&log), outcome());
    assert!(shadowed.execute(&WorkContext::default()).is_err());
    assert!(log.is_empty());
}

#[test]
fn shadow_records_carry_no_state_bytes() {
    let inner: Arc<dyn Component> = Arc::new(StubUnit);
    let log = Arc::new(ShadowLog::new());
    let shadowed = shadow_adapter(inner, Arc::clone(&log), outcome());
    let _ = shadowed.execute(&WorkContext::default()).expect("exec");
    let encoded = format!("{:?}", log.outcomes());
    assert!(!encoded.contains("hostile-state-marker"));
}
