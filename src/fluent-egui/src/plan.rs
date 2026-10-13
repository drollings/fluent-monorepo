//! DAG-ordered pane compute plan plus a fingerprint-keyed series cache.
//!
//! Frames stay dumb: a [`PanePlan`] orders caller-registered pane nodes
//! through the canonical [`DependencyGraph`] (each pane provides its own
//! name, the same self-provision convention kernel specs use) and records
//! replay checkpoints in a [`CheckpointedStepGraph`], so readiness,
//! cancellation scope, and rewind all come from one graph instead of a
//! hand-rolled sorter. The domain topology (which panes exist, how they
//! wire) is poured in by the caller — this module owns the ordering
//! machinery, never the trading shape.
//!
//! [`PaneCache`] stores computed series keyed by caller-provided fingerprints
//! hashed once through the shared FNV-1a helper, with content-hash
//! backdating: republishing bit-identical values (identical `NaN` payloads
//! included) keeps the revision, so the frame path blits cached revisions
//! and recompute touches only dirty nodes. Cached revisions are confidence
//! bookkeeping only — they never make a signal tradeable.

use std::collections::{HashMap, HashSet};

use fluent_dag::checkpointed::CheckpointedStepGraph;
use fluent_dag::dep_graph::GraphError;

use crate::error::EguiError;

/// Carry a graph refusal across the crate boundary without a second error type.
fn describe_graph(error: &GraphError) -> EguiError {
    EguiError::Dependency { reason: error.to_string() }
}

/// DAG-ordered pane compute plan over caller-registered nodes.
///
/// Nodes are named panes with dependency edges; checkpoints mark replay
/// positions for the replay controller; per-step state is the pane's
/// parameter fingerprint.
pub struct PanePlan {
    steps: CheckpointedStepGraph<String, u64>,
}

impl PanePlan {
    /// Create an empty plan; register nodes before ordering.
    #[must_use]
    pub fn new() -> Self {
        Self { steps: CheckpointedStepGraph::new() }
    }

    /// Register one pane node; each node provides its own name. Surfaces the
    /// graph's duplicate refusal instead of panicking.
    pub fn register_node(
        &mut self,
        name: &str,
        deps: &[&str],
        fingerprint: u64,
    ) -> Result<(), EguiError> {
        let owned: Vec<String> = deps.iter().map(|d| (*d).to_owned()).collect();
        self.steps.add_step(name.to_owned(), &owned, fingerprint).map_err(|e| describe_graph(&e))
    }

    /// Whether `name` is a registered pane.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.steps.status(&name.to_owned()).is_some()
    }

    /// Direct dependencies of one pane (empty for roots and unknown names).
    #[must_use]
    pub fn dependencies(&self, name: &str) -> Vec<String> {
        self.steps.graph().deps_of(&name.to_owned()).map_or_else(Vec::new, <[String]>::to_vec)
    }

    /// Topologically sorted pane names: dependencies always precede
    /// dependents. The frame schedules recompute waves in this order.
    pub fn order(&self) -> Result<Vec<String>, EguiError> {
        self.steps.graph().topo_sort().map_err(|e| describe_graph(&e))
    }

    /// Panes whose dependencies are all complete, in plan order.
    #[must_use]
    pub fn ready_steps(&self) -> Vec<String> {
        self.steps.ready_steps()
    }

    /// Whether every dependency of `name` is complete.
    #[must_use]
    pub fn is_ready(&self, name: &str) -> bool {
        self.steps.is_ready(&name.to_owned())
    }

    /// Whether `name` is complete.
    #[must_use]
    pub fn is_completed(&self, name: &str) -> bool {
        self.steps.is_completed(&name.to_owned())
    }

    /// Record `name` as computed; its dependents become ready.
    pub fn complete(&mut self, name: &str) {
        self.steps.complete(&name.to_owned());
    }

    /// Transitive dependents of `name`: the cancellation scope when `name`
    /// fails. The failed node itself is not included.
    #[must_use]
    pub fn cancel_dependents(&self, name: &str) -> Vec<String> {
        self.steps.cancel_dependents(&name.to_owned())
    }

    /// Dirty set for a failed node: the node itself plus its transitive
    /// dependents, in plan order. Recompute touches exactly these panes.
    #[must_use]
    pub fn dirty_set(&self, name: &str) -> Vec<String> {
        let owned = name.to_owned();
        let mut dirty: HashSet<String> = self.cancel_dependents(name).into_iter().collect();
        dirty.insert(owned);
        if let Ok(order) = self.steps.graph().topo_sort() {
            return order.into_iter().filter(|node| dirty.contains(node)).collect();
        }
        let mut fallback: Vec<String> = dirty.into_iter().collect();
        fallback.sort();
        fallback
    }

    /// Mark `name` as a replay checkpoint; fails for unregistered names.
    pub fn checkpoint(&mut self, name: &str) -> Result<(), EguiError> {
        self.steps.checkpoint(name.to_owned()).map_err(|e| describe_graph(&e))
    }

    /// Whether `name` is a recorded checkpoint.
    #[must_use]
    pub fn is_checkpoint(&self, name: &str) -> bool {
        self.steps.is_checkpoint(&name.to_owned())
    }

    /// Rewind to checkpoint `name`: returns the ordered steps from `name`
    /// onward (inclusive) and un-completes them for re-run.
    pub fn rewind_to(&mut self, name: &str) -> Result<Vec<String>, EguiError> {
        self.steps.rewind_to(&name.to_owned()).map_err(|e| describe_graph(&e))
    }

    /// Record a pane's parameter fingerprint; fails for unregistered names.
    pub fn set_fingerprint(&mut self, name: &str, fingerprint: u64) -> Result<(), EguiError> {
        match self.steps.state_mut(&name.to_owned()) {
            Some(slot) => {
                *slot = fingerprint;
                Ok(())
            }
            None => Err(EguiError::Dependency { reason: format!("node not found: {name:?}") }),
        }
    }

    /// A pane's parameter fingerprint, if registered.
    #[must_use]
    pub fn fingerprint(&self, name: &str) -> Option<u64> {
        self.steps.status(&name.to_owned()).copied()
    }
}

