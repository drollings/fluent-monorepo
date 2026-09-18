//! Content-node store seam + eager LOD policy + in-memory double.
//!
//! Moved mechanics from `fluent-router`'s `node_store.rs` (M10.4): the read
//! seam the LOD machinery (`fluent-knowledge`) is generic over, the eager-fill
//! policy (`ensure_lod_eager`/`derive_label`), and the hermetic double —
//! following the existing `ConceptStore`/`InMemoryConceptStore` pattern.
//! The sqlite home stays in the router and implements this trait; lazy
//! LOD1–4 derivation (summarizer/LLM) stays router-side with it.
//!
//! Lazy contract the double and the sqlite home share: LOD1–4 read
//! cached-or-absent — never derived on read without a summarizer (the
//! router's `NoSummarizer` fail-open, mirrored here as `None`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::RwLock;

use fluent_types::{ContentHash, ContentNode, NodeId, OriginRole, LOD5_LABEL};

/// The read seam over a content-node store: snapshots, single-tier text, and
/// session membership. Writes, lazy derivation, sqlite, and model calls live
/// behind implementors (the router's durable store); `fluent-knowledge`'s
/// selectors, assembler, and views are generic over this trait only.
pub trait ContentNodeStore: Send + Sync {
    /// A full clone of the node, if present.
    fn snapshot(&self, id: NodeId) -> Option<ContentNode>;
    /// One tier's text: eager tiers read direct; lazy tiers read
    /// cached-or-absent (fail-open `None`, never an error, never a derivation
    /// without a summarizer).
    fn lod_text(&self, id: NodeId, level: u8) -> Option<String>;
    /// Node ids for a session, in insertion order.
    fn session_node_ids(&self, session_id: &str) -> Vec<NodeId>;
}

/// Derive a display label from `content`: its first sentence, UTF-8-truncated
/// to 64 bytes (ellipsis past the cut); the role when content is empty.
#[must_use]
pub fn derive_label(role: &str, content: &str) -> String {
    let sentence = common_core::string::first_sentence(content);
    let snippet = if sentence.is_empty() {
        common_core::string::truncate_utf8(content, 64)
    } else {
        common_core::string::truncate_utf8(&sentence, 64)
    };
    if snippet.is_empty() {
        role.to_string()
    } else {
        snippet
    }
}

/// Guarantee LOD0 (full text) and LOD5 (label) are present on a node.
/// Verbatim eager policy: content-hash stamped from LOD0 (empty content maps
/// to the unstamped sentinel and derives nothing); LOD5 from `derive_label`
/// when empty; `active_lod` defaults to LOD0.
pub fn ensure_lod_eager(node: &mut ContentNode) {
    while node.lod.len() < LOD5_LABEL as usize + 1 {
        node.lod.push(String::new());
    }
    let content = node.lod[fluent_types::LOD0_FULL_TEXT as usize].clone();
    // The content hash is the annotation keying domain: it tracks
    // LOD0 exactly — a mutation that changes LOD0 changes the hash and
    // thereby invalidates cached annotations. Empty content → unstamped.
    node.content_hash = ContentHash::from_lod0(&content);
    if content.is_empty() {
        // Nothing to derive LOD0 from — nothing to do.
        return;
    }
    if node.lod[LOD5_LABEL as usize].is_empty() {
        let role = node
            .role
            .as_ref()
            .map(OriginRole::as_str)
            .unwrap_or_default()
            .to_string();
        node.lod[LOD5_LABEL as usize] = derive_label(&role, &content);
    }
    if node.active_lod.is_none() {
        node.active_lod = Some(fluent_types::LOD0_FULL_TEXT);
    }
}

/// Hermetic test double: `HashMap`-backed nodes with verbatim eager fill on
/// insert and cached-or-absent lazy reads. Never production (mirrors
/// `InMemoryConceptStore`'s role beside `ConceptStore`).
#[derive(Debug, Default)]
pub struct InMemoryContentNodeStore {
    nodes: RwLock<HashMap<NodeId, ContentNode>>,
    next_id: AtomicI64,
}

impl InMemoryContentNodeStore {
    /// An empty double (ids allocate from 1).
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a node with eager LOD0/LOD5 fill; returns the assigned id.
    pub fn insert(&self, mut node: ContentNode) -> NodeId {
        let id = NodeId::from_int(self.next_id.fetch_add(1, Ordering::SeqCst) + 1);
        node.id = Some(id);
        ensure_lod_eager(&mut node);
        self.nodes
            .write()
            .expect("in-memory store lock")
            .insert(id, node);
        id
    }

    /// Overwrite one tier (test control for cached lazy tiers / markers).
    pub fn set_tier(&self, id: NodeId, level: u8, text: impl Into<String>) {
        if let Some(node) = self
            .nodes
            .write()
            .expect("in-memory store lock")
            .get_mut(&id)
        {
            while node.lod.len() <= level as usize {
                node.lod.push(String::new());
            }
            node.lod[level as usize] = text.into();
        }
    }
}

impl ContentNodeStore for InMemoryContentNodeStore {
    fn snapshot(&self, id: NodeId) -> Option<ContentNode> {
        self.nodes
            .read()
            .expect("in-memory store lock")
            .get(&id)
            .cloned()
    }

    fn lod_text(&self, id: NodeId, level: u8) -> Option<String> {
        let nodes = self.nodes.read().expect("in-memory store lock");
        let node = nodes.get(&id)?;
        let text = node.lod.get(level as usize)?.clone();
        if text.is_empty() { None } else { Some(text) }
    }

    fn session_node_ids(&self, session_id: &str) -> Vec<NodeId> {
        let nodes = self.nodes.read().expect("in-memory store lock");
        let mut ids: Vec<(i64, NodeId)> = nodes
            .iter()
            .filter(|(_, n)| n.session_id.as_deref() == Some(session_id))
            .map(|(id, _)| (id.as_int(), *id))
            .collect();
        ids.sort_by_key(|(n, _)| *n);
        ids.into_iter().map(|(_, id)| id).collect()
    }
}

#[cfg(test)]
#[path = "../tests/content_store.rs"]
mod tests;
