//! `summarize` command contract: additive flags, clean errors without a
//! configured model, and an empty workspace that succeeds with no calls.

#[path = "common.rs"]
#[allow(dead_code)]
mod common;

use common::{run, stdout, workspace};

#[test]
fn summarize_help_lists_stable_flags() {
    let dir = workspace();
    let out = run(dir.path(), &["summarize", "--help"]);
    assert!(out.status.success(), "summarize --help: {out:?}");
    let text = stdout(&out);
    for flag in ["--workspace", "--json-dir", "--db"] {
        assert!(text.contains(flag), "missing {flag}:\n{text}");
    }
}

#[test]
fn summarize_without_model_is_a_named_error() {
    // Generative role disabled: the command refuses cleanly instead of
    // dialing anything.
    let dir = workspace();
    let guidance = dir.path().join(".guidance");
    std::fs::create_dir_all(&guidance).expect("mkdir");
    std::fs::write(
        guidance.join("guidance-config.json"),
        r#"{"generative": {"base_url": "http://127.0.0.1:9", "model": "x", "timeout_ms": 1000, "enabled": false}}"#,
    )
    .expect("write config");
    let root = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_dir = json_dir.to_str().unwrap().to_string();
    let db = dir.path().join("sum.db");
    let db = db.to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &[
            "summarize",
            "--workspace",
            &root,
            "--json-dir",
            &json_dir,
            "--db",
            &db,
        ],
    );
    assert!(!out.status.success(), "disabled role must fail: {out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains("generative"),
        "error must name the role: {stderr}"
    );
}

#[test]
fn summarize_empty_workspace_succeeds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_dir = json_dir.to_str().unwrap().to_string();
    let db = dir.path().join("sum.db");
    let db = db.to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &[
            "summarize",
            "--workspace",
            &root,
            "--json-dir",
            &json_dir,
            "--db",
            &db,
        ],
    );
    assert!(out.status.success(), "empty workspace: {out:?}");
    assert!(stdout(&out).contains("0 files"), "{}", stdout(&out));
}

#[test]
fn sync_summarize_flag_skips_cleanly_without_model() {
    // Opt-in sync invocation with a disabled role warns and completes the
    // sync; the deterministic index never waits on a model.
    let dir = workspace();
    let guidance = dir.path().join(".guidance");
    std::fs::create_dir_all(&guidance).expect("mkdir");
    std::fs::write(
        guidance.join("guidance-config.json"),
        r#"{"generative": {"base_url": "http://127.0.0.1:9", "model": "x", "timeout_ms": 1000, "enabled": false}}"#,
    )
    .expect("write config");
    let root = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_dir = json_dir.to_str().unwrap().to_string();
    let db = dir.path().join("sum.db");
    let db = db.to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &[
            "sync",
            "--workspace",
            &root,
            "--json-dir",
            &json_dir,
            "--db",
            &db,
            "--summarize",
        ],
    );
    assert!(out.status.success(), "sync with skipped summaries: {out:?}");
    let combined = stdout(&out) + String::from_utf8_lossy(&out.stderr).as_ref();
    assert!(combined.contains("Sync complete."), "{combined}");
    assert!(combined.contains("summary pass skipped"), "{combined}");
}