impl Default for PanePlan {
    fn default() -> Self {
        Self::new()
    }
}

/// Cache key for one pane: length-prefixed node name plus the caller's
/// parameter fingerprint, hashed once through the shared FNV-1a helper (the
/// same byte idiom parameter fingerprints use).
#[must_use]
pub fn cache_key(node: &str, fingerprint: u64) -> u64 {    let mut bytes = Vec::with_capacity(8 + node.len() + 8);
    bytes.extend_from_slice(&(node.len() as u64).to_le_bytes());
    bytes.extend_from_slice(node.as_bytes());
    bytes.extend_from_slice(&fingerprint.to_le_bytes());
    common_core::hash::fnv1a64(&bytes)
}

/// `None`-valued slot as a hash word: all-bits-set, a payload no resolved
/// output leg carries.
const NONE_WORD: u64 = u64::MAX;

/// Content hash over display slots: every slot as fixed-width little-endian
/// bits (`None` as the sentinel word), hashed once through the shared
/// FNV-1a helper. Every distinct bit pattern hashes distinctly, so
/// bit-identical payloads — identical `NaN` payloads included — backdate.
fn series_hash(values: &[Option<f64>]) -> u64 {
    let mut bytes = Vec::with_capacity(8 * values.len());
    for slot in values {
        let word = slot.map_or(NONE_WORD, f64::to_bits);
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    common_core::hash::fnv1a64(&bytes)
}

/// One cached series plus the revision bookkeeping that makes backdating work.
#[derive(Debug, Clone)]
pub struct CachedSeries {
    /// Per-bar display values with holes preserved.
    pub values: Vec<Option<f64>>,
    /// Store revision at the last compute whose values differed.
    pub revision: u64,
    /// Content hash of `values` at the last compute.
    pub values_hash: u64,
}

/// Fingerprint-keyed series store with content-hash backdating.
///
/// The frame path reads revisions; recompute publishes only dirty panes, and
/// republishing identical content keeps the revision so downstream waves
/// skip cleanly.
#[derive(Debug, Default)]
pub struct PaneCache {
    entries: HashMap<u64, CachedSeries>,
    revision: u64,
}

impl PaneCache {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish one pane's series, returning its revision. Bit-identical
    /// content backdates (revision unchanged); changed content bumps the
    /// store revision and records it on the entry.
    pub fn publish(&mut self, key: u64, values: Vec<Option<f64>>) -> u64 {
        let hash = series_hash(&values);
        if let Some(entry) = self.entries.get_mut(&key) {
            if entry.values_hash == hash {
                return entry.revision;
            }
            self.revision += 1;
            entry.values = values;
            entry.values_hash = hash;
            entry.revision = self.revision;
            return entry.revision;
        }
        self.revision += 1;
        let revision = self.revision;
        self.entries.insert(key, CachedSeries { values, revision, values_hash: hash });
        revision
    }

    /// Borrow one cached series.
    #[must_use]
    pub fn get(&self, key: u64) -> Option<&CachedSeries> {
        self.entries.get(&key)
    }

    /// Number of cached series.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no series.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
#[path = "../tests/plan.rs"]
mod tests;
