//! The canonical HNSW-backed vector index store.
//!
//! `HnswIndex` owns the cosine-distance `Hnsw` index plus its external-id
//! mapping (`id_map`: HNSW `d_id` → caller node id), and exposes
//! `insert`/`rebuild_from`/`search`/`is_built`/`len`/`id_map_snapshot`. It
//! generalizes the two near-verbatim copies that `GuidanceDb`
//! (`search-vector/src/db.rs`) and coral's `Library`
//! (`coral/src/db/hnsw.rs`) previously each maintained by hand.
//!
//! ## Lock ordering (R9)
//!
//! **Never nested.** `insert` reserves the external id under the id-map
//! guard first, releases it, then takes the index write guard for the point
//! insert; `rebuild_from` builds off-guard and swaps each finished structure
//! in under its own guard in turn; `search`/`search_with_ids` hold only the
//! index read guard across the traversal. A `generation` seqlock (even =
//! stable) spans every multi-structure mutation so readers detect a swap
//! that landed mid-episode and retry once instead of resolving hits against
//! the wrong map. The database connection lock, when held, is taken *after*
//! both index guards (`index → map → conn`). This mirrors the documented
//! discipline in `coral/src/db/hnsw.rs`.

use std::sync::{Mutex, RwLock};

use anndists::dist::DistCosine;
use common_core::constants::{HnswParams, DEFAULT_HNSW_THRESHOLD};
use common_core::sync::{lock, lock_read, lock_write};
use hnsw_rs::hnsw::Hnsw;

use crate::error::DbError;

/// Single adaptive-dispatch policy for HNSW-vs-brute-force routing (M6).
///
/// This is a policy, not an index: it owns the threshold and answers whether
/// a store of `len` vectors should consult a built HNSW graph. The one
/// threshold source is [`DEFAULT_HNSW_THRESHOLD`]; there is no second
/// threshold type — call sites reuse `HnswParams`/`DEFAULT_HNSW_THRESHOLD`
/// through this struct.
///
/// Axis: **[B] cost/recall only.** This gate measures corpus scale (where the
/// HNSW approximation pays off — recall≥0.95 at N≥512 per M5a). It is NOT a
/// confidence signal and must never gate verification, persistence, or
/// frontier escalation (those are [A]/[B]-outcome decisions owned by the
/// workflow/rubric gates calibrated in M5c–M5d).
///
/// [`DEFAULT_HNSW_THRESHOLD`]: common_core::constants::DEFAULT_HNSW_THRESHOLD
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Single adaptive-dispatch policy for HNSW-vs-brute-force routing (M6).
/// 
/// This is a policy, not an index: it owns the threshold and answers whether
/// a store of `len` vectors should consult a built HNSW graph. The one
/// threshold source is [`DEFAULT_HNSW_THRESHOLD`]; there is no second
/// threshold type — call sites reuse `HnswParams`/`DEFAULT_HNSW_THRESHOLD`
/// through this struct.
/// 
/// Axis: **[B] cost/recall only.** This gate measures corpus scale (where the
/// HNSW approximation pays off — recall≥0.95 at N≥512 per M5a). It is NOT a
/// confidence signal and must never gate verification, persistence, or
/// frontier escalation (those are [A]/[B]-outcome decisions owned by the
/// workflow/rubric gates calibrated in M5c–M5d).
/// 
/// [`DEFAULT_HNSW_THRESHOLD`]: common_core::constants::DEFAULT_HNSW_THRESHOLD
pub struct AdaptiveHnsw {
    /// Corpus size above which a built HNSW graph is consulted. Strict `>`:
    /// `len == threshold` stays brute-force.
    pub threshold: usize,
}

impl AdaptiveHnsw {
    /// Policy with an explicit threshold (tests, unusual corpora).
    pub fn new(threshold: usize) -> Self {
        Self { threshold }
    }

    /// Whether a store holding `len` vectors should use its built HNSW graph
    /// (`len > threshold`).
    pub fn should_use_built(&self, len: usize) -> bool {
        len > self.threshold
    }

    /// Query-time dispatch: probe HNSW iff the index is built *and* the corpus
    /// is above threshold. `false` means "take the call-site brute-force
    /// fallback" — the fallback bodies stay call-site code (M2), this only
    /// answers which path to take.
    pub fn dispatch(&self, hnsw_built: bool, len: usize) -> bool {
        hnsw_built && self.should_use_built(len)
    }
}

impl Default for AdaptiveHnsw {
    fn default() -> Self {
        Self {
            threshold: DEFAULT_HNSW_THRESHOLD,
        }
    }
}

