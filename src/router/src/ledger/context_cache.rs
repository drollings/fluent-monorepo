//! Ledger-backed deterministic context cache (C2).
//!
//! The escalation ladder consults a [`fluent_types::ContextCache`] before
//! engaging any frontier call ("the same question never pays frontier cost
//! twice"). This is the router's ledger-backed implementation: an exact
//! normalized-question match over previously *accepted* answers recorded in
//! the shared [`ContentNodeStore`] (see its Q→A index — rejected turns never
//! land there, so error text can never be served as fact).
//!
//! Session-blind by design: the lookup key is the question text alone. That
//! is why the cache lives strictly on the post-failure escalation path and
//! never in front of live dispatch — stateful follow-ups ("what is its mood
//! *now*?") must not short-circuit to a stale accepted answer.

use std::sync::Arc;

use fluent_types::{ContextCache, ContextHit};

use crate::node_store::ContentNodeStore;

/// Exact-match cache over the ledger's accepted Q→A pairs.
pub struct LedgerContextCache {
    store: Arc<ContentNodeStore>,
}

impl LedgerContextCache {
    pub fn new(store: Arc<ContentNodeStore>) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &Arc<ContentNodeStore> {
        &self.store
    }
}

impl ContextCache for LedgerContextCache {
    fn lookup(&self, query: &str) -> Option<ContextHit> {
        let (content, node_id) = self.store.accepted_answer_for(query)?;
        tracing::debug!(
            target: "router.dispatch.escalation",
            mode = "context",
            node = node_id.as_int(),
            "ledger Q→A hit — short-circuiting before any frontier call"
        );
        Some(ContextHit {
            source: "ledger-qa".into(),
            content,
            score: 1.0,
            metadata: Some(serde_json::json!({ "node_id": node_id.as_int() })),
        })
    }
}
#[cfg(test)]
#[path = "../../tests/ledger_context_cache.rs"]
mod tests;
