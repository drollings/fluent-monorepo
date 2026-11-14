//! Unit tests for the `search` command shell (pure plan/render layer plus
//! the hermetic `--rg` L0 path). Included via `#[path]` forwarder from
//! `src/search.rs`, per repo convention.

use super::*;
use guidance_core::query::rg_backend::RgHit;
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
    let hits = vec![RgHit {
        path: "src/a.ts".to_string(),
        line: 3,
        column: 7,
        text: "const Needle = 1;\n".to_string(),
        context_before: vec!["const Before = 0;\n".to_string()],
        context_after: Vec::new(),
    }];
    assert_eq!(
        render_rg_hits(&hits, 10),
        "coverage: rg_exhaustive\nsrc/a.ts:3:7: const Needle = 1;\n  const Before = 0;\n"
    );
    assert_eq!(
        render_rg_hits(&hits, 1),
        "coverage: rg_truncated\nsrc/a.ts:3:7: const Needle = 1;\n  const Before = 0;\n"
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
    assert!(rendered.contains("b.rs:3:9:"), "{rendered}");
    assert!(!rendered.contains("[ctx_holder]"), "{rendered}");
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
        "## Prose\n\n*Source: a.rs:0*\n\nthe alpha needle sleeps here\n\n\n---\n\n\n### Citations\n- unverified\n"
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

use guidance_core::graph_index::{
    ContextDirection, ContextEdge, ContextFamily, EdgeTier, ResolutionMethod,
};
use guidance_core::search_types::{
    Entity, EntityMetadata, FileInfo, FragmentContent, FragmentSpan, SearchHit, SearchHitEvidence,
    SearchMatchedBy,
};

fn hit(id: &str, abs: &str, rel: &str, symbol: Option<&str>, score: f64) -> SearchHit {
    let metadata = symbol.map(|name| EntityMetadata::Code {
        symbol_type: guidance_core::search_types::CodeSymbolType::Function,
        symbol_name: Some(name.to_string()),
        scope: None,
        node_type: None,
        signature: None,
        doc: None,
        modifiers: Vec::new(),
    });
    SearchHit {
        entity: Entity {
            id: id.to_string(),
            file_id: abs.to_string(),
            range: FragmentSpan::File,
            content: FragmentContent::Text {
                text: String::new(),
            },
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
            content: FragmentContent::Text {
                text: String::new(),
            },
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
    assert_eq!(
        anchor_files(&hits, 2),
        vec!["/repo/a.rs".to_string(), "/repo/b.rs".to_string()]
    );
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
        resolution: ResolutionMethod::Unique,
        tier: EdgeTier::Syntactic,
    }
}

#[test]
fn explain_rows_carry_hits_only() {
    // The table never carries expansion: hits in, rows out, rank order.
    let hits = vec![
        hit("a::x", "/repo/a.rs", "a.rs", Some("x"), 0.05),
        hit("b::z", "/repo/b.rs", "b.rs", Some("z"), 0.03),
    ];
    let rows = build_explain_rows(&hits, &[]);
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
fn render_full_has_no_table_and_orders_sections() {
    // The table is gone: sections run Answer, From the docs, Top
    // matches, Citations, footer — and scores stay available on the
    // hits for threshold decisions even though they never render.
    let rows = vec![ExplainRow {
        name: "alpha".to_string(),
        source: "a.rs".to_string(),
        score: 0.05,
        cite: "a.rs:3-5 @ b3:abc".to_string(),
    }];
    assert_eq!(rows[0].score, 0.05);
    let rendered = render_explain_full(
        "q",
        &rows,
        &[(
            "alpha".to_string(),
            "a.rs".to_string(),
            3u32,
            5u32,
            "pub fn alpha() {}".to_string(),
        )],
        &["- /repo/a.rs : lines 3-5".to_string()],
        &[("doc/notes.md#1".to_string(), "Adds one.".to_string())],
        Some("It adds one."),
        &["guidance explain \"callers of alpha\"".to_string()],
        &[],
        &[],
    );
    for section in [
        "### Answer",
        "### From the docs",
        "### Top matches",
        "### Citations",
        "If you need details, try:",
    ] {
        assert!(rendered.contains(section), "{section}:\n{rendered}");
    }
    assert!(
        !rendered.contains("| Name | Source | Score | Cite |"),
        "{rendered}"
    );
    assert!(!rendered.contains("### Context"), "{rendered}");
}

#[test]
fn explain_rows_fall_back_to_basename_for_file_hits() {
    // File-level fragments have no symbol metadata and carry the file
    // path as entity id: the Name column shows the basename, never the
    // absolute path.
    let hits = vec![hit("/repo/a.rs", "/repo/a.rs", "a.rs", None, 0.04)];
    let rows = build_explain_rows(&hits, &[]);
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
            resolution: ResolutionMethod::Unique,
            tier: EdgeTier::Syntactic,
        },
        ContextEdge {
            file: "/repo/b.rs".to_string(),
            anchor: "/repo/a.rs".to_string(),
            direction: ContextDirection::Dependency,
            family: ContextFamily::Import,
            via: "mod:b".to_string(),
            resolution: ResolutionMethod::Unique,
            tier: EdgeTier::Syntactic,
        },
    ];
    let lines = build_context_lines(&edges, 10, "/repo");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("b.rs"), "{lines:?}");
    assert!(lines[0].contains("import \"mod:b\""), "{lines:?}");
    assert!(lines[0].contains("call \"b\""), "{lines:?}");
}

#[test]
fn context_lines_name_ambiguity_and_silence_certainty() {
    use guidance_core::graph_index::ResolutionMethod as RM;
    let edges = vec![
        ContextEdge {
            file: "/repo/caller.rs".to_string(),
            anchor: "/repo/a.rs".to_string(),
            direction: ContextDirection::Dependent,
            family: ContextFamily::Call,
            via: "dup".to_string(),
            resolution: RM::Homonym { count: 3 },
            tier: EdgeTier::Syntactic,
        },
        ContextEdge {
            file: "/repo/lost.rs".to_string(),
            anchor: "/repo/a.rs".to_string(),
            direction: ContextDirection::Dependent,
            family: ContextFamily::Call,
            via: "ghost".to_string(),
            resolution: RM::Unresolved,
            tier: EdgeTier::Syntactic,
        },
    ];
    let lines = build_context_lines(&edges, 10, "/repo");
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0].contains("call \"dup\" (ambiguous: 3 candidates)"),
        "{lines:?}"
    );
    assert!(
        lines[1].contains("call \"ghost\" (unresolved)"),
        "{lines:?}"
    );
    // Certain lines render exactly as before (byte-identical).
    let certain = build_context_lines(&[context_edge("/repo/b.rs", "/repo/a.rs")], 10, "/repo");
    assert!(!certain[0].contains('('), "{certain:?}");
}