/// A single HNSW index handle — the named index path owned by a store (e.g.
/// the router's `ChartStore` workflow_library index). Moved here from
/// `fluent_router::hnsw` (D7, router shim deleted in M3): the `db` crate is
/// the canonical home for the HNSW surface (`HnswIndex`, `hnsw_lookup`), and
/// this handle is plain data (no router dependency) so it belongs with that
/// surface. Consume it as `fluent_db::hnsw::HnswIndexHandle` directly.
#[derive(Debug, Clone)]
/// A single HNSW index handle — the named index path owned by a store (e.g.
/// the router's `ChartStore` workflow_library index). Moved here from
/// `fluent_router::hnsw` (D7, router shim deleted in M3): the `db` crate is
/// the canonical home for the HNSW surface (`HnswIndex`, `hnsw_lookup`), and
/// this handle is plain data (no router dependency) so it belongs with that
/// surface. Consume it as `fluent_db::hnsw::HnswIndexHandle` directly.
pub struct HnswIndexHandle {
    pub name: String,
    pub path: String,
}

/// A generic HNSW-backed vector index with external-id mapping.
///
/// The index is built lazily on first `insert` (mirroring the historical
/// `get_or_insert_with` behavior) and wholesale-rebuilt by `rebuild_from`.
///
/// Concurrency: a `generation` seqlock (even = stable) guards torn reads.
/// Writers bump it odd before mutating either structure and even after both
/// are swapped; readers (`search_with_ids`) snapshot it around the
/// traverse-then-resolve episode and retry once on a mismatch, degrading to
/// the caller's brute-force fallback (`None`) when the race persists. No
/// lock is ever held across the index traversal.
#[derive(Default)]
/// A generic HNSW-backed vector index with external-id mapping.
/// 
/// The index is built lazily on first `insert` (mirroring the historical
/// `get_or_insert_with` behavior) and wholesale-rebuilt by `rebuild_from`.
/// 
/// Concurrency: a `generation` seqlock (even = stable) guards torn reads.
/// Writers bump it odd before mutating either structure and even after both
/// are swapped; readers (`search_with_ids`) snapshot it around the
/// traverse-then-resolve episode and retry once on a mismatch, degrading to
/// the caller's brute-force fallback (`None`) when the race persists. No
/// lock is ever held across the index traversal.
pub struct HnswIndex {
    hnsw: RwLock<Option<Hnsw<'static, f32, DistCosine>>>,
    id_map: Mutex<Vec<i64>>,
    generation: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for HnswIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HnswIndex")
            .field("built", &self.is_built())
            .field("len", &self.len())
            .finish()
    }
}

impl HnswIndex {
    /// An empty, unbuilt index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `node_id` with embedding `embedding`, creating the index on
    /// first use. Returns the external index (`d_id`) assigned to the point.
    ///
    /// Lock order: `id_map`, then `hnsw` (write) — briefly, never nested.
    /// The external id is reserved under the `id_map` lock first so the
    /// point insert never holds one guard while acquiring the other; the
    /// generation seqlock spans both mutations so concurrent readers retry
    /// instead of resolving a half-inserted point.
    pub fn insert(&self, node_id: i64, embedding: &[f32]) -> usize {
        use std::sync::atomic::Ordering;
        self.generation.fetch_add(1, Ordering::AcqRel);
        let external_id = {
            let mut id_map = lock(&self.id_map);
            let idx = id_map.len();
            id_map.push(node_id);
            idx
        };

        {
            let mut guard = lock_write(&self.hnsw);
            let hnsw = guard.get_or_insert_with(|| {
                let p = HnswParams::default();
                crate::sqlite::make_hnsw(&p, p.initial_capacity)
            });
            hnsw.insert((embedding, external_id));
        }

        self.generation.fetch_add(1, Ordering::AcqRel);
        external_id
    }
    /// Rebuild the index from a table scan of `(node_id, embedding_blob)`
    /// rows. `decode` turns each blob into an `Option<Vec<f32>>` embedding —
    /// `None` (or an empty vector) skips the row. Returns the total number of
    /// rows examined (matching the historical `rows.len()` return).
    ///
    /// Rows stream straight from the caller's iterator — decoded inline, no
    /// intermediate collection (the index grows from `initial_capacity` as
    /// points arrive). Lock order: neither guard is held during the build;
    /// the finished structures swap in under each guard in turn, inside one
    /// generation-seqlock episode.
    pub fn rebuild_from(
        &self,
        rows: impl Iterator<Item = (i64, Vec<u8>)>,
        decode: fn(&[u8]) -> Option<Vec<f32>>,
    ) -> Result<usize, DbError> {
        use std::sync::atomic::Ordering;
        let p = HnswParams::default();
        let hnsw = crate::sqlite::make_hnsw(&p, p.initial_capacity);

        let mut id_map = Vec::new();
        let mut examined = 0;
        for (node_id, blob) in rows {
            examined += 1;
            if let Some(embedding) = decode(&blob) {
                if !embedding.is_empty() {
                    hnsw.insert((&embedding, id_map.len()));
                    id_map.push(node_id);
                }
            }
        }

        self.generation.fetch_add(1, Ordering::AcqRel);
        *lock_write(&self.hnsw) = Some(hnsw);
        *lock(&self.id_map) = id_map;
        self.generation.fetch_add(1, Ordering::AcqRel);

        Ok(examined)
    }

