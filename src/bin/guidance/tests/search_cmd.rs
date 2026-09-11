//! Unit tests for the `search` command shell (pure plan/render layer plus
//! the hermetic `--rg` L0 path). Included via `#[path]` forwarder from
//! `src/search.rs`, per repo convention.

use super::*;
use guidance_core::query::rg_backend::RgHit;
use guidance_core::query::structure_enrich::EnrichedRgHit;
use guidance_core::search_types::SearchPlanRouteMode;

fn rg_opts() -> RgDisplayOptions {
    RgDisplayOptions::default()
}

#[test]
fn mode_defaults_to_fuse() {
    assert_eq!(
        resolve_mode(false, false, false, false).unwrap(),
        SearchMode::Fuse
    );
    assert_eq!(
        resolve_mode(false, false, false, true).unwrap(),
        SearchMode::Fuse
    );
    assert_eq!(
        resolve_mode(true, false, false, false).unwrap(),
        SearchMode::Fts
    );
    assert_eq!(
        resolve_mode(false, true, false, false).unwrap(),
        SearchMode::Vector
    );
    assert_eq!(
        resolve_mode(false, false, true, false).unwrap(),
        SearchMode::Rg
    );
}

#[test]
fn conflicting_mode_flags_are_rejected() {
    assert!(resolve_mode(true, true, false, false).is_err());
    assert!(resolve_mode(true, false, true, false).is_err());
    assert!(resolve_mode(false, true, true, true).is_err());
}

#[test]
fn symbol_type_parses_the_closed_6_set() {
    assert_eq!(parse_symbol_type("module").unwrap(), CodeSymbolType::Module);
    assert_eq!(parse_symbol_type("class").unwrap(), CodeSymbolType::Class);
    assert_eq!(
        parse_symbol_type("interface").unwrap(),
        CodeSymbolType::Interface
    );
    assert_eq!(
        parse_symbol_type("function").unwrap(),
        CodeSymbolType::Function
    );
    assert_eq!(parse_symbol_type("value").unwrap(), CodeSymbolType::Value);
    assert_eq!(parse_symbol_type("alias").unwrap(), CodeSymbolType::Alias);
    assert!(parse_symbol_type("namespace").is_err());
    assert!(parse_symbol_type("FUNCTION").is_err());
}

#[test]
fn limit_clamps_to_1_through_50() {
    assert_eq!(clamp_limit(0), 1);
    assert_eq!(clamp_limit(7), 7);
    assert_eq!(clamp_limit(50), 50);
    assert_eq!(clamp_limit(500), 50);
}

#[test]
fn fuse_plan_carries_trace_and_filters() {
    let plan = build_search_plan(
        "needle",
        SearchMode::Fuse,
        7,
        true,
        true,
        &["src/**".to_string()],
        &[CodeSymbolType::Function],
    );
    assert!(!plan.routes.is_empty());
    assert_eq!(plan.limit, Some(7));
    assert!(plan.trace);
    assert_eq!(plan.prefer_symbol, Some(true));
    assert_eq!(plan.globs, Some(vec!["src/**".to_string()]));
    assert_eq!(plan.symbol_types, Some(vec![CodeSymbolType::Function]));
}

#[test]
fn single_route_plans_select_exactly_one_mode() {
    let fts = build_search_plan("needle", SearchMode::Fts, 5, false, false, &[], &[]);
    assert_eq!(fts.routes.len(), 1);
    assert_eq!(fts.routes[0].mode, SearchPlanRouteMode::Fts);
    assert_eq!(fts.routes[0].query, "needle");
    let vector = build_search_plan("needle", SearchMode::Vector, 5, false, false, &[], &[]);
    assert_eq!(vector.routes.len(), 1);
    assert_eq!(vector.routes[0].mode, SearchPlanRouteMode::Vector);
}

