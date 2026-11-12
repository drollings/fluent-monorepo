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
fn loaded_registry_authorizes_where_empty_stays_shadow() {
    use crate::query::decision_points::{
        authorize_point_advisory_with, authorize_point_pinned_with, authorize_point_with,
    };
    use crate::query_engine::QueryEngine;
    use fluent_llm::decision::{
        CalibrationRegistry, CorpusProvenance, GateReport, RegistryEntry, ThresholdSet,
    };
    let entry = RegistryEntry {
        point: INTENT_POINT.to_string(),
        model_fingerprint: "test-model".to_string(),
        question_version: INTENT_VERSION,
        thresholds: ThresholdSet {
            act: 1.0,
            review: 1.0,
        },
        gate: GateReport {
            threshold: 1.0,
            precision: 1.0,
            recall: 1.0,
            fpr: 0.0,
            support: 12,
            control_fires: 0,
            passed: true,
            date: "2026-11-10".to_string(),
        },
        provenance: CorpusProvenance {
            source: "test".to_string(),
            labeller: "test".to_string(),
            date: "2026-11-10".to_string(),
            dataset: "abc123".to_string(),
        },
        dataset: "abc123".to_string(),
    };
    let mut registry = CalibrationRegistry::empty();
    // Empty registry: every flavor fails closed.
    assert!(
        authorize_point_with(&registry, INTENT_POINT, "test-model", INTENT_VERSION)
            .is_shadow_only()
    );
    assert!(
        authorize_point_advisory_with(&registry, INTENT_POINT, "test-model", INTENT_VERSION)
            .is_shadow_only()
    );
    assert!(authorize_point_pinned_with(
        &registry,
        INTENT_POINT,
        "test-model",
        INTENT_VERSION,
        "abc123"
    )
    .is_shadow_only());
    assert!(QueryEngine::new()
        .authorize(INTENT_POINT, "test-model", INTENT_VERSION)
        .is_shadow_only());
    // Loaded entry: same point authorizes; wrong model, version, or
    // dataset still fails closed, and the engine consults its registry.
    registry.upsert(entry);
    assert!(
        authorize_point_with(&registry, INTENT_POINT, "test-model", INTENT_VERSION).is_calibrated()
    );
    assert!(
        authorize_point_advisory_with(&registry, INTENT_POINT, "test-model", INTENT_VERSION)
            .is_advisory()
    );
    assert!(authorize_point_pinned_with(
        &registry,
        INTENT_POINT,
        "test-model",
        INTENT_VERSION,
        "abc123"
    )
    .is_calibrated());
    assert!(
        authorize_point_with(&registry, INTENT_POINT, "other-model", INTENT_VERSION)
            .is_shadow_only()
    );
    assert!(
        authorize_point_with(&registry, INTENT_POINT, "test-model", INTENT_VERSION + 1)
            .is_shadow_only()
    );
    assert!(authorize_point_pinned_with(
        &registry,
        INTENT_POINT,
        "test-model",
        INTENT_VERSION,
        "rotated"
    )
    .is_shadow_only());
    let engine = QueryEngine::new().with_calibration(registry);
    assert!(engine
        .authorize(INTENT_POINT, "test-model", INTENT_VERSION)
        .is_calibrated());
    assert!(engine
        .authorize(INTENT_POINT, "other-model", INTENT_VERSION)
        .is_shadow_only());
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

#[test]
fn summary_index_spec_pins_version() {
    use super::{
        summary_index_fingerprint, summary_index_spec, SUMMARY_INDEX_POINT, SUMMARY_INDEX_VERSION,
    };
    let spec = summary_index_spec();
    assert!(spec.validate().is_ok());
    assert_eq!(spec.id, SUMMARY_INDEX_POINT);
    assert_eq!(spec.version, SUMMARY_INDEX_VERSION);
    assert!(!spec.cacheable);
    assert!(!summary_index_fingerprint("test-model").is_empty());
    assert_ne!(
        summary_index_fingerprint("model-a"),
        summary_index_fingerprint("model-b")
    );
}

fn summary_corpus(passing: bool) -> fluent_llm::decision::Corpus {
    use fluent_llm::decision::{CaseProvenance, Corpus, CorpusProvenance, ScoredCase};
    let provenance = CaseProvenance::human("guidance-test", "2026-11-11");
    let control_score = if passing { 0.20 } else { 0.95 };
    Corpus {
        point: super::SUMMARY_INDEX_POINT.to_string(),
        model_fingerprint: super::summary_index_fingerprint("test-model"),
        question_version: super::SUMMARY_INDEX_VERSION,
        provenance: CorpusProvenance {
            source: "summary route calibration".to_string(),
            labeller: "guidance-test".to_string(),
            date: "2026-11-11".to_string(),
            dataset: String::new(),
        },
        cases: vec![
            ScoredCase {
                score: 0.95,
                expected: true,
                control: false,
                provenance: provenance.clone(),
            },
            ScoredCase {
                score: 0.92,
                expected: true,
                control: false,
                provenance: provenance.clone(),
            },
            ScoredCase {
                score: control_score,
                expected: false,
                control: true,
                provenance,
            },
        ],
    }
}

#[test]
fn summary_index_fails_closed_without_entry() {
    use super::{authorize_point_with, SUMMARY_INDEX_POINT, SUMMARY_INDEX_VERSION};
    use fluent_llm::decision::{CalibrationRegistry, CalibrationStatus};
    let registry = CalibrationRegistry::empty();
    let status = authorize_point_with(
        &registry,
        SUMMARY_INDEX_POINT,
        "test-model",
        SUMMARY_INDEX_VERSION,
    );
    assert!(matches!(status, CalibrationStatus::ShadowOnly { .. }));
    let engine = crate::query_engine::QueryEngine::new().with_calibration(registry);
    assert_eq!(
        engine.summary_mode("test-model"),
        crate::query::recall::SummaryMode::Shadow
    );
}

#[test]
fn summary_index_promotes_only_through_authorize() {
    use super::{authorize_point_with, summary_index_fingerprint};
    use super::{SUMMARY_INDEX_POINT, SUMMARY_INDEX_VERSION};
    use fluent_llm::decision::{CalibrationRegistry, CalibrationStatus, RegistryEntry};
    let fingerprint = summary_index_fingerprint("test-model");
    let failing = summary_corpus(false);
    let rejected = RegistryEntry::calibrate(
        SUMMARY_INDEX_POINT,
        &fingerprint,
        SUMMARY_INDEX_VERSION,
        &failing,
        0.9,
    )
    .expect("calibrate");
    assert!(
        !rejected.gate.passed,
        "a firing control must not pass the gate"
    );
    let passing = summary_corpus(true);
    let entry = RegistryEntry::calibrate(
        SUMMARY_INDEX_POINT,
        &fingerprint,
        SUMMARY_INDEX_VERSION,
        &passing,
        0.9,
    )
    .expect("calibrate");
    assert!(
        entry.gate.passed,
        "clean corpus must pass: {:?}",
        entry.gate
    );
    let mut registry = CalibrationRegistry::empty();
    registry.upsert(entry);
    let status = authorize_point_with(
        &registry,
        SUMMARY_INDEX_POINT,
        &fingerprint,
        SUMMARY_INDEX_VERSION,
    );
    assert!(matches!(status, CalibrationStatus::Calibrated(_)));
    let engine = crate::query_engine::QueryEngine::new().with_calibration(registry);
    assert_eq!(
        engine.summary_mode("test-model"),
        crate::query::recall::SummaryMode::Act
    );
    assert_eq!(
        engine.summary_mode("other-model"),
        crate::query::recall::SummaryMode::Shadow,
        "authorization binds the measured model"
    );
}
