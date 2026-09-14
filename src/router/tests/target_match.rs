use std::collections::HashMap;

use crate::config::{ModelEntry, ModelGroup, RouteRef};
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
    entries.iter().map(|(k, i, c)| candidate(k, *i, *c)).collect()
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
    }
}

fn routing_with(group: &str, keys: &[&str]) -> RoutingConfig {
    RoutingConfig {
        routes: HashMap::from([(
            "local".into(),
            RouteRef {
                group: group.into(),
                role: None,
                pipelines: vec!["default".into()],
                description: "local".into(),
        always_route: false,
            },
        )]),
        models: keys
            .iter()
            .map(|k| (k.to_string(), model_entry(k, 0, 1.0)))
            .collect(),
        model_groups: HashMap::from([(
            group.into(),
            ModelGroup::Array(keys.iter().map(ToString::to_string).collect()),
        )]),
        system_prompt: String::new(),
        safety_threshold: 0.3,
        default_route: "local".into(),
        score_matrix: None,
        onnx_keys: std::collections::BTreeSet::new(),
        roles: Default::default(),
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

#[test]
fn target_backends_get_prefers_dedicated_then_default() {
    let dedicated: Arc<dyn ChatBackend> =
        Arc::new(StubChatBackend::always(assessment(2, "dedicated")));
    let default: Arc<dyn ChatBackend> =
        Arc::new(StubChatBackend::always(assessment(8, "default")));
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
    let matcher = matcher_with(vec![assessment(4, "mid"), assessment(6, "hard")]);

    let tm = matcher
        .match_target("local", "default", &routing, &cands, Some(3), "hello")
        .expect("match");
    // start_index(Some(3)) = 1 (qwen3.5-9b, the first member with
    // intelligence >= 3). It self-assesses 4 <= 4 → match.
    assert_eq!(tm.primary.model, "qwen3.5-9b");
    assert_eq!(tm.primary.target_name.as_deref(), Some("local"));
    assert_eq!(tm.primary.group.as_deref(), Some("default"));
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
    let matcher = matcher_with(vec![
        assessment(7, "hard for qwen3.5"),
        assessment(6, "ok for 27b"),
    ]);

    let tm = matcher
        .match_target("local", "default", &routing, &cands, Some(3), "hello")
        .expect("match");
    // start_index(Some(3)) = 1. qwen3.5-9b self-assesses 7 > 4 → escalate.
    // qwen3.6-27b self-assesses 6 <= 6 → match.
    assert_eq!(tm.primary.model, "qwen3.6-27b");
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
    let matcher = matcher_with(vec![assessment(1, "easy"), assessment(3, "ok")]);

    let tm = matcher
        .match_target("local", "default", &routing, &cands, None, "hello")
        .expect("match");
    // No classifier estimate → start at 0 (swarm). Swarm self-assesses 1 <= 2 → match.
    assert_eq!(tm.primary.model, "swarm");
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
    let matcher = matcher_with(vec![
        "not json".into(),
        assessment(4, "ok for qwen3.5"),
        assessment(6, "ok for 27b"),
    ]);

    let tm = matcher
        .match_target("local", "default", &routing, &cands, None, "hello")
        .expect("match");
    assert_eq!(tm.primary.model, "qwen3.5-9b");
    assert_eq!(tm.assessments.len(), 2);
    assert_eq!(tm.assessments[0].assessed, None);
    assert!(tm.assessments[0].error.is_some());
    assert!(!tm.assessments[0].matched);
}

#[test]
fn llm_error_escalates_conservatively() {
    let cands = candidates(&[("swarm", 2, 1.0), ("qwen3.6-27b", 6, 3.0)]);
    let routing = routing_with("default", &["swarm", "qwen3.6-27b"]);
    // Empty queue → both self-assessment calls fail with NoResponse.
    // Conservative escalation: a candidate that cannot confirm (LLM error)
    // is skipped; the last member still matches (terminate-don't-loop).
    let matcher = matcher_with(vec![]);

    let tm = matcher
        .match_target("local", "default", &routing, &cands, None, "hello")
        .expect("match");
    assert_eq!(tm.primary.model, "qwen3.6-27b");
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
    let matcher = matcher_with(vec![assessment(3, "ok for swarm"), assessment(9, "hard")]);

    let tm = matcher
        .match_target("local", "default", &routing, &cands, None, "hello")
        .expect("match");
    assert_eq!(tm.primary.model, "qwen3.6-27b");
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
    let matcher = matcher_with(vec![assessment(7, "too hard"), assessment(6, "match")]);

    let tm = matcher
        .match_target("local", "default", &routing, &cands, Some(3), "hello")
        .expect("match");
    // start index 1 → swarm never self-assesses; exactly 2 calls made.
    assert_eq!(tm.assessments.len(), 2);
    assert_eq!(tm.assessments[0].model_key, "qwen3.5-9b");
    assert_eq!(tm.primary.model, "qwen3.6-27b");
}

#[test]
fn fallbacks_are_group_tail_then_cross_group() {
    let cands = candidates(&[
        ("swarm", 2, 1.0),
        ("qwen3.5-9b", 4, 3.0),
        ("qwen3.6-27b", 6, 5.0),
    ]);
    let routing = routing_with("default", &["swarm", "qwen3.5-9b", "qwen3.6-27b"]);
    let matcher = matcher_with(vec![assessment(1, "easy")]);

    let tm = matcher
        .match_target("local", "default", &routing, &cands, None, "hello")
        .expect("match");
    assert_eq!(tm.primary.model, "swarm");
    // Fallback tail = the more-intelligent members of the group, in order.
    let fb_models: Vec<&str> = tm.primary.fallbacks.iter().map(|f| f.model.as_str()).collect();
    assert_eq!(fb_models, vec!["qwen3.5-9b", "qwen3.6-27b"]);
}

#[test]
fn empty_candidates_is_none() {
    let routing = routing_with("default", &[]);
    let matcher = matcher_with(vec![]);
    assert!(
        matcher
            .match_target("local", "default", &routing, &[], Some(3), "hello")
            .is_none()
    );
}

// ── Availability-sentinel expansion (`last` / `any`) ────────────────────

fn routing_with_members(group: &str, entries: &[&str], members: &[&str]) -> RoutingConfig {
    RoutingConfig {
        routes: HashMap::from([(
            "local".into(),
            RouteRef {
                group: group.into(),
                role: None,
                pipelines: vec!["default".into()],
                description: "local".into(),
                always_route: false,
            },
        )]),
        models: entries
            .iter()
            .map(|k| (k.to_string(), model_entry(k, 0, 1.0)))
            .collect(),
        model_groups: HashMap::from([(
            group.into(),
            ModelGroup::Array(members.iter().map(ToString::to_string).collect()),
        )]),
        system_prompt: String::new(),
        safety_threshold: 0.3,
        default_route: "local".into(),
        score_matrix: None,
        onnx_keys: std::collections::BTreeSet::new(),
        roles: Default::default(),
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
    let expanded = expand_group_keys(&routing, "g", None, None, &|_| false);
    assert_eq!(expanded, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn expansion_last_with_recorded_success_orders_first() {
    let routing = routing_with_members("g", &["a", "b"], &["last", "a", "b"]);
    let recency = GroupRecency::new();
    recency.record("s", "g", &wire_of(&routing, "b"));
    let expanded = expand_group_keys(&routing, "g", Some(&recency), Some("s"), &|_| false);
    assert_eq!(expanded, vec!["b".to_string(), "a".to_string()]);
}

#[test]
fn expansion_last_without_success_skips_sentinel() {
    let routing = routing_with_members("g", &["a", "b"], &["last", "a", "b"]);
    let expanded = expand_group_keys(&routing, "g", None, None, &|_| false);
    assert_eq!(expanded, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn expansion_last_pointing_at_removed_member_skips() {
    let routing = routing_with_members("g", &["a", "b"], &["last", "a"]);
    let recency = GroupRecency::new();
    recency.record("s", "g", "zzz-removed-model");
    let expanded = expand_group_keys(&routing, "g", Some(&recency), Some("s"), &|_| false);
    assert_eq!(expanded, vec!["a".to_string()]);
}

#[test]
fn expansion_any_orders_loaded_first() {
    let routing = routing_with_members("g", &["a", "b"], &["any", "a", "b"]);
    let expanded = expand_group_keys(&routing, "g", None, None, &|m| m == "b");
    assert_eq!(expanded, vec!["b".to_string(), "a".to_string()]);
    let expanded = expand_group_keys(&routing, "g", None, None, &|m| m == "a");
    assert_eq!(expanded, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn expansion_any_all_down_keeps_config_order() {
    let routing = routing_with_members("g", &["a", "b"], &["any", "a", "b"]);
    let expanded = expand_group_keys(&routing, "g", None, None, &|_| false);
    assert_eq!(expanded, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn expansion_any_onnx_members_count_as_loaded() {
    let mut routing = routing_with_members("g", &["a"], &["any", "onnx/llm", "a"]);
    routing.onnx_keys.insert("onnx/llm".to_string());
    // No server is running anywhere; the onnx role still orders first via the
    // existing registry readiness (no new probe).
    let expanded = expand_group_keys(&routing, "g", None, None, &|_| false);
    assert_eq!(expanded, vec!["onnx/llm".to_string(), "a".to_string()]);
}

#[test]
fn recency_record_and_last_for_roundtrip() {
    let recency = GroupRecency::new();
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
    let recency = GroupRecency::new();
    recency.record("s", "g", &wire_of(&routing, "a"));
    let cands = expanded_candidates_for_group(&routing, "g", Some(&recency), Some("s"), &|_| false);
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

fn corpus_responses(assessed: &serde_json::Value) -> Vec<String> {
    assessed
        .as_array()
        .expect("assessed is an array")
        .iter()
        .map(|a| match a {
            serde_json::Value::String(s) => s.clone(),
            obj => serde_json::to_string(obj).expect("response serializes"),
        })
        .collect()
}

fn run_case(case: &serde_json::Value) -> TargetMatch {
    let cands = corpus_candidates(&case["ladder"]);
    let keys: Vec<String> = cands.iter().map(|c| c.model_key.clone()).collect();
    let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    let routing = routing_with("default", &key_refs);
    let matcher = matcher_with(corpus_responses(&case["assessed"]));
    let complexity = case
        .get("classifier_complexity")
        .and_then(serde_json::Value::as_u64)
        .map(|c| c as u8);
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
    let winner_ok = first.primary.model.as_str()
        == case["expected_winner"].as_str().expect("expected winner");
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
    let cases = corpus["control_no_escalate"].as_array().expect("control array");
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
        assert_eq!(
            tm.primary.model.as_str(),
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
    assert_eq!(
        tm.primary.model.as_str(),
        case["expected_winner"].as_str().expect("expected winner")
    );
    let recorded = tm.assessments[0].assessed.expect("assessed recorded");
    let gap = (case["label_truth_complexity"]
        .as_i64()
        .expect("label truth") - i64::from(recorded))
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
    routing.model_groups.insert(
        "g".into(),
        ModelGroup::Array(vec!["a".into(), "last".into(), "b".into()]),
    );
    routing
}

#[test]
fn recency_is_session_scoped() {
    let recency = GroupRecency::new();
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
    let recency = GroupRecency::new();
    // Another session's recency must not leak in.
    recency.record("other-session", "g", "b");
    let expanded = expand_group_keys(&routing, "g", Some(&recency), Some("s1"), &|_| false);
    assert_eq!(
        expanded,
        vec!["a".to_string(), "b".to_string()],
        "unknown session skips Last and keeps config order from the head"
    );
}

#[test]
fn known_session_expands_last_in_place() {
    let routing = last_group_with_sentinel();
    let recency = GroupRecency::new();
    recency.record("s1", "g", "b");
    let expanded = expand_group_keys(&routing, "g", Some(&recency), Some("s1"), &|_| false);
    assert_eq!(
        expanded,
        vec!["a".to_string(), "b".to_string()],
        "known session expands Last to its last-serving member, deduplicated"
    );
}

#[test]
fn last_naming_non_member_is_skipped() {
    let routing = last_group_with_sentinel();
    let recency = GroupRecency::new();
    // Recorded wire id is no longer a group member (evicted model).
    recency.record("s1", "g", "zzz-removed-model");
    let expanded = expand_group_keys(&routing, "g", Some(&recency), Some("s1"), &|_| false);
    assert_eq!(
        expanded,
        vec!["a".to_string(), "b".to_string()],
        "stale Last resolves to nothing — the head serves"
    );
}

#[test]
fn recency_map_is_bounded() {
    let recency = GroupRecency::new();
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
    let recency = Arc::new(GroupRecency::new());
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
    routing.model_groups.insert(
        "g".into(),
        ModelGroup::Array(members.iter().map(ToString::to_string).collect()),
    );
    routing
}

#[test]
fn last_never_fires_without_session_recency() {
    // Control group: every case must fall back to the head at 100%.
    let head = vec!["a".to_string(), "b".to_string()];
    type Seed = Box<dyn Fn(&GroupRecency)>;
    let cases: Vec<(&str, Seed)> = vec![
        ("first-in-session", Box::new(|_: &GroupRecency| {})),
        (
            "other-session-only",
            Box::new(|r: &GroupRecency| r.record("s-other", "g", "b")),
        ),
        (
            "stale-wire",
            Box::new(|r: &GroupRecency| r.record("s", "g", "zzz-removed")),
        ),
    ];
    for (name, seed) in &cases {
        let routing = last_any_routing(&["a", "last", "b"]);
        let recency = GroupRecency::new();
        seed(&recency);
        let expanded =
            expand_group_keys(&routing, "g", Some(&recency), Some("s"), &|_| false);
        assert_eq!(expanded, head, "control case {name} falls back to the head");
    }
    // The no-session channel is the same fallback.
    let routing = last_any_routing(&["a", "last", "b"]);
    let recency = GroupRecency::new();
    recency.record("s", "g", "b");
    let expanded = expand_group_keys(&routing, "g", Some(&recency), None, &|_| false);
    assert_eq!(
        expanded, head,
        "no session channel falls back even with records present"
    );
}

#[test]
fn any_orders_loaded_first_around_last_fallback() {
    let routing = last_any_routing(&["last", "any", "a", "b"]);
    let recency = GroupRecency::new();
    // No recency anywhere: Last falls back (audited), Any partitions.
    let expanded =
        expand_group_keys(&routing, "g", Some(&recency), Some("s"), &|m| m == "b");
    assert_eq!(
        expanded,
        vec!["b".to_string(), "a".to_string()],
        "loaded-first ordering survives the Last fallback"
    );
    let expanded =
        expand_group_keys(&routing, "g", Some(&recency), Some("s"), &|_| false);
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
    let recency = GroupRecency::new();
    recency.record("s", "g", "b");
    let expanded =
        expand_group_keys(&routing, "g", Some(&recency), Some("s"), &|_| false);
    assert_eq!(expanded, vec!["b".to_string(), "a".to_string()]);
    let cands = candidates(&[("b", 5, 1.0), ("a", 1, 1.0)]);
    for _ in 0..2 {
        let matcher = matcher_with(vec![
            assessment(9, "too hard for b"),
            assessment(1, "fits a"),
        ]);
        let tm = matcher
            .match_target("local", "g", &routing, &cands, None, "probe")
            .expect("ladder terminates");
        assert_eq!(
            tm.primary.model, "a",
            "recency-favored b does not win on order alone"
        );
        assert_eq!(tm.assessments.len(), 2);
        assert!(!tm.assessments[0].matched, "b assessed out");
        assert!(tm.assessments[1].matched, "a assessed in");
    }
}