#[test]
fn rg_hits_render_as_path_line_col() {
    let hits = vec![EnrichedRgHit {
        hit: RgHit {
            path: "src/a.ts".to_string(),
            line: 3,
            column: 7,
            text: "const Needle = 1;\n".to_string(),
            context_before: vec!["const Before = 0;\n".to_string()],
            context_after: Vec::new(),
        },
        symbol: Some("holder_fn".to_string()),
    }];
    assert_eq!(
        render_rg_hits(&hits, 10),
        "coverage: rg_exhaustive\nsrc/a.ts:3:7 [holder_fn]: const Needle = 1;\n  const Before = 0;\n"
    );
    assert_eq!(
        render_rg_hits(&hits, 1),
        "coverage: rg_truncated\nsrc/a.ts:3:7 [holder_fn]: const Needle = 1;\n  const Before = 0;\n"
    );
}

#[test]
fn missing_index_is_a_named_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("no-such.db");
    let error = run_search(
        "needle",
        dir.path().to_str().unwrap(),
        missing.to_str().unwrap(),
        Some(5),
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        &[],
        &[],
        &rg_opts(),
    )
    .expect_err("missing index must fail");
    assert!(error.contains("run `guidance index` first"), "{error}");
}

#[test]
fn invalid_symbol_type_is_a_named_error() {
    let result = run_search(
        "needle",
        ".",
        ".guidance.db",
        Some(5),
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        &[],
        &["namespace".to_string()],
        &rg_opts(),
    );
    assert!(result.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn rg_route_reports_context_and_symbols() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("b.rs"),
        "fn ctx_holder() {\n    let a = 1;\n    let CtxNeedle = a;\n    let z = 2;\n}\n",
    )
    .expect("write");
    let workspace = dir.path().to_str().unwrap().to_string();
    let unused_db = dir.path().join("unused.db");
    let unused_db = unused_db.to_str().unwrap().to_string();
    let options = RgDisplayOptions {
        before: 1,
        after: 1,
        mtime_after_ms: None,
        mtime_before_ms: None,
        enrich: true,
    };
    let rendered = run_search(
        "CtxNeedle",
        &workspace,
        &unused_db,
        Some(10),
        false,
        false,
        true,
        false,
        false,
        false,
        false,
        &[],
        &[],
        &options,
    )
    .expect("rg route");
    assert!(rendered.contains("coverage: rg_exhaustive"), "{rendered}");
    assert!(rendered.contains("b.rs:3:9 [ctx_holder]:"), "{rendered}");
    assert!(rendered.contains("let a = 1;"), "{rendered}");
    assert!(rendered.contains("let z = 2;"), "{rendered}");
}

#[tokio::test(flavor = "multi_thread")]
async fn rg_absent_limit_returns_every_match_line() {
    // Finding 2 (default line-budgeted early-kill dropped files): an
    // absent `--limit` on the L0 route must drain the sweep — 3 files ×
    // 2 match lines each, well past the old default budget of 10.
    let dir = tempfile::tempdir().expect("tempdir");
    for name in ["m1.rs", "m2.rs", "m3.rs"] {
        std::fs::write(
            dir.path().join(name),
            "let ManyNeedle = 1;\nlet ManyNeedle = 2;\n",
        )
        .expect("write");
    }
    let workspace = dir.path().to_str().unwrap().to_string();
    let unused_db = dir.path().join("unused.db");
    let unused_db = unused_db.to_str().unwrap().to_string();
    let rendered = run_search(
        "ManyNeedle",
        &workspace,
        &unused_db,
        None,
        false,
        false,
        true,
        false,
        false,
        false,
        false,
        &[],
        &[],
        &rg_opts(),
    )
    .expect("rg route");
    for name in ["m1.rs", "m2.rs", "m3.rs"] {
        assert!(rendered.contains(&format!("{name}:1:")), "{rendered}");
        assert!(rendered.contains(&format!("{name}:2:")), "{rendered}");
    }
    assert!(rendered.contains("coverage: rg_exhaustive"), "{rendered}");
}

