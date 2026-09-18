//! Reference-only view layer over the content seam.
//!
//! Moved mechanics from `fluent-router`'s `views.rs` (M10.3): views hold
//! node-id lists + per-node fidelity policy + composition, and **never own
//! text**. Text leaves the store through exactly one exit —
//! [`LedgerView::render`] → seam `lod_text` — so there is a single place
//! where a consumer can observe/transform what is rendered.
//!
//! Adaptation notes (behavior-identical):
//! - The store is `Arc<dyn ContentNodeStore>` (the concept seam), not the
//!   router's concrete store. `render` reads through `snapshot`/`lod_text`
//!   only — the `Arc`-identity remap is preserved via the snapshot's own id.
//! - `pii_redacted` stays router-side: its transform is the router's
//!   write-path guard (`scrub_for_ledger`), which must not move. Compose
//!   `FilteredLedger::with_transform` there.
//!
//! Two concrete views ship:
//!
//! - [`ParallelLedger`] — one store, N views sharing the same `Arc`; each view
//!   picks its own `Lod` (with per-node `with_override`). A lazily-derived tier
//!   is computed **at most once** on the shared node and visible to every view.
//! - [`FilteredLedger<V>`] — composes any inner view with an exclusion set and
//!   an optional render transform.
//!
//! Rendering degrades to LOD0 when a lazy tier is missing or un-derivable
//! rather than erroring the whole render.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use fluent_concept::content_store::ContentNodeStore;
use fluent_types::NodeId;

use crate::lod::LOD0_FULL_TEXT;

pub use crate::lod::Lod;

/// A reference-only view over the shared store. Views hold node-id lists and
/// configuration, never owned text. `render()` is provided and is the
/// **only** way text leaves a view.
pub trait LedgerView: Send + Sync {
    /// The shared store this view reads from.
    fn store(&self) -> &Arc<dyn ContentNodeStore>;

    /// The node ids this view covers, in render order (a snapshot).
    fn node_ids(&self) -> Vec<NodeId>;

    /// Per-view fidelity policy: the `Lod` a given node renders at.
    fn lod_for(&self, id: NodeId) -> Lod;

    /// Filtering hook: `true` excludes the node from `render` without touching
    /// the store (no LOD computation).
    fn exclude(&self, _id: NodeId) -> bool {
        false
    }

    /// Render hook: post-process the text just before it exits the view.
    fn transform(&self, _id: NodeId, text: String) -> String {
        text
    }

    /// Render every non-excluded node's text at its fidelity tier, joined by
    /// newlines. The single text-exit from the store. Nodes missing from the
    /// store are skipped (never an empty line, never an error).
    fn render(&self) -> String {
        let store = self.store();
        self.node_ids()
            .into_iter()
            .filter(|id| !self.exclude(*id))
            .filter_map(|id| {
                // The snapshot's own id wins when stamped (mirrors the
                // concrete store's Arc-identity remap); missing → skipped.
                let snap = store.snapshot(id)?;
                Some(snap.id.unwrap_or(id))
            })
            .map(|id| {
                let lod = self.lod_for(id);
                let text = store
                    .lod_text(id, lod.as_u8())
                    .or_else(|| store.lod_text(id, LOD0_FULL_TEXT)) // degrade to LOD0
                    .unwrap_or_default();
                self.transform(id, text)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A parallel view over one store: every `ParallelLedger` shares the same
/// `Arc`, so a lazily-derived tier is computed once and visible to
/// all views. `default_lod` is LOD1; per-node `with_override` wins.
pub struct ParallelLedger {
    store: Arc<dyn ContentNodeStore>,
    node_ids: Vec<NodeId>,
    default_lod: Lod,
    /// Per-node fidelity from day one.
    overrides: HashMap<NodeId, Lod>,
}

impl ParallelLedger {
    /// A view over `store` with no node ids yet. Use `for_session` (or the
    /// builder methods) to populate. Generic over the seam implementor so
    /// concrete `Arc<Store>` call sites coerce without annotation.
    pub fn new(store: Arc<impl ContentNodeStore + 'static>) -> Self {
        Self {
            store,
            node_ids: Vec::new(),
            default_lod: Lod::LOD1,
            overrides: HashMap::new(),
        }
    }

    /// Set the default fidelity tier for every node without an override.
    #[must_use]
    pub fn with_default_lod(mut self, lod: Lod) -> Self {
        self.default_lod = lod;
        self
    }

    /// Override a single node's fidelity tier (wins over `default_lod`).
    #[must_use]
    pub fn with_override(mut self, id: NodeId, lod: Lod) -> Self {
        self.overrides.insert(id, lod);
        self
    }

    /// A view over a whole session, in insertion order.
    pub fn for_session(store: Arc<impl ContentNodeStore + 'static>, session_id: &str) -> Self {
        let node_ids = store.session_node_ids(session_id);
        Self::new(store).with_node_ids(node_ids)
    }

    fn with_node_ids(mut self, node_ids: Vec<NodeId>) -> Self {
        self.node_ids = node_ids;
        self
    }
}

impl LedgerView for ParallelLedger {
    fn store(&self) -> &Arc<dyn ContentNodeStore> {
        &self.store
    }

    fn node_ids(&self) -> Vec<NodeId> {
        self.node_ids.clone()
    }

    fn lod_for(&self, id: NodeId) -> Lod {
        self.overrides.get(&id).copied().unwrap_or(self.default_lod)
    }
}

/// Composes any inner view with an exclusion set and an optional render
/// transform. Filtering is free: excluded nodes never trigger LOD derivation.
pub struct FilteredLedger<V> {
    inner: V,
    excluded: HashSet<NodeId>,
    transform: Option<Box<dyn Fn(NodeId, String) -> String + Send + Sync>>,
}

impl<V: LedgerView> FilteredLedger<V> {
    pub fn new<S: std::hash::BuildHasher>(inner: V, excluded: HashSet<NodeId, S>) -> Self {
        Self {
            inner,
            excluded: excluded.into_iter().collect(),
            transform: None,
        }
    }

    #[must_use]
    pub fn with_transform<F>(mut self, f: F) -> Self
    where
        F: Fn(NodeId, String) -> String + Send + Sync + 'static,
    {
        self.transform = Some(Box::new(f));
        self
    }
}

impl<V: LedgerView> LedgerView for FilteredLedger<V> {
    fn store(&self) -> &Arc<dyn ContentNodeStore> {
        self.inner.store()
    }

    fn node_ids(&self) -> Vec<NodeId> {
        self.inner.node_ids()
    }

    fn lod_for(&self, id: NodeId) -> Lod {
        self.inner.lod_for(id)
    }

    fn exclude(&self, id: NodeId) -> bool {
        self.excluded.contains(&id)
    }

    fn transform(&self, id: NodeId, text: String) -> String {
        self.transform
            .as_ref()
            .map_or(text.clone(), |f| f(id, text))
    }
}

#[cfg(test)]
#[path = "../tests/views.rs"]
mod tests;
