//! `sync` as the maintenance harness: fix ordering, residual feedback,
//! all-stale-file coverage, and the L2 determinism boundary.

#[path = "common.rs"]
#[allow(dead_code)]
mod common;

use std::path::Path;

use common::{run, stdout};

fn package(dir: &Path) {
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("manifest");
    std::fs::create_dir_all(dir.join(".guidance")).expect("guidance dir");
    std::fs::write(
        dir.join(".guidance").join("guidance-config.json"),
        r#"{"version":"1","guidance_dir":".guidance","db_path":".guidance.db","src_dirs":["src"],"docs_dir":"doc","root_docs":false,"decision":{"enabled":false},"generative":{"enabled":false},"policy":{"version":1,"mechanical_classes":["rustfmt","clippy-fix"],"attempt_limit":3,"autonomy":"L2"}}"#,
    )
    .expect("config");
    std::fs::create_dir_all(dir.join("src")).expect("src");
}

fn sync(dir: &Path, extra: &[&str]) -> std::process::Output {
    let root = dir.to_str().unwrap();
    let mut args = vec![
        "sync",
        "--workspace",
        root,
        "--json-dir",
        ".guidance",
        "--db",
        ".guidance.db",
    ];
    args.extend(extra.iter().copied());
    run(dir, &args)
}

#[test]
fn sync_fixes_format_and_lint_then_indexes() {
    let dir = tempfile::tempdir().expect("tempdir");
    package(dir.path());
    std::fs::write(
        dir.path().join("src").join("lib.rs"),
        "use std::collections::HashMap;\n\npub fn add(a: i32, b: i32) -> i32 {\n        a + b\n}\n",
    )
    .expect("write");

    let out = sync(dir.path(), &[]);
    let text = stdout(&out);
    assert!(out.status.success(), "sync: {out:?}\n{text}");
    assert!(text.contains("format 1, lint 1"), "{text}");
    assert!(text.contains("witness: clean"), "{text}");
    let fixed = std::fs::read_to_string(dir.path().join("src").join("lib.rs")).expect("read");
    assert!(fixed.contains("a + b"), "{fixed}");
    assert!(!fixed.contains("HashMap"), "{fixed}");
}

#[test]
fn sync_keeps_residual_compile_error_stale_with_full_detail() {
    let dir = tempfile::tempdir().expect("tempdir");
    package(dir.path());
    std::fs::write(
        dir.path().join("src").join("lib.rs"),
        "use std::collections::HashMap;\n\npub fn add(a: i32, b: i32) -> i32 {\n        a + b\n}\n\npub fn broken() -> i32 {\n    let value: i32 = \"not an int\";\n    value\n}\n",
    )
    .expect("write");

    let out = sync(dir.path(), &[]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(2), "stuck exits 2: {out:?}\n{text}");
    assert!(text.contains("E0308"), "{text}");
    assert!(text.contains("witness: not clean"), "{text}");
    let source = std::fs::read_to_string(dir.path().join("src").join("lib.rs")).expect("read");
    assert!(source.contains("not an int"), "{source}");

    let status = run(
        dir.path(),
        &[
            "status",
            "--guidance-dir",
            ".guidance",
            "--db",
            ".guidance.db",
        ],
    );
    let status_text = stdout(&status);
    assert!(status_text.contains("Stale files: 1"), "{status_text}");
    assert!(status_text.contains("Fix:         stuck"), "{status_text}");

    let again = sync(dir.path(), &[]);
    assert_eq!(again.status.code(), Some(2), "residual re-surfaces");
    assert!(stdout(&again).contains("E0308"), "{}", stdout(&again));
}

#[test]
fn sync_applies_machine_applicable_compile_fix() {
    let dir = tempfile::tempdir().expect("tempdir");
    package(dir.path());
    std::fs::write(
        dir.path().join("src").join("lib.rs"),
        "pub fn bump() -> i32 {\n    let x = 1;\n    x += 1;\n    x\n}\n",
    )
    .expect("write");

    let out = sync(dir.path(), &[]);
    let text = stdout(&out);
    assert!(out.status.success(), "compile fix: {out:?}\n{text}");
    assert!(text.contains("compile 1"), "{text}");
    let fixed = std::fs::read_to_string(dir.path().join("src").join("lib.rs")).expect("read");
    assert!(fixed.contains("let mut x"), "{fixed}");
}

#[test]
fn sync_dry_run_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    package(dir.path());
    std::fs::write(
        dir.path().join("src").join("lib.rs"),
        "fn messy(x:u32)->u32{x+1}\n",
    )
    .expect("write");

    let out = sync(dir.path(), &["--dry-run"]);
    let text = stdout(&out);
    assert!(out.status.success(), "dry run: {out:?}\n{text}");
    assert!(text.contains("dry run"), "{text}");
    assert!(!dir.path().join(".guidance.db").exists(), "no db");
    assert!(
        !dir.path().join(".guidance").join("src").exists(),
        "no sidecars"
    );
    let untouched = std::fs::read_to_string(dir.path().join("src").join("lib.rs")).expect("read");
    assert!(untouched.contains("x:u32"), "{untouched}");
}

#[test]
fn sync_processes_every_stale_file_without_a_cap() {
    let dir = tempfile::tempdir().expect("tempdir");
    package(dir.path());
    std::fs::write(dir.path().join("src").join("lib.rs"), "\n").expect("lib");
    for i in 0..33 {
        std::fs::write(
            dir.path().join("src").join(format!("f{i:02}.rs")),
            format!("pub fn f{i:02}() -> u32 {{\n        {i}\n}}\n"),
        )
        .expect("write");
    }

    let out = sync(dir.path(), &[]);
    let text = stdout(&out);
    assert!(out.status.success(), "all stale files: {out:?}\n{text}");
    assert!(text.contains("33 fixed"), "{text}");
    let fixed = std::fs::read_to_string(dir.path().join("src").join("f00.rs")).expect("read");
    assert!(fixed.contains("    0"), "{fixed}");
}
