//! Profile gate: `GUIDANCE_PROFILE=1` prints `[profile]` phase lines
//! for `explain` without changing stdout.
#[path = "common.rs"]
mod common;

use common::{stdout, sync};

#[test]
fn explain_emits_profile_lines_when_requested() {
    let dir = common::workspace();
    sync(dir.path(), ".profile.db", false);
    let root = dir.path().to_str().unwrap().to_string();
    let db = dir.path().join(".profile.db");
    let db = db.to_str().unwrap().to_string();
    let out = std::process::Command::new(common::guidance_bin())
        .args([
            "explain",
            "e2e_anchor_fn",
            "--workspace",
            &root,
            "--db",
            &db,
            "--no-llm",
        ])
        .env("GUIDANCE_PROFILE", "1")
        .current_dir(dir.path())
        .output()
        .expect("spawn guidance");
    assert!(out.status.success(), "explain: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("e2e_anchor_fn"), "{text}");
    let phases = String::from_utf8_lossy(&out.stderr);
    assert!(
        phases.contains("[profile]"),
        "profile lines on stderr: {phases}"
    );
}