#[test]
fn cited_rows_carry_verified_cites() {
    // Every row names its cited range with a hash over live bytes;
    // unreadable files degrade to `unverified`, never a bare path.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("a.rs");
    std::fs::write(&file, "fn a() {}\nfn b() {}\n").expect("write");
    let abs = file.to_string_lossy().into_owned();
    let hits = vec![crate::search::hit_for_test(&abs, "a.rs", 1, 2)];
    let cites = crate::search::cite_hits(&hits, dir.path().to_str().unwrap());
    assert_eq!(cites.len(), 1);
    let expected = common_core::hash::blake3_hex("fn a() {}\nfn b() {}\n".as_bytes());
    assert_eq!(
        cites[0],
        format!("a.rs:1-2 @ b3:{expected}"),
        "{}",
        cites[0]
    );
    let rows = crate::search::build_explain_rows(&hits, &cites);
    assert_eq!(rows[0].cite, cites[0]);
    // Scores stay on the rows for threshold decisions.
    assert_eq!(rows[0].score, 0.05);
    // Missing file: unverified, never invented.
    let missing = vec![crate::search::hit_for_test("/nope.rs", "nope.rs", 1, 1)];
    let cites = crate::search::cite_hits(&missing, dir.path().to_str().unwrap());
    assert_eq!(cites, vec!["unverified".to_string()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn item_chunk_citations_verify_item_ranges() {
    // Item chunks committed through the unified writer cite item ranges,
    // hash-verified against live bytes — never whole-file spans.
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path().to_str().unwrap().to_string();
    let text = "/// Adds one.\npub fn alpha() -> i32 {\n    1\n}\n";
    let abs = dir.path().join("a.rs").to_string_lossy().into_owned();
    std::fs::write(&abs, text).expect("write");
    let db_path = dir.path().join("cite.db");
    let db = search_vector::GuidanceDb::open(&db_path).expect("open db");
    let file = search_vector::db::FileRecord {
        id: abs.clone(),
        absolute_path: abs.clone(),
        relative_path: "a.rs".to_string(),
        root_path: workspace.clone(),
        size_bytes: text.len() as u64,
        last_modified_time: 100,
        kind: Some("code".to_string()),
        format: "rust".to_string(),
        content_hash: None,
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    };
    let fragment = guidance_core::extractor::PreparedFragment {
        id: "frag-a".to_string(),
        content_text: "pub fn alpha() -> i32 {\n    1\n}".to_string(),
        embedding_text: None,
        range: guidance_core::extractor::FragmentRange {
            start_line: 2,
            end_line: 4,
            start_offset: 15,
            end_offset: text.len(),
        },
        group: None,
        metadata: Some(guidance_core::extractor::FragmentMetadata::Code {
            symbol_type: guidance_core::search_types::CodeSymbolType::Function,
            symbol_name: Some("alpha".to_string()),
            scope: None,
            node_type: "function_item".to_string(),
            signature: Some("pub fn alpha() -> i32".to_string()),
            doc: Some("Adds one.".to_string()),
            modifiers: Vec::new(),
        }),
        calls: Vec::new(),
    };
    let block = guidance_core::ast_parser::CommentBlock {
        node_key: "alpha".to_string(),
        node_kind: "function_item".to_string(),
        name: "alpha".to_string(),
        scope: String::new(),
        start_byte: 0,
        end_byte: 14,
        start_line: 1,
        end_line: 1,
        text: "Adds one.".to_string(),
        comment_hash: common_core::hash::blake3_hex("Adds one.".as_bytes()),
    };
    guidance_core::query::writer::FragmentWriter::write_items(
        &db,
        &file,
        guidance_core::search_types::FileKind::Code,
        std::slice::from_ref(&fragment),
        std::slice::from_ref(&block),
        &[],
        None,
        guidance_core::query::writer::VectorPolicy::Optional,
    )
    .expect("commit");
    drop(db);
    let rendered = run_search(
        "adds",
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
    let expected = common_core::hash::blake3_hex("pub fn alpha() -> i32 {\n    1\n}\n".as_bytes());
    assert!(
        rendered.contains(&format!("a.rs:2-4 @ b3:{expected}")),
        "item range must cite with a live-byte hash: {rendered}"
    );
}

fn code_hit(abs: &str, rel: &str, name: &str, start: u32, end: u32) -> SearchHit {
    use guidance_core::search_types::{
        Entity, EntityMetadata, FileInfo, FragmentContent, FragmentSpan, RecallPath,
        SearchHitEvidence, SearchMatchedBy,
    };
    let meta = || {
        Some(EntityMetadata::Code {
            symbol_type: guidance_core::search_types::CodeSymbolType::Function,
            symbol_name: Some(name.to_string()),
            scope: None,
            node_type: Some("function_item".to_string()),
            signature: Some(format!("pub fn {name}()")),
            doc: None,
            modifiers: Vec::new(),
        })
    };
    SearchHit {
        entity: Entity {
            id: abs.to_string(),
            file_id: abs.to_string(),
            range: FragmentSpan::Text {
                start_line: start,
                end_line: end,
                start_offset: 0,
                end_offset: 0,
            },
            content: FragmentContent::Text {
                text: String::new(),
            },
            metadata: meta(),
        },
        file: FileInfo {
            id: abs.to_string(),
            absolute_path: abs.to_string(),
            relative_path: rel.to_string(),
            root_path: String::new(),
            size_bytes: 0,
            last_modified_time: 0,
            content_hash: None,
            kind: None,
            format: "rust".to_string(),
            index_status: None,
        },
        evidence: vec![SearchHitEvidence {
            range: FragmentSpan::Text {
                start_line: start,
                end_line: end,
                start_offset: 0,
                end_offset: 0,
            },
            content: FragmentContent::Text {
                text: String::new(),
            },
            metadata: meta(),
            is_entity: true,
            path: RecallPath::Fts,
            route_id: None,
            query: None,
            rank: None,
            score: None,
            forced: None,
        }],
        rank: 1,
        score: guidance_core::search_types::RrfScore::new(0.05),
        matched_by: SearchMatchedBy::Fts,
        trace: None,
    }
}

#[test]
fn snippet_caps_to_max_lines_with_live_bounds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let body = (0..100)
        .map(|i| format!("    let x{i} = {i};"))
        .collect::<Vec<_>>()
        .join("\n");
    let text = format!("pub fn big() -> u32 {{\n{body}\n}}\n");
    let abs = dir.path().join("a.rs");
    std::fs::write(&abs, &text).expect("write");
    let abs_str = abs.to_string_lossy().into_owned();
    let hit = code_hit(&abs_str, "a.rs", "big", 1, 102);
    let (snippet, start, end) = snippet_for_hit(&hit, dir.path()).expect("snippet");
    assert!(
        snippet.lines().count() <= EXPLAIN_SNIPPET_MAX_LINES,
        "{}",
        snippet.lines().count()
    );
    assert!(snippet.contains("pub fn big"), "{snippet:?}");
    // Bounds are live tree-sitter positions, not the stale stored range.
    assert_eq!((start, end), (1, 102));
}

#[test]
fn snippet_falls_back_to_evidence_range_without_metadata() {
    let dir = tempfile::tempdir().expect("tempdir");
    let abs = dir.path().join("a.rs");
    std::fs::write(&abs, "fn a() {}\nfn b() {}\n").expect("write");
    let abs_str = abs.to_string_lossy().into_owned();
    let hit = crate::search::hit_for_test(&abs_str, "a.rs", 1, 2);
    let (snippet, start, end) = snippet_for_hit(&hit, dir.path()).expect("snippet");
    assert_eq!(snippet, "fn a() {}\nfn b() {}");
    assert_eq!((start, end), (1, 2));
}

#[test]
fn citations_resolve_live_lines_after_edit() {
    // Stored range says lines 1-3; an edit pushes the symbol down. The
    // citation must be the live position, never the stored one.
    let dir = tempfile::tempdir().expect("tempdir");
    let abs = dir.path().join("a.rs");
    std::fs::write(&abs, "/// Adds one.\npub fn alpha() -> i32 {\n    1\n}\n").expect("write");
    let abs_str = abs.to_string_lossy().into_owned();
    std::fs::write(
        &abs,
        "// header\n// more\n/// Adds one.\npub fn alpha() -> i32 {\n    1\n}\n",
    )
    .expect("edit");
    let hit = code_hit(&abs_str, "a.rs", "alpha", 1, 3);
    let line = cite_live(&hit, dir.path());
    assert_eq!(line, format!("- {} : lines 4-6", abs_str), "{line}");
}

#[test]
fn moved_symbol_marks_unresolvable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let abs = dir.path().join("a.rs");
    std::fs::write(&abs, "pub fn other() {}\n").expect("write");
    let abs_str = abs.to_string_lossy().into_owned();
    let hit = code_hit(&abs_str, "a.rs", "alpha", 1, 3);
    assert_eq!(
        cite_live(&hit, dir.path()),
        format!("- {} : moved or removed since index", abs_str)
    );
}

#[test]
fn docs_ranking_prefers_query_overlap() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("alpha.md"),
        "# Alpha\n\nThe fee calculator adds one to every total.\n\nUnrelated trailing words here.\n",
    )
    .expect("write");
    std::fs::write(
        dir.path().join("beta.md"),
        "# Beta\n\nCompletely different topic about nothing relevant.\n",
    )
    .expect("write");
    let ranked = rank_doc_paragraphs(
        "fee calculator total",
        Some(dir.path()),
        dir.path(),
        5,
        false,
    );
    assert!(!ranked.is_empty(), "query terms must match a paragraph");
    assert_eq!(ranked[0].0, "alpha.md#1");
    assert!(ranked[0].1.contains("fee calculator"), "{:?}", ranked[0]);
}

#[test]
fn answer_prompt_fences_evidence_and_demands_cites() {
    let ctx = vec![(
        "alpha".to_string(),
        "a.rs".to_string(),
        3u32,
        5u32,
        "pub fn alpha() {}".to_string(),
    )];
    let messages = answer_messages("what does alpha do", &ctx, &[]);
    let combined: String = messages.iter().map(|m| m.content.as_str()).collect();
    assert!(combined.contains("pub fn alpha"), "{combined}");
    assert!(combined.contains("a.rs:3-5"), "{combined}");
    assert!(combined.contains("Cite every claim"), "{combined}");
}

#[test]
fn citations_dedupe_dual_rows_but_keep_unverified() {
    // Dual rows for one file cite once (canonical identity); bare
    // `unverified` lines never collapse (no file identity to compare).
    let dir = tempfile::tempdir().expect("tempdir");
    let abs = dir.path().join("a.rs");
    std::fs::write(&abs, "pub fn alpha() -> i32 {\n    1\n}\n").expect("write");
    let abs_str = abs.to_string_lossy().into_owned();
    let dotted = format!("{}/./a.rs", dir.path().to_string_lossy());
    let a = code_hit(&abs_str, "a.rs", "alpha", 1, 3);
    let mut b = code_hit(&dotted, "./a.rs", "alpha", 1, 3);
    b.rank = 2;
    let missing = crate::search::hit_for_test("/nope.rs", "nope.rs", 1, 1);
    let other_missing = crate::search::hit_for_test("/gone.rs", "gone.rs", 1, 1);
    let cites = cite_hits_live(&[a, b, missing, other_missing], dir.path());
    assert_eq!(
        cites,
        vec![
            format!("- {abs_str} : lines 1-3"),
            "- /nope.rs : unverified".to_string(),
            "- /gone.rs : unverified".to_string(),
        ],
        "{cites:?}"
    );
}

#[test]
fn duplicate_file_range_hits_render_once_as_single_snippet() {
    // Same file + same range twice (dual relative forms) must not burn
    // two snippet slots on byte-identical content.
    let dir = tempfile::tempdir().expect("tempdir");
    let abs = dir.path().join("a.rs");
    std::fs::write(&abs, "/// Adds one.\npub fn alpha() -> i32 {\n    1\n}\n").expect("write");
    let abs_str = abs.to_string_lossy().into_owned();
    let a = code_hit(&abs_str, "a.rs", "alpha", 1, 3);
    let mut b = code_hit(&abs_str, "./a.rs", "alpha", 1, 3);
    b.rank = 2;
    // Third row indexed with a non-canonical absolute path: same file,
    // same range — canonical identity must still dedupe it.
    let dotted = format!("{}/./a.rs", dir.path().to_string_lossy());
    let mut c = code_hit(&dotted, "./a.rs", "alpha", 1, 3);
    c.rank = 3;
    let hits = vec![a, b, c];
    let cites = crate::search::cite_hits(&hits, dir.path().to_str().unwrap());
    let rows = crate::search::build_explain_rows(&hits, &cites);
    let snippets = top_snippets(&rows, &hits, dir.path());
    assert_eq!(snippets.len(), 1, "{snippets:?}");
}

struct FakeBackend {
    text: String,
    fail: bool,
}

impl fluent_llm::client::ChatBackend for FakeBackend {
    fn chat_complete(
        &self,
        _messages: &[fluent_llm::protocol::ChatMessage],
    ) -> Result<String, fluent_llm::protocol::LlmError> {
        if self.fail {
            return Err(fluent_llm::protocol::LlmError::Http("boom".to_string()));
        }
        Ok(self.text.clone())
    }
}

#[test]
fn docs_judge_keeps_passing_scores() {
    // Each paragraph passes through the decision model; only items at
    // or above the floor survive, best score first.
    assert_eq!(EXPLAIN_DOC_MIN_SCORE, 60);
    let backend = FakeBackend {
        text: r#"{"1": 90, "2": 10}"#.to_string(),
        fail: false,
    };
    let docs = vec![
        ("a.md#1".to_string(), "fee calculator".to_string()),
        ("b.md#1".to_string(), "unrelated weather".to_string()),
    ];
    let kept = judge_doc_paragraphs("fee calculator", &docs, &backend);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].0, "a.md#1");
}

