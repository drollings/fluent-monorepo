//! Registry tests: single definitions, duplicate rejection, order.

use std::sync::Arc;

use crate::builtins::default_registry;
use crate::sandbox::{Guarded, GuardedConfig};
use crate::tool::{Tool, ToolError, ToolRegistry, ToolTier};

struct Stub;

impl Tool for Stub {
    fn name(&self) -> &'static str {
        "stub"
    }
    fn tier(&self) -> ToolTier {
        ToolTier::T0
    }
    fn description(&self) -> &'static str {
        "Stub."
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({"name": "stub"})
    }
    fn execute(&self, _args: serde_json::Value) -> Result<String, ToolError> {
        Ok("ok".to_string())
    }
}

#[test]
fn names_resolve_to_exactly_one_definition() {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(Stub)).expect("first registers");
    assert!(registry.get("stub").is_some());
    assert!(registry.get("missing").is_none());
    assert_eq!(registry.names(), vec!["stub"]);
    registry
        .register(Arc::new(Stub))
        .expect_err("duplicate name rejected");
}

#[test]
fn v1_catalog_pins_seven_names_once_each() {
    let dir = tempfile::tempdir().expect("tempdir");
    let guarded = Arc::new(
        Guarded::new(GuardedConfig {
            root: dir.path().to_path_buf(),
            ..GuardedConfig::default()
        })
        .expect("jail builds"),
    );
    let registry = default_registry(&guarded, Arc::new(|_| {}));
    let names = registry.names();
    assert_eq!(
        names,
        vec![
            "time_now",
            "calc",
            "notify",
            "shell",
            "file_read",
            "file_write",
            "web_fetch"
        ]
    );
    let mut sorted = names.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), names.len(), "no name twice");
    for schema in registry.schemas() {
        assert!(schema.get("name").is_some());
        assert!(schema.get("parameters").is_some());
    }
}
