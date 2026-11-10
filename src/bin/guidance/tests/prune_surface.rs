//! Pruned surface: VISION-excluded subcommands exit with a pointed
//! removal message; the kept navigation surface still works; the MCP
//! default toolset is search-only.

#[path = "common.rs"]
#[allow(dead_code)]
mod common;

use common::{run, stdout, workspace};

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn removed_subcommands_exit_pointed() {
    let dir = workspace();
    let args = vec!["commit"];
    let out = run(dir.path(), &args);
    assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
    assert!(
        stderr(&out).contains("removed:"),
        "{args:?}: {}",
        stderr(&out)
    );
}

#[test]
fn kept_surface_still_works() {
    let dir = workspace();
    let out = run(dir.path(), &["status"]);
    assert!(out.status.success(), "status: {out:?}");
    let out = run(dir.path(), &["--help"]);
    assert!(out.status.success(), "--help: {out:?}");
    assert!(stdout(&out).contains("status"), "{}", stdout(&out));
}

#[test]
fn check_fixpoint_flag_is_documented() {
    let dir = workspace();
    let out = run(dir.path(), &["check", "--help"]);
    assert!(out.status.success(), "check --help: {out:?}");
    assert!(stdout(&out).contains("fixpoint"), "{}", stdout(&out));
}

#[test]
fn mcp_default_toolset_is_search_only() {    let dir = workspace();
    let out = run(dir.path(), &["mcp", "--help"]);
    assert!(out.status.success(), "mcp --help: {out:?}");
    assert!(
        stdout(&out).contains("default: agent"),
        "{}",
        stdout(&out)
    );
}