#[test]
fn docs_judge_malformed_or_failed_omits_all() {
    // No parseable scores, no section: strict JSON or nothing.
    let malformed = FakeBackend {
        text: "very relevant, trust me".to_string(),
        fail: false,
    };
    let docs = vec![("a.md#1".to_string(), "fee calculator".to_string())];
    assert!(judge_doc_paragraphs("q", &docs, &malformed).is_empty());
    let failing = FakeBackend {
        text: String::new(),
        fail: true,
    };
    assert!(judge_doc_paragraphs("q", &docs, &failing).is_empty());
}

#[test]
fn docs_judge_unwraps_think_tagged_scores() {
    // Reasoning models wrap the verdict in think tags; the scores
    // inside still count. Fails before the shared-parser migration.
    let backend = FakeBackend {
        text: "<think>scoring each paragraph</think>{\"1\": 90, \"2\": 10}".to_string(),
        fail: false,
    };
    let docs = vec![
        ("a.md#1".to_string(), "fee calculator".to_string()),
        ("b.md#1".to_string(), "unrelated weather".to_string()),
    ];
    let kept = judge_doc_paragraphs("fee calculator", &docs, &backend);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].0, "a.md#1");
}

#[test]
fn docs_judge_unwraps_prose_wrapped_scores() {
    // Trailing prose around the verdict parses; the scores inside
    // still count. Fails before the shared-parser migration.
    let backend = FakeBackend {
        text: "Here are my scores:\n{\"1\": 90, \"2\": 10}\nHope this helps.".to_string(),
        fail: false,
    };
    let docs = vec![
        ("a.md#1".to_string(), "fee calculator".to_string()),
        ("b.md#1".to_string(), "unrelated weather".to_string()),
    ];
    let kept = judge_doc_paragraphs("fee calculator", &docs, &backend);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].0, "a.md#1");
}

#[test]
fn live_cite_marks_moved_symbol() {
    // Present symbol: live bounds plus hash of the live lines.
    // Missing symbol: the moved marker, never a bare path.
    let dir = tempfile::tempdir().expect("tempdir");
    let abs = dir.path().join("a.rs");
    std::fs::write(
        &abs,
        "// h0\n// h1\n/// Adds one.\npub fn alpha() -> i32 {\n    1\n}\n",
    )
    .expect("write");
    let abs_str = abs.to_string_lossy().into_owned();
    let hit = code_hit(&abs_str, "a.rs", "alpha", 1, 3);
    let cited = cite_live(&hit, dir.path());
    assert_eq!(cited, format!("- {abs_str} : lines 4-6"), "{cited}");
    std::fs::write(&abs, "pub fn other() {}\n").expect("edit");
    assert_eq!(
        cite_live(&hit, dir.path()),
        format!("- {abs_str} : moved or removed since index")
    );
}

#[test]
fn suggest_drill_downs_from_top_symbols() {
    // One drill-down per top symbol, module rows skipped, capped.
    let ctx = vec![
        (
            "alpha".to_string(),
            "a.rs".to_string(),
            1u32,
            3u32,
            "fn alpha() {}".to_string(),
        ),
        (
            "$file".to_string(),
            "b.rs".to_string(),
            1u32,
            2u32,
            "// docs".to_string(),
        ),
        (
            "beta".to_string(),
            "b.rs".to_string(),
            3u32,
            5u32,
            "fn beta() {}".to_string(),
        ),
        (
            "gamma".to_string(),
            "c.rs".to_string(),
            1u32,
            3u32,
            "fn gamma() {}".to_string(),
        ),
        (
            "delta".to_string(),
            "d.rs".to_string(),
            1u32,
            3u32,
            "fn delta() {}".to_string(),
        ),
    ];
    let suggestions = suggest_drill_downs(&ctx, 3);
    assert_eq!(
        suggestions,
        vec![
            "guidance explain \"callers of alpha\"".to_string(),
            "guidance explain \"callers of beta\"".to_string(),
            "guidance explain \"callers of gamma\"".to_string(),
        ],
        "{suggestions:?}"
    );
}

#[test]
fn render_full_orders_answer_docs_snippets_citations() {
    // Section order is the contract: answer, docs, snippets,
    // citations, footer — no table, no Context section. Scores stay on
    // the rows for threshold decisions even though they never render.
    let rows = vec![ExplainRow {
        name: "alpha".to_string(),
        source: "a.rs".to_string(),
        score: 0.05,
        cite: "a.rs:3-5 @ b3:abc".to_string(),
    }];
    assert_eq!(rows[0].score, 0.05);
    let snippets = vec![(
        "alpha".to_string(),
        "a.rs".to_string(),
        3u32,
        5u32,
        "pub fn alpha() {}".to_string(),
    )];
    let rendered = render_explain_full(
        "q",
        &rows,
        &snippets,
        &["- /repo/a.rs : lines 3-5".to_string()],
        &[(
            "doc/notes.md#1".to_string(),
            "Adds one to totals.".to_string(),
        )],
        Some("It adds one."),
        &["guidance explain \"callers of alpha\"".to_string()],
        &[],
        &[],
    );
    let positions = [
        "### Answer",
        "### From the docs",
        "### Top matches",
        "### Citations",
        "If you need details, try:",
    ];
    let mut cursor = 0usize;
    for section in positions {
        let found = rendered[cursor..].find(section).expect(section);
        cursor += found + section.len();
    }
    assert!(!rendered.contains("| Name |"), "{rendered}");
    assert!(!rendered.contains("### Context"), "{rendered}");
    assert!(
        rendered.contains("guidance explain \"callers of alpha\""),
        "{rendered}"
    );
    // Snippets render fenced with a blank line between blocks.
    assert!(
        rendered.contains("```rust\npub fn alpha() {}\n```\n\n"),
        "{rendered}"
    );
}

#[test]
fn render_full_omits_empty_docs_answer_citations() {
    // Empty auxiliary inputs render nothing: no hollow headers.
    let rows = vec![ExplainRow {
        name: "alpha".to_string(),
        source: "a.rs".to_string(),
        score: 0.05,
        cite: "a.rs:3-5 @ b3:abc".to_string(),
    }];
    let rendered = render_explain_full("q", &rows, &[], &[], &[], None, &[], &[], &[]);
    assert!(!rendered.contains("### From the docs"), "{rendered}");
    assert!(!rendered.contains("### Answer"), "{rendered}");
    assert!(!rendered.contains("### Citations"), "{rendered}");
    assert!(!rendered.contains("If you need details"), "{rendered}");
    assert!(!rendered.contains("| Name |"), "{rendered}");
}

#[test]
fn enum_symbol_resolves_live_lines() {
    // The closed symbol set folds Rust enums into Class: resolution
    // must match enum nodes, or every enum reads as moved or removed.
    let dir = tempfile::tempdir().expect("tempdir");
    let abs = dir.path().join("a.rs");
    std::fs::write(&abs, "pub enum Shape {\n    Circle,\n    Square,\n}\n").expect("write");
    let abs_str = abs.to_string_lossy().into_owned();
    let mut hit = code_hit(&abs_str, "a.rs", "Shape", 1, 4);
    if let Some(metadata) = hit.entity.metadata.as_mut() {
        if let guidance_core::search_types::EntityMetadata::Code { symbol_type, .. } = metadata {
            *symbol_type = guidance_core::search_types::CodeSymbolType::Class;
        }
    }
    for evidence in hit.evidence.iter_mut() {
        if let Some(guidance_core::search_types::EntityMetadata::Code { symbol_type, .. }) =
            evidence.metadata.as_mut()
        {
            *symbol_type = guidance_core::search_types::CodeSymbolType::Class;
        }
    }
    let line = cite_live(&hit, dir.path());
    assert_eq!(
        line,
        format!("- {abs_str} : lines 1-4"),
        "enum resolves: {line}"
    );
}

#[test]
fn suggest_drill_downs_skip_docs_and_module_rows() {
    // Only named code symbols get drill-downs: doc paragraphs and
    // module rows have no callers to ask about.
    let ctx = vec![
        (
            "d6ac0c97".to_string(),
            "doc/notes.md".to_string(),
            1u32,
            6u32,
            "para".to_string(),
        ),
        (
            "$file".to_string(),
            "a.rs".to_string(),
            1u32,
            2u32,
            "// crates".to_string(),
        ),
        (
            "alpha".to_string(),
            "a.rs".to_string(),
            3u32,
            5u32,
            "fn alpha() {}".to_string(),
        ),
    ];
    assert_eq!(
        suggest_drill_downs(&ctx, 3),
        vec!["guidance explain \"callers of alpha\"".to_string()]
    );
}

#[test]
fn snippet_for_hit_resolves_by_absolute_path() {
    // src-layout workspaces store relative_path against the src root
    // ("live.rs"), not the workspace root: snippet resolution must
    // follow the absolute path, never workspace.join(relative).
    // Regression: an unsynced edit above the symbol served the stored
    // range sliced from live bytes under a live header.
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir(&src).expect("mkdir");
    std::fs::write(
        src.join("live.rs"),
        "// header\npub fn live_fn() -> u32 {\n    99\n}\n",
    )
    .expect("write");
    let abs = src.join("live.rs").to_string_lossy().into_owned();
    let mut h = hit("live", &abs, "live.rs", Some("live_fn"), 0.05);
    // Stored evidence predates the edit: line 1 only.
    h.evidence[0].range = FragmentSpan::Text {
        start_line: 1,
        end_line: 1,
        start_offset: 0,
        end_offset: 0,
    };
    let snippet = snippet_for_hit(&h, dir.path()).expect("snippet");
    assert!(
        snippet.0.contains("99"),
        "live body, not stored line 1: {snippet:?}"
    );
    assert_eq!((snippet.1, snippet.2), (2, 4));
}

