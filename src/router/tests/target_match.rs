use std::collections::HashMap;

use crate::config::{ModelEntry, RoleEntry, RouteRef};
use crate::test_stubs::StubChatBackend;

use super::*;

fn candidate(key: &str, intelligence: u8, cost: f64) -> TargetCandidate {
    TargetCandidate {
        model_key: key.into(),
        model_name: key.into(),
        intelligence,
        cost,
    }
}

fn candidates(entries: &[(&str, u8, f64)]) -> Vec<TargetCandidate> {
    entries
        .iter()
        .map(|(k, i, c)| candidate(k, *i, *c))
        .collect()
}

fn assessment(complexity: u8, reason: &str) -> String {
    serde_json::to_string(&serde_json::json!({
        "complexity": complexity,
        "reason": reason,
    }))
    .unwrap()
}

fn model_entry(key: &str, intelligence: u8, cost: f64) -> ModelEntry {
    ModelEntry {
        embedding: None,
        name: Some(key.into()),
        endpoint: "http://localhost:8080/v1/chat/completions".into(),
        intelligence,
        cost_input: cost,
        cost_output: cost * 6.0,
        cost_cached_read: cost * 0.4,
        tok_s: 8.0,
        total_timeout_ms: 40_000,
        idle_timeout_ms: 8_000,
        stream: true,
        filter_thinking: true,
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
        ..ModelEntry::default()
    }
}

fn routing_with(role: &str, keys: &[&str]) -> RoutingConfig {
    RoutingConfig {
        routes: HashMap::from([(
            "local".into(),
            RouteRef {
                role: role.into(),
                pipelines: vec!["default".into()],
                description: "local".into(),
                always_route: false,
            },
        )]),
        models: keys
            .iter()
            .map(|k| (k.to_string(), model_entry(k, 0, 1.0)))
            .collect(),
        system_prompt: String::new(),
        safety_threshold: 0.3,
        default_route: "local".into(),
        score_matrix: None,
        onnx_keys: std::collections::BTreeSet::new(),
        roles: HashMap::from([(role.into(), role_entry(keys))]),
    }
}

// ── Pure selection core ──────────────────────────────────────────────

#[test]
fn start_index_with_classifier_complexity() {
    let cands = candidates(&[("swarm", 2, 1.0), ("qwen", 6, 3.0)]);
    // complexity 5 → first member with intelligence >= 5 is qwen (index 1).
    assert_eq!(start_index(&cands, Some(5)), 1);
    // complexity 1 → swarm qualifies at index 0.
    assert_eq!(start_index(&cands, Some(1)), 0);
    // complexity exactly equal → that index.
    assert_eq!(start_index(&cands, Some(6)), 1);
}

#[test]
fn start_index_none_or_unqualified_returns_zero() {
    let cands = candidates(&[("swarm", 2, 1.0), ("qwen", 6, 3.0)]);
    assert_eq!(start_index(&cands, None), 0);
    // No member qualifies (complexity beyond the top intelligence) → 0:
    // the cheapest candidate self-assesses first; the climb proceeds.
    assert_eq!(start_index(&cands, Some(9)), 0);
}

#[test]
fn start_index_empty_group_is_zero() {
    let empty: Vec<TargetCandidate> = vec![];
    assert_eq!(start_index(&empty, Some(3)), 0);
    assert_eq!(start_index(&empty, None), 0);
}

#[test]
fn start_index_skips_weak_cheaper_candidates() {
    // Cost ordering governs within the group: index 0 is cheapest but too
    // weak; the classifier already ruled it out, so the climb starts at
    // the first qualifying member.
    let cands = candidates(&[("tiny", 1, 0.5), ("small", 3, 1.0), ("big", 6, 3.0)]);
    assert_eq!(start_index(&cands, Some(3)), 1);
    assert_eq!(start_index(&cands, Some(2)), 1);
    assert_eq!(start_index(&cands, Some(6)), 2);
}

#[test]
fn is_match_boundary_inclusive() {
    let cand = candidate("qwen", 6, 3.0);
    // assessed == intelligence matches (meets or exceeds).
    assert!(is_match(&cand, 6));
    assert!(is_match(&cand, 5));
    assert!(!is_match(&cand, 7));
}

// ── Prompt + parse ───────────────────────────────────────────────────

