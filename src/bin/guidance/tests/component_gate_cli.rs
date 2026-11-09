//! `guidance component-gate` is a read-only path → gate lookup.

#[path = "common.rs"]
#[allow(dead_code)]
mod common;

use common::{run, stdout, workspace};

#[test]
fn matrix_path_reports_component_smoke_gate() {
    let dir = workspace();
    let out = run(dir.path(), &["component-gate", "src/guidance/src/lib.rs"]);
    assert!(out.status.success(), "component-gate: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("package: guidance-core"), "{text}");
    assert!(text.contains("component: guidance"), "{text}");
    assert!(text.contains("gate: guidance-smoke"), "{text}");
}

#[test]
fn non_matrix_path_reports_package_gate() {
    let dir = workspace();
    let out = run(dir.path(), &["component-gate", "src/llm/src/lib.rs"]);
    assert!(out.status.success(), "component-gate: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("package: fluent-llm"), "{text}");
    assert!(text.contains("component: (none)"), "{text}");
    assert!(text.contains("gate: cargo test -p fluent-llm"), "{text}");
}

#[test]
fn unknown_path_reports_documented_default() {
    let dir = workspace();
    let out = run(dir.path(), &["component-gate", "doc/guidance/VISION.md"]);
    assert!(out.status.success(), "component-gate: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("component: (none)"), "{text}");
    assert!(text.contains("gate: make smoke"), "{text}");
}
