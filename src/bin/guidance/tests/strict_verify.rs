//! Strict-mode and per-file verify goldens: fresh serves, stale refuses
//! with named files, `verify` reports file verdicts, `--deep` reaches the
//! skew audit. Written before the flags exist (red first).

#[path = "common.rs"]
#[allow(dead_code)]
mod common;

use common::{run, stdout, workspace};
use std::path::Path;

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn db_arg(dir: &Path) -> String {
    dir.join(".guidance.db").to_str().unwrap().to_string()
}

fn root_arg(dir: &Path) -> String {
    dir.to_str().unwrap().to_string()
}

#[test]
fn strict_serves_a_fresh_tree() {
    let dir = workspace();
    common::sync(dir.path(), ".guidance.db", false);
    let root = root_arg(dir.path());
    let out = run(
        dir.path(),
        &[
            "search",
            "e2e_anchor_fn",
            "--workspace",
            &root,
            "--db",
            &db_arg(dir.path()),
            "--strict",
        ],
    );
    assert!(out.status.success(), "fresh tree must serve: {out:?}");
    assert!(stdout(&out).contains("e2e_anchor_fn"), "{}", stdout(&out));
}

#[test]
fn strict_refuses_a_stale_tree_naming_files() {
    let dir = workspace();
    common::sync(dir.path(), ".guidance.db", false);
    std::fs::write(
        dir.path().join("lib.rs"),
        "/// Adds two.\npub fn e2e_anchor_fn(x: u64) -> u64 {\n    x + 2\n}\n",
    )
    .expect("mutate");
    let root = root_arg(dir.path());
    let out = run(
        dir.path(),
        &[
            "search",
            "e2e_anchor_fn",
            "--workspace",
            &root,
            "--db",
            &db_arg(dir.path()),
            "--strict",
        ],
    );
    assert_eq!(out.status.code(), Some(2), "stale must refuse: {out:?}");
    assert!(
        stderr(&out).contains("lib.rs"),
        "report names files: {}",
        stderr(&out)
    );
    // Without --strict the same tree still serves, labelled.
    let out = run(
        dir.path(),
        &[
            "search",
            "e2e_anchor_fn",
            "--workspace",
            &root,
            "--db",
            &db_arg(dir.path()),
        ],
    );
    assert!(out.status.success(), "non-strict must serve: {out:?}");
}

#[test]
fn verify_reports_fresh_then_stale() {
    use std::time::Duration;
    let dir = workspace();
    common::sync(dir.path(), ".guidance.db", false);
    let lib = dir.path().join("lib.rs");
    let lib_arg = lib.to_str().unwrap().to_string();
    let out = run(dir.path(), &["verify", &lib_arg]);
    assert!(out.status.success(), "fresh must exit 0: {out:?}");
    assert!(stdout(&out).contains("fresh"), "{}", stdout(&out));
    // Touch without changing bytes: hashes match under a newer clock,
    // so the audit revalidates instead of trusting the mtime.
    let sidecar_mtime =
        std::fs::metadata(dir.path().join(".guidance").join("src").join("lib.rs.json"))
            .expect("sidecar stat")
            .modified()
            .expect("mtime");
    std::fs::File::options()
        .write(true)
        .open(&lib)
        .expect("open")
        .set_modified(sidecar_mtime + Duration::from_secs(5))
        .expect("touch");
    let out = run(dir.path(), &["verify", &lib_arg]);
    assert_eq!(out.status.code(), Some(1), "stale must exit 1: {out:?}");
    assert!(stdout(&out).contains("stale"), "{}", stdout(&out));
}

#[test]
fn verify_missing_sidecar_reports_unknown() {
    let dir = workspace();
    let fresh = dir.path().join("fresh.rs");
    std::fs::write(&fresh, "pub fn fresh() {}\n").expect("write");
    let out = run(dir.path(), &["verify", fresh.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1), "unknown must exit 1: {out:?}");
    assert!(stdout(&out).contains("unknown"), "{}", stdout(&out));
}

#[test]
fn verify_deep_reaches_the_skew_audit() {
    use std::time::Duration;
    let dir = workspace();
    common::sync(dir.path(), ".guidance.db", false);
    let lib = dir.path().join("lib.rs");
    let lib_arg = lib.to_str().unwrap().to_string();
    // Changed bytes under a forged newer mtime: the clock claims work,
    // the hashes refuse it — the skew-suspect shape.
    std::fs::write(
        &lib,
        "/// Adds two.\npub fn e2e_anchor_fn(x: u64) -> u64 {\n    x + 2\n}\n",
    )
    .expect("mutate");
    let sidecar_mtime =
        std::fs::metadata(dir.path().join(".guidance").join("src").join("lib.rs.json"))
            .expect("sidecar stat")
            .modified()
            .expect("mtime");
    std::fs::File::options()
        .write(true)
        .open(&lib)
        .expect("open")
        .set_modified(sidecar_mtime + Duration::from_secs(5))
        .expect("forge");
    let out = run(dir.path(), &["verify", &lib_arg, "--deep"]);
    assert_eq!(out.status.code(), Some(1), "skew must exit 1: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("skew"), "skew branch reached: {text}");
    assert!(
        text.contains("recorded"),
        "deep prints recorded hash: {text}"
    );
    assert!(text.contains("live"), "deep prints live hash: {text}");
}
