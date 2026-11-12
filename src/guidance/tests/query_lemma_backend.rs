use super::*;
use crate::query::db_storage::GuidanceDbStorage;
use crate::query::ingest::ingest_text_file;
use crate::query::llm_filter::{LlmFilter, NoopLlmFilter};
use crate::query::search_backend::SearchContext;
use crate::search_types::FileInfo;
use crate::tests::common::make_test_doc;

fn file_info(id: &str, relative: &str) -> FileInfo {
    FileInfo {
        id: id.to_string(),
        absolute_path: format!("/repo/{relative}"),
        relative_path: relative.to_string(),
        root_path: "/repo".to_string(),
        size_bytes: 64,
        last_modified_time: 100,
        content_hash: None,
        kind: Some(crate::search_types::FileKind::Code),
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
        concept_store: None,
    }
}

#[test]
fn lemma_backend_matches_inflections_without_a_pipeline() {
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    ingest_text_file(
        &db,
        &file_info("file-a", "src/a.ts"),
        "// the runner runs every morning\nexport const anchor = 1;\n",
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
        "// the service runs every morning\nexport const anchor = 1;\n",
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

#[test]
fn lemma_backend_attributes_node_comments_to_item_fragments() {
    use crate::extractor::{FragmentMetadata, FragmentRange, PreparedFragment};
    use crate::query::writer::{FragmentWriter, VectorPolicy};
    // Two item chunks with distinct comment words: recall must surface the
    // owning item fragment for each word, never the sibling.
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let fragment = |id: &str, name: &str, doc: &str, start_line: usize| PreparedFragment {
        id: id.to_string(),
        content_text: format!("fn {name}() {{}}"),
        embedding_text: None,
        range: FragmentRange {
            start_line,
            end_line: start_line + 2,
            start_offset: 0,
            end_offset: 12,
        },
        group: None,
        metadata: Some(FragmentMetadata::Code {
            symbol_type: crate::search_types::CodeSymbolType::Function,
            symbol_name: Some(name.to_string()),
            scope: None,
            node_type: "function_item".to_string(),
            signature: Some(format!("fn {name}()")),
            doc: Some(doc.to_string()),
            modifiers: Vec::new(),
        }),
        calls: Vec::new(),
    };
    let block = |name: &str, text: &str| crate::ast_parser::CommentBlock {
        node_key: name.to_string(),
        node_kind: "function_item".to_string(),
        name: name.to_string(),
        scope: String::new(),
        start_byte: 0,
        end_byte: text.len(),
        start_line: 1,
        end_line: 1,
        text: text.to_string(),
        comment_hash: common_core::hash::blake3_hex(text.as_bytes()),
    };
    let file = search_vector::db::FileRecord {
        id: "file-a".to_string(),
        absolute_path: "/repo/src/a.rs".to_string(),
        relative_path: "src/a.rs".to_string(),
        root_path: "/repo".to_string(),
        size_bytes: 64,
        last_modified_time: 100,
        kind: Some("code".to_string()),
        format: "rust".to_string(),
        content_hash: None,
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    };
    FragmentWriter::write_items(
        &db,
        &file,
        crate::search_types::FileKind::Code,
        &[
            fragment("fa", "alpha", "Adds one", 1),
            fragment("fb", "beta", "Removes two", 10),
        ],
        &[block("alpha", "Adds one"), block("beta", "Removes two")],
        &[],
        None,
        VectorPolicy::Optional,
    )
    .expect("commit");
    let storage = GuidanceDbStorage::new(&db);
    let filter = LlmFilter::new(Some(Box::new(NoopLlmFilter)));
    let backend = LemmaBackend {
        storage: &storage,
        nlp: None,
        limit: 5,
    };
    let stages = backend
        .search("adds", &make_test_doc(), &test_context(&filter))
        .expect("search");
    assert_eq!(stages.len(), 1, "{stages:?}");
    assert_eq!(stages[0].line, Some(1), "{stages:?}");
    let stages = backend
        .search("removes", &make_test_doc(), &test_context(&filter))
        .expect("search");
    assert_eq!(stages.len(), 1, "{stages:?}");
    assert_eq!(stages[0].line, Some(10), "{stages:?}");
}
