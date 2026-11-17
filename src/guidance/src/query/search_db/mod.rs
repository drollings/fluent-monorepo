//! Guidance search database: the P1 fragment/capability/summary/answer/graph
//! schema and its methods, composed with the generic node-document store.
//!
//! `GuidanceSearchDb` owns a shared [`VectorDocStore`](search_vector::vector_store::VectorDocStore)
//! (generic `nodes` tables) and layers the guidance extension tables on the
//! same `SqliteStore` handle, so on-disk `.db` files stay byte-identical
//! with the pre-split layout. Moved verbatim from `search-vector` (same SQL,
//! same behavior); only the home changed, per the shared-crate domain rule.

pub mod answers;
pub mod capabilities;
pub mod embedding_status;
pub mod files;
pub mod fragments;
pub mod graph_rows;
pub(crate) mod schema;
pub mod summaries;
pub mod symbols;

pub use answers::AnswerCacheRecord;
pub use capabilities::{
    build_capability_fts_match, CapabilityLemma, CapabilityRecord, ProjectCard, ScoredCapability,
    CAPABILITY_SCHEMA_VERSION,
};
pub use embedding_status::EmbeddingStatusRecord;
pub use fragments::{
    build_fts_match, FileRecord, FragmentFilter, FragmentFreshness, FragmentLemma, FragmentRecord,
    FragmentRow, ScoredFragment, SymbolRecord,
};
pub use graph_rows::{GraphEdgeRow, GraphSymbolRow};
pub use search_vector::vector_store::{SearchResult, VectorDbError, VectorDocStore};
pub use summaries::{build_summary_fts_match, SummaryRecord};

use std::path::Path;

/// Full hybrid search engine for guidance: generic node documents plus the
/// P1 fragment index (FTS5 + vectors + lemmas) that the hybrid recall path
/// queries. The generic surface delegates to the shared [`VectorDocStore`].
pub struct GuidanceSearchDb {
    docs: VectorDocStore,
}

impl GuidanceSearchDb {
    pub fn open(path: &Path) -> Result<Self, VectorDbError> {
        let docs = VectorDocStore::open(path)?;
        schema::init_guidance_schema(docs.sqlite_store())?;
        Ok(Self { docs })
    }

    pub fn open_in_memory() -> Result<Self, VectorDbError> {
        let docs = VectorDocStore::open_in_memory()?;
        schema::init_guidance_schema(docs.sqlite_store())?;
        Ok(Self { docs })
    }

    /// The shared connection handle (also hosts the generic node tables).
    #[must_use]
    pub fn sqlite_store(&self) -> &fluent_db::store::SqliteStore {
        self.docs.sqlite_store()
    }

    pub fn insert_node(
        &self,
        name: &str,
        source: &str,
        signature: Option<&str>,
        comment: Option<&str>,
        module: &str,
        language: &str,
        embedding: Option<&[f32]>,
    ) -> Result<i64, VectorDbError> {
        self.docs.insert_node(
            name, source, signature, comment, module, language, embedding,
        )
    }

    /// Rebuild the HNSW index from all embedded nodes in the database.
    pub fn rebuild_hnsw(&self) -> Result<usize, VectorDbError> {
        self.docs.rebuild_hnsw()
    }

    pub fn vector_search(
        &self,
        query_vec: &[f32],
        k: usize,
    ) -> Result<Vec<SearchResult>, VectorDbError> {
        self.docs.vector_search(query_vec, k)
    }

    pub fn keyword_search(&self, query: &str) -> Result<Vec<SearchResult>, VectorDbError> {
        self.docs.keyword_search(query)
    }

    pub fn hybrid_search(
        &self,
        query: &str,
        query_vec: Option<&[f32]>,
        k: usize,
    ) -> Result<Vec<SearchResult>, VectorDbError> {
        self.docs.hybrid_search(query, query_vec, k)
    }

    pub fn get_node_count(&self) -> Result<i64, VectorDbError> {
        self.docs.get_node_count()
    }

    pub fn get_embedding_count(&self) -> Result<i64, VectorDbError> {
        self.docs.get_embedding_count()
    }

    /// Check if the HNSW index is built.
    pub fn has_hnsw(&self) -> bool {
        self.docs.has_hnsw()
    }

    /// Get the number of points in the HNSW index.
    pub fn hnsw_len(&self) -> usize {
        self.docs.hnsw_len()
    }

    /// Whether a table exists (schema-gate input).
    pub fn has_table(&self, table: &str) -> bool {
        self.docs.has_table(table)
    }
}

#[cfg(test)]
#[path = "../../../tests/fragments.rs"]
mod fragment_tests;

#[cfg(test)]
#[path = "../../../tests/capabilities.rs"]
mod capability_tests;

#[cfg(test)]
#[path = "../../../tests/summaries.rs"]
mod summary_tests;

#[cfg(test)]
#[path = "../../../tests/graph_edges.rs"]
mod graph_edge_tests;

#[cfg(test)]
#[path = "../../../tests/file_status.rs"]
mod file_status_tests;

#[cfg(test)]
#[path = "../../../tests/node_sync.rs"]
mod node_sync_tests;

#[cfg(test)]
#[path = "../../../tests/schema.rs"]
mod schema_golden;
