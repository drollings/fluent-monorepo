//! Unit tests for the `benchmark` command (token accounting, task
//! parsing, arm scoring, targeted grep budget, corpus exclusion,
//! shipping explain arm). Included via `#[path]` forwarder from
//! `src/benchmark.rs`, per repo convention.

use super::*;
use std::collections::BTreeSet;

#[test]
fn tokens_use_the_shared_tokenizer() {
    // One accounting path: the benchmark delegates to the shared
    // `fluent_llm::tokens` estimator, never a local chars/4 fork.
    assert_eq!(tokens_for_text(""), 0);
    assert!(tokens_for_text("hello world") > 0);
    assert_eq!(
        tokens_for_text("hello world"),
        fluent_llm::tokens::estimate_tokens("hello world") as usize
    );
    let long = "word ".repeat(100);
    assert!(tokens_for_text(&long) > tokens_for_text("word"));
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
fn eval_corpus_parses_without_leaking_paths() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../doc/guidance/eval/questions.jsonl"
    ))
    .expect("eval corpus read");
    let tasks = parse_tasks_file(&text).expect("eval corpus parse");
    assert!(tasks.len() >= 24);
    assert!(tasks.iter().any(|task| task.control));
    assert!(tasks.iter().any(|task| task.split == "heldout"));
    for task in &tasks {
        assert!(!task.query.is_empty(), "{:?}", task.id);
        assert!(
            task.relevant
                .iter()
                .all(|path| !path.contains("guidance/eval")),
            "{:?} leaked an eval path",
            task.id
        );
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
    let (corpus, _) = walk_corpus(&[dir.path().to_path_buf()]);
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
fn grep_arm_matches_targeted_vocabulary_only() {
    // Targeting lives in the vocabulary: scaffolding-only files never
    // surface because their words never become tokens. The match
    // itself is broad-net over the given terms.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("full.rs"), "alpha beta gamma here\n").expect("write");
    std::fs::write(dir.path().join("scaffold.rs"), "where is the thing\n").expect("write");
    let (corpus, _) = walk_corpus(&[dir.path().to_path_buf()]);
    let (_, files) = grep_arm(
        &corpus,
        dir.path().to_str().unwrap(),
        &["alpha".to_string(), "beta".to_string()],
        GREP_FILE_BUDGET,
    );
    assert_eq!(files, vec!["full.rs".to_string()], "{files:?}");
    // No tokens (pure scaffolding queries produce none): no matches.
    let (text, files) = grep_arm(&corpus, dir.path().to_str().unwrap(), &[], GREP_FILE_BUDGET);
    assert!(text.is_empty() && files.is_empty());
    // The vocabulary is the targeting: scaffolding never becomes a
    // token, so a scaffolding-only file can never match.
    assert!(guidance_core::read::vocabulary::content_terms("the a an").is_empty());
    assert_eq!(
        guidance_core::read::vocabulary::content_terms("where is alpha_unique_fn defined"),
        vec![
            "alpha".to_string(),
            "unique".to_string(),
            "defined".to_string()
        ]
    );
}

#[test]
fn walk_corpus_excludes_measurement_fixtures() {
    let dir = tempfile::tempdir().expect("tempdir");
    for rel in [
        "src/beam.rs",
        "doc/guidance/eval/zzbeam.md",
        "src/guidance/corpora/zzbeam.jsonl",
    ] {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "zzbeam calibrator\n").expect("write");
    }
    let (corpus, tokens) = walk_corpus(&[dir.path().to_path_buf()]);
    let names: Vec<String> = corpus
        .iter()
        .map(|path| {
            path.strip_prefix(dir.path())
                .expect("relative")
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    assert_eq!(names, vec!["src/beam.rs".to_string()], "{names:?}");
    assert!(tokens > 0, "token totals ride the same walk");
}

#[test]
fn explain_arm_runs_the_shipping_engine() {
    // The benchmark's indexed arm is the shipping path: `ReadEngine`
    // over a committed index, evidence-only. Missing indexes degrade
    // to empty, never a hard error.
    let (text, files, order) =
        explain_arm("where is nothing", "/nonexistent", "/nonexistent.db", 5);
    assert!(text.is_empty() && files.is_empty() && order.is_empty());
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("beam.rs"), "fn zzbeam() {}\n").expect("write");
    let db_path = dir.path().join("bench.db");
    let db = search_vector::GuidanceDb::open(&db_path).expect("db");
    db.upsert_fragments(
        &search_vector::db::FileRecord {
            id: "beam".to_string(),
            absolute_path: dir.path().join("beam.rs").to_string_lossy().into_owned(),
            relative_path: "beam.rs".to_string(),
            root_path: dir.path().to_string_lossy().into_owned(),
            size_bytes: 16,
            last_modified_time: 100,
            kind: Some("code".to_string()),
            format: "rust".to_string(),
            content_hash: None,
            index_status: Some("indexed".to_string()),
            fail_count: 0,
            last_error: None,
        },
        &[search_vector::db::FragmentRecord {
            id: "beam-fn".to_string(),
            group: None,
            file_id: "beam".to_string(),
            range_json: "{\"kind\":\"text\",\"start_line\":1,\"end_line\":1,\"start_offset\":0,\"end_offset\":14}"
                .to_string(),
            content_kind: "text".to_string(),
            content_text: Some("fn zzbeam() {}".to_string()),
            cjk_text: String::new(),
            symbol_type: Some("function".to_string()),
            symbol_name: Some("zzbeam".to_string()),
            scope: None,
            signature: None,
            doc: None,
            modifiers: String::new(),
            heading: None,
            heading_level: None,
            embedding: None,
        }],
        &[],
    )
    .expect("seed");
    drop(db);
    let (text, files, order) = explain_arm(
        "where is zzbeam defined",
        dir.path().to_str().unwrap(),
        db_path.to_str().unwrap(),
        5,
    );
    assert!(text.contains("## Explain:"), "{text}");
    assert!(text.contains("### Top matches"), "{text}");
    assert!(files.contains(&"beam.rs".to_string()), "{files:?}");
    assert_eq!(
        files
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        files.len()
    );
    assert!(!order.is_empty() && order[0] == "beam.rs", "{order:?}");
}

#[test]
fn path_scoring_is_exact_not_basename_folded() {
    let relevant: BTreeSet<String> = ["src/guidance/src/sync_engine.rs".to_string()]
        .into_iter()
        .collect();
    assert_eq!(
        score_paths(&["src/guidance/src/sync_engine.rs".to_string()], &relevant),
        (1.0, 1.0)
    );
    assert_eq!(
        score_paths(&["guidance/src/sync_engine.rs".to_string()], &relevant),
        (0.0, 0.0),
        "a basename match is not a path match"
    );
    assert_eq!(score_paths(&[], &relevant), (0.0, 1.0));
}

#[test]
fn path_scoring_controls_stay_silent_only() {
    let empty: BTreeSet<String> = BTreeSet::new();
    assert_eq!(score_paths(&[], &empty), (1.0, 1.0));
    assert_eq!(score_paths(&["src/a.rs".to_string()], &empty), (0.0, 0.0));
}

#[test]
fn required_sections_and_citation_counts_are_structural() {
    let text = "### Coverage\n### Top matches\n### Gaps\n";
    assert_eq!(
        required_sections_hit(
            text,
            &[
                "Coverage".to_string(),
                "Top matches".to_string(),
                "Answer".to_string()
            ]
        ),
        2
    );
    let citations = vec![
        "src/a.rs:1-2".to_string(),
        "src/b.rs:3-4 (stale)".to_string(),
        "src/c.rs:5 (unverified)".to_string(),
    ];
    assert_eq!(citations_verified_count(&citations), 1);
    assert!(control_abstained("### Coverage\n", &[]));
    assert!(!control_abstained("## Code\n", &["src/a.rs:1".to_string()]));
}

#[test]
fn scope_mix_separates_fixture_eval_and_product_paths() {
    let mix = scope_mix(&[
        "src/guidance/src/sync_engine.rs".to_string(),
        "src/guidance/fixture_read/README.md".to_string(),
        "doc/guidance/eval/questions.jsonl".to_string(),
        "src/guidance/tests/read_engine.rs".to_string(),
        "doc/guidance/ARCHITECTURE.md".to_string(),
    ]);
    assert_eq!(mix.get("product"), Some(&1), "{mix:?}");
    assert_eq!(mix.get("fixture"), Some(&1), "{mix:?}");
    assert_eq!(mix.get("eval"), Some(&1), "{mix:?}");
    assert_eq!(mix.get("test"), Some(&1), "{mix:?}");
    assert_eq!(mix.get("doc"), Some(&1), "{mix:?}");
}

#[test]
fn parsed_tasks_keep_exact_golden_paths_and_sections() {
    let text = "{\"id\":\"q\",\"query\":\"where\",\"relevant\":[\"src/guidance/src/a.rs\"],\
        \"required_sections\":[\"Top matches\"],\"control\":false}\n";
    let tasks = parse_tasks_file(text).expect("parse");
    assert_eq!(tasks[0].relevant, vec!["a.rs".to_string()]);
    assert_eq!(
        tasks[0].relevant_paths,
        vec!["src/guidance/src/a.rs".to_string()]
    );
    assert_eq!(tasks[0].required_sections, vec!["Top matches".to_string()]);
    assert!(!tasks[0].control);
}
