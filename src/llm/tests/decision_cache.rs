//! Bounded in-process cache tests (tests-first).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use fluent_llm::decision::{decision_cache_key, Decision, DecisionCache, DecisionError};

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

fn key(state: &str, version: u64, model: &str) -> String {
    decision_cache_key(
        state,
        "q?",
        "q",
        version,
        model,
        "http://localhost:8080",
        &serde_json::json!({"max_tokens": 1}),
        None,
    )
}

#[test]
fn same_key_hits_and_marks_cached() {
    let cache = DecisionCache::new(16).expect("capacity");
    let calls = Arc::new(AtomicUsize::new(0));
    let run = || {
        let calls = Arc::clone(&calls);
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, DecisionError>(canned("B"))
        }
    };
    let k = key("state", 1, "m");
    let first = fluent_llm::block_on(cache.get_or_decide(k.clone(), run())).expect("first");
    assert!(!first.cached);
    let second = fluent_llm::block_on(cache.get_or_decide(k, run())).expect("second");
    assert!(second.cached);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn version_bump_model_change_miss() {
    let cache = DecisionCache::new(16).expect("capacity");
    let calls = Arc::new(AtomicUsize::new(0));
    let decide = |label: &'static str| {
        let calls = Arc::clone(&calls);
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, DecisionError>(canned(label))
        }
    };
    let _ = fluent_llm::block_on(cache.get_or_decide(key("s", 1, "m"), decide("B")));
    let _ = fluent_llm::block_on(cache.get_or_decide(key("s", 2, "m"), decide("B")));
    let _ = fluent_llm::block_on(cache.get_or_decide(key("s", 1, "other"), decide("B")));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn extras_and_think_flag_change_miss() {
    let extra_v1 = serde_json::json!({"a": 1});
    let base = decision_cache_key("s", "q?", "q", 1, "m", "http://a", &extra_v1, None);
    let other_extras = decision_cache_key(
        "s",
        "q?",
        "q",
        1,
        "m",
        "http://a",
        &serde_json::json!({"a": 2}),
        None,
    );
    let think_on = decision_cache_key("s", "q?", "q", 1, "m", "http://a", &extra_v1, Some(true));
    let same = decision_cache_key("s", "q?", "q", 1, "m", "http://a", &extra_v1, None);
    assert_ne!(base, other_extras);
    assert_ne!(base, think_on);
    assert_eq!(base, same);
}

#[test]
fn key_separates_question_text_framing_and_endpoint() {
    let k = |state: &str, question: &str, endpoint: &str| {
        decision_cache_key(
            state,
            question,
            "qid",
            1,
            "m",
            endpoint,
            &serde_json::json!({}),
            None,
        )
    };
    // Same state, different questions (the bulk shape) must not collide.
    assert_ne!(
        k("shared", "qq-0?", "http://a"),
        k("shared", "qq-1?", "http://a")
    );
    // Same question, different endpoints must not collide.
    assert_ne!(
        k("shared", "qq-0?", "http://a"),
        k("shared", "qq-0?", "http://b")
    );
    // Framing: "ab"+"c" vs "a"+"bc" must not collide.
    assert_ne!(k("ab", "c", "http://a"), k("a", "bc", "http://a"));
    assert_eq!(
        k("shared", "qq-0?", "http://a"),
        k("shared", "qq-0?", "http://a")
    );
}

#[test]
fn unavailable_never_cached_control() {
    let cache = DecisionCache::new(16).expect("capacity");
    let calls = Arc::new(AtomicUsize::new(0));
    for _ in 0..2 {
        let calls = Arc::clone(&calls);
        let err = fluent_llm::block_on(cache.get_or_decide(key("s", 1, "m"), async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Err::<Decision, _>(DecisionError::Unavailable("down".to_string()))
        }))
        .expect_err("unavailable");
        assert!(err.is_unavailable());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(cache.len(), 0);
}

#[test]
fn decode_errors_never_cached_control() {
    let cache = DecisionCache::new(16).expect("capacity");
    for _ in 0..2 {
        let err = fluent_llm::block_on(cache.get_or_decide(key("s", 1, "m"), async {
            Err::<Decision, _>(DecisionError::Decode("garbage".to_string()))
        }))
        .expect_err("decode");
        assert!(matches!(err, DecisionError::Decode(_)));
    }
    assert_eq!(cache.len(), 0);
}