#[test]
fn explain_row_stale_matches_src_root_relative_rows() {
    // Row sources are src-root-relative; the stale set is absolute.
    // Regression: workspace.join(row) never matched in src/ layouts,
    // so --strict silently served stale rows (exit 0, no refusal).
    let ws = "/tmp/ws";
    let srcs = vec![std::path::PathBuf::from("/tmp/ws/src")];
    assert!(explain_row_is_stale(
        "c.rs",
        ws,
        &srcs,
        &["/tmp/ws/src/c.rs".to_string()]
    ));
    assert!(!explain_row_is_stale(
        "c.rs",
        ws,
        &srcs,
        &["/tmp/ws/src/other.rs".to_string()]
    ));
    // Root layout keeps working: workspace-relative rows match.
    assert!(explain_row_is_stale(
        "lib.rs",
        ws,
        &[std::path::PathBuf::from("/tmp/ws")],
        &["/tmp/ws/lib.rs".to_string()]
    ));
}

fn text_hit(abs: &str, rel: &str, symbol: Option<&str>, text: &str) -> SearchHit {
    let mut h = hit(abs, abs, rel, symbol, 0.05);
    h.evidence[0].content = FragmentContent::Text {
        text: text.to_string(),
    };
    h.evidence[0].range = FragmentSpan::Text {
        start_line: 1,
        end_line: 3,
        start_offset: 0,
        end_offset: text.len() as u64,
    };
    h
}

#[test]
fn significant_tokens_drop_scaffolding() {
    assert_eq!(
        significant_query_tokens("What are the major components of this codebase?"),
        vec![
            "major".to_string(),
            "components".to_string(),
            "codebase".to_string()
        ]
    );
    assert_eq!(
        significant_query_tokens("where is BenchTask defined"),
        vec!["benchtask".to_string(), "defined".to_string()]
    );
}

#[test]
fn symbol_match_keeps_hit() {
    let tokens = significant_query_tokens("What are the major components of this codebase?");
    let h = text_hit(
        "/repo/components.rs",
        "components.rs",
        Some("MajorComponents"),
        "pub struct MajorComponents",
    );
    assert!(hit_is_relevant(&h, &tokens));
}

#[test]
fn single_prose_token_drops_hit() {
    // "major" only, in prose — no symbol or stem match. Regression: this
    // shape filled Top matches and Citations, and the answer then
    // confabulated from it.
    let tokens = significant_query_tokens("What are the major components of this codebase?");
    let h = text_hit(
        "/repo/prompts.rs",
        "prompts.rs",
        Some("chat"),
        "a major update to the handler",
    );
    assert!(!hit_is_relevant(&h, &tokens));
}

#[test]
fn two_token_prose_keeps_hit() {
    let tokens = significant_query_tokens("What are the major components of this codebase?");
    let h = text_hit(
        "/repo/overview.rs",
        "overview.rs",
        Some("summarize"),
        "the major components across the whole codebase",
    );
    assert!(hit_is_relevant(&h, &tokens));
}

#[test]
fn single_token_query_keeps_everything() {
    // Narrow intent ("calibration", "BenchTask"): today's behavior is the
    // floor — a lone token cannot prove irrelevance.
    let tokens = significant_query_tokens("calibration");
    let h = text_hit(
        "/repo/notes.md",
        "notes.md",
        None,
        "calibration entry authorizes the point",
    );
    assert!(hit_is_relevant(&h, &tokens));
}

#[test]
fn stem_only_drops_hit() {
    // A bare file-stem match is not relevance: without it, any
    // `*components*.rs` file would survive its own name token on a
    // broad query and feed the answer confabulation.
    let tokens = significant_query_tokens("What are the major components of this codebase?");
    let h = text_hit(
        "/repo/components.rs",
        "components.rs",
        Some("Receipt"),
        "pub struct Receipt",
    );
    assert!(!hit_is_relevant(&h, &tokens));
}

#[test]
fn filter_relevant_hits_partitions() {
    let tokens = significant_query_tokens("What are the major components of this codebase?");
    let hits = vec![
        text_hit(
            "/repo/components.rs",
            "components.rs",
            Some("MajorComponents"),
            "components",
        ),
        text_hit("/repo/b.rs", "b.rs", Some("chat"), "a major update"),
    ];
    let kept = filter_relevant_hits(&hits, &tokens);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].file.relative_path, "components.rs");
}

#[test]
fn live_citations_collapse_dual_unresolved_rows() {
    // Dual rows (one indexed absolute, one relative) for a removed
    // symbol have no live bounds, so the (file, bounds) key never
    // fires — yet they must still cite once. Regression: the same
    // `moved or removed` file rendered twice, once per spelling.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("gone.rs"), "pub fn other() {}\n").expect("write");
    let abs = dir.path().join("gone.rs").to_string_lossy().into_owned();
    let gone = text_hit(&abs, "gone.rs", Some("GoneFn"), "fn GoneFn()");
    let mut dual = gone.clone();
    dual.file.absolute_path = "gone.rs".to_string();
    let cites = cite_hits_live(&[gone, dual], dir.path());
    assert_eq!(cites.len(), 1, "{cites:?}");
    assert!(cites[0].contains("moved or removed"), "{cites:?}");
}

fn doc_tree(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("doc")).expect("mkdir doc");
    std::fs::write(
        root.join("doc").join("alpha.md"),
        "# Alpha\n\nThe alpha helper adds one to every total.\n\nUnrelated weather note.\n",
    )
    .expect("write alpha");
    std::fs::write(
        root.join("doc").join("beta.md"),
        "# Beta\n\nNothing shared here at all.\n",
    )
    .expect("write beta");
}

#[test]
fn doc_rank_cache_key_is_token_set_order_insensitive() {
    assert_eq!(
        doc_rank_cache_key("adds ALPHA helper", 5, false),
        doc_rank_cache_key("helper alpha ADDS", 5, false)
    );
}

#[test]
fn doc_rank_cache_hit_serves_stored_ranking() {
    let dir = tempfile::tempdir().expect("tempdir");
    doc_tree(dir.path());
    let docs_dir = dir.path().join("doc");
    let fp = doc_tree_fingerprint(
        &doc_source_files(Some(&docs_dir), dir.path(), false),
        dir.path(),
    );
    assert_eq!(fp.len(), 2, "both md files fingerprinted: {fp:?}");
    let mut cache = DocRankCache::default();
    let key = doc_rank_cache_key("alpha helper adds", 5, false);
    let ranked = rank_doc_paragraphs("alpha helper adds", Some(&docs_dir), dir.path(), 5, false);
    assert!(!ranked.is_empty());
    cache.store(fp.clone(), key.clone(), ranked.clone());
    assert_eq!(cache.lookup(&fp, &key), Some(ranked));
}

#[test]
fn doc_rank_cache_misses_after_touch() {
    use std::time::Duration;
    let dir = tempfile::tempdir().expect("tempdir");
    doc_tree(dir.path());
    let docs_dir = dir.path().join("doc");
    let before = doc_tree_fingerprint(
        &doc_source_files(Some(&docs_dir), dir.path(), false),
        dir.path(),
    );
    let target = docs_dir.join("alpha.md");
    let mtime = std::fs::metadata(&target)
        .expect("stat")
        .modified()
        .expect("mtime");
    std::fs::File::options()
        .write(true)
        .open(&target)
        .expect("open")
        .set_modified(mtime + Duration::from_secs(60))
        .expect("touch");
    std::fs::write(
        &target,
        std::fs::read_to_string(&target).expect("read") + "\n",
    )
    .expect("append keeps size different too");
    let after = doc_tree_fingerprint(
        &doc_source_files(Some(&docs_dir), dir.path(), false),
        dir.path(),
    );
    assert_ne!(before, after, "touch must invalidate the fingerprint");
    let mut cache = DocRankCache::default();
    let key = doc_rank_cache_key("alpha helper adds", 5, false);
    cache.store(
        before,
        key.clone(),
        vec![("stale".to_string(), "stale".to_string())],
    );
    assert_eq!(cache.lookup(&after, &key), None);
}

#[test]
fn doc_rank_cache_round_trips_through_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    doc_tree(dir.path());
    let docs_dir = dir.path().join("doc");
    let fp = doc_tree_fingerprint(
        &doc_source_files(Some(&docs_dir), dir.path(), false),
        dir.path(),
    );
    let mut cache = DocRankCache::default();
    let key = doc_rank_cache_key("alpha helper adds", 5, false);
    let ranked = rank_doc_paragraphs("alpha helper adds", Some(&docs_dir), dir.path(), 5, false);
    cache.store(fp, key, ranked);
    let path = dir.path().join("cache.json");
    cache.save(&path).expect("save");
    let loaded = DocRankCache::load(&path);
    let fp2 = doc_tree_fingerprint(
        &doc_source_files(Some(&docs_dir), dir.path(), false),
        dir.path(),
    );
    assert_eq!(
        loaded.lookup(&fp2, &doc_rank_cache_key("alpha helper adds", 5, false)),
        Some(rank_doc_paragraphs(
            "alpha helper adds",
            Some(&docs_dir),
            dir.path(),
            5,
            false
        )),
        "disk round-trip preserves the ranked list byte-identically"
    );
}

#[test]
fn doc_rank_cache_load_missing_or_corrupt_is_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(DocRankCache::load(&dir.path().join("absent.json")).is_empty());
    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, "{not json").expect("write");
    assert!(DocRankCache::load(&bad).is_empty());
}

#[test]
fn doc_rank_cache_caps_entries() {
    let mut cache = DocRankCache::default();
    for i in 0..(DOC_RANK_CACHE_MAX_ENTRIES + 50) {
        cache.store(
            vec![(format!("f{i}.md"), 1, 2)],
            format!("q{i}"),
            vec![(format!("p{i}"), "x".to_string())],
        );
    }
    assert!(
        cache.len() <= DOC_RANK_CACHE_MAX_ENTRIES,
        "bounded cache file: {}",
        cache.len()
    );
    assert!(
        cache
            .lookup(
                &[("f177.md".to_string(), 1, 2)],
                &format!("q{}", DOC_RANK_CACHE_MAX_ENTRIES + 49)
            )
            .is_some(),
        "newest entries survive eviction"
    );
}

