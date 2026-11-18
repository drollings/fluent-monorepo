//! Fidelity selection over the content seam.
//!
//! Moved mechanics from `fluent-router`'s `ledger/prompt.rs` (M10.2):
//! the interchangeable seam behind relevance ranking (cosine today;
//! BM25/HNSW candidates plug in here without touching assembly or the
//! budget fit). Adapted from `&ContentNodeStore` (concrete) to
//! `&dyn ContentNodeStore` (concept seam) — the only change; ranking math
//! is byte-identical.
//!
//! Axis statement: a selector score is **producer confidence** (how close an
//! embedding is); the byte budget and the worker's role-need are
//! **task-value** (what the worker needs to decide). Selection never
//! surfaces a node on score alone past budget — the assembler enforces that.
//!
//! Contract: deterministic (same inputs ⇒ same outputs, ties in view
//! order), pure (no I/O, no LOD derivation — assembly must stay free), and
//! total over the band (every returned `Lod` lies within `min..=max`).

use fluent_concept::content_store::ContentNodeStore;
use fluent_types::NodeId;

use crate::lod::Lod;
use crate::lod_policy::LodSpec;

/// The fidelity-selection interface: assign each intermediate node a target
/// fidelity inside the band, most-relevant-first.
pub trait FidelitySelector: Send + Sync {
    /// Rank `intermediates` most-relevant-first with fidelity targets.
    fn select(
        &self,
        store: &dyn ContentNodeStore,
        intermediates: &[NodeId],
        band: &LodSpec,
    ) -> Vec<(NodeId, Lod)>;
}

/// Cosine-rank selection: sort by cosine similarity to the query embedding
/// descending (most relevant → finest), nodes without an embedding last,
/// interpolated across the band. The current relevance-ranked mechanism,
/// extracted verbatim.
#[derive(Debug, Clone)]
pub struct CosineRankSelector {
    query: Vec<f32>,
}

impl CosineRankSelector {
    /// A selector ranking by similarity to `query`.
    pub fn new(query: Vec<f32>) -> Self {
        Self { query }
    }
}

impl FidelitySelector for CosineRankSelector {
    fn select(
        &self,
        store: &dyn ContentNodeStore,
        intermediates: &[NodeId],
        band: &LodSpec,
    ) -> Vec<(NodeId, Lod)> {
        let mut ranked: Vec<(NodeId, f32)> = intermediates
            .iter()
            .map(|id| {
                let sim = store
                    .snapshot(*id)
                    .and_then(|n| {
                        n.embedding
                            .as_ref()
                            .map(|e| vector_math::cosine_similarity_f32(&self.query, e))
                    })
                    .unwrap_or(f32::MIN);
                (*id, sim)
            })
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked
            .into_iter()
            .enumerate()
            .map(|(r, (id, _))| (id, band.interpolate(r, intermediates.len())))
            .collect()
    }
}

/// Uniform selection: every intermediate at the band midpoint. The current
/// no-signal mechanism, extracted verbatim.
#[derive(Debug, Clone, Copy, Default)]
pub struct UniformSelector;

impl FidelitySelector for UniformSelector {
    fn select(
        &self,
        _store: &dyn ContentNodeStore,
        intermediates: &[NodeId],
        band: &LodSpec,
    ) -> Vec<(NodeId, Lod)> {
        let mid = band.midpoint();
        intermediates.iter().map(|id| (*id, mid)).collect()
    }
}

#[cfg(test)]
#[path = "../tests/selector.rs"]
mod tests;
