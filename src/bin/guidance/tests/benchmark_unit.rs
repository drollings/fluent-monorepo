//! Unit tests for the `benchmark` command (pure layer: token
//! estimates, task parsing, identifier extraction, arm scoring, grep
//! budget). Included via `#[path]` forwarder from `src/benchmark.rs`,
//! per repo convention.

use super::*;
use std::collections::BTreeSet;

#[test]
fn tokens_round_up_quarters() {
    assert_eq!(estimate_tokens(0), 0);
    assert_eq!(estimate_tokens(1), 1);
    assert_eq!(estimate_tokens(4), 1);
    assert_eq!(estimate_tokens(5), 2);
    assert_eq!(estimate_tokens(400), 100);
}

#[test]
fn identifier_tokens_keep_significant_words_only() {
    assert_eq!(
        identifier_tokens("where is alpha_fn defined"),
        vec![
            "where".to_string(),
            "alpha_fn".to_string(),
            "defined".to_string()
        ]
    );
    // Short noise never becomes a pattern; duplicates fold.
    assert_eq!(
        identifier_tokens("is it ok to to use x"),
        vec!["use".to_string()]
    );
    assert!(identifier_tokens("a b c 12").is_empty());
}

#[test]
fn scoring_matches_by_basename() {
    let relevant: BTreeSet<String> = ["a.rs".to_string()].into_iter().collect();
    assert!(matches(&relevant, "src/a.rs"));
    assert!(matches(&relevant, "a.rs"));
    assert!(!matches(&relevant, "src/b.rs"));
}

#[test]
fn score_arm_reports_recall_and_precision() {
    let relevant: BTreeSet<String> = ["a.rs".to_string(), "b.rs".to_string()]
        .into_iter()
        .collect();
    let (recall, precision) = score_arm(&["a.rs".to_string()], &relevant);
    assert!((recall - 0.5).abs() < 1e-9);
    assert!((precision - 1.0).abs() < 1e-9);
    let (recall, precision) = score_arm(&[], &relevant);
    assert_eq!((recall, precision), (0.0, 1.0));
}

#[test]
fn score_arm_controls_pass_on_silence_only() {
    let empty: BTreeSet<String> = BTreeSet::new();
    assert_eq!(score_arm(&[], &empty), (1.0, 1.0));
    assert_eq!(score_arm(&["a.rs".to_string()], &empty), (0.0, 0.0));
}

#[test]
fn tasks_file_parses_golden_shape_with_extras_ignored() {
    // Compatible with bin/guidance-bench goldens (extra keys ignored,
    // `relevant` folded to basenames).
    let text = "{\"corpus\": \"fix\", \"id\": \"EF2\", \"kind\": \"narrow\", \
        \"query\": \"where is fee defined\", \"relevant\": [\"src/mod_3.rs\"]}\n\
        \n\
        {\"id\": \"EC1\", \"query\": \"zxqy nothing\", \"relevant\": []}\n";
    let tasks = parse_tasks_file(text).expect("parses");
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].query, "where is fee defined");
    assert_eq!(tasks[0].relevant, vec!["mod_3.rs".to_string()]);
    assert!(tasks[1].relevant.is_empty());
    assert!(tasks[0].id.starts_with("EF2-"));
}

#[test]
fn orientation_tasks_file_parses_with_golden_relevant_sets() {
    // The checked-in orientation battery: six maintainer questions with
    // golden relevant-file sets. Guards the M9 ratchet input — a malformed
    // task file must fail here, not mid-benchmark.
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../guidance/corpora/benchmark_tasks.jsonl"
    ))
    .expect("orientation tasks read");
    let tasks = parse_tasks_file(&text).expect("orientation tasks parse");
    assert_eq!(tasks.len(), 6, "six orientation questions");
    for task in &tasks {
        assert!(!task.query.is_empty(), "{:?}", task.id);
        assert!(!task.relevant.is_empty(), "{:?} carries goldens", task.id);
    }
}

#[test]
fn tasks_file_rejects_missing_query() {
    assert!(parse_tasks_file("{\"id\": \"x\"}\n").is_err());
    assert!(parse_tasks_file("").expect("empty").is_empty());
}

#[test]
fn grep_arm_returns_empty_without_tokens() {
    let (text, files) = grep_arm(&[], "/repo", &[], GREP_FILE_BUDGET);
    assert!(text.is_empty());
    assert!(files.is_empty());
}

#[test]
fn grep_arm_caps_files_at_budget() {
    let dir = tempfile::tempdir().expect("tempdir");
    for name in ["a.rs", "b.rs", "c.rs"] {
        std::fs::write(dir.path().join(name), "needle here\n").expect("write");
    }
    let corpus = walk_corpus(&[dir.path().to_path_buf()]);
    assert_eq!(corpus.len(), 3);
    let (_, files) = grep_arm(
        &corpus,
        dir.path().to_str().unwrap(),
        &["needle".to_string()],
        2,
    );
    assert_eq!(files.len(), 2, "budget caps surfaced files: {files:?}");
}

#[test]
fn synthesize_tasks_reads_index_symbols() {
    let db = search_vector::GuidanceDb::open_in_memory().expect("db");
    assert!(synthesize_tasks(&db, 5).is_empty(), "empty index, no tasks");
}
