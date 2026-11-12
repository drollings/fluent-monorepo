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
    let ranked = rank_doc_paragraphs("fee calculator total", dir.path(), dir.path(), 5);
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
    let messages = answer_messages("what does alpha do", &ctx);
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
    let rendered = render_explain_full("q", &rows, &[], &[], &[], None, &[]);
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
