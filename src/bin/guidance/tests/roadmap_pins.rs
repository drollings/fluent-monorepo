//! Hermetic golden pins for read-path contracts: empty states, glob
//! selection, multibyte truncation, and the relevance gate. Included via
//! `#[path]` forwarder from `src/search.rs`, per repo convention.

use super::*;
use guidance_core::query::glob::{
    matches_file_selection, normalize_selection_glob, FileTypePatterns,
};
use guidance_core::search_types::FragmentContent;

fn types() -> FileTypePatterns {
    FileTypePatterns::default()
}

fn globs(patterns: &[&str]) -> Vec<String> {
    patterns.iter().map(|pattern| pattern.to_string()).collect()
}

#[test]
fn empty_render_with_no_rows_and_no_capabilities() {
    let out = render_explain_full(
        "what owns calibration",
        &[],
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
    );
    assert_eq!(out, "## Explain: what owns calibration\n\nNo results found.\n");
}

#[test]
fn empty_state_is_single_string() {
    // Unified contract: recall-empty and gated-empty render identically —
    // the recall-vs-gated distinction lives in the envelope gaps, never
    // in a second human string. "No results found" read as proof of
    // absence; "no relevant results" reads as what it is.
    let out = render_empty_explain("what owns calibration");
    assert_eq!(
        out,
        "## Explain: what owns calibration\n\nNo relevant results found.\n"
    );
}

#[test]
fn glob_matches_indexed_relative_form() {
    assert!(matches_file_selection(
        "guidance/src/sync_engine.rs",
        &globs(&["guidance/**"]),
        &[],
        &types(),
    ));
    assert!(matches_file_selection(
        "guidance/src/sync_engine.rs",
        &globs(&["**/sync_engine.rs"]),
        &[],
        &types(),
    ));
}

/// Records the layer contract: `matches_file_selection` matches the indexed
/// root-relative form only. Workspace-relative spellings are converted to
/// this form one layer up (`normalize_selection_glob` at the recall
/// entries), so this pin stays as-is while the round-trip pins prove the
/// end-to-end behavior.
#[test]
fn glob_records_current_workspace_prefix_behavior() {
    assert!(!matches_file_selection(
        "guidance/src/sync_engine.rs",
        &globs(&["src/guidance/**"]),
        &[],
        &types(),
    ));
}

#[test]
fn glob_rejects_cross_crate_paths() {
    assert!(!matches_file_selection(
        "router/src/ranking.rs",
        &globs(&["guidance/**"]),
        &[],
        &types(),
    ));
    assert!(!matches_file_selection(
        "guidance/src/sync_engine.rs",
        &globs(&["router/**"]),
        &[],
        &types(),
    ));
}

#[test]
fn grep_arm_survives_every_cut_alignment() {
    // Every byte residue mod 4 at the cut point: the excerpt cap must land
    // mid-character for some alignment, and none may panic.
    for pad in 0..8 {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("notes.md");
        let prefix = "q".repeat(pad);
        let line = format!("{prefix}calibration: {} end", "日本語テスト✓端".repeat(30));
        std::fs::write(&path, format!("{line}\n")).expect("write");
        let corpus = vec![path];
        let (out, files) = crate::benchmark::grep_arm(
            &corpus,
            dir.path().to_str().expect("utf8"),
            &["calibration".to_string()],
            50,
        );
        assert_eq!(files.len(), 1, "pad {pad}");
        assert!(out.contains("calibration"), "pad {pad}");
    }
}

fn relevant_hit_with_text(name: &str, stem_abs: &str, stem: &str, text: &str) -> SearchHit {
    let mut hit = hit_for_test(stem_abs, stem, 1, 10);
    hit.entity.content = FragmentContent::Text {
        text: format!("fn {name}() {{}} {text}"),
    };
    hit
}

#[test]
fn relevance_single_token_keeps_everything() {
    let hit = relevant_hit_with_text("unrelated_symbol", "/w/a.rs", "a.rs", "nothing");
    assert!(hit_is_relevant(&hit, &significant_query_tokens("calibration")));
}

#[test]
fn relevance_structural_match_keeps() {
    let hit = relevant_hit_with_text(
        "calibrate_point",
        "/w/registry.rs",
        "registry.rs",
        "nothing matching here",
    );
    assert!(hit_is_relevant(
        &hit,
        &significant_query_tokens("calibrate the decision point"),
    ));
}

#[test]
fn relevance_prose_needs_two_distinct_tokens() {
    let one = relevant_hit_with_text(
        "other",
        "/w/policy.rs",
        "policy.rs",
        "this mentions calibration only",
    );
    assert!(!hit_is_relevant(
        &one,
        &significant_query_tokens("calibration decision registry"),
    ));
    let two = relevant_hit_with_text(
        "other",
        "/w/policy.rs",
        "policy.rs",
        "calibration registry entries authorize decision points",
    );
    assert!(hit_is_relevant(
        &two,
        &significant_query_tokens("calibration decision registry"),
    ));
}

