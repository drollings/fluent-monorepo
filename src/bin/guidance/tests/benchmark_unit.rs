//! Unit tests for the `benchmark` command (token accounting, task
//! parsing, arm scoring, targeted grep budget, corpus exclusion,
//! shipping explain arm). Included via `#[path]` forwarder from
//! `src/benchmark.rs`, per repo convention.

use super::*;
use guidance_core::eval as eval_contract;
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
fn spam_helpers_reuse_the_eval_contracts() {
    // Single home: `guidance_core::eval`. The benchmark module's former
    // re-exports were deleted as dead duplicates; the contract is pinned
    // here against the live home.
    use guidance_core::eval as eval_contract;
    assert_eq!(eval_contract::SPAM_MAX_CITATIONS, 8);
    assert_eq!(eval_contract::SPAM_MAX_SPAN_LINES, 200);
    assert_eq!(eval_contract::citation_span_lines("src/a.rs:1-536"), 535);
    assert_eq!(
        eval_contract::max_citation_span(&["src/a.rs:1-10".to_string()]),
        9
    );
    assert_eq!(
        eval_contract::fixture_path_count(&[
            "src/a.rs".to_string(),
            "src/guidance/fixture_read/a.rs".to_string(),
        ]),
        1
    );
    assert!(eval_contract::spam_ok(8, 0, 200));
    assert!(!eval_contract::spam_ok(9, 0, 10));
}

