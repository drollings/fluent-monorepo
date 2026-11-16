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

#[test]
fn sync_twice_converges_to_zero_stale() {
    // Fix-first, index-second, stamp-last converges: a second pass
    // over a synced tree reports zero stale and does no work.
    let dir = tempfile::tempdir().expect("tempdir");
    package(dir.path());
    std::fs::write(
        dir.path().join("src").join("lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )
    .expect("write");

    let out = sync(dir.path(), &[]);
    assert!(
        out.status.success(),
        "first sync: {out:?}\n{}",
        stdout(&out)
    );
    let status = run(
        dir.path(),
        &["status", "--guidance-dir", ".guidance", "--json"],
    );
    assert!(status.status.success(), "status: {status:?}");
    let report: serde_json::Value = serde_json::from_str(&stdout(&status)).expect("json parses");
    assert_eq!(report.get("stale"), Some(&serde_json::json!(0)), "{report}");
    assert!(report.get("fresh").is_some(), "{report}");
    assert!(report.get("revalidated").is_some(), "{report}");

    let again = sync(dir.path(), &[]);
    let text = stdout(&again);
    assert!(again.status.success(), "second sync: {again:?}\n{text}");
    assert!(text.contains("clean"), "second pass is a no-op: {text}");
}

#[test]
fn sync_dry_run_reports_fresh_revalidated_stale() {
    let dir = tempfile::tempdir().expect("tempdir");
    package(dir.path());
    std::fs::write(
        dir.path().join("src").join("lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )
    .expect("write");

    let out = sync(dir.path(), &[]);
    assert!(out.status.success(), "sync: {out:?}");

    let out = sync(dir.path(), &["--dry-run"]);
    let text = stdout(&out);
    assert!(out.status.success(), "dry run: {out:?}\n{text}");
    assert!(text.contains("dry run"), "{text}");
    for key in ["fresh", "revalidated", "stale"] {
        assert!(text.contains(key), "dry run reports {key}: {text}");
    }
}

#[test]
fn explain_coverage_agrees_with_status() {
    // The read plane and `status` count the same source set: the
    // coverage mirror behind `explain` matches `status --json`.
    let dir = tempfile::tempdir().expect("tempdir");
    package(dir.path());
    std::fs::write(
        dir.path().join("src").join("lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )
    .expect("write");
    std::fs::write(
        dir.path().join("stray.rs"),
        "pub fn stray() -> u32 {\n    0\n}\n",
    )
    .expect("write stray");

    let out = sync(dir.path(), &[]);
    assert!(out.status.success(), "sync: {out:?}");

    let status = run(
        dir.path(),
        &["status", "--guidance-dir", ".guidance", "--json"],
    );
    let report: serde_json::Value = serde_json::from_str(&stdout(&status)).expect("json parses");
    let total = report.get("total").and_then(|v| v.as_u64()).expect("total");
    let stale = report.get("stale").and_then(|v| v.as_u64()).expect("stale");

    let root = dir.path().to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &[
            "explain",
            "what does add do",
            "--workspace",
            &root,
            "--db",
            dir.path().join(".guidance.db").to_str().unwrap(),
            "--format",
            "json",
            "--no-llm",
            "--allow-degraded",
        ],
    );
    assert!(out.status.success(), "explain: {out:?}");
    let envelope: serde_json::Value =
        serde_json::from_str(stdout(&out).trim()).expect("json parses");
    let mirror = envelope
        .get("coverage")
        .and_then(|coverage| coverage.get("mirror"))
        .expect("mirror");
    let state = mirror.get("state").and_then(|v| v.as_str()).unwrap_or("");
    let (covered_total, covered_stale) = match state {
        "fresh" => (mirror.get("total").and_then(|v| v.as_u64()).unwrap_or(0), 0),
        "stale" => (
            mirror.get("total").and_then(|v| v.as_u64()).unwrap_or(0),
            mirror.get("stale").and_then(|v| v.as_u64()).unwrap_or(0),
        ),
        other => panic!("unexpected mirror shape: {mirror} ({other})"),
    };
    assert_eq!(
        covered_total, total,
        "coverage reads the same source as status"
    );
    assert_eq!(covered_stale, stale, "coverage agrees on stale");
}
