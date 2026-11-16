//! Module-header rows: the already-extracted top-of-file comment blocks
//! index as ordinary module symbols, so a how-does-it-work question can
//! retrieve the header that names the mechanism.
use super::*;
use crate::read::engine::{AnswerMode, ExplainContext, ExplainRequest, ReadEngine};
use search_vector::db::GuidanceDb;
use std::sync::Arc;
use tempfile::TempDir;

fn selection(root: &std::path::Path) -> FileSelection {
    FileSelection {
        workspace_root: root.to_path_buf(),
        include_fixtures: false,
        roots: vec![root.to_path_buf()],
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        extra_skip_dirs: Vec::new(),
        honor_gitignore: false,
        max_bytes_override: None,
    }
}

fn options() -> IndexOptions {
    IndexOptions {
        max_embed_batch: 8,
        wave_files: 4,
        retry_max_attempts: 1,
        retry_base_ms: 1,
        rebuild: false,
    }
}

fn module_workspace() -> (TempDir, Arc<GuidanceDb>) {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join("freshness.rs"),
        "//! Staleness decisions: which files sync reprocesses after an edit.\n\
         pub fn stale_files() -> usize {\n    0\n}\n",
    )
    .expect("write");
    std::fs::write(
        dir.path().join("sync_engine.rs"),
        "//! Sync engine: reprocess after an edit; status names stale_files.\n\
         pub fn status() -> usize {\n    0\n}\n",
    )
    .expect("write");
    let db = Arc::new(GuidanceDb::open_in_memory().expect("db"));
    index_workspace(
        Arc::clone(&db),
        fluent_llm::embeddings::NoopEmbedding::new(4),
        &selection(dir.path()),
        &options(),
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .expect("index");
    (dir, db)
}

#[test]
fn module_headers_index_as_module_symbols() {
    let (_dir, db) = module_workspace();
    let file = db
        .list_files()
        .expect("files")
        .into_iter()
        .find(|file| file.relative_path.ends_with("freshness.rs"))
        .expect("freshness file");
    let module_id = crate::extractor::code::module_fragment_id(&file.id);
    let rows = db.fragments_for_entity(&module_id).expect("module row");
    let row = rows.first().expect("module row stored");
    assert_eq!(row.symbol_type.as_deref(), Some("module"));
    assert_eq!(
        row.symbol_name.as_deref(),
        Some("freshness"),
        "anchored to the module symbol, not a pseudo-symbol"
    );
    assert!(
        row.content_text
            .as_deref()
            .is_some_and(|text| text.contains("reprocesses")),
        "the header text is the row content"
    );
    assert!(
        classify_embed_fragment(row).is_ok(),
        "module headers are embed units"
    );
}

#[test]
fn sync_reprocess_query_surfaces_the_module_header() {
    let (dir, db) = module_workspace();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let answer = engine.explain(
        &ExplainRequest {
            query: "How does guidance sync decide what to reprocess after an edit?".to_string(),
            limit: 10,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
            answer_mode: AnswerMode::Auto,
        },
        &ctx,
        None,
    );
    let citations: Vec<String> = answer
        .citations
        .iter()
        .map(crate::source_ref::Citation::text)
        .collect();
    assert!(
        citations
            .iter()
            .take(5)
            .any(|citation| citation.starts_with("freshness.rs")),
        "module header surfaces in the top five: {citations:?}"
    );
}

#[test]
fn module_header_routes_stay_fts_and_vector() {
    // The promotion is data, not a new route: fusion arithmetic is
    // pinned in `query_fusion.rs` and untouched here.
    let plan = crate::read::planes::code::code_plan(
        "How does guidance sync decide what to reprocess after an edit?",
        10,
        false,
    );
    let modes: Vec<crate::search_types::SearchPlanRouteMode> =
        plan.routes.iter().map(|route| route.mode).collect();
    assert_eq!(
        modes,
        vec![
            crate::search_types::SearchPlanRouteMode::Fts,
            crate::search_types::SearchPlanRouteMode::Vector,
        ]
    );
}