#[test]
fn doc_walk_covers_readme_and_docs_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    for rel in [
        "README.md",
        "doc/guide.md",
        "doc/nested/deep.md",
        "src/code.rs",
        "doc/guidance/eval/zz.md",
    ] {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "walkterm here\n").expect("write");
    }
    let (corpus, _) = walk_corpus(&[dir.path().to_path_buf()]);
    let docs = doc_walk_corpus(&corpus);
    let names: Vec<String> = docs
        .iter()
        .map(|path| {
            path.strip_prefix(dir.path())
                .expect("relative")
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    assert!(names.contains(&"README.md".to_string()), "{names:?}");
    assert!(names.contains(&"doc/guide.md".to_string()), "{names:?}");
    assert!(
        names.contains(&"doc/nested/deep.md".to_string()),
        "{names:?}"
    );
    assert!(
        !names.iter().any(|name| name.starts_with("src/")),
        "{names:?}"
    );
    assert!(
        !names.iter().any(|name| name.contains("eval")),
        "measurement never enters: {names:?}"
    );
    let (text, files, _) = doc_walk_arm_with_paths(
        &corpus,
        dir.path().to_str().unwrap(),
        &["walkterm".to_string()],
        GREP_FILE_BUDGET,
    );
    assert!(text.contains("README.md"), "{text}");
    assert!(files.contains(&"README.md".to_string()), "{files:?}");
    assert!(!files.contains(&"code.rs".to_string()), "{files:?}");
}

#[test]
fn benchmark_reports_spam_doc_walk_and_control_floor() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("README.md"),
        "# Fixture\n\nwalkterm alpha\n",
    )
    .expect("write");
    std::fs::create_dir_all(dir.path().join("doc")).expect("mkdir");
    std::fs::write(dir.path().join("doc/guide.md"), "walkterm beta\n").expect("write");
    std::fs::write(dir.path().join("code.rs"), "fn walkterm() {}\n").expect("write");
    let db_path = dir.path().join("bench.db");
    let db = guidance_core::query::search_db::GuidanceSearchDb::open(&db_path).expect("db");
    db.upsert_fragments(
        &guidance_core::query::search_db::FileRecord {
            id: "code.rs".to_string(),
            absolute_path: dir.path().join("code.rs").to_string_lossy().into_owned(),
            relative_path: "code.rs".to_string(),
            root_path: dir.path().to_string_lossy().into_owned(),
            size_bytes: 18,
            last_modified_time: 100,
            kind: Some("code".to_string()),
            format: "rust".to_string(),
            content_hash: None,
            index_status: Some("indexed".to_string()),
            fail_count: 0,
            last_error: None,
        },
        &[guidance_core::query::search_db::FragmentRecord {
            id: "code-walkterm".to_string(),
            group: None,
            file_id: "code.rs".to_string(),
            range_json: "{\"kind\":\"text\",\"start_line\":1,\"end_line\":1,\"start_offset\":0,\"end_offset\":14}"
                .to_string(),
            content_kind: "text".to_string(),
            content_text: Some("fn walkterm() {}".to_string()),
            cjk_text: String::new(),
            symbol_type: Some("function".to_string()),
            symbol_name: Some("walkterm".to_string()),
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
    let tasks = vec![
        BenchTask {
            id: "t-walk-1".to_string(),
            query: "where is walkterm defined".to_string(),
            relevant: vec!["code.rs".to_string()],
            relevant_paths: vec!["code.rs".to_string()],
            required_sections: vec!["Top matches".to_string()],
            split: "tune".to_string(),
            control: false,
        },
        BenchTask {
            id: "c-none-2".to_string(),
            query: "what is the zzqy nonexistent subsystem".to_string(),
            relevant: Vec::new(),
            relevant_paths: Vec::new(),
            required_sections: Vec::new(),
            split: "heldout".to_string(),
            control: true,
        },
    ];
    let report = run_benchmark(
        dir.path().to_str().unwrap(),
        db_path.to_str().unwrap(),
        &tasks,
        5,
    );
    assert_eq!(report.queries.len(), 2);
    let tune = &report.queries[0];
    let citations = tune.explain.order.len();
    let fixtures = eval_contract::fixture_path_count(&tune.explain.order);
    let span = eval_contract::max_citation_span(
        &tune
            .explain
            .order
            .iter()
            .map(|base| format!("{base}:1"))
            .collect::<Vec<_>>(),
    );
    assert!(
        eval_contract::spam_ok(citations, fixtures, span),
        "{tune:?}"
    );
    assert_eq!(fixtures, 0, "zero fixture paths: {tune:?}");
    assert!(span <= eval_contract::SPAM_MAX_SPAN_LINES, "{span}");
    assert!(
        tune.explain.order.len() <= eval_contract::SPAM_MAX_CITATIONS + 3,
        "citation budget holds: {:?}",
        tune.explain.order
    );
    assert!(
        !tune.doc_walk.files.is_empty(),
        "doc-walk finds the manual baseline: {:?}",
        tune.doc_walk.files
    );
    assert!(
        tune.doc_walk.files.contains(&"README.md".to_string())
            || tune.doc_walk.files.contains(&"guide.md".to_string()),
        "{:?}",
        tune.doc_walk.files
    );
    assert!(
        !tune.doc_walk.files.contains(&"code.rs".to_string()),
        "doc-walk never surfaces code: {:?}",
        tune.doc_walk.files
    );
    let control = &report.queries[1];
    assert!(
        control.explain.control_abstained,
        "control abstains on the explain arm: {:?}",
        control.explain
    );
    assert!(
        control.doc_walk.control_abstained,
        "control stays silent on the doc-walk arm: {:?}",
        control.doc_walk
    );
    assert!(
        report.totals.contains_key("doc_walk"),
        "totals cover doc-walk"
    );
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
    // golden relevant-file sets plus the three cold-start controls.
    // Guards the M9 ratchet input — a malformed task file must fail
    // here, not mid-benchmark.
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../guidance/corpora/benchmark_tasks.jsonl"
    ))
    .expect("orientation tasks read");
    let tasks = parse_tasks_file(&text).expect("orientation tasks parse");
    assert_eq!(tasks.len(), 9, "six tune questions plus three controls");
    let tune: Vec<&BenchTask> = tasks.iter().filter(|task| !task.control).collect();
    assert_eq!(tune.len(), 6);
    for task in &tune {
        assert!(!task.query.is_empty(), "{:?}", task.id);
        assert!(!task.relevant.is_empty(), "{:?} carries goldens", task.id);
        assert!(
            !task.required_sections.is_empty(),
            "{:?} carries sections",
            task.id
        );
        assert_eq!(task.split, "tune", "{:?}", task.id);
    }
    let controls: Vec<&BenchTask> = tasks.iter().filter(|task| task.control).collect();
    assert_eq!(controls.len(), 3);
    for task in &controls {
        assert!(task.relevant.is_empty(), "{:?} stays silent", task.id);
        assert_eq!(task.split, "heldout", "{:?}", task.id);
    }
}