    /// Approximate nearest-neighbour search. Returns `(external idx, distance)`
    /// pairs — resolve each `idx` through `id_map` to recover the caller's
    /// node id.
    ///
    /// Only the index read guard is held, and only for the traversal itself
    /// — never the id-map guard alongside it. Pairs from a bare `search`
    /// are therefore generation-qualified: a concurrent `rebuild_from` may
    /// have swapped the map they resolve against. Prefer
    /// [`HnswIndex::search_with_ids`] (or [`hnsw_lookup`]), which re-checks
    /// the generation and retries, when the ids must belong to one
    /// generation.
    pub fn search(&self, query: &[f32], k: usize) -> Vec<(usize, f32)> {
        let guard = lock_read(&self.hnsw);
        let Some(hnsw) = guard.as_ref() else {
            return Vec::new();
        };

        let ef = (k * 4).max(64);
        hnsw.search(query, k, ef)
            .into_iter()
            .map(|n| (n.d_id, n.distance))
            .collect()
    }

    /// Traverse the index and resolve hits to caller keys in one
    /// generation-checked episode: snapshot the generation, traverse under
    /// the index read guard, snapshot the id map, and re-check the
    /// generation. On a mismatch (a rebuild or insert landed mid-episode)
    /// retry once, then return `None` — the caller takes its exact
    /// brute-force fallback through the existing `None` semantics, with no
    /// new fallback protocol. `None` also covers the unbuilt/empty index
    /// and fully unresolvable hits, exactly as before.
    pub fn search_with_ids(&self, query: &[f32], k: usize) -> Option<Vec<(i64, f32)>> {
        use std::sync::atomic::Ordering;
        for _ in 0..2 {
            let generation = self.generation.load(Ordering::Acquire);
            if generation % 2 != 0 {
                continue; // a writer is mid-mutation; retry once
            }
            let hits: Vec<(usize, f32)> = {
                let guard = lock_read(&self.hnsw);
                let hnsw = guard.as_ref()?;
                let ef = (k * 4).max(64);
                hnsw.search(query, k, ef)
                    .into_iter()
                    .map(|n| (n.d_id, n.distance))
                    .collect()
            };
            // Resolve only the `k` hits under a short id-map guard — no
            // full-map clone. The index read guard is already released, so
            // the `index → map` lock order holds and the map is never held
            // across the traversal.
            let resolved: Vec<(i64, f32)> = {
                let id_map = lock(&self.id_map);
                hits.into_iter()
                    .filter_map(|(d_id, distance)| id_map.get(d_id).map(|key| (*key, distance)))
                    .collect()
            };
            if self.generation.load(Ordering::Acquire) != generation {
                continue; // swapped mid-episode; retry once
            }
            if resolved.is_empty() {
                return None;
            }
            return Some(resolved);
        }
        None
    }

    /// Whether the index has been built (any `insert`/`rebuild_from` ran).
    pub fn is_built(&self) -> bool {
        lock_read(&self.hnsw).is_some()
    }

    /// Number of points currently in the index.
    pub fn len(&self) -> usize {
        lock_read(&self.hnsw).as_ref().map_or(0, Hnsw::get_nb_point)
    }

    /// Whether the index contains no points.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A snapshot of the external-id map (`d_id` → caller node id).
    pub fn id_map_snapshot(&self) -> Vec<i64> {
        lock(&self.id_map).clone()
    }
}

/// Probe a possibly-unbuilt [`HnswIndex`], resolving external ids to the
/// caller keys the index was built with.
///
/// M5: the one shared HNSW-then-brute-force stanza. Returns `Some((key,
/// distance))` pairs in HNSW order (cosine distance, NOT similarity — each
/// site applies its own `1 - dist` mapping), or `None` when the caller must
/// use its exact brute-force fallback: the index is unbuilt, the probe is
/// empty (`k == 0`, malformed query), or no hit resolves through the id map.
///
/// Deliberately lookup-only: the fallback bodies stay call-site code because
/// sort/truncate/re-score/filter semantics differ per store (unifying them
/// would be a behavior change). Compose as
/// `hnsw_lookup(...).map_or_else(fallback, scored)` — see
/// `node_store::knn_search`, `charts::store::search`,
/// `ledger::workflow_store::nearest` for the three production shapes.
pub fn hnsw_lookup(hnsw: &HnswIndex, query: &[f32], k: usize) -> Option<Vec<(i64, f32)>> {
    if !hnsw.is_built() {
        return None;
    }
    if query.is_empty() || k == 0 {
        // Malformed probe: an empty query panics inside the distance kernel
        // (dim assertion), so short-circuit to the brute-force fallback
        // signal instead of touching the index.
        return None;
    }
    // One generation-checked traverse-and-resolve episode (no second full
    // `id_map_snapshot` clone): the lookup resolves against the same map
    // generation it traversed.
    hnsw.search_with_ids(query, k)
}

#[cfg(all(test, feature = "sqlite"))]
#[path = "../tests/hnsw.rs"]
mod tests;