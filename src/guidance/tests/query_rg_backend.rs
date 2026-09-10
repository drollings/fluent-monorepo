use super::*;
use fluent_wvr::capability::{CapabilitySet, FsCapability, CURRENT_CAPS};
use std::ffi::OsString;
use std::path::Path;

// L0 minimal: capability gate, literal search, hard excludes, limit
// early-kill, ENOENT-only binary chain. rg is a local binary (allowed in
// hermetic tests); the capability scope is entered explicitly.

fn write_tree(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("src");
    std::fs::create_dir_all(root.join(".git")).expect("git");
    std::fs::write(
        root.join("src").join("a.ts"),
        "const AlphaNeedle = 1;\nconst AlphaNeedle = 2;\nconst Other = 3;\n",
    )
    .expect("write");
    std::fs::write(
        root.join(".git").join("secret.ts"),
        "const AlphaNeedle = 9;\n",
    )
    .expect("write");
}

fn with_caps<T>(run: impl FnOnce() -> T) -> T {
    let caps = CapabilitySet::new().with(FsCapability::new());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    runtime.block_on(CURRENT_CAPS.scope(caps, async move { run() }))
}

#[test]
fn rg_finds_literal_matches_with_positions() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_tree(dir.path());
    let backend = RgBackend {
        root: dir.path().to_path_buf(),
        exe_override: None,
        path_override: None,
    };
    let hits = with_caps(|| backend.search("AlphaNeedle", 10)).expect("search");
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].path, "src/a.ts");
    assert_eq!(hits[0].line, 1);
    assert_eq!(hits[0].column, 7);
    assert!(hits[0].text.contains("AlphaNeedle"));
}

#[test]
fn rg_hard_excludes_git_and_state_dirs() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_tree(dir.path());
    let backend = RgBackend {
        root: dir.path().to_path_buf(),
        exe_override: None,
        path_override: None,
    };
    let hits = with_caps(|| backend.search("AlphaNeedle", 10)).expect("search");
    assert!(hits.iter().all(|hit| !hit.path.starts_with(".git")));
}

#[test]
fn rg_early_kills_past_the_limit() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_tree(dir.path());
    let backend = RgBackend {
        root: dir.path().to_path_buf(),
        exe_override: None,
        path_override: None,
    };
    let hits = with_caps(|| backend.search("AlphaNeedle", 1)).expect("search");
    assert_eq!(hits.len(), 1);
}

#[test]
fn rg_fails_closed_without_capability() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_tree(dir.path());
    let backend = RgBackend {
        root: dir.path().to_path_buf(),
        exe_override: None,
        path_override: None,
    };
    // No CURRENT_CAPS scope on this thread → denied.
    let result = backend.search("AlphaNeedle", 10);
    assert!(matches!(result, Err(RgError::DeniedCapability)));
}

#[test]
fn rg_reports_no_binary_on_empty_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_tree(dir.path());
    let empty = tempfile::tempdir().expect("empty path dir");
    let backend = RgBackend {
        root: dir.path().to_path_buf(),
        exe_override: None,
        path_override: Some(OsString::from(empty.path())),
    };
    let result = with_caps(|| backend.search("AlphaNeedle", 10));
    assert!(matches!(result, Err(RgError::NoBinary)));
}

// P5 full: -A/-B context, mtime post-filter, missing-path diagnostics.

fn write_context_tree(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("src");
    std::fs::write(
        root.join("src").join("b.ts"),
        "line one\nline two\nconst CtxNeedle = 1;\nline four\nline five\n",
    )
    .expect("write");
}

fn backend_for(root: &Path) -> RgBackend {
    RgBackend {
        root: root.to_path_buf(),
        exe_override: None,
        path_override: None,
    }
}

#[test]
fn rg_captures_before_and_after_context() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_context_tree(dir.path());
    let options = RgOptions {
        context_before: 2,
        context_after: 2,
        ..Default::default()
    };
    let hits = with_caps(|| backend_for(dir.path()).search_with_options("CtxNeedle", 10, &options))
        .expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].line, 3);
    assert_eq!(hits[0].context_before, vec!["line one\n", "line two\n"]);
    assert_eq!(hits[0].context_after, vec!["line four\n", "line five\n"]);
}