#[test]
fn checked_in_battery_carries_coldstart_goldens() {
    // The battery mirrors `doc/guidance/eval/coldstart.jsonl`: same
    // tune queries with golden sections, same heldout controls.
    let battery = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../guidance/corpora/benchmark_tasks.jsonl"
    ))
    .expect("battery reads");
    let tasks = parse_tasks_file(&battery).expect("battery parses");
    let coldstart = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../doc/guidance/eval/coldstart.jsonl"
    ))
    .expect("coldstart reads");
    let mut golden_sections = std::collections::BTreeMap::new();
    let mut golden_splits = std::collections::BTreeMap::new();
    for line in coldstart.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line).expect("coldstart parses");
        let query = value["query"].as_str().expect("query").to_string();
        let sections: Vec<String> = value["required_sections"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|entry| entry.as_str().map(str::to_string))
            .collect();
        golden_sections.insert(query.clone(), sections);
        golden_splits.insert(query, value["split"].as_str().unwrap_or("tune").to_string());
    }
    for task in &tasks {
        let expected = golden_sections.get(&task.query).unwrap_or_else(|| {
            panic!(
                "battery query missing from coldstart goldens: {:?}",
                task.id
            )
        });
        assert_eq!(
            &task.required_sections, expected,
            "{:?} carries the golden sections",
            task.id
        );
        assert_eq!(
            &task.split,
            golden_splits.get(&task.query).expect("split"),
            "{:?} carries the golden split",
            task.id
        );
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
    let db = guidance_core::query::search_db::GuidanceSearchDb::open(&db_path).expect("db");
    db.upsert_fragments(
        &guidance_core::query::search_db::FileRecord {
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
        &[guidance_core::query::search_db::FragmentRecord {
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

#[test]
fn shipping_arm_matches_cli_compact_body_and_envelope() {
    // Parity: the benchmark arm assembles through the same
    // `ReadEngine` request the CLI builds, renders through the one
    // CLI body renderer, and carries the envelope breakdown beside
    // the body so token ratios stay honest.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("beam.rs"), "fn zzbeam() {}\n").expect("write");
    let db_path = dir.path().join("parity.db");
    let db = guidance_core::query::search_db::GuidanceSearchDb::open(&db_path).expect("db");
    db.upsert_fragments(
        &guidance_core::query::search_db::FileRecord {
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
        &[guidance_core::query::search_db::FragmentRecord {
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
    let ws = dir.path().to_str().unwrap();
    let db_str = db_path.to_str().unwrap();
    let (body, files, _, _, _) = explain_arm_detailed("where is zzbeam defined", ws, db_str, 5);
    let (body2, compact_body, envelope, files2, _, _, _) =
        explain_arm_with_envelope("where is zzbeam defined", ws, db_str, 5);
    assert_eq!(body, body2, "detailed delegates to the envelope arm");
    assert_eq!(files, files2);
    assert!(body.contains("## Explain:"), "{body}");
    assert!(
        !compact_body.contains("### Coverage"),
        "compact omits Coverage like --compact: {compact_body}"
    );
    assert!(
        body.contains("### Coverage"),
        "full body keeps Coverage: {body}"
    );
    assert_eq!(envelope.get("schema_version"), Some(&serde_json::json!(2)));
    let body_tokens = tokens_for_text(&body);
    let compact_tokens = tokens_for_text(&compact_body);
    let envelope_text = serde_json::to_string(&envelope).unwrap_or_default();
    let envelope_tokens = tokens_for_text(&envelope_text);
    assert!(
        body_tokens >= compact_tokens,
        "{body_tokens} vs {compact_tokens}"
    );
    assert!(envelope_tokens > 0, "envelope breakdown counted");
    assert!(
        envelope
            .get("citations")
            .and_then(|value| value.as_array())
            .is_some_and(|cites| !cites.is_empty()),
        "{envelope}"
    );
}
