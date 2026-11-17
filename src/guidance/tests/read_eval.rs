//! Evaluation-corpus parsing and exclusion contracts.

use super::*;
use std::io::Write as _;

#[test]
fn parses_golden_files_alias_and_defaults() {
    let text = r#"{"id":"a","query":"where is A","golden_files":["src/a.rs"],"split":"tune"}
{"id":"b","query":"what is B","golden_facts":["B"],"control":true}
"#;
    let tasks = parse_eval_tasks(text).expect("parse");
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].relevant, vec!["src/a.rs".to_string()]);
    assert_eq!(tasks[0].intent, "general");
    assert_eq!(tasks[0].split, "tune");
    assert!(tasks[1].control);
    assert_eq!(tasks[1].split, "tune");
}

#[test]
fn duplicate_ids_reject_the_corpus() {
    let text = "{\"id\":\"same\",\"query\":\"a\"}\n{\"id\":\"same\",\"query\":\"b\"}\n";
    let error = parse_eval_tasks(text).expect_err("duplicate");
    assert!(error.contains("duplicate id"), "{error}");
}

#[test]
fn missing_query_and_id_reject() {
    assert!(parse_eval_tasks("{\"id\":\"a\"}\n").is_err());
    assert!(parse_eval_tasks("{\"query\":\"a\"}\n").is_err());
    assert!(parse_eval_tasks("").expect("empty").is_empty());
}

#[test]
fn load_reads_one_jsonl_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("questions.jsonl");
    let mut file = std::fs::File::create(&path).expect("create");
    writeln!(file, "{{\"id\":\"a\",\"query\":\"q\"}}").expect("write");
    let tasks = load_eval_tasks(&path).expect("load");
    assert_eq!(tasks.len(), 1);
}

#[test]
fn eval_paths_are_excluded() {
    assert!(is_eval_path("doc/guidance/eval/questions.jsonl"));
    assert!(is_eval_path("doc/guidance/eval/findings.jsonl"));
    assert!(is_eval_path("doc/guidance/eval/claims.jsonl"));
    assert!(is_eval_path("./doc/guidance/eval/README.md"));
    assert!(!is_eval_path("doc/guidance/overview.md"));
    assert!(!is_eval_path("src/guidance/src/eval.rs"));
}

#[test]
fn checked_in_corpus_is_well_formed() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../doc/guidance/eval/questions.jsonl");
    let tasks = load_eval_tasks(&path).expect("checked-in eval loads");
    assert!(tasks.len() >= 24, "{} cases", tasks.len());
    assert!(tasks.iter().any(|task| task.control));
    assert!(tasks.iter().any(|task| task.split == "heldout"));
    assert!(tasks.iter().any(|task| task.intent == "orientation"));
}

#[test]
fn citation_spans_parse_trailing_ranges() {
    assert_eq!(super::citation_span_lines("src/a.rs:1-536"), 535);
    assert_eq!(super::citation_span_lines("src/a.rs:5"), 0);
    assert_eq!(super::citation_span_lines("src/a.rs:1-2 (stale)"), 1);
    assert_eq!(super::citation_span_lines("src/a.rs"), 0);
    assert_eq!(super::citation_span_lines(""), 0);
}

#[test]
fn max_span_and_fixture_counts_pin_spam_contracts() {
    assert_eq!(
        super::max_citation_span(&["src/a.rs:1-10".to_string(), "src/b.rs:3".to_string()]),
        9
    );
    assert_eq!(super::max_citation_span(&[]), 0);
    assert_eq!(
        super::fixture_path_count(&[
            "src/a.rs".to_string(),
            "src/guidance/fixture_read/a.rs".to_string(),
            "doc/guidance/eval/questions.jsonl".to_string(),
        ]),
        2
    );
    assert_eq!(super::fixture_path_count(&["src/a.rs".to_string()]), 0);
    assert!(super::spam_ok(8, 0, 200));
    assert!(!super::spam_ok(9, 0, 10));
    assert!(!super::spam_ok(2, 1, 10));
    assert!(!super::spam_ok(2, 0, 201));
}

#[test]
fn rendered_citation_count_ignores_the_more_line() {
    let rendered = "### Citations\n- a.rs:1\n- b.rs:2\n- + 3 more\n### Gaps\n- x\n";
    assert_eq!(super::rendered_citation_count(rendered), 2);
    assert_eq!(super::rendered_citation_count("### Coverage\n"), 0);
}
