//! The neutral shared home for concept lookup (moved here from `spacy-rs`).
//!
//! [`ConceptStore`](crate::concept_store::ConceptStore) is the seam between the
//! pure resolver (`spacy-rs` `InterlinguaResolver`) and whatever backend serves
//! it. All three backends are **homes, not owners**, of the trait:
//!
//! - [`InMemoryConceptStore`](crate::concept_store_mem::InMemoryConceptStore)
//!   (here) — the hermetic test double, never production;
//! - the router's `SqliteConceptStore` (`interlingua_concepts`) — the durable
//!   materialized index;
//! - coral's content-addressed graph (`context_nodes`, keyed by the full
//!   64-bit `hash_iri`) — the durable graph.
//!
//! Boot reconciliation (one loader, two durable homes) locks the homes equal.
//!
//! This crate depends on `fluent-types` + `fluent-dag` (+ `common-core`
//! string shaping, `fluent-db`/`rusqlite`/`serde_json` for the annotation
//! linkage — all leaves w.r.t. this crate), so `spacy-rs`, the router, and
//! coral depend on it without a cycle.

#![forbid(unsafe_code)]

pub mod annotation_store;
pub mod canonical;
pub mod concept_store;
pub mod concept_store_mem;
pub mod content_store;
pub mod entity;
pub mod inference;
pub mod loader;
pub mod mapper;
pub mod migration;
pub mod normalize;
pub mod plausibility;
pub mod prefix;
pub mod taxonomy;
pub(crate) mod turtle;
pub mod yago;
pub mod yago_view;
pub mod ysm1;

pub use annotation_store::{AnnotationStore, ANNOTATIONS_TABLE};
pub use concept_store::{ConceptStore, ConceptStoreError, ConceptStoreState, TaxonomyHierarchy};
pub use concept_store_mem::InMemoryConceptStore;
pub use content_store::{
    derive_label, ensure_lod_eager, ContentNodeStore, InMemoryContentNodeStore,
};
pub use loader::{
    canonical_class_name, yago_class_id, LoadStats, RegistryEntry, YaGoLoader,
    EMBEDDED_YAGO_CLASSES,
};
pub use plausibility::{PlausibilityTriple, ScoredLemma};
pub use yago_view::YagoView;

#[cfg(test)]
#[path = "../tests/cleanup_baseline.rs"]
mod cleanup_baseline_tests;

#[cfg(test)]
#[path = "../tests/yago_runtime.rs"]
mod yago_runtime_tests;
