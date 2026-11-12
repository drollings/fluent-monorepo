//! `impact` command goldens: index-backed tests, unseen channels,
//! and a conservative deletion verdict. Written before the wiring
//! (red: index-backed rows missing).

#[path = "common.rs"]
#[allow(dead_code)]
mod common;

use common::{run, stdout, workspace};

#[test]
fn impact_reports_index_tests_and_unseen_channels() {
    let dir = workspace();
    std::fs::write(
        dir.path().join("a.rs"),
        "pub fn target_a() -> u64 {\n    1\n}\n",
    )
    .expect("write");
    let tests = dir.path().join("tests");
    std::fs::create_dir_all(&tests).expect("mkdir");
    std::fs::write(
        tests.join("test_t.rs"),
        "use crate::a::target_a;\n#[test]\nfn t() {\n    assert_eq!(target_a(), 1);\n}\n",
    )
    .expect("write");
    let root = dir.path().to_str().unwrap().to_string();
    let out = run(dir.path(), &["impact", "target_a", "--workspace", &root]);
    let text = stdout(&out);
    assert!(out.status.success(), "impact runs: {out:?}\n{text}");
    assert!(
        text.contains("test_t.rs"),
        "index-backed test listed: {text}"
    );
    assert!(
        !text.contains("heuristic"),
        "index hits are not labelled heuristic: {text}"
    );
    assert!(
        text.contains("unseen:"),
        "unseen channels published: {text}"
    );
    assert!(
        text.contains("deletion: no (has dependents)"),
        "dependent test file proves use: {text}"
    );
}
