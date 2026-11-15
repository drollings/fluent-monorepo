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
fn mcp_default_toolset_is_search_only() {
    let dir = workspace();
    let out = run(dir.path(), &["mcp", "--help"]);
    assert!(out.status.success(), "mcp --help: {out:?}");
    assert!(stdout(&out).contains("default: agent"), "{}", stdout(&out));
}

/// M12 duplicate-path pins: the legacy explain assembly is gone —
/// `ReadEngine` is the only DB-backed answer path, so its former
/// helpers must not exist in the binary. Each assertion names the
/// function whose absence proves the migration; re-adding any of
/// them fails here first.
const SEARCH_SRC: &str = include_str!("../src/search.rs");
const MAIN_SRC: &str = include_str!("../src/main.rs");

fn assert_absent(source: &str, name: &str) {
    for marker in [format!("fn {name}("), format!("fn {name}<")] {
        assert!(
            !source.contains(&marker),
            "duplicate read path resurrected: {name}"
        );
    }
}

#[test]
fn legacy_explain_assembly_is_gone() {
    for name in [
        "fused_hits",
        "explain_adapter",
        "recall_capability_briefs_for",
        "capability_branch_pulls",
        "merge_branch_hits",
        "graph_callers_hits",
        "judge_doc_paragraphs",
        "judge_capability_briefs",
        "answer_with_backend",
        "top_snippets",
        "build_explain_rows",
        "rank_doc_paragraphs",
        "rank_doc_paragraphs_cached",
        "doc_rank_cache_key",
    ] {
        assert_absent(SEARCH_SRC, name);
    }
    assert_absent(SEARCH_SRC, "constrain_answer_to_evidence");
    assert!(
        !MAIN_SRC.contains("async fn cmd_explain("),
        "legacy cmd_explain resurrected"
    );
    // The rank cache went with query-time doc walking: no struct, no
    // file constant, no tests-only key helper left behind.
    assert!(
        !SEARCH_SRC.contains("DocRankCache"),
        "doc rank cache resurrected"
    );
    assert!(
        !SEARCH_SRC.contains("DOC_RANK_CACHE_FILE"),
        "doc rank cache file const resurrected"
    );
}
