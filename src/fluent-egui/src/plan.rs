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

use std::collections::HashSet;
use std::sync::Arc;

use common_core::cache::ArcLoadCache;
use fluent_dag::checkpointed::CheckpointedStepGraph;
use fluent_dag::dep_graph::GraphError;

use crate::error::EguiError;

/// Carry a graph refusal across the crate boundary without a second error type.
fn describe_graph(error: &GraphError) -> EguiError {
    EguiError::Dependency {
        reason: error.to_string(),
    }
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
        Self {
            steps: CheckpointedStepGraph::new(),
        }
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
        self.steps
            .add_step(name.to_owned(), &owned, fingerprint)
            .map_err(|e| describe_graph(&e))
    }

    /// Whether `name` is a registered pane.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.steps.status(&name.to_owned()).is_some()
    }

    /// Direct dependencies of one pane (empty for roots and unknown names).
    #[must_use]
    pub fn dependencies(&self, name: &str) -> Vec<String> {
        self.steps
            .graph()
            .deps_of(&name.to_owned())
            .map_or_else(Vec::new, <[String]>::to_vec)
    }

    /// Topologically sorted pane names: dependencies always precede
    /// dependents. The frame schedules recompute waves in this order.
    pub fn order(&self) -> Result<Vec<String>, EguiError> {
        self.steps
            .graph()
            .topo_sort()
            .map_err(|e| describe_graph(&e))
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
            return order
                .into_iter()
                .filter(|node| dirty.contains(node))
                .collect();
        }
        let mut fallback: Vec<String> = dirty.into_iter().collect();
        fallback.sort();
        fallback
    }

    /// Mark `name` as a replay checkpoint; fails for unregistered names.
    pub fn checkpoint(&mut self, name: &str) -> Result<(), EguiError> {
        self.steps
            .checkpoint(name.to_owned())
            .map_err(|e| describe_graph(&e))
    }

    /// Whether `name` is a recorded checkpoint.
    #[must_use]
    pub fn is_checkpoint(&self, name: &str) -> bool {
        self.steps.is_checkpoint(&name.to_owned())
    }

    /// Rewind to checkpoint `name`: returns the ordered steps from `name`
    /// onward (inclusive) and un-completes them for re-run.
    pub fn rewind_to(&mut self, name: &str) -> Result<Vec<String>, EguiError> {
        self.steps
            .rewind_to(&name.to_owned())
            .map_err(|e| describe_graph(&e))
    }

    /// Record a pane's parameter fingerprint; fails for unregistered names.
    pub fn set_fingerprint(&mut self, name: &str, fingerprint: u64) -> Result<(), EguiError> {
        match self.steps.state_mut(&name.to_owned()) {
            Some(slot) => {
                *slot = fingerprint;
                Ok(())
            }
            None => Err(EguiError::Dependency {
                reason: format!("node not found: {name:?}"),
            }),
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
/// parameter fingerprint, hashed once through the shared length-framed
/// builder (the same byte idiom parameter fingerprints use: the `u64`
/// fingerprint rides as one fixed-width payload). The length prefix removes
/// framing ambiguity — name/fingerprint splits that concatenate identically
/// would otherwise key identically — but, like [`series_hash`] below, this
/// does not claim hash injectivity: FNV-1a-64 can collide, so a key names
/// the pane and parameters it was built from short of a collision.
#[must_use]
pub fn cache_key(node: &str, fingerprint: u64) -> u64 {
    common_core::hash::fingerprint_framed(&[(node, &fingerprint.to_le_bytes())])
}

/// Slot tags in the content hash: a hole encodes distinctly from every
/// resolved leg, including any `NaN` payload.
const NONE_TAG: u8 = 0;
const SOME_TAG: u8 = 1;

/// Content hash over display slots: every slot as a one-byte tag plus
/// fixed-width little-endian bits (`None` carries zero payload bits),
/// hashed once through the shared FNV-1a helper. Correctness (key
/// identity): `None` and any `Some(NaN)` — `f64::from_bits(u64::MAX)`
/// included — hash differently, while bit-identical payloads (identical
/// `NaN` payloads included) hash identically and backdate. This removes the
/// sentinel alias; it does not claim hash injectivity.
fn series_hash(values: &[Option<f64>]) -> u64 {
    let mut bytes = Vec::with_capacity(9 * values.len());
    for slot in values {
        match slot {
            None => {
                bytes.push(NONE_TAG);
                bytes.extend_from_slice(&0_u64.to_le_bytes());
            }
            Some(value) => {
                bytes.push(SOME_TAG);
                bytes.extend_from_slice(&value.to_bits().to_le_bytes());
            }
        }
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

/// Default pane capacity. Task value (residency): a frame publishes a
/// handful of panes (price, ribbon, delta, oscillators), so 64 holds the
/// working set plus replay and scratch headroom while bounding
/// replay-driven growth.
pub const DEFAULT_PANE_CACHE_CAPACITY: usize = 64;

/// Fingerprint-keyed series store with content-hash backdating, bounded by
/// the shared LRU.
///
/// Entries ride the shared [`ArcLoadCache`](common_core::cache::ArcLoadCache)
/// — one cache shape, not a second cloning one — so reads return an
/// `Arc`-shared entry (a refcount bump, never a series clone) whether the
/// frame pins, publishes, or replays. The frame path reads revisions;
/// recompute publishes only dirty panes, and republishing identical content
/// keeps the revision so downstream waves skip cleanly. Eviction drops the
/// least-recently-used entry, which reads back as a miss (recompute) —
/// never as a stale revision. The frame calls [`pin`](Self::pin) once per
/// frame so a working set not larger than capacity never misses.
/// Write-through only: entries arrive via [`publish`](Self::publish); the
/// load closure never runs.
pub struct PaneCache {
    entries: ArcLoadCache<u64, CachedSeries, String>,
    revision: u64,
}

impl PaneCache {
    /// Create an empty store with the default capacity.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_PANE_CACHE_CAPACITY)
            .expect("default pane cache capacity is a non-zero constant")
    }

    /// Create an empty store holding up to `capacity` entries; refuses zero.
    pub fn with_capacity(capacity: usize) -> Result<Self, EguiError> {
        let entries =
            ArcLoadCache::new(capacity, never_load).map_err(|reason| EguiError::InvalidParam {
                what: "capacity".to_owned(),
                reason,
            })?;
        Ok(Self {
            entries,
            revision: 0,
        })
    }

    /// Refresh LRU order for the current working set: every present key
    /// becomes most-recently-used, so transient publishes between frames
    /// evict scratch entries first. Absent keys are ignored — pinning never
    /// inserts. The read promotes the key and its shared handle drops
    /// without touching the series. Call once per frame before publishing.
    pub fn pin(&self, keys: &[u64]) {
        for key in keys {
            let _touched = self.entries.get(key);
        }
    }

    /// Maximum entries before the least-recently-used entry is evicted.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.entries.capacity()
    }

    /// Publish one pane's series, returning its revision. Bit-identical
    /// content backdates (revision unchanged); changed content bumps the
    /// store revision and records it on the entry. The existence probe
    /// reads through the shared `Arc`, so an unchanged republish clones no
    /// series either.
    pub fn publish(&mut self, key: u64, values: Vec<Option<f64>>) -> u64 {
        let hash = series_hash(&values);
        if let Some(entry) = self.entries.get(&key) {
            if entry.values_hash == hash {
                return entry.revision;
            }
        }
        self.revision += 1;
        let revision = self.revision;
        self.entries.insert_owned(
            key,
            CachedSeries {
                values,
                revision,
                values_hash: hash,
            },
        );
        revision
    }

    /// Read one cached series, if present. `Arc`-shared, so an entry
    /// evicted after the read can never surface as a stale borrow — and the
    /// read itself clones no series.
    #[must_use]
    pub fn get(&self, key: u64) -> Option<Arc<CachedSeries>> {
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

/// Write-through load: the frame path only inserts computed series, so a
/// mistaken `get_or_load` fails closed instead of synthesizing a value.
/// Takes the key by reference because `ArcLoadCache::new` dictates the shape.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn never_load(_key: &u64) -> Result<Arc<CachedSeries>, String> {
    Err("pane cache is write-through; load is never called".to_owned())
}

impl Default for PaneCache {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PaneCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaneCache")
            .field("len", &self.entries.len())
            .field("capacity", &self.entries.capacity())
            .field("revision", &self.revision)
            .finish()
    }
}

#[cfg(test)]
#[path = "../tests/plan.rs"]
mod tests;