#[test]
fn relevance_file_stem_alone_never_qualifies() {
    // One token matching only via the file stem is not enough, even when
    // the other token is absent everywhere: the stem never contributes the
    // second distinct token on its own.
    let hit = relevant_hit_with_text(
        "other",
        "/w/components.rs",
        "components.rs",
        "nothing matching here at all",
    );
    assert!(!hit_is_relevant(
        &hit,
        &significant_query_tokens("components portal"),
    ));
}

#[test]
fn relevance_filter_preserves_rank_order() {
    let keep = relevant_hit_with_text(
        "calibrate_point",
        "/w/registry.rs",
        "registry.rs",
        "calibration registry authorizes",
    );
    let drop = relevant_hit_with_text(
        "other",
        "/w/policy.rs",
        "policy.rs",
        "nothing relevant at all here",
    );
    let out = filter_relevant_hits(
        &[drop, keep.clone()],
        &significant_query_tokens("calibration registry"),
    );
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].entity.id, keep.entity.id);
}

#[test]
fn selection_glob_strips_indexing_root_once() {
    let roots = ["src"];
    assert_eq!(
        normalize_selection_glob("src/guidance/**", &roots),
        "guidance/**"
    );
    assert_eq!(
        normalize_selection_glob("guidance/**", &roots),
        "guidance/**"
    );
    assert_eq!(
        normalize_selection_glob("**/sync_engine.rs", &roots),
        "**/sync_engine.rs"
    );
    assert_eq!(normalize_selection_glob("src", &roots), "src");
    assert_eq!(
        normalize_selection_glob("!src/guidance/**", &roots),
        "!guidance/**"
    );
    assert_eq!(
        normalize_selection_glob("/abs/**", &roots),
        "/abs/**"
    );
    assert_eq!(normalize_selection_glob("other/**", &roots), "other/**");
    assert_eq!(normalize_selection_glob("src/**", &roots), "**");
}

#[test]
fn branch_scope_round_trip_selects_indexed_paths() {
    let scope = branch_scope_glob("guidance-core").expect("mapped");
    assert_eq!(scope, "src/guidance/**");
    let roots = ["src"];
    let normal = normalize_selection_glob(&scope, &roots);
    assert_eq!(normal, "guidance/**");
    assert!(matches_file_selection(
        "guidance/src/sync_engine.rs",
        &[normal],
        &[],
        &types(),
    ));
}

#[test]
fn normalized_scopes_never_cross_crates() {
    let roots = ["src"];
    let scope = normalize_selection_glob(
        &branch_scope_glob("fluent-router").expect("mapped"),
        &roots,
    );
    assert_eq!(scope, "router/**");
    assert!(!matches_file_selection(
        "guidance/src/sync_engine.rs",
        &[scope],
        &[],
        &types(),
    ));
}

#[test]
fn envelope_json_carries_machine_handoff_shape() {
    use guidance_core::query::envelope::AnswerEnvelope;
    use guidance_core::query::strategy::FsmEngine;
    let mut fsm = FsmEngine::new();
    let class = fsm.run("what owns calibration");
    let route = fsm.route(&class).to_string();
    let envelope = AnswerEnvelope::from_stages(&class, &route, Vec::new());
    let value = envelope_json(&envelope);
    assert_eq!(value["schema_version"], serde_json::json!(1));
    assert!(value["route"].is_string(), "{value}");
    assert!(value["intent"].is_string(), "{value}");
    assert!(value["gaps"].is_array(), "{value}");
    assert!(!value["gaps"].as_array().expect("gaps").is_empty());
    assert_eq!(
        value["confidence"]["axis"],
        serde_json::json!("ProducerOrderOnly")
    );
}

#[test]
fn answer_messages_never_carry_unjudged_doc_prose() {
    // The synthesis builder takes code snippets and Strong briefs only:
    // doc paragraphs — judged or `[unjudged]` — have no parameter to
    // travel through, so ranked prose cannot steer the answer.
    let snippets = vec![(
        "alpha".to_string(),
        "lib.rs".to_string(),
        1,
        3,
        "pub fn alpha() {}".to_string(),
    )];
    let messages = answer_messages("alpha helper adds", &snippets, &[]);
    let body = messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(body.contains("pub fn alpha()"), "{body}");
    assert!(
        !body.contains("adds one to every total"),
        "doc prose must not reach synthesis: {body}"
    );
}

#[test]
fn judged_floor_keeps_passing_drops_failing() {
    use std::collections::BTreeMap;
    let docs = vec![
        ("a.md".to_string(), "relevant prose".to_string()),
        ("b.md".to_string(), "irrelevant prose".to_string()),
    ];
    let scores: BTreeMap<String, u64> =
        [("1".to_string(), 95), ("2".to_string(), 5)].into_iter().collect();
    let kept = select_judged_docs(&docs, &scores, 60, 3);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0].0, "a.md");
}

