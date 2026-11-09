//! Breaker + ladder composition tests (tests-first).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use fluent_llm::decision::{
    decide_with_ladder, Decision, DecisionBreaker, DecisionCache, DecisionError,
};

fn canned(label: &str) -> Decision {
    Decision::from_distribution(
        label,
        vec![(label.to_string(), 1.0)],
        "m".to_string(),
        1,
        5,
        false,
    )
    .expect("canned")
}

fn breaker() -> DecisionBreaker {
    DecisionBreaker::new(3, 60_000).expect("breaker")
}

fn cache() -> DecisionCache {
    DecisionCache::new(16).expect("capacity")
}

#[test]
fn rule_accept_short_circuits_model() {
    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = Arc::clone(&calls);
    let out = fluent_llm::block_on(decide_with_ladder(
        Some(canned("RULE")),
        &cache(),
        "k".to_string(),
        true,
        &breaker(),
        || {
            let factory_calls = Arc::clone(&factory_calls);
            async move {
                factory_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, DecisionError>(canned("MODEL"))
            }
        },
        canned("FALLBACK"),
    ))
    .expect("ladder");
    assert_eq!(out.label, "RULE");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn cache_hit_short_circuits_model() {
    let cache = cache();
    fluent_llm::block_on(cache.get_or_decide("k".to_string(), async {
        Ok::<_, DecisionError>(canned("CACHED"))
    }))
    .expect("seed");
    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = Arc::clone(&calls);
    let out = fluent_llm::block_on(decide_with_ladder(
        None,
        &cache,
        "k".to_string(),
        true,
        &breaker(),
        || {
            let factory_calls = Arc::clone(&factory_calls);
            async move {
                factory_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, DecisionError>(canned("MODEL"))
            }
        },
        canned("FALLBACK"),
    ))
    .expect("ladder");
    assert_eq!(out.label, "CACHED");
    assert!(out.cached);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn model_unavailable_falls_to_static_fallback() {
    let out = fluent_llm::block_on(decide_with_ladder(
        None,
        &cache(),
        "k".to_string(),
        true,
        &breaker(),
        || async { Err::<Decision, _>(DecisionError::Unavailable("down".to_string())) },
        canned("FALLBACK"),
    ))
    .expect("fallback wins");
    assert_eq!(out.label, "FALLBACK");
}

#[test]
fn tripped_breaker_skips_send() {
    let breaker = DecisionBreaker::new(2, 60_000).expect("breaker");
    breaker.record_transport_failure();
    breaker.record_transport_failure();
    assert!(breaker.is_open());
    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = Arc::clone(&calls);
    let out = fluent_llm::block_on(decide_with_ladder(
        None,
        &cache(),
        "k".to_string(),
        true,
        &breaker,
        || {
            let factory_calls = Arc::clone(&factory_calls);
            async move {
                factory_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, DecisionError>(canned("MODEL"))
            }
        },
        canned("FALLBACK"),
    ))
    .expect("fallback wins");
    assert_eq!(out.label, "FALLBACK");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn terminal_reject_stops_walk_while_transient_continues() {
    let err = fluent_llm::block_on(decide_with_ladder(
        None,
        &cache(),
        "k".to_string(),
        true,
        &breaker(),
        || async { Err::<Decision, _>(DecisionError::Rejected("HTTP 400".to_string())) },
        canned("FALLBACK"),
    ))
    .expect_err("terminal");
    assert!(matches!(err, DecisionError::Rejected(_)));
}

#[test]
fn permanent_rejection_neither_trips_nor_retries_control() {
    let breaker = breaker();
    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = Arc::clone(&calls);
    for _ in 0..5 {
        let factory_calls = Arc::clone(&factory_calls);
        let _ = fluent_llm::block_on(decide_with_ladder(
            None,
            &cache(),
            "k".to_string(),
            true,
            &breaker,
            || {
                let factory_calls = Arc::clone(&factory_calls);
                async move {
                    factory_calls.fetch_add(1, Ordering::SeqCst);
                    Err::<Decision, _>(DecisionError::Rejected("HTTP 400".to_string()))
                }
            },
            canned("FALLBACK"),
        ));
    }
    // One call per walk (no ladder-level retry) and no trip (endpoint alive).
    assert_eq!(calls.load(Ordering::SeqCst), 5);
    assert!(!breaker.is_open());
}

#[test]
fn breaker_cooldown_closes_and_success_resets() {
    let breaker = DecisionBreaker::new(1, 50).expect("breaker");
    breaker.record_transport_failure();
    assert!(breaker.is_open());
    std::thread::sleep(std::time::Duration::from_millis(60));
    assert!(!breaker.is_open());
    breaker.record_transport_failure();
    breaker.record_success();
    assert!(!breaker.is_open());
}

#[test]
fn degraded_model_success_never_cached() {
    use fluent_llm::decision::decode_choice;
    let cache = cache();
    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = Arc::clone(&calls);
    let run = || {
        let factory_calls = Arc::clone(&factory_calls);
        move || {
            let factory_calls = Arc::clone(&factory_calls);
            async move {
                factory_calls.fetch_add(1, Ordering::SeqCst);
                decode_choice(
                    &serde_json::json!({
                        "choices": [{"message": {"role": "assistant", "content": "B"}}]
                    }),
                    &["A", "B"],
                    "m",
                    1,
                )
            }
        }
    };
    for _ in 0..2 {
        let out = fluent_llm::block_on(decide_with_ladder(
            None,
            &cache,
            "k".to_string(),
            true,
            &breaker(),
            run(),
            canned("FALLBACK"),
        ))
        .expect("ladder");
        assert_eq!(out.label, "B");
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "degraded success must not populate the cache"
    );
}

#[test]
fn sync_prefix_cascade_rule_then_cache() {
    use fluent_wvr::WorkContext;
    let ctx = WorkContext::default();
    let cascade =
        fluent_llm::decision::prefix_cascade(Some(canned("RULE")), Some(canned("CACHED")));
    let out = cascade.execute_first_ok(&ctx).expect("rule wins");
    let d: Decision = out.data_as().expect("typed");
    assert_eq!(d.label, "RULE");

    let cascade = fluent_llm::decision::prefix_cascade(None, Some(canned("CACHED")));
    let out = cascade.execute_first_ok(&ctx).expect("cache wins");
    let d: Decision = out.data_as().expect("typed");
    assert_eq!(d.label, "CACHED");
}

#[test]
fn breaker_trips_on_transport_failures_only() {
    let breaker = DecisionBreaker::new(2, 60_000).expect("breaker");
    breaker.record_transport_failure();
    assert!(!breaker.is_open());
    breaker.record_transport_failure();
    assert!(breaker.is_open());
    // Permanent and decode outcomes reset without tripping.
    let fresh = DecisionBreaker::new(2, 60_000).expect("breaker");
    fresh.record_permanent();
    fresh.record_permanent();
    assert!(!fresh.is_open());
}

#[test]
fn choice_boundary_fires_at_exact_threshold() {
    use fluent_llm::decision::{decide_verdict, ThresholdSet, Verdict};
    let edge = Decision::from_distribution(
        "B",
        vec![("B".to_string(), 0.9), ("A".to_string(), 0.1)],
        "m".to_string(),
        1,
        0,
        false,
    )
    .expect("edge");
    assert_eq!(
        decide_verdict(
            &edge,
            &ThresholdSet {
                act: 0.9,
                review: 0.7
            }
        ),
        Verdict::Act
    );
}

#[test]
fn breaker_half_open_admits_one_trial() {
    let breaker = DecisionBreaker::new(1, 50).expect("breaker");
    breaker.record_transport_failure();
    assert!(breaker.is_open());
    std::thread::sleep(std::time::Duration::from_millis(60));
    assert!(breaker.check().is_ok(), "expired breaker admits one trial");
    assert!(
        breaker.check().is_err(),
        "concurrent calls wait out the trial"
    );
    breaker.record_success();
    assert!(breaker.check().is_ok(), "success closes the breaker");
}

#[test]
fn failed_trial_reopens() {
    let breaker = DecisionBreaker::new(1, 50).expect("breaker");
    breaker.record_transport_failure();
    std::thread::sleep(std::time::Duration::from_millis(60));
    assert!(breaker.check().is_ok());
    breaker.record_transport_failure();
    assert!(breaker.is_open());
}