#[test]
fn self_assessment_prompt_embeds_user_text() {
    let prompt = build_self_assessment_prompt("what is 2+2?");
    assert!(prompt.contains("0 (trivial) to 10 (requires the most capable model available)"));
    assert!(prompt.contains("User request: what is 2+2?"));
    assert!(prompt.contains(r#"{"complexity": <integer 0-10>, "reason": "<brief justification>"}"#));
    assert!(prompt.ends_with("Only output JSON, no other text."));
}

#[test]
fn parse_pristine_json() {
    let s = parse_self_assessment(&assessment(4, "simple math")).unwrap();
    assert_eq!(s.complexity, 4);
    assert_eq!(s.reason, "simple math");
}

#[test]
fn parse_fenced_json() {
    let s = parse_self_assessment(&format!("```json\n{}\n```", assessment(7, "complex"))).unwrap();
    assert_eq!(s.complexity, 7);
}

#[test]
fn parse_json_inside_prose() {
    let s = parse_self_assessment(&format!(
        "Here you go: {} Hope that helps!",
        assessment(3, "moderate")
    ))
    .unwrap();
    assert_eq!(s.complexity, 3);
}

#[test]
fn parse_string_number_coerces() {
    let s = parse_self_assessment(r#"{"complexity": "6", "reason": "coerced"}"#).unwrap();
    assert_eq!(s.complexity, 6);
}

#[test]
fn parse_missing_fields_default() {
    let s = parse_self_assessment(r"{}").unwrap();
    assert_eq!(s.complexity, 5, "missing complexity defaults to 5");
    assert_eq!(s.reason, "");
}

#[test]
fn parse_unparseable_is_error() {
    assert!(parse_self_assessment("not json at all").is_err());
    assert!(parse_self_assessment("").is_err());
}

// ── Matcher climb ────────────────────────────────────────────────────

/// A backend that records every message list it is called with and always
/// answers with a canned assessment. Pins the assessment call shape: strict
/// chat templates raise on conversations without a genuine user turn, so
/// every assessment must include a `user` message.
struct RecordingBackend {
    seen: std::sync::Mutex<Vec<Vec<fluent_llm::ChatMessage>>>,
    response: String,
}

impl fluent_llm::client::ChatBackend for RecordingBackend {
    fn chat_complete(
        &self,
        messages: &[fluent_llm::ChatMessage],
    ) -> Result<String, fluent_llm::LlmError> {
        self.seen.lock().unwrap().push(messages.to_vec());
        Ok(self.response.clone())
    }
}

#[test]
fn self_assessment_calls_include_a_user_turn() {
    // Two candidates so the climb actually assesses (singletons run
    // assessment-free): the first vetoes itself, the last always matches.
    let backend = Arc::new(RecordingBackend {
        seen: std::sync::Mutex::new(Vec::new()),
        response: assessment(9, "too hard"),
    });
    let matcher = TargetMatcher::new(
        TargetBackends::new(HashMap::new(), backend.clone()),
        Arc::new(fluent_concurrency::pool::Limiter::new(4)),
        0,
    );
    let cands = candidates(&[("small", 1, 1.0), ("qwen3.5-9b", 4, 3.0)]);
    let routing = routing_with("default", &["small", "qwen3.5-9b"]);
    matcher
        .match_target("local", "default", &routing, &cands, Some(3), "hello")
        .expect("match");
    let seen = backend.seen.lock().unwrap();
    assert!(!seen.is_empty(), "assessment ran");
    for messages in seen.iter() {
        assert!(
            messages.iter().any(|m| m.role == "user"),
            "assessment must carry a user turn (strict templates raise without one): {messages:?}"
        );
    }
}

fn matcher_with(default_response: Vec<String>) -> TargetMatcher {
    TargetMatcher::new(
        TargetBackends::new(
            HashMap::new(),
            Arc::new(StubChatBackend::new(default_response)),
        ),
        Arc::new(Limiter::new(4)),
        0,
    )
}

/// A matcher whose stub backend answers per candidate key instead of from
/// one shared queue. The ladder assesses the reachable suffix concurrently,
/// so call order is nondeterministic — a shared queue cannot deterministically
/// map responses to candidates, but a per-key backend can (mirroring
/// production, where every model key has its own backend). Keys without an
/// entry fall through to a benign always-9 default; tests pinning failure
/// map those keys to empty queues explicitly.
fn matcher_with_per_key(by_key: Vec<(&str, Vec<String>)>) -> TargetMatcher {
    TargetMatcher::new(
        TargetBackends::new(
            by_key
                .into_iter()
                .map(|(key, responses)| {
                    (
                        key.to_string(),
                        Arc::new(StubChatBackend::new(responses)) as Arc<dyn ChatBackend>,
                    )
                })
                .collect(),
            Arc::new(StubChatBackend::always(assessment(
                9,
                "default fan-out tail",
            ))),
        ),
        Arc::new(Limiter::new(4)),
        0,
    )
}

#[test]
fn target_backends_get_prefers_dedicated_then_default() {
    let dedicated: Arc<dyn ChatBackend> =
        Arc::new(StubChatBackend::always(assessment(2, "dedicated")));
    let default: Arc<dyn ChatBackend> = Arc::new(StubChatBackend::always(assessment(8, "default")));
    let backends = TargetBackends::new(
        HashMap::from([("swarm".to_string(), Arc::clone(&dedicated))]),
        Arc::clone(&default),
    );

    assert!(
        Arc::ptr_eq(&backends.get("swarm"), &dedicated),
        "dedicated backend wins for a mapped key"
    );
    assert!(
        Arc::ptr_eq(&backends.get("qwen3.6-27b"), &default),
        "default backend (injected mock/transcript) serves keys absent from the map"
    );
}

#[test]
fn climb_matches_first_candidate_within_intelligence() {
    let cands = candidates(&[
        ("swarm", 2, 1.0),
        ("qwen3.5-9b", 4, 3.0),
        ("qwen3.6-27b", 6, 5.0),
    ]);
    let routing = routing_with("default", &["swarm", "qwen3.5-9b", "qwen3.6-27b"]);
    let matcher = matcher_with_per_key(vec![
        ("qwen3.5-9b", vec![assessment(4, "mid")]),
        ("qwen3.6-27b", vec![assessment(6, "hard")]),
    ]);

    let tm = matcher
        .match_target("local", "default", &routing, &cands, Some(3), "hello")
        .expect("match");
    // start_index(Some(3)) = 1 (qwen3.5-9b, the first member with
    // intelligence >= 3). It self-assesses 4 <= 4 → match.
    assert_eq!(tm.primary.model, "qwen3.5-9b:default");
    assert_eq!(tm.primary.target_name.as_deref(), Some("local"));
    assert_eq!(tm.primary.role.as_deref(), Some("default"));
    // Exactly 1 self-assessment: swarm skipped by the start index, qwen3.5-9b
    // assessed and matched.
    assert_eq!(tm.assessments.len(), 1);
    let rec = &tm.assessments[0];
    assert_eq!(rec.assessed, Some(4));
    assert!(rec.matched);
}

#[test]
fn climb_escalates_to_more_intelligent_member() {
    let cands = candidates(&[
        ("swarm", 2, 1.0),
        ("qwen3.5-9b", 4, 3.0),
        ("qwen3.6-27b", 6, 5.0),
    ]);
    let routing = routing_with("default", &["swarm", "qwen3.5-9b", "qwen3.6-27b"]);
    let matcher = matcher_with_per_key(vec![
        ("qwen3.5-9b", vec![assessment(7, "hard for qwen3.5")]),
        ("qwen3.6-27b", vec![assessment(6, "ok for 27b")]),
    ]);

    let tm = matcher
        .match_target("local", "default", &routing, &cands, Some(3), "hello")
        .expect("match");
    // start_index(Some(3)) = 1. qwen3.5-9b self-assesses 7 > 4 → escalate.
    // qwen3.6-27b self-assesses 6 <= 6 → match.
    assert_eq!(tm.primary.model, "qwen3.6-27b:default");
    assert_eq!(tm.assessments.len(), 2);
    assert_eq!(tm.assessments[0].assessed, Some(7));
    assert!(!tm.assessments[0].matched);
    assert_eq!(tm.assessments[1].assessed, Some(6));
    assert!(tm.assessments[1].matched);
}

#[test]
fn climb_starts_at_classifier_seed() {
    let cands = candidates(&[
        ("swarm", 2, 1.0),
        ("qwen3.5-9b", 4, 3.0),
        ("qwen3.6-27b", 6, 5.0),
    ]);
    let routing = routing_with("default", &["swarm", "qwen3.5-9b", "qwen3.6-27b"]);
    let matcher = matcher_with_per_key(vec![
        ("swarm", vec![assessment(1, "easy")]),
        ("qwen3.5-9b", vec![assessment(3, "ok")]),
    ]);

    let tm = matcher
        .match_target("local", "default", &routing, &cands, None, "hello")
        .expect("match");
    // No classifier estimate → start at 0 (swarm). Swarm self-assesses 1 <= 2 → match.
    assert_eq!(tm.primary.model, "swarm:default");
    assert_eq!(tm.assessments.len(), 1);
}

#[test]
fn parse_failure_escalates_conservatively() {
    let cands = candidates(&[
        ("swarm", 2, 1.0),
        ("qwen3.5-9b", 4, 3.0),
        ("qwen3.6-27b", 6, 5.0),
    ]);
    let routing = routing_with("default", &["swarm", "qwen3.5-9b", "qwen3.6-27b"]);
    // swarm's response is unparseable → treated as can't-confirm → escalate.
    let matcher = matcher_with_per_key(vec![
        ("swarm", vec!["not json".into()]),
        ("qwen3.5-9b", vec![assessment(4, "ok for qwen3.5")]),
        ("qwen3.6-27b", vec![assessment(6, "ok for 27b")]),
    ]);

    let tm = matcher
        .match_target("local", "default", &routing, &cands, None, "hello")
        .expect("match");
    assert_eq!(tm.primary.model, "qwen3.5-9b:default");
    assert_eq!(tm.assessments.len(), 2);
    assert_eq!(tm.assessments[0].assessed, None);
    assert!(tm.assessments[0].error.is_some());
    assert!(!tm.assessments[0].matched);
}

#[test]
fn llm_error_escalates_conservatively() {
    let cands = candidates(&[("swarm", 2, 1.0), ("qwen3.6-27b", 6, 3.0)]);
    let routing = routing_with("default", &["swarm", "qwen3.6-27b"]);
    // Empty per-key queues → every self-assessment call fails with NoResponse.
    // Conservative escalation: a candidate that cannot confirm (LLM error)
    // is skipped; the last member still matches (terminate-don't-loop).
    let matcher = matcher_with_per_key(vec![("swarm", vec![]), ("qwen3.6-27b", vec![])]);

    let tm = matcher
        .match_target("local", "default", &routing, &cands, None, "hello")
        .expect("match");
    assert_eq!(tm.primary.model, "qwen3.6-27b:default");
    assert_eq!(tm.assessments.len(), 2);
    assert_eq!(tm.assessments[0].assessed, None);
    assert!(tm.assessments[0].error.is_some());
    assert!(!tm.assessments[0].matched);
}

#[test]
fn last_member_always_matches() {
    let cands = candidates(&[("swarm", 2, 1.0), ("qwen3.6-27b", 6, 3.0)]);
    let routing = routing_with("default", &["swarm", "qwen3.6-27b"]);
    // The last member self-assesses 9 > 6 — still matches (terminate).
    let matcher = matcher_with_per_key(vec![
        ("swarm", vec![assessment(3, "ok for swarm")]),
        ("qwen3.6-27b", vec![assessment(9, "hard")]),
    ]);

    let tm = matcher
        .match_target("local", "default", &routing, &cands, None, "hello")
        .expect("match");
    assert_eq!(tm.primary.model, "qwen3.6-27b:default");
    assert_eq!(tm.assessments.len(), 2);
    assert!(tm.assessments[1].matched, "last member always matches");
    assert_eq!(tm.assessments[1].assessed, Some(9));
}

#[test]
fn exact_call_count_two_assessments() {
    let cands = candidates(&[
        ("swarm", 2, 1.0),
        ("qwen3.5-9b", 4, 3.0),
        ("qwen3.6-27b", 6, 5.0),
    ]);
    let routing = routing_with("default", &["swarm", "qwen3.5-9b", "qwen3.6-27b"]);
    let matcher = matcher_with_per_key(vec![
        ("qwen3.5-9b", vec![assessment(7, "too hard")]),
        ("qwen3.6-27b", vec![assessment(6, "match")]),
    ]);

    let tm = matcher
        .match_target("local", "default", &routing, &cands, Some(3), "hello")
        .expect("match");
    // start index 1 → swarm never self-assesses; exactly 2 calls made.
    assert_eq!(tm.assessments.len(), 2);
    assert_eq!(tm.assessments[0].model_key, "qwen3.5-9b");
    assert_eq!(tm.primary.model, "qwen3.6-27b:default");
}

#[test]
fn fallbacks_are_group_tail_then_cross_group() {
    let cands = candidates(&[
        ("swarm", 2, 1.0),
        ("qwen3.5-9b", 4, 3.0),
        ("qwen3.6-27b", 6, 5.0),
    ]);
    let routing = routing_with("default", &["swarm", "qwen3.5-9b", "qwen3.6-27b"]);
    let matcher = matcher_with_per_key(vec![("swarm", vec![assessment(1, "easy")])]);

    let tm = matcher
        .match_target("local", "default", &routing, &cands, None, "hello")
        .expect("match");
    assert_eq!(tm.primary.model, "swarm:default");
    // Fallback tail = the more-intelligent members of the group, in order.
    let fb_models: Vec<&str> = tm
        .primary
        .fallbacks
        .iter()
        .map(|f| f.model.as_str())
        .collect();
    assert_eq!(
        fb_models,
        vec!["qwen3.5-9b:default", "qwen3.6-27b:default"],
        "fallbacks serve the request role too"
    );
}

#[test]
fn empty_candidates_is_none() {
    let routing = routing_with("default", &[]);
    let matcher = matcher_with(vec![]);
    assert!(matcher
        .match_target("local", "default", &routing, &[], Some(3), "hello")
        .is_none());
}

// ── Availability-sentinel expansion (`last` / `any`) ────────────────────

fn role_entry(members: &[&str]) -> RoleEntry {
    RoleEntry {
        max_parallel: None,
        pinned: false,
        enable_thinking: None,
        filter_thinking: true,
        preserve_thinking: None,
        params: Default::default(),
        models: members.iter().map(ToString::to_string).collect(),
        snapshot: None,
        escalation: None,
    }
}

fn routing_with_members(role: &str, entries: &[&str], members: &[&str]) -> RoutingConfig {
    RoutingConfig {
        routes: HashMap::from([(
            "local".into(),
            RouteRef {
                role: role.into(),
                pipelines: vec!["default".into()],
                description: "local".into(),
                always_route: false,
            },
        )]),
        models: entries
            .iter()
            .map(|k| (k.to_string(), model_entry(k, 0, 1.0)))
            .collect(),
        system_prompt: String::new(),
        safety_threshold: 0.3,
        default_route: "local".into(),
        score_matrix: None,
        onnx_keys: std::collections::BTreeSet::new(),
        roles: HashMap::from([(role.into(), role_entry(members))]),
    }
}

fn wire_of(routing: &RoutingConfig, key: &str) -> String {
    routing
        .target_for_key(key)
        .unwrap_or_else(|| panic!("member {key} resolves"))
        .model
}

#[test]
fn expansion_without_sentinels_is_identity() {
    let routing = routing_with_members("g", &["a", "b"], &["a", "b"]);
    let expanded = expand_role_keys(&routing, "g", None, None, &|_| false, None);
    assert_eq!(expanded, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn expansion_last_with_recorded_success_orders_first() {
    let routing = routing_with_members("g", &["a", "b"], &["last", "a", "b"]);
    let recency = RoleRecency::new();
    recency.record("s", "g", &wire_of(&routing, "b"));
    let expanded = expand_role_keys(&routing, "g", Some(&recency), Some("s"), &|_| false, None);
    assert_eq!(expanded, vec!["b".to_string(), "a".to_string()]);
}

#[test]
fn expansion_last_without_success_skips_sentinel() {
    let routing = routing_with_members("g", &["a", "b"], &["last", "a", "b"]);
    let expanded = expand_role_keys(&routing, "g", None, None, &|_| false, None);
    assert_eq!(expanded, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn expansion_last_pointing_at_removed_member_skips() {
    let routing = routing_with_members("g", &["a", "b"], &["last", "a"]);
    let recency = RoleRecency::new();
    recency.record("s", "g", "zzz-removed-model");
    let expanded = expand_role_keys(&routing, "g", Some(&recency), Some("s"), &|_| false, None);
    assert_eq!(expanded, vec!["a".to_string()]);
}

#[test]
fn expansion_any_orders_loaded_first() {
    let routing = routing_with_members("g", &["a", "b"], &["any", "a", "b"]);
    let expanded = expand_role_keys(&routing, "g", None, None, &|m| m == "b", None);
    assert_eq!(expanded, vec!["b".to_string(), "a".to_string()]);
    let expanded = expand_role_keys(&routing, "g", None, None, &|m| m == "a", None);
    assert_eq!(expanded, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn expansion_any_all_down_keeps_config_order() {
    let routing = routing_with_members("g", &["a", "b"], &["any", "a", "b"]);
    let expanded = expand_role_keys(&routing, "g", None, None, &|_| false, None);
    assert_eq!(expanded, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn expansion_any_onnx_members_count_as_loaded() {
    let mut routing = routing_with_members("g", &["a"], &["any", "onnx/llm", "a"]);
    routing.onnx_keys.insert("onnx/llm".to_string());
    // No server is running anywhere; the onnx role still orders first via the
    // existing registry readiness (no new probe).
    let expanded = expand_role_keys(&routing, "g", None, None, &|_| false, None);
    assert_eq!(expanded, vec!["onnx/llm".to_string(), "a".to_string()]);
}

/// Fixture with separated capability and cost axes: `a` weak but pricey
/// (the config head), `b` capable and cheap, `c` most capable, pricey, and
/// pool-managed (weights set, so coldness vetoes it for cheap duties).
fn last_viable_routing() -> RoutingConfig {
    let mut routing = routing_with_members("g", &["a", "b", "c"], &["last", "a", "b", "c"]);
    let entry = routing.models.get_mut("a").expect("a");
    entry.intelligence = 1;
    entry.cost_input = 9.0;
    entry.cost_output = 9.0;
    let entry = routing.models.get_mut("b").expect("b");
    entry.intelligence = 5;
    entry.cost_input = 1.0;
    entry.cost_output = 1.0;
    let entry = routing.models.get_mut("c").expect("c");
    entry.intelligence = 9;
    entry.cost_input = 5.0;
    entry.cost_output = 5.0;
    entry.weights = Some("/models/c.gguf".into());
    routing
}

#[test]
fn last_viable_session_pick_serves_when_viable() {
    let routing = last_viable_routing();
    let recency = RoleRecency::new();
    recency.record("s", "g", &wire_of(&routing, "b"));
    let expanded = expand_role_keys(
        &routing,
        "g",
        Some(&recency),
        Some("s"),
        &|_| false,
        Some(4),
    );
    assert_eq!(
        expanded,
        vec!["b".to_string(), "a".to_string(), "c".to_string()],
        "viable session pick orders first"
    );
}

#[test]
fn last_viable_stale_pick_falls_through_with_single_audit() {
    let routing = last_viable_routing();
    let recency = RoleRecency::new();
    // Session pick `a` is stale (intelligence 1 < complexity 4); the global
    // pick `b` serves instead. The session miss audits exactly once.
    recency.record("s", "g", &wire_of(&routing, "a"));
    recency.record_any(&wire_of(&routing, "b"));
    let (expanded, logs) = crate::test_support::capture_logs(|| {
        expand_role_keys(
            &routing,
            "g",
            Some(&recency),
            Some("s"),
            &|_| false,
            Some(4),
        )
    });
    assert_eq!(
        expanded,
        vec!["b".to_string(), "a".to_string(), "c".to_string()],
        "stale session pick vetoed, global pick wins"
    );
    assert_eq!(
        logs.iter()
            .filter(|l| l.contains("last-empty-fallback"))
            .count(),
        1,
        "stale session recency audits exactly once"
    );
}

#[test]
fn last_viable_absent_recency_promotes_viable_first() {
    let routing = last_viable_routing();
    // No history anywhere and nothing running: viability (cheapest passing)
    // promotes `b` ahead of the pricey config head `a`.
    let (expanded, logs) = crate::test_support::capture_logs(|| {
        expand_role_keys(&routing, "g", None, None, &|_| false, None)
    });
    assert_eq!(
        expanded,
        vec!["b".to_string(), "a".to_string(), "c".to_string()],
        "viable-first orders ahead of config order on cold start"
    );
    assert_eq!(
        logs.iter()
            .filter(|l| l.contains("last-empty-fallback"))
            .count(),
        1,
        "missing recency still audits once (coverage signal, not outcome)"
    );
}

#[test]
fn last_viable_classifier_key_serves_global_when_fittable() {
    let routing = last_viable_routing();
    let recency = RoleRecency::new();
    recency.record_any(&wire_of(&routing, "b"));
    // `b` is remote (no weights): always fittable, no load to trigger.
    let picked = viable_last_key(&routing, Some(&recency), &|_| false);
    assert_eq!(picked.as_deref(), Some("b"));
}

#[test]
fn last_viable_classifier_key_vetoes_cold_managed_pick() {
    let routing = last_viable_routing();
    let recency = RoleRecency::new();
    recency.record_any(&wire_of(&routing, "c"));
    // `c` is pool-managed but cold: the cheap duty must not cold-load it.
    // Fleet fallback prefers resident-cheapest (`b`, not the vetoed `c`).
    let picked = viable_last_key(&routing, Some(&recency), &|_| false);
    assert_eq!(picked.as_deref(), Some("b"), "cold managed pick vetoed");
}

#[test]
fn last_viable_classifier_key_ghost_falls_back_to_fleet() {
    let routing = last_viable_routing();
    let recency = RoleRecency::new();
    recency.record_any("ghost-wire");
    let picked = viable_last_key(&routing, Some(&recency), &|m| m == "b");
    // Unresolvable wire vetoed; fleet viable with `b` running: all fittable,
    // cheapest (`b`) wins.
    assert_eq!(picked.as_deref(), Some("b"));
}

#[test]
fn last_viable_classifier_key_empty_recency_uses_fleet_viable() {
    let routing = last_viable_routing();
    let picked = viable_last_key(&routing, None, &|_| false);
    assert_eq!(picked.as_deref(), Some("b"));
}

#[test]
fn last_viable_classifier_key_vetoes_cold_managed_fleet() {
    // Every fleet member is pool-managed and down: the static fallback must
    // fail closed (None) instead of cold-loading the cheapest weights for
    // a duty. Duties never cold-load; dispatch owns loading.
    let mut routing = routing_with_members("g", &["m1", "m2"], &["last", "m1", "m2"]);
    for key in ["m1", "m2"] {
        routing.models.get_mut(key).expect("member").weights = Some(format!("/models/{key}.gguf"));
    }
    let picked = viable_last_key(&routing, None, &|_| false);
    assert_eq!(
        picked, None,
        "cold managed fleet fails closed instead of cold-picking"
    );
}

#[test]
fn last_viable_classifier_key_prefers_running_managed_over_cheap_cold() {
    // Both managed; `cheap` is down and `big` is running and pricier:
    // residency outranks price, so the running model serves without any
    // load instead of cold-loading the cheap one.
    let mut routing = routing_with_members("g", &["cheap", "big"], &["last", "cheap", "big"]);
    let cheap = routing.models.get_mut("cheap").expect("cheap");
    cheap.cost_input = 0.1;
    cheap.cost_output = 0.1;
    cheap.weights = Some("/models/cheap.gguf".into());
    let big = routing.models.get_mut("big").expect("big");
    big.intelligence = 5;
    big.weights = Some("/models/big.gguf".into());
    let picked = viable_last_key(&routing, None, &|m| m == "big");
    assert_eq!(picked.as_deref(), Some("big"));
}

/// Calibration for the recency fallback (measurement only).
///
/// Replays empty / stale / valid history against a fixed capability split
/// (`a` too weak, `b` capable and cheap, `c` capable but pool-managed and
/// cold) at complexity 4. The control rows (valid session pick that passes
/// the capability bar) must never emit the coverage audit; every other row
/// must still land on a capability-passing head. Precision (fallback implies
/// a viable head) and recall (session-viable implies no fallback) are
/// asserted as exact bounds so later decision caching cannot silently change
/// the threshold. Each row runs twice to pin determinism.
#[test]
fn last_calibration_fallback_precision_recall() {
    struct Case {
        name: &'static str,
        session_key: &'static str,
        session_wire: Option<String>,
        global_wire: Option<String>,
        use_recency: bool,
        expected_head: &'static str,
        expect_fallback: bool,
        session_viable_present: bool,
    }
    let wire_b = wire_of(&last_viable_routing(), "b");
    let wire_a = wire_of(&last_viable_routing(), "a");
    let wire_c = wire_of(&last_viable_routing(), "c");
    let cases = vec![
        Case {
            name: "valid session capable-cheap serves",
            session_key: "s",
            session_wire: Some(wire_b.clone()),
            global_wire: None,
            use_recency: true,
            expected_head: "b",
            expect_fallback: false,
            session_viable_present: true,
        },
        Case {
            name: "valid session most-capable serves",
            session_key: "s",
            session_wire: Some(wire_c.clone()),
            global_wire: None,
            use_recency: true,
            expected_head: "c",
            expect_fallback: false,
            session_viable_present: true,
        },
        Case {
            name: "stale session yields to viable global",
            session_key: "s",
            session_wire: Some(wire_a.clone()),
            global_wire: Some(wire_b.clone()),
            use_recency: true,
            expected_head: "b",
            expect_fallback: true,
            session_viable_present: false,
        },
        Case {
            name: "ghost session falls back to viable",
            session_key: "s",
            session_wire: Some("ghost-wire".to_string()),
            global_wire: None,
            use_recency: true,
            expected_head: "b",
            expect_fallback: true,
            session_viable_present: false,
        },
        Case {
            name: "both rungs stale fall back to viable",
            session_key: "s",
            session_wire: Some(wire_a.clone()),
            global_wire: Some(wire_a.clone()),
            use_recency: true,
            expected_head: "b",
            expect_fallback: true,
            session_viable_present: false,
        },
        Case {
            name: "unknown session with viable global still audits",
            session_key: "s-new",
            session_wire: None,
            global_wire: Some(wire_b.clone()),
            use_recency: true,
            expected_head: "b",
            expect_fallback: true,
            session_viable_present: false,
        },
        Case {
            name: "empty history falls back to viable",
            session_key: "s",
            session_wire: None,
            global_wire: None,
            use_recency: true,
            expected_head: "b",
            expect_fallback: true,
            session_viable_present: false,
        },
        Case {
            name: "missing recency channel falls back to viable",
            session_key: "s",
            session_wire: None,
            global_wire: None,
            use_recency: false,
            expected_head: "b",
            expect_fallback: true,
            session_viable_present: false,
        },
    ];
    // Seed the "unknown session" row with a record from another session so
    // the global rung is genuinely populated while the requesting session
    // has no history.
    let mut fallback_total = 0;
    let mut fallback_viable = 0;
    let mut session_viable_total = 0;
    let mut session_viable_clean = 0;
    let mut control_fallbacks = 0;
    let mut control_total = 0;
    let mut mismatches = Vec::new();
    for case in &cases {
        let routing = last_viable_routing();
        let recency = RoleRecency::new();
        if case.use_recency {
            if let Some(wire) = &case.session_wire {
                // Session records always land on "s" except the unknown-session
                // row, which seeds "s-other" to keep the requester unknown.
                let session = if case.name == "unknown session with viable global still audits" {
                    "s-other"
                } else {
                    "s"
                };
                recency.record(session, "g", wire);
            }
            if let Some(wire) = &case.global_wire {
                recency.record_any(wire);
            }
        }
        let recency_opt = if case.use_recency {
            Some(&recency)
        } else {
            None
        };
        let run_once = |routing: &RoutingConfig, recency: Option<&RoleRecency>| {
            crate::test_support::capture_logs(|| {
                expand_role_keys(
                    routing,
                    "g",
                    recency,
                    Some(case.session_key),
                    &|_| false,
                    Some(4),
                )
            })
        };
        let (expanded, logs) = run_once(&routing, recency_opt);
        let (expanded2, _) = run_once(&routing, recency_opt);
        if expanded != expanded2 {
            mismatches.push(format!("case {}: expansion not deterministic", case.name));
        }
        let head = expanded.first().cloned().unwrap_or_default();
        if head != case.expected_head {
            mismatches.push(format!(
                "case {}: head {head:?}, expected {:?}",
                case.name, case.expected_head
            ));
        }
        let fallbacks = logs
            .iter()
            .filter(|l| l.contains("last-empty-fallback"))
            .count();
        let fired = fallbacks > 0;
        if fired != case.expect_fallback {
            mismatches.push(format!(
                "case {}: fallback fired={fired} ({} events), expected {}",
                case.name, fallbacks, case.expect_fallback
            ));
        }
        if fallbacks > 1 {
            mismatches.push(format!(
                "case {}: {} fallback events, expected at most 1",
                case.name, fallbacks
            ));
        }
        // Viability of the head: passes the capability bar at complexity 4.
        let head_viable = routing
            .models
            .get(head.as_str())
            .is_some_and(|e| e.intelligence >= 4);
        if fired {
            fallback_total += 1;
            if head_viable {
                fallback_viable += 1;
            }
        }
        if case.session_viable_present {
            session_viable_total += 1;
            control_total += 1;
            if fired {
                control_fallbacks += 1;
            } else {
                session_viable_clean += 1;
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "calibration mismatches:\n{}",
        mismatches.join("\n")
    );
    assert_eq!(
        (control_total, control_fallbacks),
        (2, 0),
        "control (valid session-viable) must emit zero fallbacks"
    );
    assert_eq!(
        (fallback_viable, fallback_total),
        (6, 6),
        "fallback precision must be 1.0: every fallback lands on a viable head"
    );
    assert_eq!(
        (session_viable_clean, session_viable_total),
        (2, 2),
        "fallback recall must be 1.0: session-viable never falls back"
    );
}

#[test]
fn recency_record_and_last_for_roundtrip() {
    let recency = RoleRecency::new();
    assert!(recency.last_for("s", "g").is_none());
    recency.record("s", "g", "b-wire");
    assert_eq!(recency.last_for("s", "g").as_deref(), Some("b-wire"));
    recency.record("s", "g", "a-wire");
    assert_eq!(recency.last_for("s", "g").as_deref(), Some("a-wire"));
    assert!(recency.last_for("s", "other-group").is_none());
}

#[test]
fn expanded_candidates_yield_literal_targets_only() {
    let routing = routing_with_members("g", &["a", "b"], &["last", "a", "any", "zzz-unknown"]);
    let recency = RoleRecency::new();
    recency.record("s", "g", &wire_of(&routing, "a"));
    let cands =
        expanded_candidates_for_role(&routing, "g", Some(&recency), Some("s"), &|_| false, None);
    let keys: Vec<&str> = cands.iter().map(|c| c.model_key.as_str()).collect();
    // Sentinels never become candidates; unknown literals are skipped exactly
    // as the literal path skips members with no `models` entry.
    assert_eq!(keys, vec!["a"]);
}

// ── Ladder calibration corpus (confidence ≠ correctness) ─────────────────
// Measurement only: no production threshold changes. The corpus pins the
// contract a future live-model calibration must beat — selection agreement,
// parse-failure conservatism, the rung-1 control, and termination under an
// overconfident assessment. Nothing here caches or persists a verdict: every
// case runs the matcher twice and demands identical outcomes (the
// determinism property that would make caching safe later).

fn ladder_corpus() -> serde_json::Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/target_match_ladder_corpus.json");
    serde_json::from_str(&std::fs::read_to_string(&path).expect("corpus readable"))
        .expect("corpus parses")
}

fn corpus_candidates(ladder: &serde_json::Value) -> Vec<TargetCandidate> {
    ladder
        .as_array()
        .expect("ladder is an array")
        .iter()
        .map(|c| {
            candidate(
                c.get("key").and_then(|k| k.as_str()).expect("key"),
                c.get("intelligence")
                    .and_then(serde_json::Value::as_u64)
                    .expect("intelligence") as u8,
                1.0,
            )
        })
        .collect()
}

fn corpus_response(entry: &serde_json::Value) -> String {
    match entry {
        serde_json::Value::String(s) => s.clone(),
        obj => serde_json::to_string(obj).expect("response serializes"),
    }
}

fn run_case(case: &serde_json::Value) -> TargetMatch {
    let cands = corpus_candidates(&case["ladder"]);
    let keys: Vec<String> = cands.iter().map(|c| c.model_key.clone()).collect();
    let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    let routing = routing_with("default", &key_refs);
    let complexity = case
        .get("classifier_complexity")
        .and_then(serde_json::Value::as_u64)
        .map(|c| c as u8);
    // The corpus `assessed` array aligns with the walked rungs from the
    // start index: rung `j` answers with `assessed[j - start]`. Map each
    // ladder key to its rung response (empty queue past the array end
    // preserves the old queue-exhaustion `NoResponse`), so the mapping is
    // deterministic under any assessment schedule.
    let start = start_index(&cands, complexity);
    let assessed = case["assessed"].as_array().expect("assessed array");
    let by_key: Vec<(&str, Vec<String>)> = cands
        .iter()
        .enumerate()
        .map(|(j, cand)| {
            let responses = assessed
                .get(j.saturating_sub(start))
                .filter(|_| j >= start)
                .map(corpus_response)
                .map(|response| vec![response])
                .unwrap_or_default();
            (cand.model_key.as_str(), responses)
        })
        .collect();
    let matcher = matcher_with_per_key(by_key);
    matcher
        .match_target("local", "default", &routing, &cands, complexity, "probe")
        .expect("non-empty ladder always terminates with a winner")
}

fn check_case(case: &serde_json::Value) -> bool {
    let first = run_case(case);
    let second = run_case(case);
    // Determinism first: identical inputs must produce identical walks (the
    // no-hidden-state property; the matcher holds no verdict cache).
    assert_eq!(
        first.primary.model, second.primary.model,
        "case {} is deterministic",
        case["name"]
    );
    assert_eq!(
        first.assessments.len(),
        second.assessments.len(),
        "case {} walk length is deterministic",
        case["name"]
    );
    // Corpus pins who serves (base key); the role-window qualifier is
    // asserted separately — every dispatch names `<base>:<role>`.
    let (winner_base, _) = fluent_types::model_key::split_model_key(&first.primary.model);
    let winner_ok = winner_base == case["expected_winner"].as_str().expect("expected winner");
    let count_ok = first.assessments.len()
        == case["expected_assessments"]
            .as_u64()
            .expect("expected count") as usize;
    assert!(winner_ok, "case {} winner", case["name"]);
    assert!(count_ok, "case {} assessment count", case["name"]);
    winner_ok && count_ok
}

#[test]
fn ladder_corpus_selection_agreement() {
    let corpus = ladder_corpus();
    let cases = corpus["selection"].as_array().expect("selection array");
    let mut correct = 0;
    for case in cases {
        if check_case(case) {
            correct += 1;
        }
    }
    let accuracy_numer = correct;
    let accuracy_denom = cases.len();
    assert_eq!(
        accuracy_numer, accuracy_denom,
        "ladder selection agreement over {accuracy_numer}/{accuracy_denom} labeled cases"
    );
}

#[test]
fn control_trivial_prompts_never_escalate() {
    let corpus = ladder_corpus();
    let cases = corpus["control_no_escalate"]
        .as_array()
        .expect("control array");
    assert!(!cases.is_empty(), "control group must not be empty");
    for case in cases {
        let tm = run_case(case);
        assert_eq!(
            tm.assessments.len(),
            1,
            "control case {} must stop at rung 1",
            case["name"]
        );
        assert!(
            tm.assessments[0].matched,
            "control case {} first rung matches",
            case["name"]
        );
        // Corpus pins who serves (base key); the role-window qualifier is
        // asserted separately — every dispatch names `<base>:<role>`.
        let (winner_base, _) = fluent_types::model_key::split_model_key(&tm.primary.model);
        assert_eq!(
            winner_base,
            case["expected_winner"].as_str().expect("expected winner"),
            "control case {} stays on the cheapest member",
            case["name"]
        );
    }
}

#[test]
fn parse_failure_rate_and_conservatism() {
    let corpus = ladder_corpus();
    let valid = corpus["parse_responses"]["valid"]
        .as_array()
        .expect("valid array");
    let invalid = corpus["parse_responses"]["invalid"]
        .as_array()
        .expect("invalid array");
    assert!(!valid.is_empty() && !invalid.is_empty());
    let valid_ok = valid
        .iter()
        .filter(|v| {
            let s = match v {
                serde_json::Value::String(text) => text.clone(),
                obj => serde_json::to_string(obj).expect("serializes"),
            };
            parse_self_assessment(&s).is_ok()
        })
        .count();
    let invalid_err = invalid
        .iter()
        .filter(|v| parse_self_assessment(v.as_str().expect("raw")).is_err())
        .count();
    assert_eq!(valid_ok, valid.len(), "every valid response parses");
    assert_eq!(
        invalid_err,
        invalid.len(),
        "every malformed response fails (failure rate 1.0 on the invalid set)"
    );
    // Conservatism at the matcher level: the corpus mid-climb case opens
    // with garbage — rung 0 must record assessed=None with an error and
    // must NOT match (the climb continues to a capable member).
    let failing = ladder_corpus()["selection"]
        .as_array()
        .expect("selection array")
        .iter()
        .find(|c| c["name"] == "parse_failure_mid_climb_escalates")
        .cloned()
        .expect("mid-climb failure case present");
    let tm = run_case(&failing);
    let first = &tm.assessments[0];
    assert_eq!(first.assessed, None, "unparseable rung assesses nothing");
    assert!(first.error.is_some(), "unparseable rung records the error");
    assert!(!first.matched, "unparseable rung never matches mid-ladder");
}

#[test]
fn confidence_gap_terminates_with_truthful_record() {
    let corpus = ladder_corpus();
    let case = &corpus["confidence_gap"][0];
    // The producer is confident the request is simple (assessed 2) while the
    // label truth is 8: confidence must not stand in for correctness. The
    // bound this milestone pins is termination with a truthful audit trail —
    // the ladder stops, the winner is in-bounds, and the assessed value is
    // recorded as 2 (never inflated to justify the outcome).
    let tm = run_case(case);
    // Corpus pins who serves (base key); the role-window qualifier is
    // asserted separately — every dispatch names `<base>:<role>`.
    let (winner_base, _) = fluent_types::model_key::split_model_key(&tm.primary.model);
    assert_eq!(
        winner_base,
        case["expected_winner"].as_str().expect("expected winner")
    );
    let recorded = tm.assessments[0].assessed.expect("assessed recorded");
    let gap = (case["label_truth_complexity"]
        .as_i64()
        .expect("label truth")
        - i64::from(recorded))
    .abs();
    assert!(
        gap >= 3,
        "corpus must contain a genuine confidence gap, got {gap}"
    );
    assert_eq!(recorded, 2, "record stays truthful to the assessment");
}

// ── Session-scoped recency (M5) ────────────────────────────────────────────
// `last` resolves to the model that last answered IN THIS SESSION; an
// unknown session falls back to the group head (with a `last-empty-fallback`
// audit note at the expansion site); failures never record (pinned at the
// dispatch site — record happens behind the `Ok` arm only).

fn last_group_routing() -> RoutingConfig {
    routing_with("g", &["a", "b"])
}

fn last_group_with_sentinel() -> RoutingConfig {
    let mut routing = last_group_routing();
    routing
        .roles
        .insert("g".into(), role_entry(&["a", "last", "b"]));
    routing
}

#[test]
fn recency_is_session_scoped() {
    let recency = RoleRecency::new();
    recency.record("s1", "g", "a");
    recency.record("s2", "g", "b");
    assert_eq!(recency.last_for("s1", "g").as_deref(), Some("a"));
    assert_eq!(recency.last_for("s2", "g").as_deref(), Some("b"));
    assert!(
        recency.last_for("s3", "g").is_none(),
        "unknown session has no recency"
    );
    assert!(
        recency.last_for("s1", "other").is_none(),
        "recency never crosses groups"
    );
}

#[test]
fn unknown_session_falls_back_to_head() {
    let routing = last_group_with_sentinel();
    let recency = RoleRecency::new();
    // Another session's recency must not leak in.
    recency.record("other-session", "g", "b");
    let expanded = expand_role_keys(&routing, "g", Some(&recency), Some("s1"), &|_| false, None);
    assert_eq!(
        expanded,
        vec!["a".to_string(), "b".to_string()],
        "unknown session skips Last and keeps config order from the head"
    );
}

#[test]
fn known_session_expands_last_in_place() {
    let routing = last_group_with_sentinel();
    let recency = RoleRecency::new();
    recency.record("s1", "g", "b");
    let expanded = expand_role_keys(&routing, "g", Some(&recency), Some("s1"), &|_| false, None);
    assert_eq!(
        expanded,
        vec!["a".to_string(), "b".to_string()],
        "known session expands Last to its last-serving member, deduplicated"
    );
}

#[test]
fn last_naming_non_member_is_skipped() {
    let routing = last_group_with_sentinel();
    let recency = RoleRecency::new();
    // Recorded wire id is no longer a group member (evicted model).
    recency.record("s1", "g", "zzz-removed-model");
    let expanded = expand_role_keys(&routing, "g", Some(&recency), Some("s1"), &|_| false, None);
    assert_eq!(
        expanded,
        vec!["a".to_string(), "b".to_string()],
        "stale Last resolves to nothing — the head serves"
    );
}

#[test]
fn recency_map_is_bounded() {
    let recency = RoleRecency::new();
    for i in 0..(crate::target_match::RECENCY_CAPACITY + 16) {
        recency.record(&format!("session-{i}"), "g", "a");
    }
    assert!(
        recency.len() <= crate::target_match::RECENCY_CAPACITY,
        "oldest entries evicted under the cap"
    );
    // Fresh sessions still resolve after eviction pressure.
    recency.record("fresh", "g", "b");
    assert_eq!(recency.last_for("fresh", "g").as_deref(), Some("b"));
}

#[test]
fn recency_concurrent_records_serialize() {
    use std::sync::Arc;
    let recency = Arc::new(RoleRecency::new());
    let handles: Vec<_> = (0..8)
        .map(|t| {
            let recency = Arc::clone(&recency);
            std::thread::spawn(move || {
                for i in 0..100 {
                    let session = format!("t{t}-s{i}");
                    recency.record(&session, "g", "a");
                    let _ = recency.last_for(&session, "g");
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("no panic under concurrent access");
    }
    assert!(
        recency.len() <= crate::target_match::RECENCY_CAPACITY,
        "cap holds under concurrency"
    );
}

// ── last/any ordering calibration (resource axis, not correctness) ─────────
// Measurement only: no production change. Control tables where `last` must
// NOT fire, `any` loaded-first ordering that leaves the climb underneath
// untouched, and the proof that ordering never outranks capability. No
// verdict is cached or persisted anywhere here.

fn last_any_routing(members: &[&str]) -> RoutingConfig {
    let mut routing = routing_with("g", &["a", "b"]);
    routing.roles.insert("g".into(), role_entry(members));
    routing
}

#[test]
fn last_never_fires_without_session_recency() {
    // Control group: every case must fall back to the head at 100%.
    let head = vec!["a".to_string(), "b".to_string()];
    type Seed = Box<dyn Fn(&RoleRecency)>;
    let cases: Vec<(&str, Seed)> = vec![
        ("first-in-session", Box::new(|_: &RoleRecency| {})),
        (
            "other-session-only",
            Box::new(|r: &RoleRecency| r.record("s-other", "g", "b")),
        ),
        (
            "stale-wire",
            Box::new(|r: &RoleRecency| r.record("s", "g", "zzz-removed")),
        ),
    ];
    for (name, seed) in &cases {
        let routing = last_any_routing(&["a", "last", "b"]);
        let recency = RoleRecency::new();
        seed(&recency);
        let expanded = expand_role_keys(&routing, "g", Some(&recency), Some("s"), &|_| false, None);
        assert_eq!(expanded, head, "control case {name} falls back to the head");
    }
    // The no-session channel is the same fallback.
    let routing = last_any_routing(&["a", "last", "b"]);
    let recency = RoleRecency::new();
    recency.record("s", "g", "b");
    let expanded = expand_role_keys(&routing, "g", Some(&recency), None, &|_| false, None);
    assert_eq!(
        expanded, head,
        "no session channel falls back even with records present"
    );
}

#[test]
fn any_orders_loaded_first_around_last_fallback() {
    let routing = last_any_routing(&["last", "any", "a", "b"]);
    let recency = RoleRecency::new();
    // No recency anywhere: Last falls back (audited), Any partitions.
    let expanded = expand_role_keys(
        &routing,
        "g",
        Some(&recency),
        Some("s"),
        &|m| m == "b",
        None,
    );
    assert_eq!(
        expanded,
        vec!["b".to_string(), "a".to_string()],
        "loaded-first ordering survives the Last fallback"
    );
    let expanded = expand_role_keys(&routing, "g", Some(&recency), Some("s"), &|_| false, None);
    assert_eq!(
        expanded,
        vec!["a".to_string(), "b".to_string()],
        "all-down keeps config order"
    );
}

#[test]
fn ordering_never_outranks_capability() {
    // Last puts weak-b first, but the climb still decides: b assesses too
    // hard, a assesses fitting — a wins from second position. Run twice:
    // identical walks (no hidden verdict cache anywhere).
    let routing = last_any_routing(&["last", "a", "b"]);
    let recency = RoleRecency::new();
    recency.record("s", "g", "b");
    let expanded = expand_role_keys(&routing, "g", Some(&recency), Some("s"), &|_| false, None);
    assert_eq!(expanded, vec!["b".to_string(), "a".to_string()]);
    let cands = candidates(&[("b", 5, 1.0), ("a", 1, 1.0)]);
    for _ in 0..2 {
        let matcher = matcher_with_per_key(vec![
            ("b", vec![assessment(9, "too hard for b")]),
            ("a", vec![assessment(1, "fits a")]),
        ]);
        let tm = matcher
            .match_target("local", "g", &routing, &cands, None, "probe")
            .expect("ladder terminates");
        assert_eq!(
            tm.primary.model, "a:g",
            "recency-favored b does not win on order alone"
        );
        assert_eq!(tm.assessments.len(), 2);
        assert!(!tm.assessments[0].matched, "b assessed out");
        assert!(tm.assessments[1].matched, "a assessed in");
    }
}

#[test]
fn group_recency_eviction_is_seq_total_ordered() {
    // Fill to capacity, re-record the oldest entry, then overflow by one:
    // the re-recorded entry must survive and the true oldest must leave.
    // Sequence stamps are totally ordered, so same-instant re-records can
    // never tie with (and lose to) the entry they just refreshed.
    let recency = RoleRecency::new();
    for i in 0..RECENCY_CAPACITY {
        recency.record("sess", &format!("group-{i}"), &format!("model-{i}"));
    }
    assert_eq!(recency.len(), RECENCY_CAPACITY);
    recency.record("sess", "group-0", "model-0-new");
    recency.record("sess", "group-overflow", "model-overflow");
    assert_eq!(recency.len(), RECENCY_CAPACITY);
    assert_eq!(
        recency.last_for("sess", "group-0").as_deref(),
        Some("model-0-new"),
        "re-recorded entry survives its own refresh"
    );
    assert_eq!(
        recency.last_for("sess", "group-1"),
        None,
        "true oldest entry evicted"
    );
    assert_eq!(
        recency.last_for("sess", "group-overflow").as_deref(),
        Some("model-overflow")
    );
}
