//! KnowledgeStore — the cross-crate read/write vocabulary for a
//! ContentNode store.
//!
//! This is **pure vocabulary**: it must stay leaf-clean (no `fluent-wvr`
//! dependency, no `fluent-db`, no `fluent-router`). Its signatures use only
//! `fluent-types` types — `ContentNode`, `NodeId`, `KnnHit` — plus
//! `Option`/`Result`. Capability *mediation* (gating behind a token) is the
//! implementor's job, not the trait's: each implementor (coral's `Library`,
//! the router's `ContentNodeStore`) defines its own marker `Capability`
//! token and asserts it via `fluent_wvr::capability::check_capability`
//! in its own crate (the trait never references fluent-wvr).
//!
//! The name says what it is, not how it is gated: "Capability" here is
//! the long-standing storage-vocabulary name, not a `fluent-wvr`
//! capability token. `ContentNodeStore` (in `fluent-concept`) is the
//! read-only view over the same nodes (snapshot, single-tier text,
//! session membership) consumed by knowledge selectors; `KnowledgeStore`
//! is the full read/write cross-crate vocabulary (fetch, insert, KNN).
//! One store can implement both; neither implies the other.

use crate::{ContentNode, KnnHit, NodeId};

/// Errors from a `KnowledgeStore` implementation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
/// Errors from a `KnowledgeStore` implementation.
pub enum KnowledgeError {
    /// A node with the given id was not found.
    #[error("node not found: {0:?}")]
    NotFound(NodeId),
    /// The node has no embedding to search against.
    #[error("node has no embedding")]
    NoEmbedding,
    /// Serialization of a node to its durable form failed.
    #[error("serialization error: {0}")]
    Serialization(String),
    /// Any other implementation-specific failure.
    #[error("knowledge error: {0}")]
    Other(String),
}

/// The cross-crate boundary for reading and writing a `ContentNode` store.
///
/// Concrete implementations are composed at the binary (coral-context's
/// `Library`, the router's `ContentNodeStore`); a consumer never imports a concrete
/// store crate through this trait.
pub trait KnowledgeStore: Send + Sync {
    /// Fetch a node by id (`None` when absent).
    fn get_node(&self, node_id: NodeId) -> Option<ContentNode>;
    /// All nodes for a session, most recent first, capped at `limit`.
    fn get_session_nodes(&self, session_id: &str, limit: usize) -> Vec<ContentNode>;
    /// Persist a node, allocating an id when none is set. Returns the node id.
    fn insert_node(&self, node: &ContentNode) -> Result<NodeId, KnowledgeError>;
    /// Cosine KNN search over node embeddings; `k` nearest hits.
    fn knn_search(&self, embedding: &[f32], k: usize) -> Result<Vec<KnnHit>, KnowledgeError>;
}

/// Deprecated alias: use [`KnowledgeStore`] instead. The old name read
/// as a `fluent-wvr` capability token, which this vocabulary never is.
#[deprecated(note = "use KnowledgeStore instead")]
pub type KnowledgeCapability = dyn KnowledgeStore;

#[cfg(test)]
#[path = "../tests/knowledge.rs"]
mod tests;
