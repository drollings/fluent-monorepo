use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::test_stubs::{CountingBackend, StubChatBackend};

use super::*;

fn candidate(key: &str, intelligence: u8) -> TargetCandidate {
    TargetCandidate {
        model_key: key.into(),
        model_name: key.into(),
        intelligence,
        cost: 1.0,
    }
}

fn assessment(complexity: u8, reason: &str) -> String {
    serde_json::to_string(&serde_json::json!({
        "complexity": complexity,
        "reason": reason,
    }))
    .unwrap()
}

fn routing_with(group: &str, keys: &[&str]) -> RoutingConfig {
    RoutingConfig {
        routes: HashMap::from([(
            "local".into(),
            crate::config::RouteRef {
                group: group.into(),
                role: None,
                pipelines: vec!["default".into()],
                description: "local".into(),
                always_route: false,
            },
        )]),
        models: keys
            .iter()
            .map(|k| {
                (
                    k.to_string(),
                    crate::config::ModelEntry {
                        embedding: None,
                        name: Some(k.to_string()),
                        endpoint: "http://localhost:8080/v1/chat/completions".into(),
                        intelligence: 0,
                        cost_input: 1e-6,
                        cost_output: 6e-6,
                        cost_cached_read: 4e-7,
                        tok_s: 8.0,
                        total_timeout_ms: 40_000,
                        idle_timeout_ms: 8_000,
                        stream: true,
                        filter_thinking: true,
                        thinking: None,
                        retry_count: 0,
                        retry_base_interval_s: 1,
                        params: None,
                        effective_profiles: None,
                        weights: None,
                        hf_repo: None,
                        hf_file: None,
                        template: None,
                        role_params: None,
                        api_key: None,
                    },
                )
            })
            .collect(),
        model_groups: HashMap::from([(
            group.into(),
            crate::config::ModelGroup::Array(keys.iter().map(ToString::to_string).collect()),
        )]),
        system_prompt: String::new(),
        safety_threshold: 0.3,
        default_route: "local".into(),
        score_matrix: None,
        onnx_keys: std::collections::BTreeSet::new(),
        roles: Default::default(),
    }
}

/// A scripted backend with a per-key artificial delay: the first candidate
/// is slow, so completions arrive out of candidate order. The matcher must
/// still decide, record, and audit in candidate order.
struct DelayBackend {
    delay: Duration,
    response: String,
}

impl fluent_llm::client::ChatBackend for DelayBackend {
    fn chat_complete(
        &self,
        _messages: &[fluent_llm::ChatMessage],
    ) -> Result<String, fluent_llm::LlmError> {
        std::thread::sleep(self.delay);
        Ok(self.response.clone())
    }
}

fn matcher_with_keys(
    pairs: Vec<(&str, Arc<dyn fluent_llm::client::ChatBackend>)>,
) -> TargetMatcher {
    let by_key: HashMap<String, Arc<dyn fluent_llm::client::ChatBackend>> =
        pairs.into_iter().map(|(k, b)| (k.to_string(), b)).collect();
    TargetMatcher::new(
        TargetBackends::new(
            by_key,
            Arc::new(StubChatBackend::always(assessment(9, "default")))
                as Arc<dyn fluent_llm::client::ChatBackend>,
        ),
        Arc::new(fluent_concurrency::pool::Limiter::new(4)),
        0,
    )
}

fn delay_backend(millis: u64, response: String) -> Arc<dyn fluent_llm::client::ChatBackend> {
    Arc::new(DelayBackend {
        delay: Duration::from_millis(millis),
        response,
    })
}

fn audit_assessment_order(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| l.contains("router.audit") && l.contains("\"stage\":\"assessment\""))
        .filter_map(|l| {
            let key = "\"model_key\":\"";
            let start = l.find(key)? + key.len();
            let end = l[start..].find('"')? + start;
            Some(l[start..end].to_string())
        })
        .collect()
}

