//! `run` command goldens: the maintain loop as a shipped command.
//! Written before the command (red first: unknown subcommand).

#[path = "common.rs"]
#[allow(dead_code)]
mod common;

use common::{run, stdout, workspace};

fn run_args(root: &str, json_dir: &str, db: &str, extra: &[&str]) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "--workspace".to_string(),
        root.to_string(),
        "--json-dir".to_string(),
        json_dir.to_string(),
        "--db".to_string(),
        db.to_string(),
    ];
    args.extend(extra.iter().map(|s| s.to_string()));
    args
}

#[test]
fn run_converges_formatting_then_reports_clean() {
    let dir = workspace();
    let root = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_dir_str = json_dir.to_str().unwrap().to_string();
    let db = dir.path().join(".guidance.db");
    let db_str = db.to_str().unwrap().to_string();
    // rustfmt-dirty private item, no Cargo package: clippy is skipped by
    // construction, D1 formats through the real rustfmt binary. Private
    // on purpose: reformatting a `pub` signature needs approval (P2).
    std::fs::write(dir.path().join("messy.rs"), "fn messy(x:u32)->u32{x+1}\n").expect("write");
    let args = run_args(&root, &json_dir_str, &db_str, &[]);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run(dir.path(), &arg_refs);
    let text = stdout(&out);
    assert!(out.status.success(), "run converges: {out:?}\n{text}");
    assert!(text.contains("fixed"), "{text}");
    let formatted = std::fs::read_to_string(dir.path().join("messy.rs")).expect("read");
    assert!(formatted.contains("x: u32"), "{formatted}");
    // Second run is the idempotence golden: zero work, still exit 0.
    let out = run(dir.path(), &arg_refs);
    let text = stdout(&out);
    assert!(out.status.success(), "second run clean: {out:?}\n{text}");
    assert!(text.contains("clean"), "{text}");
}

#[test]
fn run_reports_clean_on_converged_tree() {
    let dir = workspace();
    let root = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_dir_str = json_dir.to_str().unwrap().to_string();
    let db = dir.path().join(".guidance.db");
    let db_str = db.to_str().unwrap().to_string();
    // Unfixable-by-any-rung content in a package-less file: no
    // diagnostics (no clippy without a package), so this converges
    // clean — the escalation path is pinned by the unit tests.
    std::fs::write(
        dir.path().join("fine.rs"),
        "pub fn fine() -> u64 {\n    1\n}\n",
    )
    .expect("write");
    let args = run_args(&root, &json_dir_str, &db_str, &["--max-waves", "1"]);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run(dir.path(), &arg_refs);
    let text = stdout(&out);
    assert!(
        out.status.success(),
        "clean fixture exits 0: {out:?}\n{text}"
    );
}
