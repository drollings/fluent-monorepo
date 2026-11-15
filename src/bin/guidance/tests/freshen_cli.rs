//! Read-path freshness: `--freshen` converges the stale files behind
//! an answer over bounded re-runs. No fix waves, no model calls.
#[path = "common.rs"]
mod common;

use common::{run, stdout, sync, workspace};

// Wall budget for twenty dirty files (gen + ingest + re-runs),
// measured 118ms; 2s holds CI headroom yet catches pathologies.
const FRESHEN_20_BUDGET_MS: u64 = 2_000;

fn explain_args(query: &str, ws: &str, json_dir: &str, db: &str, extra: &[&str]) -> Vec<String> {
    let mut args = vec![
        "explain".to_string(),
        query.to_string(),
        "--workspace".to_string(),
        ws.to_string(),
        "--guidance".to_string(),
        json_dir.to_string(),
        "--db".to_string(),
        db.to_string(),
        "--no-llm".to_string(),
        "--allow-degraded".to_string(),
    ];
    args.extend(extra.iter().map(|flag| flag.to_string()));
    args
}

/// Fixture with two anchored functions; the caller dirties files
/// without re-syncing to stage staleness.
fn stale_fixture() -> (tempfile::TempDir, String, String, String) {
    let dir = workspace();
    std::fs::write(
        dir.path().join("lib.rs"),
        "/// Adds one.\npub fn alpha() -> u64 {\n    1\n}\n",
    )
    .expect("write");
    std::fs::write(
        dir.path().join("beta.rs"),
        "/// Adds two.\npub fn beta() -> u64 {\n    2\n}\n",
    )
    .expect("write");
    sync(dir.path(), ".freshen.db", false);
    let root = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance").to_str().unwrap().to_string();
    let db = dir.path().join(".freshen.db").to_str().unwrap().to_string();
    (dir, root, json_dir, db)
}

fn explain(dir: &std::path::Path, args: &[String]) -> std::process::Output {
    run(dir, &args.iter().map(String::as_str).collect::<Vec<_>>())
}

#[test]
fn freshen_converges_cited_stale_files() {
    let (dir, root, json_dir, db) = stale_fixture();
    // Prepend without re-syncing: the index goes stale behind live bytes.
    std::fs::write(
        dir.path().join("lib.rs"),
        "// header\n// more\n/// Adds one.\npub fn alpha() -> u64 {\n    1\n}\n",
    )
    .expect("edit without re-sync");
    let stale_out = explain(
        dir.path(),
        &explain_args("adds", &root, &json_dir, &db, &[]),
    );
    assert!(stale_out.status.success(), "stale explain: {stale_out:?}");
    assert!(
        stdout(&stale_out).contains("(stale)"),
        "precondition: citation is stale without --freshen"
    );
    let fresh_out = explain(
        dir.path(),
        &explain_args("adds", &root, &json_dir, &db, &["--freshen"]),
    );
    assert!(fresh_out.status.success(), "freshen explain: {fresh_out:?}");
    let text = stdout(&fresh_out);
    assert!(
        !text.contains("(stale)"),
        "freshened citations verify: {text}"
    );
    assert!(text.contains("lib.rs:4-6"), "{text}");
    let lib = dir.path().join("lib.rs").to_str().unwrap().to_string();
    let verified = run(dir.path(), &["verify", &lib]);
    assert!(
        verified.status.success(),
        "cited file verifies fresh: {verified:?}"
    );
}

#[test]
fn freshen_never_rewrites_source() {
    // Convergence updates sidecars and the index, never source bytes:
    // no fix wave runs on the read path.
    let (dir, root, json_dir, db) = stale_fixture();
    std::fs::write(
        dir.path().join("lib.rs"),
        "// header\n/// Adds one.\npub fn alpha() -> u64 {\n    1\n}\n",
    )
    .expect("edit without re-sync");
    let before = std::fs::read(dir.path().join("lib.rs")).expect("read");
    let out = explain(
        dir.path(),
        &explain_args("adds", &root, &json_dir, &db, &["--freshen"]),
    );
    assert!(out.status.success(), "freshen explain: {out:?}");
    let after = std::fs::read(dir.path().join("lib.rs")).expect("read");
    assert_eq!(before, after, "source bytes untouched by --freshen");
    assert!(!stdout(&out).contains("(stale)"), "{}", stdout(&out));
}

#[test]
fn freshen_failure_serves_stale_labelled() {
    // A deleted file cannot converge: the read serves the stale-labelled
    // answer with a named gap — never an error, never a fix attempt.
    let (dir, root, json_dir, db) = stale_fixture();
    std::fs::remove_file(dir.path().join("beta.rs")).expect("delete");
    let out = explain(
        dir.path(),
        &explain_args("beta", &root, &json_dir, &db, &["--freshen"]),
    );
    assert!(out.status.success(), "failure still serves: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("(stale)"), "stale label survives: {text}");
    assert!(text.contains("freshen"), "failure is named: {text}");
}

#[test]
fn freshen_twenty_dirty_files_within_budget() {
    // Twenty stale files converge on one read: gen plus targeted
    // ingest plus one re-run stays inside the measured budget below.
    let dir = tempfile::tempdir().expect("tempdir");
    for index in 0..20 {
        std::fs::write(
            dir.path().join(format!("file{index:02}.rs")),
            format!("/// Does {index}.\npub fn work{index:02}() -> u64 {{\n    {index}\n}}\n"),
        )
        .expect("write");
    }
    sync(dir.path(), ".freshen20.db", false);
    for index in 0..20 {
        std::fs::write(
            dir.path().join(format!("file{index:02}.rs")),
            format!("// touched\n/// Does {index}.\npub fn work{index:02}() -> u64 {{\n    {index}\n}}\n"),
        )
        .expect("dirty without re-sync");
    }
    let root = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance").to_str().unwrap().to_string();
    let db = dir
        .path()
        .join(".freshen20.db")
        .to_str()
        .unwrap()
        .to_string();
    let start = std::time::Instant::now();
    let out = explain(
        dir.path(),
        &explain_args("work", &root, &json_dir, &db, &["--freshen"]),
    );
    let elapsed_ms = start.elapsed().as_millis() as u64;
    eprintln!("FRESHEN_20 elapsed_ms={elapsed_ms}");
    assert!(out.status.success(), "freshen explain: {out:?}");
    let text = stdout(&out);
    assert!(
        !text.contains("(stale)"),
        "all cited files converged: {text}"
    );
    assert!(
        elapsed_ms < FRESHEN_20_BUDGET_MS,
        "twenty dirty files in {elapsed_ms}ms over {FRESHEN_20_BUDGET_MS}ms"
    );
}