#[test]
fn cached_rank_matches_direct_rank_byte_identically() {
    // Recall-neutrality: the cached path must render exactly what the
    // direct rank returns, or the cache is a behavior change.
    let dir = tempfile::tempdir().expect("tempdir");
    doc_tree(dir.path());
    let docs_dir = dir.path().join("doc");
    let mut cache = DocRankCache::default();
    let direct = rank_doc_paragraphs("alpha helper adds", Some(&docs_dir), dir.path(), 5, false);
    let (via_cache, first_changed) = rank_doc_paragraphs_cached(
        "alpha helper adds",
        Some(&docs_dir),
        dir.path(),
        5,
        false,
        &mut cache,
    );
    assert!(first_changed, "first call misses and records");
    assert_eq!(via_cache, direct);
    let (again, second_changed) = rank_doc_paragraphs_cached(
        "alpha helper adds",
        Some(&docs_dir),
        dir.path(),
        5,
        false,
        &mut cache,
    );
    assert!(!second_changed, "second call is a clean hit");
    assert_eq!(again, direct, "second call is a hit, same bytes");
}

fn answer_ctx() -> Vec<(String, String, u32, u32, String)> {
    vec![(
        "alpha".to_string(),
        "a.rs".to_string(),
        3u32,
        5u32,
        "pub fn alpha() {}".to_string(),
    )]
}

#[test]
fn answer_strips_think_tags() {
    // Reasoning models wrap the answer; the rendered text must not
    // carry the wrapper. Fails before the M1-helper post-processing.
    let backend = FakeBackend {
        text: "<think>drafting</think>Alpha adds one.".to_string(),
        fail: false,
    };
    let out = answer_with_backend("what does alpha do", &answer_ctx(), &[], &backend, "m")
        .expect("answer")
        .expect("some answer");
    assert_eq!(out, "Alpha adds one.");
    assert!(!out.contains("think"), "{out}");
}

#[test]
fn answer_think_only_body_is_unavailable() {
    // Nothing but reasoning means no answer: unavailable, not rendered.
    let backend = FakeBackend {
        text: "<think>only thinking here</think>".to_string(),
        fail: false,
    };
    let out = answer_with_backend("what does alpha do", &answer_ctx(), &[], &backend, "m")
        .expect("no transport error");
    assert_eq!(out, None);
}

#[test]
fn answer_truncates_at_sentence_boundary() {
    // Over-long answers end at the last in-cap sentence, never mid-word.
    // Cap is EXPLAIN_ANSWER_MAX_TOKENS * 4 chars by the repo's token
    // estimate convention.
    let filler: String = "x".repeat(2000);
    let backend = FakeBackend {
        text: format!("First sentence ends here. {filler} tail words here."),
        fail: false,
    };
    let out = answer_with_backend("what does alpha do", &answer_ctx(), &[], &backend, "m")
        .expect("answer")
        .expect("some answer");
    assert_eq!(out, "First sentence ends here.");
}

#[test]
fn answer_hard_cuts_boundaryless_text() {
    // No sentence boundary at all: fall back to the current hard cut at
    // the char cap (byte-identical to the old behavior for this shape).
    let backend = FakeBackend {
        text: "z".repeat(3000),
        fail: false,
    };
    let out = answer_with_backend("what does alpha do", &answer_ctx(), &[], &backend, "m")
        .expect("answer")
        .expect("some answer");
    assert_eq!(
        out.len(),
        256 * 4,
        "cap is tokens*4 chars, got {}",
        out.len()
    );
    assert!(out.chars().all(|c| c == 'z'));
}

#[test]
fn answer_short_text_is_byte_identical() {
    // Control: short clean answers pass through untouched.
    let backend = FakeBackend {
        text: "  Alpha adds one.  ".to_string(),
        fail: false,
    };
    let out = answer_with_backend("what does alpha do", &answer_ctx(), &[], &backend, "m")
        .expect("answer")
        .expect("some answer");
    assert_eq!(out, "Alpha adds one.");
}

#[test]
fn callers_query_parses_symbol_forms() {
    assert_eq!(
        parse_callers_query("callers of alpha"),
        Some("alpha".to_string())
    );
    assert_eq!(
        parse_callers_query("Callers of Beta"),
        Some("Beta".to_string())
    );
    assert_eq!(
        parse_callers_query("  callers of   gamma  "),
        Some("gamma".to_string())
    );
    assert_eq!(
        parse_callers_query("callers of \"delta\""),
        Some("delta".to_string())
    );
    assert_eq!(parse_callers_query("callers of"), Some(String::new()));
    assert_eq!(parse_callers_query("alpha"), None);
    assert_eq!(parse_callers_query("callers"), None);
    assert_eq!(parse_callers_query(""), None);
}

#[test]
fn callers_resolve_expand_and_empty() {
    use guidance_core::graph_index::{GraphIndex, HarvestInput};
    let files = vec![
        HarvestInput {
            path: "/repo/caller.rs".to_string(),
            format: "rust".to_string(),
            text: "fn outer() {\n    inner();\n}\n".to_string(),
        },
        HarvestInput {
            path: "/repo/other.rs".to_string(),
            format: "rust".to_string(),
            text: "fn second() {\n    inner();\n}\n".to_string(),
        },
        HarvestInput {
            path: "/repo/callee.rs".to_string(),
            format: "rust".to_string(),
            text: "pub fn inner() {}\n".to_string(),
        },
    ];
    let graph = GraphIndex::build(&files, &["/repo".to_string()]).expect("build");
    let mut callers = callers_for_symbol(graph.call_edges(), "inner");
    callers.sort();
    let files_only: Vec<&str> = callers.iter().map(|(file, _, _)| file.as_str()).collect();
    assert_eq!(files_only, vec!["/repo/caller.rs", "/repo/other.rs"]);
    assert!(callers_for_symbol(graph.call_edges(), "absent").is_empty());
    assert!(callers_for_symbol(graph.call_edges(), "").is_empty());
}

#[test]
fn callers_homonym_definitions_expand() {
    use guidance_core::graph_index::{GraphIndex, HarvestInput, ResolutionMethod};
    let files = vec![
        HarvestInput {
            path: "/repo/a.rs".to_string(),
            format: "rust".to_string(),
            text: "pub fn dup() {}\n".to_string(),
        },
        HarvestInput {
            path: "/repo/b.rs".to_string(),
            format: "rust".to_string(),
            text: "pub fn dup() {}\n".to_string(),
        },
        HarvestInput {
            path: "/repo/c.rs".to_string(),
            format: "rust".to_string(),
            text: "fn uses() {\n    dup();\n}\n".to_string(),
        },
    ];
    let graph = GraphIndex::build(&files, &["/repo".to_string()]).expect("build");
    assert_eq!(graph.definitions_of("dup").len(), 2);
    let callers = callers_for_symbol(graph.call_edges(), "dup");
    assert_eq!(callers.len(), 1);
    assert_eq!(callers[0].0, "/repo/c.rs");
    assert!(
        matches!(callers[0].2, ResolutionMethod::Homonym { count: 2 }),
        "{:?}",
        callers[0].2
    );
}

fn dual_hit(abs: &str, rel: &str) -> SearchHit {
    hit_for_test(abs, rel, 2, 5)
}

#[test]
fn dual_identity_hits_merge_to_one_keep_first_rank() {
    let workspace = std::path::Path::new("/repo");
    let first = dual_hit("/repo/./a.rs", "a.rs");
    let second = dual_hit("/repo/a.rs", "a.rs");
    assert_eq!(first.rank, 1);
    let merged = dedupe_dual_hits(vec![first, second], workspace);
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].rank, 1);
    assert_eq!(merged[0].file.absolute_path, "/repo/./a.rs");
}

#[test]
fn same_basename_distinct_files_never_collapse() {
    let workspace = std::path::Path::new("/repo");
    let hits = vec![
        dual_hit("/x/same.rs", "sub1/same.rs"),
        dual_hit("/y/same.rs", "sub2/same.rs"),
    ];
    let merged = dedupe_dual_hits(hits, workspace);
    assert_eq!(merged.len(), 2);
}

#[test]
fn same_file_distinct_spans_never_collapse() {
    let workspace = std::path::Path::new("/repo");
    let first = hit_for_test("/repo/a.rs", "a.rs", 2, 5);
    let mut second = hit_for_test("/repo/a.rs", "a.rs", 8, 9);
    second.entity.range = guidance_core::search_types::FragmentSpan::Text {
        start_line: 8,
        end_line: 9,
        start_offset: 0,
        end_offset: 0,
    };
    let merged = dedupe_dual_hits(vec![first, second], workspace);
    assert_eq!(merged.len(), 2);
}

#[test]
fn dual_merge_keeps_both_provenance_traces() {
    use guidance_core::search_types::{SearchFinalTrace, SearchHitTrace, SearchRecallTrace};
    let workspace = std::path::Path::new("/repo");
    let trace_for = |route: &str| SearchHitTrace {
        recall: vec![SearchRecallTrace {
            path: guidance_core::search_types::RecallPath::Fts,
            route_id: Some(route.to_string()),
            query: Some("q".to_string()),
            found: true,
            forced: None,
            rank: Some(1),
            score: Some(1.0),
            reason: None,
        }],
        fusion: None,
        ranking: None,
        final_trace: SearchFinalTrace {
            returned_by_limit: true,
            cutoff_rank: 10,
        },
    };
    let mut first = dual_hit("/repo/./a.rs", "a.rs");
    first.trace = Some(trace_for("r1"));
    let mut second = dual_hit("/repo/a.rs", "a.rs");
    second.trace = Some(trace_for("r2"));
    let merged = dedupe_dual_hits(vec![first, second], workspace);
    assert_eq!(merged.len(), 1);
    let trace = merged[0].trace.as_ref().expect("trace kept");
    let routes: Vec<&str> = trace
        .recall
        .iter()
        .filter_map(|entry| entry.route_id.as_deref())
        .collect();
    assert!(routes.contains(&"r1"), "{routes:?}");
    assert!(routes.contains(&"r2"), "{routes:?}");
}

