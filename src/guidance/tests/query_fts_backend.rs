use super::*;
use crate::query::db_storage::GuidanceDbStorage;
use crate::query::ingest::ingest_text_file;
use crate::query::llm_filter::{LlmFilter, NoopLlmFilter};
use crate::query::search_backend::SearchContext;
use crate::tests::common::make_test_doc;
use crate::zg_types::FileInfo;

// Shared seed helpers live here (first backend suite); sibling suites
// repeat the ten-line seed rather than cross-including test modules.

fn file_info(id: &str, relative: &str) -> FileInfo {
    FileInfo {
        id: id.to_string(),
        absolute_path: format!("/repo/{relative}"),
        relative_path: relative.to_string(),
        root_path: "/repo".to_string(),
        size_bytes: 64,
        last_modified_time: 100,
        content_hash: None,
        kind: Some(crate::zg_types::FileKind::Code),
        format: "typescript".to_string(),
        index_status: None,
    }
}

fn seed_two_files(db: &search_vector::db::GuidanceDb) {
    ingest_text_file(
        db,
        &file_info("file-a", "src/a.ts"),
        "export function AlphaSymbol() { return 1; }\n",
        None,
        None,
    )
    .expect("ingest a");
    ingest_text_file(
        db,
        &file_info("file-b", "src/b.ts"),
        "export function BetaSymbol() { return 2; }\n",
        None,
        None,
    )
    .expect("ingest b");
}

fn test_context(filter: &LlmFilter) -> SearchContext<'_> {
    SearchContext {
        storage: None,
        embedder: None,
        nlp: None,
        llm_filter: filter,
    }
}

#[test]
fn fts_backend_answers_from_the_index() {
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    seed_two_files(&db);
    let storage = GuidanceDbStorage::new(&db);
    let filter = LlmFilter::new(Some(Box::new(NoopLlmFilter)));
    let backend = FtsBackend {
        storage: &storage,
        limit: 5,
    };
    let stages = backend
        .search("AlphaSymbol", &make_test_doc(), &test_context(&filter))
        .expect("search");
    assert!(!stages.is_empty());
    assert!(stages.iter().all(|stage| stage.source == "src/a.ts"));
    assert!(stages
        .iter()
        .any(|stage| stage.content.contains("AlphaSymbol")));
    // Trace provenance rides along.
    assert!(stages.iter().any(|stage| stage.trace.is_some()));
}

#[test]
fn fts_backend_declines_when_nothing_matches() {
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    seed_two_files(&db);
    let storage = GuidanceDbStorage::new(&db);
    let filter = LlmFilter::new(Some(Box::new(NoopLlmFilter)));
    let backend = FtsBackend {
        storage: &storage,
        limit: 5,
    };
    let result = backend.search("zzz-not-indexed", &make_test_doc(), &test_context(&filter));
    assert!(matches!(
        result,
        Err(crate::query_engine::QueryEngineError::NoResults)
    ));
}
