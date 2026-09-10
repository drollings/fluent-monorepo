use super::*;
use crate::query::db_storage::GuidanceDbStorage;
use crate::query::ingest::ingest_text_file;
use crate::query::llm_filter::{LlmFilter, NoopLlmFilter};
use crate::query::search_backend::SearchContext;
use crate::tests::common::make_test_doc;
use crate::zg_types::FileInfo;

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

fn test_context(filter: &LlmFilter) -> SearchContext<'_> {
    SearchContext {
        storage: None,
        embedder: None,
        nlp: None,
        llm_filter: filter,
    }
}

#[test]
fn lemma_backend_matches_inflections_without_a_pipeline() {
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    ingest_text_file(
        &db,
        &file_info("file-a", "src/a.ts"),
        "the runner runs every morning\n",
        None,
        None,
    )
    .expect("ingest");
    let storage = GuidanceDbStorage::new(&db);
    let filter = LlmFilter::new(Some(Box::new(NoopLlmFilter)));
    let backend = LemmaBackend {
        storage: &storage,
        nlp: None,
        limit: 5,
    };
    // Fallback normalization: "runs" is stored verbatim, so an exact-token
    // query hits without any model.
    let stages = backend
        .search("runs", &make_test_doc(), &test_context(&filter))
        .expect("search");
    assert!(!stages.is_empty());
    assert_eq!(stages[0].source, "src/a.ts");
}

#[test]
fn lemma_backend_collapses_inflections_with_spacy() {
    let nlp = spacy_rs::pipeline::NlpPipeline::en_default().expect("pipeline");
    // Rule lemmatizer in sentence context: "runs" shares the lemma "run".
    let lemmas = crate::query::ingest::query_lemmas(Some(&nlp), "the service runs every morning");
    assert!(lemmas.contains(&"run".to_string()), "lemmas: {lemmas:?}");
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    ingest_text_file(
        &db,
        &file_info("file-a", "src/a.ts"),
        "the service runs every morning\n",
        None,
        Some(&nlp),
    )
    .expect("ingest");
    let storage = GuidanceDbStorage::new(&db);
    let filter = LlmFilter::new(Some(Box::new(NoopLlmFilter)));
    let backend = LemmaBackend {
        storage: &storage,
        nlp: Some(&nlp),
        limit: 5,
    };
    // "running" never appears in the text — only its sentence-context
    // lemma does ("service is running" normalizes to service/be/run).
    let stages = backend
        .search(
            "service is running",
            &make_test_doc(),
            &test_context(&filter),
        )
        .expect("search");
    assert!(!stages.is_empty());
    assert_eq!(stages[0].source, "src/a.ts");
}

#[test]
fn lemma_backend_declines_on_empty_normalization() {
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let storage = GuidanceDbStorage::new(&db);
    let filter = LlmFilter::new(Some(Box::new(NoopLlmFilter)));
    let backend = LemmaBackend {
        storage: &storage,
        nlp: None,
        limit: 5,
    };
    let result = backend.search("!!!", &make_test_doc(), &test_context(&filter));
    assert!(matches!(
        result,
        Err(crate::query_engine::QueryEngineError::NoResults)
    ));
}