#[test]
fn dual_indexed_db_rows_fuse_to_single_hit() {
    use search_vector::db::{FileRecord, FragmentRecord};
    let dir = tempfile::tempdir().expect("tempdir");
    let ws = dir.path().to_str().unwrap().to_string();
    let db_path = dir.path().join("dual.db");
    let db = search_vector::GuidanceDb::open(&db_path).expect("open db");
    let file = |id: &str, abs: &str| FileRecord {
        id: id.to_string(),
        absolute_path: abs.to_string(),
        relative_path: "a.rs".to_string(),
        root_path: ws.clone(),
        size_bytes: 64,
        last_modified_time: 100,
        kind: Some("code".to_string()),
        format: "rust".to_string(),
        content_hash: None,
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    };
    let frag = |id: &str, file_id: &str| FragmentRecord {
        id: id.to_string(),
        group: None,
        file_id: file_id.to_string(),
        range_json: "{\"kind\":\"file\"}".to_string(),
        content_kind: "text".to_string(),
        content_text: Some("pub fn zephyr_quark() {}".to_string()),
        cjk_text: String::new(),
        symbol_type: Some("function".to_string()),
        symbol_name: Some("zephyr_quark".to_string()),
        scope: None,
        signature: Some("function zephyr_quark()".to_string()),
        doc: None,
        modifiers: String::new(),
        heading: None,
        heading_level: None,
        embedding: None,
    };
    let abs_dotted = format!("{}/./a.rs", ws.trim_end_matches('/'));
    db.upsert_fragments(
        &file("file-a", &format!("{ws}/a.rs")),
        &[frag("frag-a", "file-a")],
        &[],
    )
    .expect("upsert a");
    db.upsert_fragments(
        &file("file-b", &abs_dotted),
        &[frag("frag-b", "file-b")],
        &[],
    )
    .expect("upsert b");
    let plan = build_search_plan("zephyr quark", SearchMode::Fuse, 10, false, false, &[], &[]);
    let hits = recall_on_index(&ws, db_path.to_str().unwrap(), &plan).expect("recall");
    assert_eq!(hits.len(), 1, "dual rows fuse to one: {hits:?}");
    assert_eq!(hits[0].rank, 1);
}

fn span_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("shapes.rs"),
        "pub fn plain_fn() {}\n\
         pub struct PlainStruct;\n\
         pub enum PlainEnum {\n    A,\n    B,\n}\n\
         pub trait Things {\n    fn trait_method(&self);\n}\n\
         pub struct WithImpl;\n\
         impl WithImpl {\n    pub fn impl_method(&self) {}\n}\n\
         macro_rules! gen {\n    ($n:ident) => {\n        pub fn $n() {}\n    };\n}\n\
         gen!(macro_made);\n\
         #[cfg(test)]\n\
         mod tests {\n    fn mod_nested_test() {}\n}\n\
         pub struct GenericSlot<T> {\n    _marker: std::marker::PhantomData<T>,\n}\n\
         impl<T> GenericSlot<T> {\n    pub fn generic_get(&self) {}\n}\n\
         const _: () = {\n    fn const_block_nested() {}\n};\n",
    )
    .expect("write shapes");
    dir
}

#[test]
fn span_shape_matrix_pins_resolver_behavior() {
    use fluent_types::MemberType;
    use guidance_core::ast_parser::resolve_span;
    let dir = span_fixture();
    let path = dir.path().join("shapes.rs");
    let cases = [
        ("plain_fn", MemberType::FnDecl, true),
        ("PlainStruct", MemberType::Struct, true),
        ("PlainEnum", MemberType::Struct, true),
        ("WithImpl", MemberType::Struct, true),
        ("impl_method", MemberType::FnDecl, true),
        ("trait_method", MemberType::FnDecl, false),
        ("macro_made", MemberType::FnDecl, false),
        ("mod_nested_test", MemberType::FnDecl, true),
        ("GenericSlot<T>", MemberType::Struct, true),
        ("generic_get", MemberType::FnDecl, true),
        ("const_block_nested", MemberType::FnDecl, true),
    ];
    for (name, member, expected) in cases {
        assert_eq!(
            resolve_span(&path, name, member).is_some(),
            expected,
            "shape {name}"
        );
    }
}

#[test]
fn span_unresolvable_control_stays_unresolved() {
    use fluent_types::MemberType;
    use guidance_core::ast_parser::resolve_span;
    let dir = span_fixture();
    // Missing files and unknown symbols never invent lines.
    assert!(resolve_span(
        &dir.path().join("absent.rs"),
        "plain_fn",
        MemberType::FnDecl
    )
    .is_none());
    assert!(resolve_span(
        &dir.path().join("shapes.rs"),
        "no_such_symbol",
        MemberType::FnDecl
    )
    .is_none());
    assert!(resolve_span(
        &dir.path().join("shapes.rs"),
        "no_such_symbol",
        MemberType::Struct
    )
    .is_none());
}

/// Probe: does `fn {name}` occur inside a `trait ... { ... }` block?
/// Naive brace matching over the file text — a measurement heuristic,
/// not a parser.
fn inside_trait_block(text: &str, name: &str) -> bool {
    let needle = format!("fn {name}");
    let mut search = 0usize;
    while let Some(pos) = text[search..].find("trait ") {
        let block_start = search + pos;
        let Some(open) = text[block_start..].find('{') else {
            return false;
        };
        let mut depth = 0usize;
        for (index, cell) in text[block_start + open..].char_indices() {
            if cell == '{' {
                depth += 1;
            } else if cell == '}' {
                depth -= 1;
                if depth == 0 {
                    let block = &text[block_start + open..block_start + open + index];
                    if block.contains(needle.as_str()) {
                        return true;
                    }
                    break;
                }
            }
        }
        search = block_start + open + 1;
    }
    false
}

#[test]
#[ignore]
fn span_repo_sweep_classifies_unresolved() {
    use fluent_types::MemberType;
    use guidance_core::ast_parser::resolve_span;
    let db_path = std::env::var("GUIDANCE_MEASURE_DB").unwrap_or_default();
    if db_path.is_empty() {
        eprintln!("SKIP: set GUIDANCE_MEASURE_DB to a .guidance.db path");
        return;
    }
    let db = search_vector::GuidanceDb::open(std::path::Path::new(&db_path)).expect("open db");
    let files = db.list_files().expect("list files");
    let mut checked = 0usize;
    let mut resolved = 0usize;
    let mut classes: std::collections::BTreeMap<String, usize> = Default::default();
    let mut samples: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for file in &files {
        if !file.absolute_path.ends_with(".rs") {
            continue;
        }
        let path = std::path::Path::new(&file.absolute_path);
        if !path.is_file() {
            continue;
        }
        let Ok(ids) = db.entity_ids_for_file(&file.id) else {
            continue;
        };
        for id in ids {
            let Ok(rows) = db.fragments_for_entity(&id) else {
                continue;
            };
            for row in rows {
                let (Some(name), Some(kind)) = (row.symbol_name.clone(), row.symbol_type.clone())
                else {
                    continue;
                };
                let member = match kind.as_str() {
                    "function" => MemberType::FnDecl,
                    "class" | "interface" => MemberType::Struct,
                    _ => continue,
                };
                checked += 1;
                if resolve_span(path, &name, member).is_some() {
                    resolved += 1;
                    continue;
                }
                // Bare recorded name (generics stripped) for text probes:
                // the index keeps type parameters in the symbol name.
                let base = name.split(['<', ' ', '(']).next().unwrap_or(&name);
                let text = std::fs::read_to_string(path).unwrap_or_default();
                let class = if name.contains('<') {
                    "generic-params"
                } else if inside_trait_block(&text, &name) {
                    "trait-signature"
                } else if text.contains(&format!("trait {base}")) {
                    "trait-name"
                } else if !text.contains(base) {
                    "absent-from-text"
                } else if text.contains("macro_rules") {
                    "macro-adjacent"
                } else {
                    "other"
                };
                *classes.entry(class.to_string()).or_default() += 1;
                if samples.get(class).map_or(0, Vec::len) < 10 {
                    samples
                        .entry(class.to_string())
                        .or_default()
                        .push(format!("{}::{}", file.absolute_path, name));
                }
            }
        }
    }
    eprintln!("span sweep: {checked} checked, {resolved} resolved");
    for (class, count) in &classes {
        eprintln!("  {class}: {count}");
        for sample in samples.get(class).cloned().unwrap_or_default() {
            eprintln!("    e.g. {sample}");
        }
    }
    assert!(checked > 0, "sweep covered no symbols");
}

#[test]
fn capability_trigger_counts_hits_and_score_floor() {
    assert!(capability_trigger(&[]), "no hits is code-thin");
    assert!(
        capability_trigger(&[hit("a", "/r/a.rs", "a.rs", None, 0.05)]),
        "one hit is code-thin"
    );
    let strong = vec![
        hit("a", "/r/a.rs", "a.rs", None, 0.05),
        hit("b", "/r/b.rs", "b.rs", None, 0.04),
        hit("c", "/r/c.rs", "c.rs", None, 0.03),
    ];
    assert!(
        !capability_trigger(&strong),
        "three agreeing hits stay shut"
    );
    let weak = vec![
        hit("a", "/r/a.rs", "a.rs", None, 0.01),
        hit("b", "/r/b.rs", "b.rs", None, 0.01),
        hit("c", "/r/c.rs", "c.rs", None, 0.01),
    ];
    assert!(
        capability_trigger(&weak),
        "three single-route hits open the branch"
    );
}

#[test]
fn branch_gate_relaxed_needs_one_token() {
    let hit = text_hit(
        "/r/dispatch.rs",
        "src/router/dispatch.rs",
        None,
        "Flux drive calibration.",
    );
    assert!(branch_hit_passes_gate(
        &hit,
        &["zxqy".to_string(), "flux".to_string()]
    ));
    assert!(!branch_hit_passes_gate(
        &hit,
        &["zxqy".to_string(), "wobble".to_string()]
    ));
    assert!(
        branch_hit_passes_gate(&hit, &["flux".to_string()]),
        "single-token queries keep everything"
    );
}

#[test]
fn brief_opens_traversal_only_for_rank_one() {
    assert!(brief_opens_traversal(1));
    assert!(!brief_opens_traversal(2));
    assert!(!brief_opens_traversal(3));
}

#[test]
fn branch_scope_glob_maps_crate_prefix() {
    assert_eq!(
        branch_scope_glob("fluent-router"),
        Some("src/router/**".to_string())
    );
    assert_eq!(
        branch_scope_glob("guidance-core"),
        Some("src/guidance/**".to_string())
    );
    assert_eq!(branch_scope_glob("unknown"), None);
    assert_eq!(branch_scope_glob(""), None);
    assert_eq!(branch_scope_glob("no-such-package"), None);
}

