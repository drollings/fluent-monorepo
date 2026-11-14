//! Extra `ProjectConfig` coverage: root-doc scope defaults. Included
//! via `#[path]` forwarder from `src/config.rs`, per repo convention.

use super::*;

#[test]
fn root_docs_default_on() {
    // Absent key means included: existing configs gain root orientation
    // files without edits (the stated M4 behavior change).
    let config: ProjectConfig = serde_json::from_value(serde_json::json!({})).expect("parses");
    assert!(config.root_docs);
}

#[test]
fn root_docs_explicit_opt_out() {
    let config: ProjectConfig =
        serde_json::from_value(serde_json::json!({"root_docs": false})).expect("parses");
    assert!(!config.root_docs);
}

#[test]
fn root_docs_explicit_opt_in_round_trips() {
    let config: ProjectConfig =
        serde_json::from_value(serde_json::json!({"root_docs": true})).expect("parses");
    assert!(config.root_docs);
}
