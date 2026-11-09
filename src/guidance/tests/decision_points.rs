use super::{
    authorize_point, decision_key, excerpt_spec, intent_spec, staleness_spec, sufficiency_spec,
    EXCERPT_POINT, EXCERPT_VERSION, INTENT_POINT, INTENT_VERSION, STALENESS_POINT,
    STALENESS_VERSION, SUFFICIENCY_POINT, SUFFICIENCY_VERSION,
};
use fluent_llm::decision::{
    decide_with_ladder, CalibrationStatus, Decision, DecisionBreaker, DecisionCache, DecisionError,
};

fn canned(label: &str, top: f64) -> Decision {
    Decision::from_distribution(
        label,
        vec![(label.to_string(), top), ("other".to_string(), 1.0 - top)],
        "test".to_string(),
        1,
        7,
        false,
    )
    .expect("decision")
}

#[test]
fn specs_validate_and_pin_versions() {
    for (spec, point, version) in [
        (intent_spec(), INTENT_POINT, INTENT_VERSION),
        (sufficiency_spec(), SUFFICIENCY_POINT, SUFFICIENCY_VERSION),
        (excerpt_spec(), EXCERPT_POINT, EXCERPT_VERSION),
        (staleness_spec(), STALENESS_POINT, STALENESS_VERSION),
    ] {
        assert!(spec.validate().is_ok(), "{point}");
        assert_eq!(spec.id, point);
        assert_eq!(spec.version, version);
    }
    assert_eq!(intent_spec().kind, "choice");
    assert_eq!(sufficiency_spec().kind, "choice");
    assert_eq!(excerpt_spec().kind, "score");
    assert_eq!(staleness_spec().kind, "noul");
    // Caches stay cold until a calibration entry authorizes the point.
    assert!(!intent_spec().cacheable);
    assert!(!sufficiency_spec().cacheable);
    assert!(!excerpt_spec().cacheable);
    assert!(!staleness_spec().cacheable);
}

#[test]
fn empty_registry_authorizes_shadow_only() {
    for (point, version) in [
        (INTENT_POINT, INTENT_VERSION),
        (SUFFICIENCY_POINT, SUFFICIENCY_VERSION),
        (EXCERPT_POINT, EXCERPT_VERSION),
        (STALENESS_POINT, STALENESS_VERSION),
    ] {
        let status = authorize_point(point, "unmeasured-model", version);
        assert!(
            matches!(status, CalibrationStatus::ShadowOnly { .. }),
            "{point}"
        );
    }
}

#[test]
fn decision_key_is_fingerprint_scoped() {
    let spec = intent_spec();
    let a = decision_key(
        INTENT_POINT,
        "state",
        "query?",
        &spec,
        "m",
        "http://a",
        None,
    );
    let b = decision_key(
        INTENT_POINT,
        "state",
        "query?",
        &spec,
        "m",
        "http://a",
        Some("fp1".to_string()),
    );
    assert_ne!(a, b);
    assert_eq!(a.len(), 64);
}

#[test]
fn ladder_rule_wins_without_calling_model() {
    let out = fluent_llm::client::block_on(decide_with_ladder(
        Some(canned("RULE", 1.0)),
        &DecisionCache::new(4).expect("cache"),
        "k".to_string(),
        true,
        &DecisionBreaker::new(3, 60_000).expect("breaker"),
        || async { panic!("model must not run when the rule wins") },
        canned("FALLBACK", 1.0),
    ))
    .expect("ladder");
    assert_eq!(out.label, "RULE");
}

#[test]
fn ladder_unavailable_falls_through_to_fallback() {
    let out = fluent_llm::client::block_on(decide_with_ladder(
        None,
        &DecisionCache::new(4).expect("cache"),
        "k".to_string(),
        true,
        &DecisionBreaker::new(3, 60_000).expect("breaker"),
        || async { Err(DecisionError::Unavailable("down".to_string())) },
        canned("FALLBACK", 1.0),
    ))
    .expect("ladder");
    assert_eq!(out.label, "FALLBACK");
}

#[test]
fn ladder_rejected_is_terminal() {
    let err = fluent_llm::client::block_on(decide_with_ladder(
        None,
        &DecisionCache::new(4).expect("cache"),
        "k".to_string(),
        true,
        &DecisionBreaker::new(3, 60_000).expect("breaker"),
        || async { Err(DecisionError::Rejected("bad".to_string())) },
        canned("FALLBACK", 1.0),
    ))
    .expect_err("rejected is terminal");
    assert!(!err.is_unavailable());
}

#[test]
fn degraded_success_answers_once_without_caching() {
    let cache = DecisionCache::new(4).expect("cache");
    let mut degraded = canned("MODEL", 0.6);
    degraded.degraded = true;
    let out = fluent_llm::client::block_on(decide_with_ladder(
        None,
        &cache,
        "k".to_string(),
        true,
        &DecisionBreaker::new(3, 60_000).expect("breaker"),
        || async { Ok(degraded.clone()) },
        canned("FALLBACK", 1.0),
    ))
    .expect("ladder");
    assert_eq!(out.label, "MODEL");
    assert!(cache.get("k").is_none());
}

#[test]
fn spec_thresholds_feed_shadow_without_changing_verdict() {
    use crate::query::strategy::FsmEngine;
    use fluent_llm::decision::{decide_verdict, Verdict};

    let mut engine = FsmEngine::new();
    let plain = engine.run("how do I parse JSON");
    let thresholds = intent_spec().thresholds();
    let verdict_decision = canned("HowTo", 0.97);
    let (shadowed, record) = engine.classify_with_shadow(&plain, Some(&verdict_decision));
    assert_eq!(shadowed.intent, plain.intent);
    let record = record.expect("logged");
    assert_eq!(
        record.would_have_acted,
        decide_verdict(&verdict_decision, &thresholds) == Verdict::Act
    );
}
