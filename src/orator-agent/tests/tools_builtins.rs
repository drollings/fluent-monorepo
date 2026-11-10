//! Builtin tests: calculator math, arg validation, jail behavior.

use std::sync::Arc;

use crate::tools::builtins::{default_registry, Calc, TimeNow};
use crate::tools::sandbox::{Guarded, GuardedConfig};
use crate::tools::tool::{Tool, ToolTier};

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
    calc.execute(serde_json::json!({}))
        .expect_err("missing arg fails");
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
    use crate::tools::tool::TOOL_OUTPUT_CHARS;
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
    assert!(
        back.contains("truncated"),
        "large reads truncate with marker"
    );
}

#[test]
fn file_write_swap_race_creates_nothing_outside() {
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
    // Symlinked parent pointing outside: the write must refuse and
    // must not mkdir anything outside the jail.
    let outside = tempfile::tempdir().expect("outside");
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).expect("link outside");
    write
        .execute(serde_json::json!({"path": "link/evil.txt", "content": "x"}))
        .expect_err("escape through a linked parent must refuse");
    assert!(
        !outside.path().join("evil.txt").exists(),
        "refused write leaves no outside file"
    );
    assert!(
        !outside.path().join("link").exists(),
        "refused write creates no outside dir"
    );
    // Legit nested writes still work through the jail.
    write
        .execute(serde_json::json!({"path": "sub/nested.txt", "content": "hi"}))
        .expect("nested jailed write runs");
    assert!(dir.path().join("sub/nested.txt").exists());
}

#[test]
fn web_fetch_refuses_loopback_and_metadata_before_connect() {
    use crate::tools::builtins::WebFetch;
    use crate::tools::tool::Tool;
    let fetch = WebFetch;
    for url in [
        "http://127.0.0.1/",
        "http://127.0.0.1:8080/v1/chat",
        "http://localhost/",
        "http://[::1]/",
        "http://169.254.169.254/latest/meta-data/",
        "http://0.0.0.0/",
        "http://10.0.0.5/",
        "http://192.168.1.1/",
        "ftp://example.com/file",
    ] {
        fetch
            .execute(serde_json::json!({"url": url}))
            .expect_err(&format!("SSRF deny must refuse {url} before connect"));
    }
}
