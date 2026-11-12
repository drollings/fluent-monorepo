//! `kb` command golden: compact JSON knowledge-base summary for
//! orchestrators. Written with the command (stable keys pinned here).

#[path = "common.rs"]
#[allow(dead_code)]
mod common;

use common::{run, stdout, workspace};

#[test]
fn kb_reports_files_members_and_commands() {
    let dir = workspace();
    let root = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_dir_str = json_dir.to_str().unwrap().to_string();
    // Mirror first so members and freshness are real, not zero.
    let sync_out = run(
        dir.path(),
        &["sync", "--workspace", &root, "--json-dir", &json_dir_str],
    );
    assert!(sync_out.status.success(), "sync: {sync_out:?}");
    let out = run(
        dir.path(),
        &[
            "kb",
            "--workspace",
            &root,
            "--json-dir",
            &json_dir_str,
            "--db",
            dir.path().join(".guidance.db").to_str().unwrap(),
        ],
    );
    let text = stdout(&out);
    assert!(out.status.success(), "kb runs: {out:?}\n{text}");
    let value: serde_json::Value = serde_json::from_str(&text).expect("kb prints JSON");
    assert_eq!(value["files"], 1);
    assert_eq!(value["stale"], 0);
    assert!(value["fresh"].as_bool().unwrap(), "{value}");
    assert!(value["members"].as_u64().unwrap() >= 1, "{value}");
    assert_eq!(value["schema"], 1);
    let query = value["query"].as_array().expect("query hints");
    assert!(query
        .iter()
        .any(|c| c.as_str().unwrap_or("").contains("search")));
    let maintain = value["maintain"].as_array().expect("maintain hints");
    assert!(maintain
        .iter()
        .any(|c| c.as_str().unwrap_or("").contains("run")));
}