#[test]
fn merge_branch_hits_renders_new_files_once_with_tags() {
    use std::path::Path;
    let workspace = Path::new("/repo");
    let base = vec![hit("a", "/repo/a.rs", "a.rs", None, 0.05)];
    let pulls = vec![BranchPull {
        crate_name: "fluent-router".to_string(),
        hits: vec![hit("b", "/repo/b.rs", "b.rs", None, 0.05)],
    }];
    let (merged, tags, duals) = merge_branch_hits(base, &pulls, workspace);
    assert_eq!(merged.len(), 2);
    assert_eq!(tags, vec![None, Some("fluent-router".to_string())]);
    assert!(duals.is_empty());
}

#[test]
fn merge_branch_hits_collapses_dual_files_with_both_provenances() {
    use std::path::Path;
    let workspace = Path::new("/repo");
    let base = vec![hit("a", "/repo/a.rs", "a.rs", None, 0.05)];
    let pulls = vec![BranchPull {
        crate_name: "fluent-router".to_string(),
        hits: vec![hit("a", "/repo/a.rs", "a.rs", None, 0.05)],
    }];
    let (merged, tags, duals) = merge_branch_hits(base, &pulls, workspace);
    assert_eq!(merged.len(), 1, "dual file renders once");
    assert_eq!(tags, vec![None]);
    assert_eq!(
        duals,
        vec![(
            "a.rs".to_string(),
            "via capability: fluent-router".to_string()
        )]
    );
}

#[test]
fn tag_snippets_by_file_joins_multi_crate_tags() {
    let snippets = vec![
        (
            "a".to_string(),
            "a.rs".to_string(),
            1u32,
            2u32,
            "t".to_string(),
        ),
        (
            "b".to_string(),
            "b.rs".to_string(),
            1u32,
            2u32,
            "t".to_string(),
        ),
    ];
    let tags = tag_snippets_by_file(
        &snippets,
        &[
            ("a.rs".to_string(), "via capability: x".to_string()),
            ("a.rs".to_string(), "via capability: y".to_string()),
        ],
    );
    assert_eq!(
        tags,
        vec![
            "via capability: x, via capability: y".to_string(),
            String::new()
        ]
    );
}

fn capability_row(
    id: &str,
    purpose: &str,
    predicate: &str,
    roles: &str,
) -> search_vector::db::CapabilityRecord {
    search_vector::db::CapabilityRecord {
        id: id.to_string(),
        project: "fixture".to_string(),
        crate_name: "fluent-router".to_string(),
        domain: "router".to_string(),
        purpose: purpose.to_string(),
        predicate: predicate.to_string(),
        roles_json: roles.to_string(),
        source_path: "doc/router/guide.md".to_string(),
        anchor: id.to_string(),
        content_hash: format!("hash-{id}"),
        model: "test-model".to_string(),
        prompt_version: 1,
        status: "ready".to_string(),
        gap_reason: None,
        generated_at: 1700000000000,
    }
}

fn seed_capability_db(dir: &tempfile::TempDir) -> (String, search_vector::GuidanceDb) {
    use search_vector::db::{CapabilityLemma, FileRecord, FragmentLemma, FragmentRecord};
    let ws = dir.path().to_str().unwrap().to_string();
    let db_path = dir.path().join("cap.db");
    let db = search_vector::GuidanceDb::open(&db_path).expect("open db");
    let file = |id: &str, rel: &str| FileRecord {
        id: id.to_string(),
        absolute_path: format!("{ws}/{rel}"),
        relative_path: rel.to_string(),
        root_path: ws.clone(),
        size_bytes: 64,
        last_modified_time: 100,
        kind: Some("code".to_string()),
        format: "rust".to_string(),
        content_hash: None,
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    };
    let frag = |id: &str, file_id: &str, text: &str, symbol: &str| FragmentRecord {
        id: id.to_string(),
        group: None,
        file_id: file_id.to_string(),
        range_json: "{\"kind\":\"file\"}".to_string(),
        content_kind: "text".to_string(),
        content_text: Some(text.to_string()),
        cjk_text: String::new(),
        symbol_type: Some("function".to_string()),
        symbol_name: Some(symbol.to_string()),
        scope: None,
        signature: Some(format!("fn {symbol}()")),
        doc: None,
        modifiers: String::new(),
        heading: None,
        heading_level: None,
        embedding: None,
    };
    db.upsert_fragments(
        &file("f1", "src/router/dispatch.rs"),
        &[frag(
            "frag-1",
            "f1",
            "Flux drive calibration sequence.",
            "calibrate_drive",
        )],
        &[],
    )
    .expect("upsert code");
    db.upsert_fragments(
        &file("f2", "src/router/drive.rs"),
        &[frag(
            "frag-2",
            "f2",
            "Drive flux regulator cleaning.",
            "regulate_drive",
        )],
        &[],
    )
    .expect("upsert code");
    db.upsert_fragments(
        &file("f3", "src/router/flux.rs"),
        &[frag(
            "frag-3",
            "f3",
            "Calibrate drive output levels.",
            "calibrate_output",
        )],
        &[],
    )
    .expect("upsert code");
    db.insert_fragment_lemmas(&[
        FragmentLemma {
            fragment_id: "frag-1".to_string(),
            lemma: "drive".to_string(),
            confidence: 1.0,
        },
        FragmentLemma {
            fragment_id: "frag-2".to_string(),
            lemma: "drive".to_string(),
            confidence: 1.0,
        },
        FragmentLemma {
            fragment_id: "frag-2".to_string(),
            lemma: "flux".to_string(),
            confidence: 1.0,
        },
        FragmentLemma {
            fragment_id: "frag-3".to_string(),
            lemma: "drive".to_string(),
            confidence: 1.0,
        },
        FragmentLemma {
            fragment_id: "frag-3".to_string(),
            lemma: "calibrate".to_string(),
            confidence: 1.0,
        },
    ])
    .expect("upsert fragment lemmas");
    db.replace_capabilities(&[
        capability_row(
            "doc/router/guide.md#guide",
            "Flux wobble calibration.",
            "calibrate",
            r#"["flux","wobble"]"#,
        ),
        capability_row("doc/misc/x.md#x", "Unrelated prose here.", "", r#"[]"#),
    ])
    .expect("upsert briefs");
    db.insert_capability_lemmas(&[
        CapabilityLemma {
            capability_id: "doc/router/guide.md#guide".to_string(),
            lemma: "flux".to_string(),
            confidence: 1.0,
        },
        CapabilityLemma {
            capability_id: "doc/router/guide.md#guide".to_string(),
            lemma: "wobble".to_string(),
            confidence: 1.0,
        },
        CapabilityLemma {
            capability_id: "doc/router/guide.md#guide".to_string(),
            lemma: "calibrate".to_string(),
            confidence: 1.0,
        },
    ])
    .expect("upsert lemmas");
    (ws, db)
}

#[test]
fn recall_briefs_ranks_svo_match_first() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_ws, db) = seed_capability_db(&dir);
    let nlp = spacy_rs::pipeline::NlpPipeline::en_default().expect("rule pipeline");
    let briefs = recall_capability_briefs("calibrate flux drive", &db, Some(&nlp));
    assert_eq!(briefs.len(), 1, "{briefs:?}");
    assert_eq!(briefs[0].id, "doc/router/guide.md#guide");
    assert_eq!(briefs[0].rank, 1);
    assert!(!briefs[0].overlap.is_empty(), "{:?}", briefs[0].overlap);
}

#[test]
fn recall_briefs_falls_back_to_fts_without_nlp() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_ws, db) = seed_capability_db(&dir);
    let briefs = recall_capability_briefs("calibrate flux drive", &db, None);
    assert!(!briefs.is_empty(), "fts-only recall still ranks");
    assert_eq!(briefs[0].id, "doc/router/guide.md#guide");
}

#[test]
fn capability_branch_pulls_rank_one_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (ws, _db) = seed_capability_db(&dir);
    let db_path = dir.path().join("cap.db").to_str().unwrap().to_string();
    let branch = capability_branch("zxqy flux wobble", &ws, &db_path, &[]);
    assert!(!branch.briefs.is_empty(), "triggered branch ranks briefs");
    assert_eq!(branch.briefs[0].rank, 1);
    assert_eq!(branch.pulls.len(), 1, "exactly one scoped pull: {branch:?}");
    assert_eq!(branch.pulls[0].crate_name, "fluent-router");
    assert!(
        !branch.pulls[0].hits.is_empty(),
        "scoped pull finds code: {branch:?}"
    );
    assert!(
        branch.pulls[0]
            .hits
            .iter()
            .all(|hit| hit.file.relative_path.starts_with("src/router")),
        "pull stays in scope: {branch:?}"
    );
}

#[test]
fn capability_branch_skips_unknown_crate_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (ws, db) = seed_capability_db(&dir);
    let mut held = capability_row("doc/misc/x.md#x", "Unrelated prose here.", "", r#"[]"#);
    held.crate_name = "unknown".to_string();
    db.replace_capabilities(&[held]).expect("replace");
    let db_path = dir.path().join("cap.db").to_str().unwrap().to_string();
    let branch = capability_branch("unrelated prose here", &ws, &db_path, &[]);
    assert!(!branch.briefs.is_empty(), "unknown-crate brief still ranks");
    assert!(
        branch.pulls.is_empty(),
        "no scope means no pull: {branch:?}"
    );
}

#[test]
fn capability_branch_stays_shut_on_strong_code_recall() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (ws, _db) = seed_capability_db(&dir);
    let db_path = dir.path().join("cap.db").to_str().unwrap().to_string();
    let plan = build_search_plan(
        "calibrate drive",
        SearchMode::Fuse,
        10,
        false,
        false,
        &[],
        &[],
    );
    let hits = recall_on_index(&ws, &db_path, &plan).expect("recall");
    assert!(
        !capability_trigger(&hits),
        "test guards the shut branch: {hits:?}"
    );
    let branch = capability_branch("calibrate drive", &ws, &db_path, &hits);
    assert!(branch.briefs.is_empty() && branch.pulls.is_empty());
}