#[test]
fn slow_first_candidate_keeps_decision_record_and_audit_order() {
    // m9-a is slow (250ms) and does not match (7 > 2); m9-b is instant and
    // matches (3 <= 4); m9-c is instant but discarded by the early match.
    // Completions arrive b, c, a — every observable must still read a, b.
    let matcher = matcher_with_keys(vec![
        ("m9-a", delay_backend(250, assessment(7, "too hard for a"))),
        ("m9-b", delay_backend(0, assessment(3, "fits b"))),
        ("m9-c", delay_backend(0, assessment(1, "fits c"))),
    ]);
    let cands = vec![
        candidate("m9-a", 2),
        candidate("m9-b", 4),
        candidate("m9-c", 6),
    ];
    let routing = routing_with("default", &["m9-a", "m9-b", "m9-c"]);

    let capture = crate::test_support::install_global_subscriber();
    let base = crate::test_support::global_capture_len(&capture);
    let tm = matcher
        .match_target("local", "default", &routing, &cands, None, "hello")
        .expect("ladder terminates");
    let lines = crate::test_support::global_capture_since(&capture, base);

    assert_eq!(tm.primary.model, "m9-b");
    let keys: Vec<&str> = tm
        .assessments
        .iter()
        .map(|r| r.model_key.as_str())
        .collect();
    assert_eq!(keys, vec!["m9-a", "m9-b"]);
    assert_eq!(tm.assessments[0].assessed, Some(7));
    assert!(!tm.assessments[0].matched);
    assert_eq!(tm.assessments[1].assessed, Some(3));
    assert!(tm.assessments[1].matched);

    let audit_order = audit_assessment_order(&lines);
    // Sibling tests run in parallel into the same global buffer under
    // their own distinct keys — scope to this run's keys only.
    let ours: Vec<String> = audit_order
        .into_iter()
        .filter(|k| k == "m9-a" || k == "m9-b")
        .collect();
    assert_eq!(ours, vec!["m9-a".to_string(), "m9-b".to_string()]);
}

#[test]
fn empty_group_returns_none_without_calls() {
    let backend = Arc::new(CountingBackend::new(assessment(1, "x")));
    let calls_backend = Arc::clone(&backend);
    let matcher = TargetMatcher::new(
        TargetBackends::new(
            HashMap::new(),
            calls_backend as Arc<dyn fluent_llm::client::ChatBackend>,
        ),
        Arc::new(fluent_concurrency::pool::Limiter::new(4)),
        0,
    );
    let routing = routing_with("default", &[]);
    assert!(matcher
        .match_target("local", "default", &routing, &[], Some(3), "hello")
        .is_none());
    assert_eq!(backend.calls(), 0);
}

#[test]
fn all_failures_match_last() {
    let failing =
        || Arc::new(StubChatBackend::new(Vec::new())) as Arc<dyn fluent_llm::client::ChatBackend>;
    let matcher = matcher_with_keys(vec![
        ("m9-x", failing()),
        ("m9-y", failing()),
        ("m9-z", failing()),
    ]);
    let cands = vec![
        candidate("m9-x", 2),
        candidate("m9-y", 4),
        candidate("m9-z", 6),
    ];
    let routing = routing_with("default", &["m9-x", "m9-y", "m9-z"]);
    let tm = matcher
        .match_target("local", "default", &routing, &cands, None, "hello")
        .expect("ladder terminates");
    assert_eq!(tm.primary.model, "m9-z");
    assert_eq!(tm.assessments.len(), 3);
    for record in &tm.assessments[..2] {
        assert_eq!(record.assessed, None);
        assert!(record.error.is_some());
        assert!(!record.matched);
    }
    assert!(tm.assessments[2].matched, "last member always matches");
}

#[test]
fn single_candidate_makes_exactly_one_call() {
    let backend = Arc::new(CountingBackend::new(assessment(2, "fits")));
    let matcher = TargetMatcher::new(
        TargetBackends::new(
            HashMap::from([(
                "m9-only".to_string(),
                Arc::clone(&backend) as Arc<dyn fluent_llm::client::ChatBackend>,
            )]),
            Arc::new(StubChatBackend::always(assessment(9, "default")))
                as Arc<dyn fluent_llm::client::ChatBackend>,
        ),
        Arc::new(fluent_concurrency::pool::Limiter::new(4)),
        0,
    );
    let cands = vec![candidate("m9-only", 4)];
    let routing = routing_with("default", &["m9-only"]);
    let tm = matcher
        .match_target("local", "default", &routing, &cands, None, "hello")
        .expect("single candidate matches");
    assert_eq!(tm.primary.model, "m9-only");
    assert_eq!(tm.assessments.len(), 1);
    assert!(tm.assessments[0].matched);
    assert_eq!(backend.calls(), 1);
}

#[test]
fn assess_suffix_outside_runtime_matches_sequential_outcomes() {
    // No ambient runtime here (plain #[test]): the fan-out drives a
    // throwaway runtime. Outcomes must equal the sequential single-candidate
    // walk — the degradation the serving path falls back to when no runtime
    // can be built.
    let matcher = matcher_with_keys(vec![
        ("a", delay_backend(30, assessment(8, "slow-a"))),
        ("b", delay_backend(0, assessment(3, "fast-b"))),
    ]);
    let suffix = vec![candidate("a", 5), candidate("b", 5)];
    let fanned: Vec<String> = matcher
        .assess_suffix(&suffix, "hello")
        .into_iter()
        .map(|r| format!("{r:?}"))
        .collect();
    let sequential: Vec<String> = suffix
        .iter()
        .map(|c| {
            format!(
                "{:?}",
                matcher
                    .assess_suffix(std::slice::from_ref(c), "hello")
                    .remove(0)
            )
        })
        .collect();
    assert_eq!(fanned, sequential);
}