#[test]
fn judged_floor_sensitivity_pins_why_sixty() {
    // A near-miss control at 59 drops at the locked floor but would pass
    // at 50: the floor is the control-group barrier, not a round number.
    use std::collections::BTreeMap;
    let docs = vec![("trap.md".to_string(), "plausible prose".to_string())];
    let scores: BTreeMap<String, u64> = [("1".to_string(), 59)].into_iter().collect();
    assert!(select_judged_docs(&docs, &scores, 60, 3).is_empty());
    assert_eq!(select_judged_docs(&docs, &scores, 50, 3).len(), 1);
}

#[test]
fn judged_floor_ignores_bad_keys_and_caps_top_k() {
    use std::collections::BTreeMap;
    let docs = vec![
        ("a.md".to_string(), "one".to_string()),
        ("b.md".to_string(), "two".to_string()),
        ("c.md".to_string(), "three".to_string()),
    ];
    let scores: BTreeMap<String, u64> = [
        ("0".to_string(), 99),
        ("1".to_string(), 90),
        ("2".to_string(), 80),
        ("9".to_string(), 100),
        ("bogus".to_string(), 100),
    ]
    .into_iter()
    .collect();
    let kept = select_judged_docs(&docs, &scores, 60, 2);
    assert_eq!(kept.len(), 2, "top_k caps: {kept:?}");
    assert_eq!(kept[0].0, "a.md", "best-first: {kept:?}");
    let uncut = select_judged_docs(&docs, &scores, 60, 10);
    assert_eq!(uncut.len(), 2, "out-of-range keys never index: {uncut:?}");
}

#[test]
fn doc_judge_corpus_is_valid() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../guidance/corpora/doc_judge_goldens.json"
    ))
    .expect("corpus reads");
    let corpus: serde_json::Value = serde_json::from_str(&text).expect("corpus parses");
    assert_eq!(
        corpus["provenance"]["origin"],
        serde_json::json!("human"),
        "labels are human judgments, never model output"
    );
    let mut controls = std::collections::HashSet::new();
    for entry in corpus["control_group"].as_array().expect("control group") {
        controls.insert(entry.as_str().expect("str").to_string());
    }
    assert!(!controls.is_empty());
    for case in corpus["cases"].as_array().expect("cases") {
        let id = case["id"].as_str().expect("id");
        let paragraphs = case["paragraphs"].as_array().expect("paragraphs");
        for index in case["expected_keep"].as_array().expect("expected_keep") {
            let i = index.as_u64().expect("index") as usize;
            assert!(i < paragraphs.len(), "{id}: keep index in range");
            assert!(
                !controls.contains(&format!("{id}/{i}")),
                "{id}/{i}: expected-keep must not be a control"
            );
        }
    }
}

#[test]
fn recall_policy_needs_a_passing_summary_entry() {
    // Default-deny: an empty registry resolves to `Disabled` (byte-identical
    // recall); only a passing entry on all three keys opens `Act`. The
    // fingerprint binds the grant to the generating model.
    use fluent_llm::decision::{CalibrationRegistry, CorpusProvenance, GateReport, RegistryEntry};
    use guidance_core::query::decision_points::{
        SUMMARY_INDEX_POINT, SUMMARY_INDEX_VERSION, summary_index_fingerprint,
    };
    use guidance_core::query::recall::{RecallPolicy, SummaryMode};
    let model = "policy-test-model";
    assert_eq!(
        recall_policy_for(&CalibrationRegistry::empty(), model),
        RecallPolicy::default()
    );
    let mut registry = CalibrationRegistry::empty();
    registry.upsert(RegistryEntry {
        point: SUMMARY_INDEX_POINT.to_string(),
        model_fingerprint: summary_index_fingerprint(model),
        question_version: SUMMARY_INDEX_VERSION,
        thresholds: fluent_llm::decision::ThresholdSet { act: 0.9, review: 0.7 },
        gate: GateReport {
            threshold: 0.9,
            precision: 1.0,
            recall: 1.0,
            fpr: 0.0,
            support: 2,
            control_fires: 0,
            passed: true,
            date: "test".to_string(),
        },
        provenance: CorpusProvenance {
            source: "test".to_string(),
            labeller: "test".to_string(),
            date: "test".to_string(),
            dataset: String::new(),
        },
        dataset: String::new(),
    });
    assert!(
        matches!(recall_policy_for(&registry, model).summary, SummaryMode::Act),
        "passing entry authorizes Act"
    );
    assert_eq!(
        recall_policy_for(&registry, "other-model"),
        RecallPolicy::default(),
        "grant is fingerprint-bound"
    );
}
