//! P5 `rg-cli` port: the managed-rg bypass stays independent of index
//! state, ignores staleness, and honors output flags — through the built
//! binary.

use std::path::PathBuf;
use std::process::Command;

fn guidance_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_guidance"))
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("src")).expect("src");
    std::fs::write(
        dir.path().join("src").join("main.rs"),
        "fn main() {\n    let rg_bypass_needle = 1;\n    println!(\"{rg_bypass_needle}\");\n}\n",
    )
    .expect("write");
    dir
}

fn run(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(guidance_bin())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn guidance")
}

#[test]
fn rg_bypass_needs_no_index() {
    let dir = workspace();
    let root = dir.path().to_str().unwrap().to_string();
    // No index, no db anywhere — L0 still retrieves.
    let out = run(
        dir.path(),
        &["search", "rg_bypass_needle", "--workspace", &root, "--rg"],
    );
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("coverage: rg_exhaustive"), "{text}");
    assert!(text.contains("src/main.rs:2:9"), "{text}");
}

#[test]
fn rg_context_flags_shape_output() {
    let dir = workspace();
    let root = dir.path().to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &[
            "search",
            "rg_bypass_needle",
            "--workspace",
            &root,
            "--rg",
            "-B",
            "1",
            "-A",
            "1",
        ],
    );
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("fn main()"), "{text}");
    assert!(text.contains("println!"), "{text}");
}

#[test]
fn rg_enrichment_attaches_symbols() {
    let dir = workspace();
    let root = dir.path().to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &["search", "rg_bypass_needle", "--workspace", &root, "--rg"],
    );
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("[main]"), "{text}");
    let out = run(
        dir.path(),
        &[
            "search",
            "rg_bypass_needle",
            "--workspace",
            &root,
            "--rg",
            "--no-enrich",
        ],
    );
    assert!(out.status.success(), "{out:?}");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("[main]"));
}

#[test]
fn rg_rejects_conflicting_modes_and_names_missing_paths() {
    let dir = workspace();
    let root = dir.path().to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &["search", "x", "--workspace", &root, "--rg", "--fts"],
    );
    assert!(!out.status.success(), "conflicting modes must fail");
    let missing = dir.path().join("no-such-root");
    let missing = missing.to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &["search", "x", "--workspace", &missing, "--rg"],
    );
    assert!(!out.status.success(), "missing root must fail");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("workspace not found"), "{stderr}");
}
