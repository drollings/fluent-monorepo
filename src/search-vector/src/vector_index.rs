//! Test-support vector-store seam: the `VectorStore` trait and its two
//! implementations.
//!
//! Moved verbatim from `fluent-router`'s `testing/vector_index.rs` (M7.3):
//! no production caller consumes this trait (ranking/retrieval use
//! ledger-backed signals, not vectors), so it lives here as a recall harness
//! and test double — HNSW recall is measured against the byte-identical
//! brute-force ground truth, never asserted on absolute values.
//!
//! This trait is the stateful **store seam** (`insert`/`search`/`len`,
//! `NodeId`-keyed, similarity-mapped) — deliberately distinct from
//! `fluent_db::vector::VectorIndex::knn`, the stateless **math seam** (the
//! shared name was a collision; the store side is `VectorStore`). Both
//! implementations compose the shared primitives
//! (`fluent_db::hnsw::HnswIndex` + `fluent_db::vector::knn_brute_force`)
//! instead of re-implementing top-K or cosine math — no second HNSW.

use std::sync::Mutex;

use fluent_db::hnsw::HnswIndex;
use fluent_types::NodeId;

pub trait VectorStore: Send + Sync {
    fn insert(&self, id: NodeId, emb: &[f32]) -> Result<(), fluent_db::error::DbError>;
    fn search(&self, query: &[f32], k: usize) -> Vec<(NodeId, f64)>;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Back-compat shim for the pre-rename `VectorIndex` store-trait name: a
/// deprecated supertrait with a blanket impl, so both `impl VectorIndex`
/// and `dyn VectorIndex` positions keep compiling (with a warning).
/// `fluent_db::vector::VectorIndex` (the math seam) keeps that name; new code
/// uses `VectorStore`. Remove next minor.
#[deprecated(
    note = "use VectorStore: VectorIndex unambiguously means fluent_db::vector::VectorIndex"
)]
pub trait VectorIndex: VectorStore {}
#[allow(deprecated)]
impl<T: VectorStore> VectorIndex for T {}

/// Byte-identical brute-force implementation — the ground-truth recall
/// baseline for the HNSW harness.
pub struct BruteForceIndex {
    entries: Mutex<Vec<(NodeId, Vec<f32>)>>,
}

impl BruteForceIndex {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }
}

impl Default for BruteForceIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl VectorStore for BruteForceIndex {
    fn insert(&self, id: NodeId, emb: &[f32]) -> Result<(), fluent_db::error::DbError> {
        common_core::sync::lock(&self.entries).push((id, emb.to_vec()));
        Ok(())
    }
    fn search(&self, query: &[f32], k: usize) -> Vec<(NodeId, f64)> {
        let entries = common_core::sync::lock(&self.entries);
        let results = fluent_db::vector::knn_brute_force(
            query,
            entries.iter().map(|(id, e)| (*id, e.as_slice())),
            k,
        );
        fluent_db::vector::scored_hits(results, fluent_db::vector::distance_to_similarity)
            .into_iter()
            .map(|(id, similarity)| (id, f64::from(similarity)))
            .collect()
        // distance = 1 - cosine_similarity, so similarity = 1 - dist
    }
    fn len(&self) -> usize {
        common_core::sync::lock(&self.entries).len()
    }
}

/// HNSW-backed ANN index composing `fluent_db::hnsw::HnswIndex`.
pub struct HnswVectorIndex {
    inner: HnswIndex,
}

impl HnswVectorIndex {
    pub fn new() -> Self {
        Self {
            inner: HnswIndex::new(),
        }
    }
    pub fn is_built(&self) -> bool {
        self.inner.is_built()
    }
}

impl Default for HnswVectorIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl VectorStore for HnswVectorIndex {
    fn insert(&self, id: NodeId, emb: &[f32]) -> Result<(), fluent_db::error::DbError> {
        self.inner.insert(id.as_int(), emb);
        Ok(())
    }
    fn search(&self, query: &[f32], k: usize) -> Vec<(NodeId, f64)> {
        // The HNSW probe + id resolution is the shared
        // `fluent_db::hnsw::hnsw_lookup` (`None` = no fallback here — the
        // test seam returns whatever the graph yields, empty when unbuilt).
        fluent_db::hnsw::hnsw_lookup(&self.inner, query, k)
            .map(|resolved| {
                fluent_db::vector::scored_hits(resolved, fluent_db::vector::distance_to_similarity)
                    .into_iter()
                    .map(|(raw, similarity)| (NodeId::from_int(raw), f64::from(similarity)))
                    .collect()
            })
            .unwrap_or_default()
    }
    fn len(&self) -> usize {
        self.inner.len()
    }
}

#[cfg(test)]
#[path = "../tests/vector_index.rs"]
mod tests;
