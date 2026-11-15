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
