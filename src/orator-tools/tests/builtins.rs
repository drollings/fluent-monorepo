//! Builtin tests: calculator math, arg validation, jail behavior.

use std::sync::Arc;

use crate::builtins::{default_registry, Calc, TimeNow};
use crate::sandbox::{Guarded, GuardedConfig};
use crate::tool::{Tool, ToolTier};

#[test]
fn calc_evaluates_with_precedence() {
    let calc = Calc;
    let value = |expr: &str| {
        calc.execute(serde_json::json!({"expr": expr}))
            .expect("math runs")
    };
    assert_eq!(value("2 + 3 * 4"), "14");
    assert_eq!(value("(2 + 3) * 4"), "20");
    assert_eq!(value("2 ^ 10"), "1024");
    assert_eq!(value("-3 + 10 / 2"), "2");
    assert_eq!(value("7 % 3"), "1");
    calc.execute(serde_json::json!({"expr": "1 / 0"}))
        .expect_err("division by zero fails");
    calc.execute(serde_json::json!({"expr": "rm -rf /"}))
        .expect_err("prose is not math");
    calc.execute(serde_json::json!({})).expect_err("missing arg fails");
}

#[test]
fn time_runs_without_args() {
    let now = TimeNow.execute(serde_json::json!({})).expect("clock runs");
    assert!(now.contains('-'), "{now}");
    assert_eq!(TimeNow.tier(), ToolTier::T0);
}

#[test]
fn file_tools_stay_in_jail() {
    let dir = tempfile::tempdir().expect("tempdir");
    let guarded = Arc::new(
        Guarded::new(GuardedConfig {
            root: dir.path().to_path_buf(),
            ..GuardedConfig::default()
        })
        .expect("jail builds"),
    );
    let registry = default_registry(&guarded, Arc::new(|_| {}));
    let write = registry.get("file_write").expect("writer listed");
    write
        .execute(serde_json::json!({"path": "sub/note.txt", "content": "hi"}))
        .expect("jailed write runs");
    let read = registry.get("file_read").expect("reader listed");
    let back = read
        .execute(serde_json::json!({"path": "sub/note.txt"}))
        .expect("jailed read runs");
    assert_eq!(back, "hi");
    write
        .execute(serde_json::json!({"path": "../escape.txt", "content": "x"}))
        .expect_err("escape refused");
}

#[test]
fn file_read_caps_large_files() {
    use crate::tool::TOOL_OUTPUT_CHARS;
    let dir = tempfile::tempdir().expect("tempdir");
    let guarded = Arc::new(
        Guarded::new(GuardedConfig {
            root: dir.path().to_path_buf(),
            ..GuardedConfig::default()
        })
        .expect("jail builds"),
    );
    let registry = default_registry(&guarded, Arc::new(|_| {}));
    let write = registry.get("file_write").expect("writer listed");
    let big = "z".repeat(TOOL_OUTPUT_CHARS + 500);
    write
        .execute(serde_json::json!({"path": "big.txt", "content": big}))
        .expect("write runs");
    let read = registry.get("file_read").expect("reader listed");
    let back = read
        .execute(serde_json::json!({"path": "big.txt"}))
        .expect("read runs");
    assert!(back.contains("truncated"), "large reads truncate with marker");
}
