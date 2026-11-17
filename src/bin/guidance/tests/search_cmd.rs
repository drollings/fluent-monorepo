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
        false,
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
        false,
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
        false,
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
        false,
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
        false,
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
        false,
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
    let db = guidance_core::query::search_db::GuidanceSearchDb::open(&db_path).expect("open db");
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
        false,
    )
    .expect("fuse route");
    assert_eq!(
        rendered,
        "## Prose\n\n*Source: a.rs*\n\nthe alpha needle sleeps here\n\n\n---\n\n\n### Citations\n- unverified\n"
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
        false,
    )
    .expect_err("vector mode without a backend must decline");
    assert!(
        error.contains("embed"),
        "decline must name the missing backend, got: {error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn vector_mode_refuses_without_stored_vectors() {
    // Backend configured but `sync` never embedded: `--vector` names
    // the missing vectors and the sync that populates them, instead
    // of silently answering from the lemma route. No endpoint is
    // dialed: the refusal fires before any embedding call.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".guidance")).expect("mkdir guidance");
    std::fs::write(
        dir.path().join(".guidance/guidance-config.json"),
        r#"{"models": {"embed": "llama:embed"},
            "providers": {"llama": {"base_url": "http://localhost:11434/v1",
                                     "chat_endpoint": "/chat/completions"}},
            "embedding_dims": 8}"#,
    )
    .expect("write config");
    std::fs::write(dir.path().join("a.rs"), "fn needle_fn() {}\n").expect("write");
    let workspace = dir.path().to_str().unwrap().to_string();
    let db = dir.path().join("empty.db");
    let db = db.to_str().unwrap().to_string();
    let error = run_search(
        "needle_fn",
        &workspace,
        &db,
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
        false,
    )
    .expect_err("vector mode without stored vectors must decline");
    assert!(
        error.contains("no stored vectors") && error.contains("guidance sync"),
        "decline must name the repair, got: {error}"
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
fn context_helper_degrades_to_empty_without_anchors() {
    // No hits means no anchors: empty lines, never an error — the
    // caller renders hits only.
    let db = GuidanceSearchDb::open_in_memory().expect("memory db");
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
    let db = guidance_core::query::search_db::GuidanceSearchDb::open(&db_path).expect("open db");
    let file = guidance_core::query::search_db::FileRecord {
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
        false,
    )
    .expect("fuse route");
    let expected = common_core::hash::blake3_hex("pub fn alpha() -> i32 {\n    1\n}\n".as_bytes());
    assert!(
        rendered.contains(&format!("a.rs:2-4 @ b3:{expected}")),
        "item range must cite with a live-byte hash: {rendered}"
    );
}

#[test]
fn significant_tokens_drop_scaffolding() {
    assert_eq!(
        guidance_core::read::vocabulary::query_terms(
            "What are the major components of this codebase?"
        ),
        vec![
            "major".to_string(),
            "components".to_string(),
            "codebase".to_string()
        ]
    );
    assert_eq!(
        guidance_core::read::vocabulary::query_terms("where is BenchTask defined"),
        vec!["benchtask".to_string(), "defined".to_string()]
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
    use guidance_core::query::search_db::{FileRecord, FragmentRecord};
    let dir = tempfile::tempdir().expect("tempdir");
    let ws = dir.path().to_str().unwrap().to_string();
    let db_path = dir.path().join("dual.db");
    let db = guidance_core::query::search_db::GuidanceSearchDb::open(&db_path).expect("open db");
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
    let hits = recall_on_index(
        &ws,
        db_path.to_str().unwrap(),
        &plan,
        &LazyNlp::new(),
        guidance_core::query::recall::RecallPolicy::default(),
    )
    .expect("recall");
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
    let db =
        guidance_core::query::search_db::GuidanceSearchDb::open(std::path::Path::new(&db_path))
            .expect("open db");
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
