//! Ported `structure-enrichment.test.mjs` cases: symbol context attaches
//! from the tree-sitter adapters, unparsable files keep raw hits with
//! per-file diagnostics, the 100-file budget holds, and enrichment is
//! capability-gated. Included via `#[path]` forwarder from
//! `src/query/structure_enrich.rs`, per repo convention.

use super::*;
use crate::query::rg_backend::{RgHit, RgOptions};
use fluent_wvr::capability::{CapabilitySet, FsCapability, CURRENT_CAPS};

fn with_caps<T>(run: impl FnOnce() -> T) -> T {
    let caps = CapabilitySet::new().with(FsCapability::new());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    runtime.block_on(CURRENT_CAPS.scope(caps, async move { run() }))
}

fn hit(path: &str, line: u32) -> RgHit {
    RgHit {
        path: path.to_string(),
        line,
        column: 1,
        text: "needle\n".to_string(),
        context_before: Vec::new(),
        context_after: Vec::new(),
    }
}

fn write_rust_file(root: &std::path::Path) {
    std::fs::write(
        root.join("lib.rs"),
        "fn outer_fn() {\n    let first = 1;\n    let Needle = first;\n}\n",
    )
    .expect("write");
}

#[test]
fn enclosing_symbol_attaches_to_hits() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_rust_file(dir.path());
    let (enriched, diagnostics) = with_caps(|| enrich_hits(dir.path(), &[hit("lib.rs", 3)]));
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(enriched.len(), 1);
    assert_eq!(enriched[0].symbol.as_deref(), Some("outer_fn"));
}

#[test]
fn lines_above_all_members_keep_raw_hits() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("lib.rs"), "// header\nfn outer_fn() {}\n").expect("write");
    let (enriched, _) = with_caps(|| enrich_hits(dir.path(), &[hit("lib.rs", 1)]));
    assert_eq!(enriched.len(), 1);
    assert_eq!(enriched[0].symbol, None);
}

#[test]
fn unsupported_languages_keep_raw_hits_with_diagnostics() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("notes.txt"), "plain Needle text\n").expect("write");
    let (enriched, diagnostics) = with_caps(|| enrich_hits(dir.path(), &[hit("notes.txt", 1)]));
    assert_eq!(enriched.len(), 1);
    assert_eq!(enriched[0].symbol, None);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].starts_with("notes.txt: "), "{diagnostics:?}");
}

#[test]
fn missing_files_keep_raw_hits_with_diagnostics() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (enriched, diagnostics) = with_caps(|| enrich_hits(dir.path(), &[hit("gone.rs", 1)]));
    assert_eq!(enriched[0].symbol, None);
    assert_eq!(diagnostics.len(), 1);
}

#[test]
fn one_parse_serves_many_hits_in_a_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_rust_file(dir.path());
    let (enriched, diagnostics) =
        with_caps(|| enrich_hits(dir.path(), &[hit("lib.rs", 2), hit("lib.rs", 3)]));
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert!(enriched
        .iter()
        .all(|item| item.symbol.as_deref() == Some("outer_fn")));
}

#[test]
fn file_budget_is_100() {
    assert_eq!(STRUCTURE_ENRICH_FILE_LIMIT, 100);
}

#[test]
fn enrichment_fails_closed_without_capability() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_rust_file(dir.path());
    // No CURRENT_CAPS scope → raw hits plus a skip diagnostic.
    let (enriched, diagnostics) = enrich_hits(dir.path(), &[hit("lib.rs", 3)]);
    assert_eq!(enriched[0].symbol, None);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].contains("FsCapability"), "{diagnostics:?}");
}

#[test]
fn enrichment_composes_with_rg_options_and_hard_excludes() {
    use crate::query::rg_backend::RgBackend;
    let dir = tempfile::tempdir().expect("tempdir");
    write_rust_file(dir.path());
    std::fs::create_dir_all(dir.path().join(".git")).expect("git");
    std::fs::write(dir.path().join(".git").join("x.rs"), "let Needle = 1;\n").expect("write");
    let backend = RgBackend {
        root: dir.path().to_path_buf(),
        exe_override: None,
        path_override: None,
    };
    let options = RgOptions::default();
    let (enriched, _) = with_caps(|| {
        let hits = backend
            .search_with_options("Needle", 10, &options)
            .expect("search");
        assert!(hits.iter().all(|hit| !hit.path.starts_with(".git")));
        enrich_hits(dir.path(), &hits)
    });
    assert_eq!(enriched.len(), 1);
    assert_eq!(enriched[0].symbol.as_deref(), Some("outer_fn"));
}
