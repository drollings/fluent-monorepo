use super::{evaluate_with_metrics, DecisionMetrics, LadderOutcome};
use fluent_llm::decision::{Decision, DecisionBreaker, DecisionCache, DecisionError};

fn canned(label: &str) -> Decision {
    Decision::from_distribution(
        label,
        vec![(label.to_string(), 1.0), ("other".to_string(), 0.0)],
        "test".to_string(),
        1,
        0,
        false,
    )
    .expect("decision")
}

fn breaker() -> DecisionBreaker {
    DecisionBreaker::new(3, 60_000).expect("breaker")
}

fn drive(
    metrics: &DecisionMetrics,
    rule: Option<Decision>,
    cache: &DecisionCache,
    key: &str,
    model: impl FnOnce() -> std::future::Ready<Result<Decision, DecisionError>> + Send,
    shadow_agreed: Option<bool>,
) -> Result<Decision, DecisionError> {
    fluent_llm::client::block_on(evaluate_with_metrics(
        metrics,
        rule,
        cache,
        key.to_string(),
        true,
        &breaker(),
        model,
        canned("FALLBACK"),
        shadow_agreed,
    ))
}

#[test]
fn counters_move_in_the_right_direction() {
    let metrics = DecisionMetrics::new();
    metrics.record(LadderOutcome::Rule);
    metrics.record(LadderOutcome::Rule);
    metrics.record(LadderOutcome::CacheHit);
    metrics.record(LadderOutcome::Model);
    metrics.record(LadderOutcome::Fallback);
    metrics.record(LadderOutcome::Dropped);
    let snap = metrics.snapshot();
    assert_eq!(snap.rule_wins, 2);
    assert_eq!(snap.cache_hits, 1);
    assert_eq!(snap.model_answers, 1);
    assert_eq!(snap.fallbacks, 1);
    assert_eq!(snap.drops, 1);
    assert_eq!(snap.shadow_disagreements, 0);
}

#[test]
fn shadow_disagreement_counts_only_disagreement() {
    let metrics = DecisionMetrics::new();
    metrics.record_shadow(true);
    metrics.record_shadow(false);
    metrics.record_shadow(false);
    assert_eq!(metrics.snapshot().shadow_disagreements, 2);
}

#[test]
fn latency_flows_through_the_shared_histogram() {
    let metrics = DecisionMetrics::new();
    metrics.observe_latency_ms(10);
    metrics.observe_latency_ms(30);
    let snap = metrics.snapshot();
    assert_eq!(snap.latency_samples, 2);
    assert!(snap.latency_p50_ms <= snap.latency_p99_ms);
}

#[test]
fn snapshot_round_trips_for_health_output() {
    // Parser-safe: the health JSON gains one additive object key with
    // stable field names; existing keys are untouched.
    let metrics = DecisionMetrics::new();
    metrics.record(LadderOutcome::Fallback);
    let value = serde_json::to_value(metrics.snapshot()).expect("serialize");
    assert_eq!(value["fallbacks"], serde_json::json!(1));
    assert_eq!(value["rule_wins"], serde_json::json!(0));
    let back: super::DecisionMetricsSnapshot = serde_json::from_value(value).expect("parse");
    assert_eq!(back.fallbacks, 1);
}

#[test]
fn zero_snapshot_renders_without_traffic() {
    let snap = DecisionMetrics::new().snapshot();
    assert_eq!(snap.rule_wins, 0);
    assert_eq!(snap.latency_samples, 0);
    assert_eq!(snap.latency_p50_ms, 0);
}

// Each ladder rung records exactly its own counter once per evaluation,
// plus one latency sample; every other counter stays zero.

#[test]
fn rule_win_records_rule_once() {
    let metrics = DecisionMetrics::new();
    let cache = DecisionCache::new(4).expect("cache");
    let out = drive(
        &metrics,
        Some(canned("RULE")),
        &cache,
        "k",
        || std::future::ready(Err(DecisionError::Unavailable("must not run".to_string()))),
        None,
    )
    .expect("ladder");
    assert_eq!(out.label, "RULE");
    let snap = metrics.snapshot();
    assert_eq!(snap.rule_wins, 1);
    assert_eq!(snap.cache_hits, 0);
    assert_eq!(snap.model_answers, 0);
    assert_eq!(snap.fallbacks, 0);
    assert_eq!(snap.drops, 0);
    assert_eq!(snap.shadow_disagreements, 0);
    assert_eq!(snap.latency_samples, 1);
}