#[tokio::test(flavor = "multi_thread")]
async fn rg_explicit_limit_still_truncates() {
    // Must-NOT-fire control: an explicit `--limit` keeps the bounded
    // contract — the bound is preserved, only the default changes.
    let dir = tempfile::tempdir().expect("tempdir");
    for name in ["m1.rs", "m2.rs", "m3.rs"] {
        std::fs::write(
            dir.path().join(name),
            "let ManyNeedle = 1;\nlet ManyNeedle = 2;\n",
        )
        .expect("write");
    }
    let workspace = dir.path().to_str().unwrap().to_string();
    let unused_db = dir.path().join("unused.db");
    let unused_db = unused_db.to_str().unwrap().to_string();
    let rendered = run_search(
        "ManyNeedle",
        &workspace,
        &unused_db,
        Some(2),
        false,
        false,
        true,
        false,
        false,
        false,
        false,
        &[],
        &[],
        &rg_opts(),
    )
    .expect("rg route");
    assert!(rendered.contains("coverage: rg_truncated"), "{rendered}");
    let matches = rendered
        .lines()
        .filter(|line| line.contains("ManyNeedle ="))
        .count();
    assert_eq!(matches, 2, "{rendered}");
}

#[tokio::test(flavor = "multi_thread")]
async fn rg_route_searches_without_an_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.rs"), "fn needle_fn() {}\n").expect("write");
    let workspace = dir.path().to_str().unwrap().to_string();
    let unused_db = dir.path().join("unused.db");
    let unused_db = unused_db.to_str().unwrap().to_string();
    let rendered = run_search(
        "needle_fn",
        &workspace,
        &unused_db,
        Some(10),
        false,
        false,
        true,
        false,
        false,
        false,
        false,
        &[],
        &[],
        &rg_opts(),
    )
    .expect("rg route");
    assert!(rendered.contains("a.rs:1:"), "{rendered}");
}

