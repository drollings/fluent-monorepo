//! Baseline lock for the six benchmark questions: the measurement
//! instrument (task file) plus hermetic characterization of the three
//! failure inputs. Included via `#[path]` forwarder from `src/search.rs`,
//! per repo convention.

use super::*;
use std::collections::BTreeSet;

fn task_file() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("guidance")
        .join("corpora")
        .join("benchmark_tasks.jsonl")
}

fn task_ids() -> Vec<(String, Vec<String>)> {
    let text = std::fs::read_to_string(task_file()).expect("benchmark tasks load");
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).expect("task line parses");
            let id = value["id"].as_str().expect("task id").to_string();
            let relevant = value["relevant"]
                .as_array()
                .expect("relevant array")
                .iter()
                .map(|entry| entry.as_str().expect("relevant entry").to_string())
                .collect();
            (id, relevant)
        })
        .collect()
}

#[test]
fn benchmark_instrument_covers_six_questions() {
    let tasks = task_ids();
    assert_eq!(tasks.len(), 9);
    let ids: BTreeSet<String> = tasks.iter().map(|task| task.0.clone()).collect();
    for expected in [
        "q1-components",
        "q2-calibration",
        "q3-sync",
        "q4-capabilities",
        "q5-clean",
        "q6-ranking",
        "q7-control-nonexistent",
        "q8-control-unknown-symbol",
        "q9-control-scope-eval",
    ] {
        assert!(ids.contains(expected), "missing task {expected}");
    }
}