#[test]
fn rg_context_defaults_to_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_context_tree(dir.path());
    let hits = with_caps(|| backend_for(dir.path()).search("CtxNeedle", 10)).expect("search");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].context_before.is_empty());
    assert!(hits[0].context_after.is_empty());
}

#[test]
fn rg_mtime_post_filter_keeps_and_drops() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_context_tree(dir.path());
    let far_future = i64::MAX / 2;
    let dropped = with_caps(|| {
        backend_for(dir.path()).search_with_options(
            "CtxNeedle",
            10,
            &RgOptions {
                mtime_after_ms: Some(far_future),
                ..Default::default()
            },
        )
    })
    .expect("search");
    assert!(dropped.is_empty(), "future floor drops every hit");
    let kept = with_caps(|| {
        backend_for(dir.path()).search_with_options(
            "CtxNeedle",
            10,
            &RgOptions {
                mtime_before_ms: Some(far_future),
                ..Default::default()
            },
        )
    })
    .expect("search");
    assert_eq!(kept.len(), 1, "future ceiling keeps the hit");
}

#[test]
fn rg_missing_root_is_a_named_diagnostic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("no-such-root");
    let result = with_caps(|| backend_for(&missing).search("CtxNeedle", 10));
    assert!(
        matches!(result, Err(RgError::MissingRoot { .. })),
        "missing root must diagnose, got {result:?}"
    );
}

fn hit(path: &str, line: u32, column: u32, before: &[&str]) -> RgHit {
    RgHit {
        path: path.to_string(),
        line,
        column,
        text: format!("match at {path}:{line}\n"),
        context_before: before.iter().map(|s| format!("{s}\n")).collect(),
        context_after: Vec::new(),
    }
}

#[test]
fn rg_hits_sort_by_path_then_line_then_column() {
    // Finding: parallel-rg arrival order flows through unsorted, so
    // byte-level output varies run to run (F3/S2/S3 stable=False).
    // Shuffled input must come out in (path, line, column) order with
    // context lines still glued to their own hit.
    let mut hits = vec![
        hit("web/svc_2.ts", 1, 30, &[]),
        hit("docs/overview.md", 7, 39, &["ctx"]),
        hit("web/svc_1.ts", 2, 30, &[]),
        hit("web/svc_1.ts", 1, 30, &["first"]),
        hit("web/svc_1.ts", 1, 12, &[]),
    ];
    sort_rg_hits(&mut hits);
    let order: Vec<(&str, u32, u32)> = hits
        .iter()
        .map(|h| (h.path.as_str(), h.line, h.column))
        .collect();
    assert_eq!(
        order,
        vec![
            ("docs/overview.md", 7, 39),
            ("web/svc_1.ts", 1, 12),
            ("web/svc_1.ts", 1, 30),
            ("web/svc_1.ts", 2, 30),
            ("web/svc_2.ts", 1, 30),
        ]
    );
    assert_eq!(hits[0].context_before, vec!["ctx\n".to_string()]);
    assert_eq!(hits[2].context_before, vec!["first\n".to_string()]);
}

#[test]
fn rg_explicit_limit_still_bounds_match_lines() {
    // Must-NOT-fire control: the sort must not defeat the explicit
    // `--limit` bound — the kill still fires on arrival count.
    let dir = tempfile::tempdir().expect("tempdir");
    for name in ["m1.rs", "m2.rs", "m3.rs"] {
        std::fs::write(
            dir.path().join(name),
            "let ManyNeedle = 1;\nlet ManyNeedle = 2;\n",
        )
        .expect("write");
    }
    let hits = with_caps(|| backend_for(dir.path()).search("ManyNeedle", 2)).expect("search");
    assert_eq!(hits.len(), 2, "explicit limit still bounds output");
}