fn ranked_brief(
    id: &str,
    purpose: &str,
    rank: usize,
) -> guidance_core::query::capability_match::RankedBrief {
    use guidance_core::query::capability_match::CapabilityOverlap;
    guidance_core::query::capability_match::RankedBrief {
        id: id.to_string(),
        purpose: purpose.to_string(),
        crate_name: "fluent-router".to_string(),
        domain: "router".to_string(),
        source_path: "doc/router/guide.md".to_string(),
        anchor: id.to_string(),
        predicate: String::new(),
        roles: Vec::new(),
        overlap: CapabilityOverlap {
            predicate: 1,
            roles: 0,
        },
        fts_score: 1.0,
        rank,
    }
}

fn brief_set() -> Vec<guidance_core::query::capability_match::RankedBrief> {
    vec![
        ranked_brief("doc/router/guide.md#guide", "Routes requests onward.", 1),
        ranked_brief("doc/router/notes.md#scope", "Unrelated tooling notes.", 2),
    ]
}

#[test]
fn capability_judge_scores_tier_without_reordering() {
    assert_eq!(EXPLAIN_CAPABILITY_MIN_SCORE, 60);
    let backend = FakeBackend {
        text: r#"{"1": 90, "2": 10}"#.to_string(),
        fail: false,
    };
    let scored = judge_capability_briefs("routing", &brief_set(), &backend);
    assert_eq!(scored.len(), 2);
    assert_eq!(
        scored[0].brief.rank, 1,
        "judge tiers, never reorders: {scored:?}"
    );
    assert_eq!(scored[1].brief.rank, 2);
    assert_eq!(scored[0].score, Some(90));
    assert_eq!(capability_tier(scored[0].score), CapabilityTier::Strong);
    assert_eq!(scored[1].score, Some(10));
    assert_eq!(capability_tier(scored[1].score), CapabilityTier::Considered);
}

#[test]
fn capability_judge_think_wrapped_and_trailing_prose_accept() {
    let backend = FakeBackend {
        text: "<think>scoring</think> {\"1\": 80, \"2\": 20} done.".to_string(),
        fail: false,
    };
    let scored = judge_capability_briefs("routing", &brief_set(), &backend);
    assert_eq!(scored[0].score, Some(80), "{scored:?}");
    assert_eq!(scored[1].score, Some(20));
}

#[test]
fn capability_judge_empty_and_failure_keep_order_unjudged() {
    // Fail open (the deliberate contrast with the docs judge):
    // unparseable or unreachable judging keeps deterministic order.
    let empty = FakeBackend {
        text: String::new(),
        fail: false,
    };
    let scored = judge_capability_briefs("routing", &brief_set(), &empty);
    assert!(scored.iter().all(|s| s.score.is_none()), "{scored:?}");
    assert_eq!(scored[0].brief.rank, 1);
    let failing = FakeBackend {
        text: String::new(),
        fail: true,
    };
    let scored = judge_capability_briefs("routing", &brief_set(), &failing);
    assert!(scored.iter().all(|s| s.score.is_none()), "{scored:?}");
    assert!(judge_capability_briefs("routing", &[], &failing).is_empty());
}

#[test]
fn capability_judge_unknown_id_keeps_all_unjudged() {
    let backend = FakeBackend {
        text: r#"{"1": 90, "rogue": 95}"#.to_string(),
        fail: false,
    };
    let scored = judge_capability_briefs("routing", &brief_set(), &backend);
    assert!(scored.iter().all(|s| s.score.is_none()), "{scored:?}");
}

#[test]
fn capability_tier_floor_shape() {
    assert_eq!(capability_tier(Some(100)), CapabilityTier::Strong);
    assert_eq!(capability_tier(Some(60)), CapabilityTier::Strong);
    assert_eq!(capability_tier(Some(59)), CapabilityTier::Considered);
    assert_eq!(capability_tier(Some(0)), CapabilityTier::Considered);
    assert_eq!(capability_tier(None), CapabilityTier::Considered);
}

#[test]
fn answer_messages_appends_brief_block() {
    let ctx = answer_ctx();
    let without = answer_messages("what does alpha do", &ctx, &[]);
    let combined: String = without.iter().map(|m| m.content.as_str()).collect();
    assert!(!combined.contains("Capability briefs"), "{combined}");
    let with = answer_messages(
        "what does alpha do",
        &ctx,
        &["Routes requests onward.".to_string()],
    );
    let combined: String = with.iter().map(|m| m.content.as_str()).collect();
    assert!(combined.contains("Capability briefs"), "{combined}");
    assert!(combined.contains("Routes requests onward."), "{combined}");
    assert!(combined.contains("pub fn alpha"), "{combined}");
}

#[test]
fn answer_needs_code_evidence_even_with_briefs() {
    // Briefs join synthesis context; they never substitute for it:
    // no code excerpts means no answer, however strong the briefs.
    let backend = FakeBackend {
        text: "Routes onward.".to_string(),
        fail: false,
    };
    let out = answer_with_backend(
        "what does alpha do",
        &[],
        &["Routes requests onward.".to_string()],
        &backend,
        "m",
    )
    .expect("no transport error");
    assert_eq!(out, None);
}

#[test]
fn capability_tier_calibration_against_golden_labels() {
    // M14-cal (measurement only): scripted scores stand in for a
    // calibrated judge; the harness reports tier precision/recall
    // against golden relevance. Model quality itself is M15's
    // measurement against a real endpoint, never this stub.
    let briefs = vec![
        ranked_brief("doc/g.md#a", "Indexes fragments for recall.", 1),
        ranked_brief("doc/g.md#b", "Routes requests onward.", 2),
        ranked_brief("doc/g.md#c", "Unrelated tooling notes.", 3),
    ];
    let golden = vec![
        ("doc/g.md#a", true),
        ("doc/g.md#b", true),
        ("doc/g.md#c", false),
    ];
    let backend = FakeBackend {
        text: r#"{"1": 90, "2": 75, "3": 20}"#.to_string(),
        fail: false,
    };
    let scored = judge_capability_briefs("index and route", &briefs, &backend);
    let mut tp = 0;
    let mut fp = 0;
    let mut retrieved_relevant = 0;
    for (id, relevant) in &golden {
        let tier = scored
            .iter()
            .find(|s| s.brief.id == *id)
            .map(|s| capability_tier(s.score))
            .expect("every golden brief scored");
        let strong = tier == CapabilityTier::Strong;
        if strong && *relevant {
            tp += 1;
        }
        if strong && !*relevant {
            fp += 1;
        }
        if *relevant {
            retrieved_relevant += usize::from(strong);
        }
    }
    let relevant_total = golden.iter().filter(|(_, r)| *r).count();
    let precision = tp as f64 / (tp + fp) as f64;
    let recall = retrieved_relevant as f64 / relevant_total as f64;
    eprintln!("capability tier calibration: precision={precision:.3} recall={recall:.3}");
    assert!(
        (precision - 1.0).abs() < f64::EPSILON,
        "precision={precision}"
    );
    assert!((recall - 1.0).abs() < f64::EPSILON, "recall={recall}");
    // Fail-open control: an empty judge response keeps every brief
    // (order intact) rather than dropping the section.
    let empty = FakeBackend {
        text: String::new(),
        fail: false,
    };
    let scored = judge_capability_briefs("index and route", &briefs, &empty);
    assert_eq!(scored.len(), 3);
    assert!(scored
        .iter()
        .all(|s| capability_tier(s.score) == CapabilityTier::Considered));
}

#[test]
fn capability_judge_prompt_is_byte_stable() {
    // Agent-loop primitive: the agent and in-CLI judging share one
    // prompt shape and one floor — goldens pin every byte.
    let messages = build_capability_judge_prompt(
        "routing",
        &["requests".to_string(), "onward".to_string()],
        &[(
            "fluent-router/router (doc/router/guide.md#guide)".to_string(),
            "Routes requests onward.".to_string(),
        )],
    );
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0].content,
        "Rate how relevant each brief is to the question, 0-100. \
         Respond with ONLY a strict JSON object mapping 1-based brief \
         number to integer score, e.g. {\"1\": 85}."
    );
    assert_eq!(
        messages[1].content,
        "Question: routing\n\nBriefs:\n\
         1. fluent-router/router (doc/router/guide.md#guide)\n\
         Routes requests onward.\n\n\
         Query signals: requests, onward\n"
    );
}

#[test]
fn capability_judge_prompt_pins_empty_understanding() {
    let messages = build_capability_judge_prompt("routing", &[], &[]);
    assert_eq!(messages.len(), 2);
    let body = &messages[1].content;
    assert!(body.contains("Briefs:\n"), "{body}");
    assert!(body.contains("Query signals: (none)\n"), "{body}");
}

#[test]
fn scored_brief_json_carries_handoff_fields() {
    // Schema golden for the agent handoff (crate/domain/SVO/tier):
    // exact keys, JSON types, byte-stable serialization.
    let scored = ScoredBrief {
        brief: ranked_brief("doc/router/guide.md#guide", "Routes requests onward.", 1),
        score: Some(85),
    };
    let value = scored_brief_json(&scored);
    let object = value.as_object().expect("object");
    let keys: Vec<&str> = object.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        vec![
            "anchor",
            "crate",
            "domain",
            "id",
            "predicate",
            "purpose",
            "rank",
            "roles",
            "score",
            "source_path",
            "tier"
        ]
    );
    assert_eq!(value["tier"], serde_json::json!("strong"));
    assert_eq!(value["score"], serde_json::json!(85));
    assert_eq!(value["predicate"], serde_json::json!(""));
    assert_eq!(value["roles"], serde_json::json!([]));
    let text = serde_json::to_string(&value).expect("serialize");
    assert!(text.contains("\"crate\":\"fluent-router\""), "{text}");
    assert!(text.contains("\"domain\":\"router\""), "{text}");
}

#[test]
fn scored_brief_json_marks_unjudged_considered() {
    let scored = ScoredBrief {
        brief: ranked_brief("doc/a.md#x", "Something.", 2),
        score: None,
    };
    let value = scored_brief_json(&scored);
    assert_eq!(value["tier"], serde_json::json!("considered"));
    assert!(value["score"].is_null(), "{value}");
    assert_eq!(value["rank"], serde_json::json!(2));
}