#[test]
fn cache_hit_records_cache_hit_once() {
    let metrics = DecisionMetrics::new();
    let cache = DecisionCache::new(4).expect("cache");
    cache.insert("k".to_string(), canned("CACHED"));
    let out = drive(
        &metrics,
        None,
        &cache,
        "k",
        || std::future::ready(Err(DecisionError::Unavailable("must not run".to_string()))),
        None,
    )
    .expect("ladder");
    assert_eq!(out.label, "CACHED");
    let snap = metrics.snapshot();
    assert_eq!(snap.cache_hits, 1);
    assert_eq!(snap.rule_wins, 0);
    assert_eq!(snap.model_answers, 0);
    assert_eq!(snap.fallbacks, 0);
    assert_eq!(snap.drops, 0);
    assert_eq!(snap.latency_samples, 1);
}

#[test]
fn model_answer_records_model_once() {
    let metrics = DecisionMetrics::new();
    let cache = DecisionCache::new(4).expect("cache");
    let out = drive(
        &metrics,
        None,
        &cache,
        "k",
        || std::future::ready(Ok(canned("MODEL"))),
        None,
    )
    .expect("ladder");
    assert_eq!(out.label, "MODEL");
    let snap = metrics.snapshot();
    assert_eq!(snap.model_answers, 1);
    assert_eq!(snap.rule_wins, 0);
    assert_eq!(snap.cache_hits, 0);
    assert_eq!(snap.fallbacks, 0);
    assert_eq!(snap.drops, 0);
    assert_eq!(snap.latency_samples, 1);
}

#[test]
fn unavailable_model_records_fallback_once() {
    let metrics = DecisionMetrics::new();
    let cache = DecisionCache::new(4).expect("cache");
    let out = drive(
        &metrics,
        None,
        &cache,
        "k",
        || std::future::ready(Err(DecisionError::Unavailable("down".to_string()))),
        None,
    )
    .expect("ladder");
    assert_eq!(out.label, "FALLBACK");
    let snap = metrics.snapshot();
    assert_eq!(snap.fallbacks, 1);
    assert_eq!(snap.rule_wins, 0);
    assert_eq!(snap.cache_hits, 0);
    assert_eq!(snap.model_answers, 0);
    assert_eq!(snap.drops, 0);
    assert_eq!(snap.latency_samples, 1);
}

#[test]
fn degraded_model_records_dropped_once() {
    let metrics = DecisionMetrics::new();
    let cache = DecisionCache::new(4).expect("cache");
    let mut degraded = canned("MODEL");
    degraded.degraded = true;
    let out = drive(
        &metrics,
        None,
        &cache,
        "k",
        || std::future::ready(Ok(degraded.clone())),
        None,
    )
    .expect("ladder");
    assert_eq!(out.label, "MODEL");
    let snap = metrics.snapshot();
    assert_eq!(snap.drops, 1);
    assert_eq!(snap.model_answers, 0);
    assert_eq!(snap.fallbacks, 0);
    assert_eq!(snap.latency_samples, 1);
}

#[test]
fn rejected_model_records_dropped_and_propagates() {
    let metrics = DecisionMetrics::new();
    let cache = DecisionCache::new(4).expect("cache");
    let err = drive(
        &metrics,
        None,
        &cache,
        "k",
        || std::future::ready(Err(DecisionError::Rejected("bad".to_string()))),
        None,
    )
    .expect_err("rejected is terminal");
    assert!(!err.is_unavailable());
    let snap = metrics.snapshot();
    assert_eq!(snap.drops, 1);
    assert_eq!(snap.model_answers, 0);
    assert_eq!(snap.fallbacks, 0);
    assert_eq!(snap.latency_samples, 1);
}

#[test]
fn shadow_agreement_flows_through_evaluation() {
    for (agreed, expected) in [(Some(false), 1), (Some(true), 0), (None, 0)] {
        let metrics = DecisionMetrics::new();
        let cache = DecisionCache::new(4).expect("cache");
        drive(
            &metrics,
            Some(canned("RULE")),
            &cache,
            "k",
            || std::future::ready(Err(DecisionError::Unavailable("must not run".to_string()))),
            agreed,
        )
        .expect("ladder");
        let snap = metrics.snapshot();
        assert_eq!(snap.shadow_disagreements, expected);
        assert_eq!(snap.rule_wins, 1);
    }
}
