//! The single source of truth for concept lookup (neutral home
//! `fluent-concept` — moved here from `spacy-rs`).
//!
//! [`InterlinguaResolver`](https://docs.rs/spacy-rs) (in `spacy-rs`) consumes
//! it; `YaGoLoader` (in `guidance-ontology`) feeds it; the router implements
//! the SQLite backend; [`InMemoryConceptStore`](crate::concept_store_mem::InMemoryConceptStore)
//! is the hermetic test double. **No second registry anywhere.**
//!
//! All IDs are content-addressed (`trunc48(hash(content))`); a store only
//! *records* which canonicals claim which ids (first-wins, §2.3) and resolves
//! names/IRIs back to ids.

use fluent_dag::hierarchy::HierarchyIndex;
use fluent_types::{ConceptMetadata, InterlinguaId};

/// Store readiness — `Loading` until batched WAL completes, `Ready` thereafter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
/// Store readiness — `Loading` until batched WAL completes, `Ready` thereafter.
pub enum ConceptStoreState {
    #[default]
    Ready,
    Loading,
}

/// Errors surfaced by a [`ConceptStore`].
#[derive(Debug, thiserror::Error)]
/// Errors surfaced by a [`ConceptStore`].
pub enum ConceptStoreError {
    #[error("concept not found: {0}")]
    NotFound(String),
    #[error("storage error: {0}")]
    Storage(String),
}

/// A registry of concepts keyed by content-addressed [`InterlinguaId`]s. The
/// trait is the seam between the pure resolver and whatever backend serves it
/// (in-memory test double here; SQLite in the router; coral's durable graph).
pub trait ConceptStore: Send + Sync {
    /// The metadata for `id`, or `NotFound`.
    fn get(&self, id: InterlinguaId) -> Result<ConceptMetadata, ConceptStoreError>;

    /// Resolve a canonical name (e.g. `"schema:Person"`) to its id.
    fn resolve_name(&self, name: &str) -> Result<InterlinguaId, ConceptStoreError>;

    /// Resolve a YaGO IRI (e.g. `"http://yago-knowledge.org/resource/Person"`)
    /// to its id.
    fn resolve_yago_iri(&self, iri: &str) -> Result<InterlinguaId, ConceptStoreError>;

    /// Register `meta`. First-wins: a colliding canonical for an already-taken
    /// id is kept alongside, not rejected (and never mutates the id).
    fn insert(&self, meta: ConceptMetadata) -> Result<(), ConceptStoreError>;

    /// Whether `id` is registered.
    fn contains(&self, id: InterlinguaId) -> bool;

    /// Iterate registered ids (used to pre-warm resolver caches and to audit).
    fn iter_ids(&self) -> Box<dyn Iterator<Item = InterlinguaId> + '_>;

    /// Every registered canonical under `id` (a bucket may hold several
    /// canonicals that truncated to the same local id — first-wins keeps them
    /// alongside, §2.3). The first entry is the incumbent canonical. Defaults
    /// to empty for stores that do not track colliding canonicals; a frame
    /// extractor reads it to detect predicate polysemy (a lemma id resolving
    /// to >1 candidate).
    fn candidates(&self, id: InterlinguaId) -> Vec<ConceptMetadata> {
        let _ = id;
        Vec::new()
    }

    /// All ancestors of `id` up the subclass chain, nearest first.
    fn ancestors_of(&self, id: InterlinguaId) -> Vec<InterlinguaId>;

    /// Transitive `rdfs:subClassOf` check (identity returns true).
    fn is_subclass_of(&self, child: InterlinguaId, parent: InterlinguaId) -> bool;

    /// Store readiness — default `Ready`; `SqliteConceptStore` overrides to `Loading` while batched.
    fn state(&self) -> ConceptStoreState {
        ConceptStoreState::Ready
    }
}

/// A subclass hierarchy keyed by [`InterlinguaId`], built once at boot from
/// the `rdfs:subClassOf` edges. Composes the shared
/// [`HierarchyIndex`] (the workspace's single hierarchy-closure primitive); it
/// backs [`ConceptStore::ancestors_of`]/[`ConceptStore::is_subclass_of`] on
/// every store.
///
/// The coral `is_a()` CTE remains the authoritative *durable* traversal over
/// `context_nodes`; this is the runtime mirror built from the same edges.
pub struct TaxonomyHierarchy {
    index: HierarchyIndex<InterlinguaId>,
}

impl Default for TaxonomyHierarchy {
    fn default() -> Self {
        Self {
            index: HierarchyIndex::new(),
        }
    }
}

impl TaxonomyHierarchy {
    /// Build from `(child ← parent)` subclass edges, so
    /// [`ancestors`](Self::ancestors) yields the transitive superclass chain,
    /// nearest first. Both endpoints are registered — a parent that only ever
    /// appears as an object is still queryable.
    pub fn from_edges(edges: &[(InterlinguaId, InterlinguaId)]) -> Result<Self, ConceptStoreError> {
        Ok(Self {
            index: HierarchyIndex::from_edges(edges.iter().copied()),
        })
    }

    /// All transitive superclasses of `id`, nearest first, cycle-resilient.
    /// Empty when `id` is unregistered or has no ancestors.
    pub fn ancestors(&self, id: InterlinguaId) -> Vec<InterlinguaId> {
        self.index.ancestors(&id)
    }

    /// Transitive `rdfs:subClassOf` check; identity returns true.
    pub fn is_subclass(&self, child: InterlinguaId, parent: InterlinguaId) -> bool {
        self.index.is_subclass_of(&child, &parent)
    }

    /// Whether `id` is a registered node in the hierarchy.
    pub fn contains(&self, id: InterlinguaId) -> bool {
        self.index.contains(&id)
    }
}

#[cfg(test)]
#[path = "../tests/concept_store.rs"]
mod tests;