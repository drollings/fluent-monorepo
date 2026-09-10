use super::*;
use crate::query::db_storage::GuidanceDbStorage;
use crate::query::ingest::ingest_text_file;
use crate::query::llm_filter::{LlmFilter, NoopLlmFilter};
use crate::query::search_backend::SearchContext;
use crate::tests::common::make_test_doc;
use crate::zg_types::FileInfo;

// Deterministic 2-D embedder: texts mentioning "alpha" point at [1,0],
// everything else at [0,1] — the `FakeEmbeddingModel` role.

struct TestEmbedder;

impl fluent_llm::embeddings::EmbeddingProvider for TestEmbedder {
    fn name(&self) -> &'static str {
        "test-embedder"
    }

    fn dimensions(&self) -> u32 {
        2
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, fluent_llm::embeddings::EmbeddingError> {
        Ok(if text.to_lowercase().contains("alpha") {
            vec![1.0, 0.0]
        } else {
            vec![0.0, 1.0]
        })
    }

    fn embed_batch(
        &self,
        texts: &[&str],
    ) -> Result<fluent_llm::embeddings::BatchEmbedding, fluent_llm::embeddings::EmbeddingError>
    {
        let mut flat = Vec::with_capacity(texts.len() * 2);
        for text in texts {
            flat.extend(self.embed(text)?);
        }
        Ok(fluent_llm::embeddings::BatchEmbedding {
            flat,
            count: texts.len(),
            dims: 2,
        })
    }
}

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

#[test]
fn vector_backend_ranks_by_embedding_similarity() {
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let embedder = TestEmbedder;
    ingest_text_file(
        &db,
        &file_info("file-a", "src/a.ts"),
        "export function AlphaSymbol() { return 1; }\n",
        Some(&embedder),
        None,
    )
    .expect("ingest a");
    ingest_text_file(
        &db,
        &file_info("file-b", "src/b.ts"),
        "export function BetaSymbol() { return 2; }\n",
        Some(&embedder),
        None,
    )
    .expect("ingest b");
    let storage = GuidanceDbStorage::new(&db);
    let filter = LlmFilter::new(Some(Box::new(NoopLlmFilter)));
    let ctx = SearchContext {
        storage: None,
        embedder: None,
        nlp: None,
        llm_filter: &filter,
    };
    let backend = VectorBackend {
        storage: &storage,
        embedder: Some(&embedder),
        limit: 5,
    };
    let stages = backend
        .search("alpha query", &make_test_doc(), &ctx)
        .expect("search");
    assert!(!stages.is_empty());
    assert_eq!(stages[0].source, "src/a.ts");
}

#[test]
fn vector_backend_declines_without_an_embedder() {
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let storage = GuidanceDbStorage::new(&db);
    let filter = LlmFilter::new(Some(Box::new(NoopLlmFilter)));
    let ctx = SearchContext {
        storage: None,
        embedder: None,
        nlp: None,
        llm_filter: &filter,
    };
    let backend = VectorBackend {
        storage: &storage,
        embedder: None,
        limit: 5,
    };
    // Empty index + no embedder → NoResults (L3 degradation, not an error).
    let result = backend.search("anything", &make_test_doc(), &ctx);
    assert!(matches!(
        result,
        Err(crate::query_engine::QueryEngineError::NoResults)
    ));
}