#[test]
fn benchmark_goldens_name_expected_owners() {
    let tasks: std::collections::HashMap<String, Vec<String>> = task_ids().into_iter().collect();
    let basenames = |id: &str| -> BTreeSet<String> {
        tasks[id]
            .iter()
            .map(|path| {
                std::path::Path::new(path)
                    .file_name()
                    .expect("basename")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    };
    assert!(basenames("q1-components").contains("README.md"));
    assert!(basenames("q1-components").contains("Cargo.toml"));
    assert!(basenames("q2-calibration").contains("decision_points.rs"));
    assert!(basenames("q2-calibration").contains("registry.rs"));
    assert!(basenames("q2-calibration").contains("calibration.rs"));
    assert!(basenames("q3-sync").contains("sync_engine.rs"));
    assert!(basenames("q3-sync").contains("staleness.rs"));
    assert!(basenames("q3-sync").contains("freshness.rs"));
    assert!(basenames("q4-capabilities").contains("capability_match.rs"));
    assert!(basenames("q5-clean").contains("main.rs"));
    assert!(basenames("q6-ranking").contains("search.rs"));
    assert!(basenames("q6-ranking").contains("hybrid.rs"));
    assert!(basenames("q6-ranking").contains("search_types.rs"));
}

#[test]
fn q1_orientation_query_carries_no_symbol_signal() {
    let tokens = guidance_core::read::vocabulary::query_terms(
        "What are the major components of this codebase?",
    );
    assert!(!tokens.is_empty());
    let symbols = guidance_core::query::tokens::symbol_names(
        "What are the major components of this codebase?",
    );
    // Over-minting characterization: every prose word passes the symbol
    // filter, yet none is identifier-shaped (no `_` or `::`), so the
    // symbol-filtered routes fuse prose matches into rank. M4 gates this.
    assert!(!symbols.is_empty());
    assert!(
        symbols
            .iter()
            .all(|name| !name.contains('_') && !name.contains("::")),
        "orientation prose mints no identifier-shaped filter: {symbols:?}"
    );
    let plan = build_search_plan(
        "What are the major components of this codebase?",
        SearchMode::Fuse,
        10,
        false,
        true,
        &[],
        &[],
    );
    assert!(!plan.routes.is_empty());
}

#[test]
fn q3_sync_query_mints_the_decide_collision_pair() {
    let tokens = guidance_core::read::vocabulary::query_terms(
        "How does guidance sync decide what to reprocess after an edit?",
    );
    assert!(tokens.contains(&"decide".to_string()), "tokens: {tokens:?}");
    assert!(tokens.contains(&"sync".to_string()), "tokens: {tokens:?}");
}

#[test]
fn q6_ranking_query_names_both_competing_vocabulary() {
    let tokens = guidance_core::read::vocabulary::query_terms(
        "How does the query pipeline rank code hits vs capability briefs?",
    );
    assert!(
        tokens.contains(&"capability".to_string()),
        "tokens: {tokens:?}"
    );
    assert!(
        tokens.contains(&"rank".to_string()) || tokens.contains(&"code".to_string()),
        "tokens: {tokens:?}"
    );
}

/// M2-cal: rank-half calibration over the doc-judge goldens.
///
/// Measured with the real `rank_doc_paragraphs` (overlap >= 1 keeps):
/// keeps surviving rank 3/5 (recall 0.6), traps surviving rank 3/4.
/// The lexical pre-gate is a cost control (judge-call budget), never a
/// relevance authority: semantic keeps with no surface overlap
/// (sync-reprocess keep 0, major-components keep 0) fall out here and
/// can only return via the judge over a wider candidate set or via the
/// capability tier. Bars lock the current behavior against drift; they
/// do not claim the rank half is sufficient.
///
/// M2-cal control: an exact-symbol query over a doc tree ranks nothing,
/// so the judge is never invoked. Control fire-rate at the judge-call
/// level is exactly zero.
fn evidence_snippets() -> Vec<(String, String, u32, u32, String)> {
    vec![
        (
            "gen_if_stale".to_string(),
            "src/guidance/src/sync_engine.rs".to_string(),
            280,
            310,
            "fn gen_if_stale body".to_string(),
        ),
        (
            "is_stale".to_string(),
            "src/guidance/src/sync/staleness.rs".to_string(),
            1,
            30,
            "fn is_stale body".to_string(),
        ),
    ]
}

#[test]
fn answer_citing_undisplayed_crate_falls_back() {
    let answer = "The decide function in orator-agent/src/tools/policy.rs:85 \
        decides what to reprocess after an edit.";
    let constrained =
        guidance_core::read::claims::constrain_answer_to_evidence(answer, &evidence_snippets());
    assert!(
        !constrained.contains("orator"),
        "undisplayed crate must not survive: {constrained}"
    );
    assert!(
        constrained.contains("src/guidance/src/sync_engine.rs:280-310"),
        "fallback must list shown evidence: {constrained}"
    );
}

#[test]
fn answer_citing_shown_evidence_passes_through() {
    let answer = "Reprocessing is decided by gen_if_stale \
        (src/guidance/src/sync_engine.rs:295) with is_stale \
        (src/guidance/src/sync/staleness.rs:5).";
    let constrained =
        guidance_core::read::claims::constrain_answer_to_evidence(answer, &evidence_snippets());
    assert_eq!(constrained, answer);
}

#[test]
fn answer_citing_out_of_range_line_falls_back() {
    let answer = "See gen_if_stale (src/guidance/src/sync_engine.rs:99999).";
    let constrained =
        guidance_core::read::claims::constrain_answer_to_evidence(answer, &evidence_snippets());
    assert!(
        !constrained.contains("99999"),
        "unshown line must not survive: {constrained}"
    );
    assert!(
        constrained.contains("do not support a verified answer"),
        "fallback template: {constrained}"
    );
}

#[test]
fn answer_without_citations_passes_through() {
    let answer = "The excerpts do not answer the question.";
    let constrained =
        guidance_core::read::claims::constrain_answer_to_evidence(answer, &evidence_snippets());
    assert_eq!(constrained, answer);
}

#[test]
fn real_doc_tree_jumps_resolve_to_golden_owners() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..");
    for (doc, owner) in [
        ("doc/guidance/ARCHITECTURE.md", "guidance-core"),
        ("doc/guidance/CALIBRATION.md", "guidance-core"),
        ("doc/guidance/CAPABILITY_SUMMARIES.md", "guidance-core"),
    ] {
        let text = std::fs::read_to_string(root.join(doc))
            .unwrap_or_else(|_| panic!("repo doc readable: {doc}"));
        let jumps = guidance_core::jumps::derive_jumps_for_doc(doc, &text);
        assert!(!jumps.is_empty(), "{doc} chunks");
        assert!(
            jumps.iter().all(|jump| jump.crate_name == owner),
            "{doc} owner"
        );
    }
    let text =
        std::fs::read_to_string(root.join("doc/guidance/ARCHITECTURE.md")).expect("architecture");
    let jumps = guidance_core::jumps::derive_jumps_for_doc("doc/guidance/ARCHITECTURE.md", &text);
    let terms: Vec<&str> = jumps
        .iter()
        .flat_map(|jump| jump.terms.iter().map(String::as_str))
        .collect();
    assert!(terms.contains(&"sync"), "overview funnel mentions sync");
}