#[test]
fn degraded_ok_never_cached() {
    use fluent_llm::decision::decode_choice;
    let cache = DecisionCache::new(16).expect("capacity");
    let calls = Arc::new(AtomicUsize::new(0));
    for _ in 0..2 {
        let calls = Arc::clone(&calls);
        let d = fluent_llm::block_on(cache.get_or_decide(key("s", 1, "m"), async move {
            calls.fetch_add(1, Ordering::SeqCst);
            decode_choice(
                &serde_json::json!({
                    "choices": [{"message": {"role": "assistant", "content": "B"}}]
                }),
                &["A", "B"],
                "m",
                1,
            )
        }))
        .expect("degraded ok");
        assert_eq!(d.label, "B");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(cache.len(), 0);
}

#[test]
fn eviction_bounded_under_flood() {
    let cache = DecisionCache::new(2).expect("capacity");
    for i in 0..10 {
        let k = key(&format!("state-{i}"), 1, "m");
        let _ = fluent_llm::block_on(
            cache.get_or_decide(k, async { Ok::<_, DecisionError>(canned("B")) }),
        );
    }
    assert!(cache.len() <= 2, "len={}", cache.len());
    assert_eq!(cache.capacity(), 2);
}

#[test]
fn eviction_bounded_under_concurrent_flood() {
    use std::thread;
    let cache = std::sync::Arc::new(DecisionCache::new(8).expect("capacity"));
    thread::scope(|s| {
        for t in 0..8 {
            let cache = std::sync::Arc::clone(&cache);
            s.spawn(move || {
                for i in 0..50 {
                    let k = key(&format!("t{t}-s{i}"), 1, "m");
                    let _ = fluent_llm::block_on(
                        cache.get_or_decide(k, async { Ok::<_, DecisionError>(canned("B")) }),
                    );
                }
            });
        }
    });
    assert!(cache.len() <= 8, "len={}", cache.len());
}

#[test]
fn key_struct_matches_legacy_bytes_and_scopes_fingerprint() {
    use fluent_llm::decision::CacheKeyInput;
    let extras = serde_json::json!({"a": 1});
    let legacy = decision_cache_key("s", "q?", "qid", 1, "m", "http://a", &extras, None);
    let input = CacheKeyInput {
        state: "s".to_string(),
        question: "q?".to_string(),
        question_id: "qid".to_string(),
        version: 1,
        model: "m".to_string(),
        endpoint: "http://a".to_string(),
        extras: extras.clone(),
        think: None,
        fingerprint: None,
    };
    assert_eq!(legacy, input.key());
    let scoped = CacheKeyInput {
        fingerprint: Some("fp-weights-v2".to_string()),
        ..input.clone()
    };
    let other = CacheKeyInput {
        fingerprint: Some("fp-weights-v3".to_string()),
        ..input.clone()
    };
    assert_ne!(input.key(), scoped.key());
    assert_ne!(scoped.key(), other.key());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn simultaneous_miss_coalesces_to_one_load() {
    use std::sync::Arc as StdArc;
    let cache = StdArc::new(DecisionCache::new(16).expect("capacity"));
    let calls = StdArc::new(AtomicUsize::new(0));
    let gate = StdArc::new(tokio::sync::Barrier::new(16));
    let mut handles = Vec::new();
    for _ in 0..16 {
        let (cache, calls, gate) = (Arc::clone(&cache), Arc::clone(&calls), Arc::clone(&gate));
        handles.push(tokio::spawn(async move {
            gate.wait().await;
            cache
                .get_or_decide(key("stampede", 1, "m"), async move {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, DecisionError>(canned("B"))
                })
                .await
        }));
    }
    let mut labels = Vec::new();
    for handle in handles {
        labels.push(handle.await.expect("task").expect("decision").label);
    }
    assert!(labels.iter().all(|l| l == "B"));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one leader loads");
}

async fn slow_load(calls: Arc<AtomicUsize>) -> Result<Decision, DecisionError> {
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    calls.fetch_add(1, Ordering::SeqCst);
    Ok(canned("B"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn canceled_leader_does_not_strand_waiters() {
    let cache = std::sync::Arc::new(DecisionCache::new(16).expect("capacity"));
    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    let leader = {
        let (cache, calls) = (Arc::clone(&cache), Arc::clone(&calls));
        tokio::spawn(async move {
            cache
                .get_or_decide(key("s", 1, "m"), slow_load(calls))
                .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let waiter = {
        let (cache, calls) = (Arc::clone(&cache), Arc::clone(&calls));
        tokio::spawn(async move {
            cache
                .get_or_decide(key("s", 1, "m"), slow_load(calls))
                .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    leader.abort();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
        .await
        .expect("waiter not stranded")
        .expect("task");
    assert_eq!(outcome.expect("decision").label, "B");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "only the waiter loaded");
}
