//! `benchmark` command goldens: orchestrator-cost comparison through
//! the built binary on a hermetic temp workspace. Narrow + control
//! tasks assert recall relationships; JSON output asserts the agent
//! handoff schema.

#[path = "common.rs"]
#[allow(dead_code)]
mod common;

use common::{run, stdout, sync};

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("a.rs"),
        "/// Alpha.\npub fn alpha_unique_fn(x: u64) -> u64 {\n    x + 1\n}\n",
    )
    .expect("write");
    std::fs::write(
        dir.path().join("b.rs"),
        "/// Beta.\npub fn beta_unique_fn(x: u64) -> u64 {\n    x + 2\n}\n",
    )
    .expect("write");
    std::fs::write(
        dir.path().join("c.rs"),
        "/// Unrelated.\npub fn lonely_fn() -> u64 {\n    99\n}\n",
    )
    .expect("write");
    dir
}

fn tasks_file() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tasks.jsonl");
    std::fs::write(
        &path,
        "{\"id\": \"T1\", \"query\": \"where is alpha_unique_fn defined\", \"relevant\": [\"a.rs\"]}\n\
         {\"id\": \"T2\", \"query\": \"zxqy florp wobble\", \"relevant\": []}\n",
    )
    .expect("write");
    let arg = path.to_str().unwrap().to_string();
    (dir, arg)
}

fn bench_args(root: &str, db: &str, tasks: &str, extra: &[&str]) -> Vec<String> {
    let mut args = vec![
        "benchmark".to_string(),
        "--workspace".to_string(),
        root.to_string(),
        "--db".to_string(),
        db.to_string(),
        "--tasks".to_string(),
        tasks.to_string(),
    ];
    args.extend(extra.iter().map(|flag| flag.to_string()));
    args
}

#[test]
fn benchmark_reports_per_arm_cost_and_recall() {
    let dir = fixture();
    sync(dir.path(), ".bench.db", false);
    let (_tasks_dir, tasks) = tasks_file();
    let root = dir.path().to_str().unwrap().to_string();
    let db = dir.path().join(".bench.db");
    let db = db.to_str().unwrap().to_string();
    let args = bench_args(&root, &db, &tasks, &[]);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run(dir.path(), &arg_refs);
    assert!(out.status.success(), "benchmark: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("## Benchmark: 2 queries"), "{text}");
    assert!(text.contains("Corpus (wholesale)"), "{text}");
    assert!(text.contains("explain"), "{text}");
    assert!(text.contains("wholesale"), "{text}");
    assert!(text.contains("Ratios"), "{text}");
}

#[test]
fn benchmark_json_handoff_carries_recall_per_arm() {
    let dir = fixture();
    sync(dir.path(), ".bench.db", false);
    let (_tasks_dir, tasks) = tasks_file();
    let root = dir.path().to_str().unwrap().to_string();
    let db = dir.path().join(".bench.db");
    let db = db.to_str().unwrap().to_string();
    let args = bench_args(&root, &db, &tasks, &["--format", "json"]);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run(dir.path(), &arg_refs);
    assert!(out.status.success(), "benchmark json: {out:?}");
    let report: serde_json::Value =
        serde_json::from_str(&stdout(&out)).expect("report parses as JSON");
    let queries = report
        .get("queries")
        .and_then(|q| q.as_array())
        .expect("queries");
    assert_eq!(queries.len(), 2);
    let narrow = &queries[0];
    assert_eq!(narrow.get("id").and_then(|id| id.as_str()).unwrap(), "T1-1");
    for arm in ["explain", "grep", "wholesale"] {
        let recall = narrow
            .get(arm)
            .and_then(|row| row.get("recall"))
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(-1.0);
        assert!(
            (recall - 1.0).abs() < 1e-9,
            "{arm} must cover the narrow task: {}",
            stdout(&out)
        );
        for key in ["wall_ms", "chars", "tokens", "files", "recall", "precision"] {
            assert!(
                narrow.get(arm).and_then(|row| row.get(key)).is_some(),
                "{arm}.{key} present: {}",
                stdout(&out)
            );
        }
    }
    // Control task stays silent on every arm: fully-absent tokens
    // match nothing, so silence is by construction. A real word here
    // (notably `token`) fires the grep arm on almost any corpus and
    // the control measures nothing.
    let control = &queries[1];
    for arm in ["explain", "grep", "wholesale"] {
        let files = control
            .get(arm)
            .and_then(|row| row.get("files"))
            .and_then(|files| files.as_array())
            .expect("control arm files");
        assert!(
            files.is_empty(),
            "control {arm} must not fire: {}",
            stdout(&out)
        );
        let recall = control
            .get(arm)
            .and_then(|row| row.get("recall"))
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(-1.0);
        assert!(
            (recall - 1.0).abs() < 1e-9,
            "control {arm}.recall is 1.0 when silent: {}",
            stdout(&out)
        );
    }
    // Precision prices the load: retrieval arms read 1.0 when silent,
    // wholesale reads 0.0 over a non-empty corpus (documented on the
    // task format).
    for arm in ["explain", "grep"] {
        let precision = control
            .get(arm)
            .and_then(|row| row.get("precision"))
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(-1.0);
        assert!(
            (precision - 1.0).abs() < 1e-9,
            "control {arm}.precision is 1.0 when silent: {}",
            stdout(&out)
        );
    }
    let ratios = report.get("ratios").expect("ratios");
    assert!(ratios.get("explain_tokens_per_wholesale").is_some());
    assert!(ratios.get("grep_tokens_per_wholesale").is_some());
}

#[test]
fn benchmark_synthesizes_tasks_from_the_index() {
    let dir = fixture();
    sync(dir.path(), ".bench.db", false);
    let root = dir.path().to_str().unwrap().to_string();
    let db = dir.path().join(".bench.db");
    let db = db.to_str().unwrap().to_string();
    let args = vec!["benchmark", "--workspace", &root, "--db", &db, "--num", "2"];
    let out = run(dir.path(), &args);
    assert!(out.status.success(), "synthesized: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("## Benchmark: 2 queries"), "{text}");
    assert!(text.contains("synth-1"), "{text}");
}
