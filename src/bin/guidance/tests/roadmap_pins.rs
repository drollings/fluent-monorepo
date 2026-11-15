//! Hermetic golden pins for read-path contracts: empty states, glob
//! selection, multibyte truncation, and the relevance gate. Included via
//! `#[path]` forwarder from `src/search.rs`, per repo convention.

use super::*;
use guidance_core::query::glob::{
    matches_file_selection, normalize_selection_glob, FileTypePatterns,
};

fn types() -> FileTypePatterns {
    FileTypePatterns::default()
}

fn globs(patterns: &[&str]) -> Vec<String> {
    patterns.iter().map(|pattern| pattern.to_string()).collect()
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

/// Fixed behavior: workspace (`src/<crate>/**`) and index-relative
/// (`<crate>/**`) spellings select the same rows at every layer, so a
/// scoped glob never silently empties. Normalization one layer up stays,
/// and this layer tolerates the prefix too.
#[test]
fn glob_records_current_workspace_prefix_behavior() {
    assert!(matches_file_selection(
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
    assert_eq!(normalize_selection_glob("/abs/**", &roots), "/abs/**");
    assert_eq!(normalize_selection_glob("other/**", &roots), "other/**");
    assert_eq!(normalize_selection_glob("src/**", &roots), "**");
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
