//! M4 compat: explain not-found + `--help` stability through the built
//! binary. The port changes table content/scores/order (declared) but
//! never the shape, the empty state, or the flags.

use std::path::{Path, PathBuf};
use std::process::Command;

fn guidance_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_guidance"))
}

fn run(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(guidance_bin())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn guidance")
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.rs"), "pub fn alpha() {}\n").expect("write");
    dir
}

fn sync(dir: &Path) {
    let root = dir.to_str().unwrap().to_string();
    let json_dir = dir.join(".guidance");
    let json_dir = json_dir.to_str().unwrap().to_string();
    let db = dir.join(".explain.db");
    let db = db.to_str().unwrap().to_string();
    let out = run(
        dir,
        &["index", &root, "--workspace", &root, "--json-dir", &json_dir],
    );
    assert!(out.status.success(), "index: {out:?}");
    let out = run(
        dir,
        &[
            "sync",
            "--workspace",
            &root,
            "--json-dir",
            &json_dir,
            "--db",
            &db,
        ],
    );
    assert!(out.status.success(), "sync: {out:?}");
}

#[test]
fn explain_help_lists_stable_flags() {
    let dir = workspace();
    let out = run(dir.path(), &["explain", "--help"]);
    assert!(out.status.success(), "explain --help: {out:?}");
    let text = stdout(&out);
    for flag in ["--guidance", "--db", "--workspace", "--limit", "--no-llm", "--filter"] {
        assert!(text.contains(flag), "missing {flag}:\n{text}");
    }
}

#[test]
fn explain_not_found_state_is_stable() {
    let dir = workspace();
    sync(dir.path());
    let root = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_dir = json_dir.to_str().unwrap().to_string();
    let db = dir.path().join(".explain.db");
    let db = db.to_str().unwrap().to_string();
    // Out-of-corpus query: empty on the ported path (no fallback
    // reintroduces hits, no closure invents them).
    let out = run(
        dir.path(),
        &[
            "explain",
            "zxqy nonexistent token zzz",
            "--workspace",
            &root,
            "--guidance",
            &json_dir,
            "--db",
            &db,
        ],
    );
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(text.starts_with("## Explain: "), "{text}");
    assert!(text.contains("No results found."), "{text}");
    assert!(!text.contains('|'), "no table rows for empty results: {text}");
}

#[test]
fn explain_missing_db_is_empty_not_error() {
    let dir = workspace();
    let root = dir.path().to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &[
            "explain",
            "alpha",
            "--workspace",
            &root,
            "--db",
            dir.path().join(".missing.db").to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "explain without db: {out:?}");
    assert!(stdout(&out).contains("No results found."));
}