#[tokio::test(flavor = "multi_thread")]
async fn fuse_search_output_is_byte_stable() {
    // Byte-parity pin for the Fuse recall assembly: the rendered output
    // on this hermetic fixture is recorded verbatim; unifying the recall
    // assembly must reproduce it byte-for-byte (golden-string, not fuzzy).
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path().to_str().unwrap().to_string();
    let db_path = dir.path().join("parity.db");
    let db = search_vector::GuidanceDb::open(&db_path).expect("open db");
    for (id, rel, text) in [
        ("file-a", "a.rs", "the alpha needle sleeps here\n"),
        ("file-b", "b.rs", "the beta stone rests here\n"),
    ] {
        guidance_core::query::ingest::ingest_text_file(
            &db,
            &guidance_core::search_types::FileInfo {
                id: id.to_string(),
                absolute_path: dir.path().join(rel).to_string_lossy().into_owned(),
                relative_path: rel.to_string(),
                root_path: workspace.clone(),
                size_bytes: text.len() as u64,
                last_modified_time: 100,
                content_hash: None,
                kind: Some(guidance_core::search_types::FileKind::Code),
                format: "rust".to_string(),
                index_status: None,
            },
            text,
            None,
            None,
        )
        .expect("ingest");
    }
    drop(db);
    let rendered = run_search(
        "needle",
        &workspace,
        db_path.to_str().unwrap(),
        Some(10),
        false,
        false,
        false,
        true,
        false,
        false,
        false,
        &[],
        &[],
        &rg_opts(),
    )
    .expect("fuse route");
    assert_eq!(
        rendered,
        "## Prose\n\n*Source: a.rs:0*\n\nthe alpha needle sleeps here\n\n\n---\n\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn embedder_builder_returns_none_without_backend() {
    // No config in a fresh workspace: no embedder, no dial, no panic.
    // (Compile-red first: the builder does not exist yet.)
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = guidance_core::config::load_config(dir.path()).expect("default config");
    assert!(crate::embed::embedder_from_config(&cfg).is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn vector_mode_declines_without_embedder() {
    // `--vector` documents "declines without an embedder" — today it
    // silently answers from the lemma route instead. Must be a named
    // error naming the missing backend.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.rs"), "fn needle_fn() {}\n").expect("write");
    let workspace = dir.path().to_str().unwrap().to_string();
    let unused_db = dir.path().join("unused.db");
    let unused_db = unused_db.to_str().unwrap().to_string();
    let error = run_search(
        "needle_fn",
        &workspace,
        &unused_db,
        None,
        false,
        true,
        false,
        false,
        false,
        false,
        false,
        &[],
        &[],
        &rg_opts(),
    )
    .expect_err("vector mode without a backend must decline");
    assert!(
        error.contains("embed"),
        "decline must name the missing backend, got: {error}"
    );
}

use guidance_core::graph_index::{ContextDirection, ContextEdge, ContextFamily};
use guidance_core::search_types::{
    Entity, EntityMetadata, FileInfo, SearchHit, SearchHitEvidence, SearchMatchedBy,
    FragmentContent, FragmentSpan,
};

fn hit(id: &str, abs: &str, rel: &str, symbol: Option<&str>, score: f64) -> SearchHit {
    let metadata = symbol.map(|name| {
        EntityMetadata::Code {
            symbol_type: guidance_core::search_types::CodeSymbolType::Function,
            symbol_name: Some(name.to_string()),
            scope: None,
            node_type: None,
            signature: None,
            doc: None,
            modifiers: Vec::new(),
        }
    });
    SearchHit {
        entity: Entity {
            id: id.to_string(),
            file_id: abs.to_string(),
            range: FragmentSpan::File,
            content: FragmentContent::Text { text: String::new() },
            metadata: None,
        },
        file: FileInfo {
            id: abs.to_string(),
            absolute_path: abs.to_string(),
            relative_path: rel.to_string(),
            root_path: "/repo".to_string(),
            size_bytes: 10,
            last_modified_time: 10,
            content_hash: None,
            kind: None,
            format: "rust".to_string(),
            index_status: None,
        },
        evidence: vec![SearchHitEvidence {
            range: FragmentSpan::File,
            content: FragmentContent::Text { text: String::new() },
            metadata,
            is_entity: true,
            path: guidance_core::search_types::RecallPath::Fts,
            route_id: None,
            query: None,
            rank: None,
            score: None,
            forced: None,
        }],
        rank: 1,
        score: guidance_core::search_types::RrfScore::new(score),
        matched_by: SearchMatchedBy::Fts,
        trace: None,
    }
}

#[test]
fn anchor_files_dedupes_and_caps() {
    let hits = vec![
        hit("a::x", "/repo/a.rs", "a.rs", Some("x"), 0.05),
        hit("a::y", "/repo/a.rs", "a.rs", Some("y"), 0.04),
        hit("b::z", "/repo/b.rs", "b.rs", Some("z"), 0.03),
        hit("c::w", "/repo/c.rs", "c.rs", Some("w"), 0.02),
    ];
    assert_eq!(anchor_files(&hits, 2), vec!["/repo/a.rs".to_string(), "/repo/b.rs".to_string()]);
    assert_eq!(
        anchor_files(&hits, 10).len(),
        3,
        "distinct files only, order preserved"
    );
    assert!(anchor_files(&[], 3).is_empty());
}

fn context_edge(file: &str, anchor: &str) -> ContextEdge {
    ContextEdge {
        file: file.to_string(),
        anchor: anchor.to_string(),
        direction: ContextDirection::Dependent,
        family: ContextFamily::Import,
        via: "mod:base".to_string(),
    }
}

#[test]
fn explain_rows_carry_hits_only() {
    // The table never carries expansion: hits in, rows out, rank order.
    let hits = vec![
        hit("a::x", "/repo/a.rs", "a.rs", Some("x"), 0.05),
        hit("b::z", "/repo/b.rs", "b.rs", Some("z"), 0.03),
    ];
    let rows = build_explain_rows(&hits);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "x");
    assert_eq!(rows[0].source, "a.rs");
    assert!((rows[0].score - 0.05).abs() < 1e-9);
    assert_eq!(rows[1].name, "z");
}

#[test]
fn context_lines_carry_provenance_and_cap() {
    let edges = vec![
        context_edge("/repo/b.rs", "/repo/a.rs"),
        context_edge("/repo/c.rs", "/repo/a.rs"),
    ];
    let lines = build_context_lines(&edges, 10, "/repo");
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("b.rs"), "{lines:?}");
    assert!(lines[0].contains("dependent of"), "{lines:?}");
    assert!(lines[0].contains("a.rs"), "{lines:?}");
    assert!(lines[0].contains("mod:base"), "{lines:?}");
    // The cap bounds the flood axis deterministically (sorted order).
    let capped = build_context_lines(&edges, 1, "/repo");
    assert_eq!(capped, vec![lines[0].clone()]);
}

#[test]
fn explain_table_section_ignores_expansion() {
    // Additivity proof: the table section is byte-identical with and
    // without context — expansion can only append the section below.
    let rows = vec![ExplainRow {
        name: "x".to_string(),
        source: "a.rs".to_string(),
        score: 0.05,
    }];
    let without = render_explain_table("q", &rows, &[]);
    let rendered = render_explain_table("q", &rows, &["- b.rs — dependent of a.rs via import \"m\"".to_string()]);
    assert!(rendered.starts_with(&without));
    assert!(rendered.contains("### Context\n"));
    assert!(rendered.starts_with("## Explain: q\n\n"));
    assert!(rendered.contains("| Name | Source | Score |"));
    assert!(rendered.contains("| x | a.rs | 0.05 |"));
    // Not-found compat: the empty state never changes across the port.
    assert_eq!(
        render_explain_table("q", &[], &[]),
        "## Explain: q\n\nNo results found.\n"
    );
}

#[test]
fn explain_rows_fall_back_to_basename_for_file_hits() {
    // File-level fragments have no symbol metadata and carry the file
    // path as entity id: the Name column shows the basename, never the
    // absolute path.
    let hits = vec![hit("/repo/a.rs", "/repo/a.rs", "a.rs", None, 0.04)];
    let rows = build_explain_rows(&hits);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "a.rs");
    assert_eq!(rows[0].source, "a.rs");
}

#[test]
fn context_helper_degrades_to_empty_without_anchors() {
    // No hits means no anchors: empty lines, never an error — the
    // caller renders hits only.
    let db = GuidanceDb::open_in_memory().expect("memory db");
    let lines = context_lines_for_hits(&[], &db, "/repo").expect("empty");
    assert!(lines.is_empty());
}

#[test]
fn context_lines_merge_families_per_file() {
    use guidance_core::graph_index::{ContextDirection, ContextFamily};
    let edges = vec![
        ContextEdge {
            file: "/repo/b.rs".to_string(),
            anchor: "/repo/a.rs".to_string(),
            direction: ContextDirection::Dependency,
            family: ContextFamily::Call,
            via: "b".to_string(),
        },
        ContextEdge {
            file: "/repo/b.rs".to_string(),
            anchor: "/repo/a.rs".to_string(),
            direction: ContextDirection::Dependency,
            family: ContextFamily::Import,
            via: "mod:b".to_string(),
        },
    ];
    let lines = build_context_lines(&edges, 10, "/repo");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("b.rs"), "{lines:?}");
    assert!(lines[0].contains("import \"mod:b\""), "{lines:?}");
    assert!(lines[0].contains("call \"b\""), "{lines:?}");
}
