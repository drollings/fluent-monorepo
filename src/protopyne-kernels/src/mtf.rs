//! Multi-timeframe alignment primitives over millisecond grids.
//!
//! Every timeframe the prototype serves is a fixed millisecond span (see
//! [`Timeframe`](protopyne_types::Timeframe)), so cross-timeframe work is
//! grid arithmetic: floor a stamp to its bucket, test whether the bucket is
//! fully formed, merge timestamped tapes, pair two legs on equal stamps.
//! These helpers are the single home for that math, shared by the native
//! bar path ([`bars`](crate::bars)) and the server stacker — no second
//! floor/merge/join may be introduced elsewhere.
//!
//! Grids are [`NonZeroI64`](std::num::NonZeroI64) on hot paths (a dead grid
//! is unrepresentable, never a runtime check per call); the checked
//! constructor ([`floor_to_grid_checked`]) and [`BucketSpec`] validate at
//! the boundary. Inputs are assumed ascending by timestamp where merging or
//! gap-scanning; order is the caller's contract, documented per function.

use std::collections::HashMap;
use std::fmt;
use std::num::NonZeroI64;

use fluent_dag::dep_graph::{DependencyGraph, GraphError};
use fluent_wvr::prelude::*;

use crate::bars::{bucket_bar_ranges, fold_bucket_range, project_bars, ClosedBar};
use crate::error::KernelError;
use crate::indicator::{IndicatorId, IndicatorRegistry, Ohlcv, OwnedOhlcv, ParamSet};

/// Floor a millisecond stamp to its grid bucket (`floor(ts / grid) * grid`,
/// Euclidean, so pre-epoch stamps land in their own bucket, never in the
/// future one). Mirrors the Pine candle-index math.
#[must_use]
pub fn floor_to_grid(ts_ms: i64, grid: NonZeroI64) -> i64 {
    ts_ms.div_euclid(grid.get()) * grid.get()
}

/// Checked [`floor_to_grid`]: a non-positive grid refuses with a reason
/// instead of dividing by zero or flipping sign.
pub fn floor_to_grid_checked(ts_ms: i64, grid_ms: i64) -> Result<i64, KernelError> {
    match NonZeroI64::new(grid_ms) {
        Some(grid) if grid_ms > 0 => Ok(floor_to_grid(ts_ms, grid)),
        _ => Err(KernelError::InvalidInput(format!(
            "grid must be a positive span, got {grid_ms}"
        ))),
    }
}

/// Whether the bucket opening at `bucket_open_ms` is fully formed at
/// `now_ms`: its end (`open + grid`) has passed. The lookahead guard —
/// unconfirmed buckets are never projected.
#[must_use]
pub fn fully_formed(bucket_open_ms: i64, grid: NonZeroI64, now_ms: i64) -> bool {
    bucket_open_ms.saturating_add(grid.get()) <= now_ms
}

/// Merge two ascending timestamped streams, keeping every item; ties keep
/// both with the earlier side's item first. Order within each side is
/// preserved (stable); the inputs must already ascend.
#[must_use]
pub fn interleave_sorted_by<T: Clone>(a: &[T], b: &[T], ms: impl Fn(&T) -> i64) -> Vec<T> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        // Ties keep both with the earlier side first: consuming `a` on
        // equality leaves `b[j]` to follow once `a` advances past it.
        match ms(&a[i]).cmp(&ms(&b[j])) {
            std::cmp::Ordering::Greater => {
                out.push(b[j].clone());
                j += 1;
            }
            std::cmp::Ordering::Less | std::cmp::Ordering::Equal => {
                out.push(a[i].clone());
                i += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

/// Merge two ascending `(stamp_ms, value)` tapes, keeping every point; ties
/// keep both with the earlier side's point first.
#[must_use]
pub fn interleave_sorted(a: &[(i64, f64)], b: &[(i64, f64)]) -> Vec<(i64, f64)> {
    interleave_sorted_by(a, b, |row| row.0)
}

/// Merge two ascending timestamped streams with the overlap contract: on
/// equal stamps the later side's item wins (tick tapes key on time; tapes
/// carry no price comparison). Inputs must already ascend.
#[must_use]
pub fn merge_sorted_by<T: Clone>(a: &[T], b: &[T], ms: impl Fn(&T) -> i64) -> Vec<T> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match ms(&a[i]).cmp(&ms(&b[j])) {
            std::cmp::Ordering::Less => {
                out.push(a[i].clone());
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                out.push(b[j].clone());
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                out.push(b[j].clone());
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

/// Merge two ascending `(stamp_ms, value)` tapes with the overlap contract:
/// on equal stamps the later tape's point wins.
#[must_use]
pub fn merge_sorted(a: &[(i64, f64)], b: &[(i64, f64)]) -> Vec<(i64, f64)> {
    merge_sorted_by(a, b, |row| row.0)
}

/// Inner-join two ascending timestamped streams on equal stamps, in stamp
/// order: the cross-timeframe pairing behind higher-timeframe diffs. Stamps
/// present on one side only pair with nothing and are skipped.
#[must_use]
pub fn pairs_from_sorted<T: Clone>(a: &[T], b: &[T], ms: impl Fn(&T) -> i64) -> Vec<(T, T)> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match ms(&a[i]).cmp(&ms(&b[j])) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push((a[i].clone(), b[j].clone()));
                i += 1;
                j += 1;
            }
        }
    }
    out
}

/// Candle index per stamp (`floor(ts / grid)`), in input order.
#[must_use]
pub fn bucket_ids(tss: &[i64], grid: NonZeroI64) -> Vec<i64> {
    tss.iter().map(|ts| ts.div_euclid(grid.get())).collect()
}

/// Whether ascending stamps skip a bucket: any consecutive index jump over
/// one reads as a gap (the sparseHole the resampler leaves empty).
#[must_use]
pub fn bucket_has_gap(tss: &[i64], grid: NonZeroI64) -> bool {
    let ids = bucket_ids(tss, grid);
    ids.windows(2).any(|pair| pair[1] - pair[0] > 1)
}

/// Whether a stamp is already grid-aligned: floors are fixed points, so
/// `floor(floor(ts)) == floor(ts)` always holds — this names the check for
/// the bucket round-trip property.
#[must_use]
pub fn grid_roundtrip(ts_ms: i64, grid: NonZeroI64) -> bool {
    floor_to_grid(floor_to_grid(ts_ms, grid), grid) == floor_to_grid(ts_ms, grid)
}

/// Smallest grid-aligned stamp at or above `bound_ms` (ceiling to grid):
/// the aligned split/train edge at or after a bound.
#[must_use]
pub fn aligned_geq(bound_ms: i64, grid: NonZeroI64) -> i64 {
    let step = grid.get();
    bound_ms.div_euclid(step) * step
        + if bound_ms.rem_euclid(step) == 0 {
            0
        } else {
            step
        }
}

/// Validate a train/test split: the grid is positive, the split sits on it,
/// and it falls strictly inside `(first, last]` — both sides keep bars.
/// Anything else refuses with a reason.
pub fn validate_split(
    first_ms: i64,
    last_ms: i64,
    split_ms: i64,
    grid_ms: i64,
) -> Result<(), KernelError> {
    if grid_ms <= 0 {
        return Err(KernelError::InvalidInput(format!(
            "split grid must be a positive span, got {grid_ms}"
        )));
    }
    if first_ms > last_ms {
        return Err(KernelError::InvalidInput(format!(
            "split range inverted: first {first_ms} after last {last_ms}"
        )));
    }
    if split_ms.rem_euclid(grid_ms) != 0 {
        return Err(KernelError::InvalidInput(format!(
            "split {split_ms} sits off the {grid_ms}ms grid"
        )));
    }
    if split_ms < first_ms || split_ms >= last_ms {
        return Err(KernelError::InvalidInput(format!(
            "split {split_ms} leaves a side empty over [{first_ms}, {last_ms}]"
        )));
    }
    Ok(())
}

/// Lookahead policy for projections: confirmed buckets only, or refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookahead {
    /// Project confirmed buckets only; unconfirmed tails are excluded.
    Off,
    /// Project including unconfirmed data: refused fail-closed, never
    /// silently mixed into confirmed output.
    On,
}

impl Lookahead {
    /// Refuse [`Lookahead::On`] with a reason; [`Lookahead::Off`] passes.
    pub fn require_off(self) -> Result<(), KernelError> {
        match self {
            Self::Off => Ok(()),
            Self::On => Err(KernelError::InvalidInput(
                "lookahead projection refused: unconfirmed data never mixes into confirmed output"
                    .to_owned(),
            )),
        }
    }
}

/// Validated bucket-grid handle for supervised assembly: the boundary form
/// of a raw millisecond grid. Construction is plain (validation lives in
/// [`Validate`](fluent_wvr::validate::Validate), bridged through
/// [`check_for_execute`](fluent_wvr::validate::check_for_execute) at execution),
/// so invalid specs exist just long enough to refuse loudly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BucketSpec {
    grid_ms: i64,
}

impl BucketSpec {
    /// Record a grid; call [`validate`](Self::validate) (or build through a
    /// validated path) before hot use.
    #[must_use]
    pub fn new(grid_ms: i64) -> Self {
        Self { grid_ms }
    }

    /// Recorded grid span in milliseconds.
    #[must_use]
    pub fn grid_ms(self) -> i64 {
        self.grid_ms
    }

    /// Hot-path grid when valid.
    #[must_use]
    pub fn nonzero_grid(self) -> Option<NonZeroI64> {
        NonZeroI64::new(self.grid_ms).filter(|_| self.grid_ms > 0)
    }
}

impl fluent_wvr::validate::Validate for BucketSpec {
    type Err = KernelError;

    fn validate(&self) -> Result<(), Self::Err> {
        if self.grid_ms <= 0 {
            return Err(KernelError::InvalidInput(format!(
                "bucket grid must be a positive span, got {}",
                self.grid_ms
            )));
        }
        if self.grid_ms % 1_000 != 0 {
            return Err(KernelError::InvalidInput(format!(
                "bucket grid must span whole seconds, got {}ms",
                self.grid_ms
            )));
        }
        Ok(())
    }
}

impl FieldAccess for BucketSpec {
    fn set_field(&mut self, name: &str, _value: &str) -> Result<(), FieldError> {
        Err(FieldError::NotFound(name.into()))
    }

    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        match name {
            "grid_ms" => Ok(self.grid_ms.to_string()),
            _ => Err(FieldError::NotFound(name.into())),
        }
    }

    fn field_names(&self) -> &'static [&'static str] {
        &["grid_ms"]
    }
}

impl Describable for BucketSpec {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "grid_ms": {
                    "type": "integer",
                    "description": "Bucket grid span in milliseconds (read-only; positive whole seconds)"
                }
            },
            "required": ["grid_ms"]
        })
    }
}

impl WorkUnit for BucketSpec {
    fn name(&self) -> &str {
        "bucket_spec"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        fluent_wvr::validate::check_for_execute(self, "bucket spec invalid")?;
        Ok(WorkOutput::ok(format!(
            "bucket spec ready (grid {}ms)",
            self.grid_ms
        )))
    }
}

impl_component!(BucketSpec);

// --- Shared MTF store -------------------------------------------------------
//
// One cache per series key over resampled OHLCV frames. Frames resample
// through the M2 bucket core (`bars :: project_bars`, confirmed buckets
// only — the single integer-bucket implementation, never a second one) and
// kernels run through `Indicator :: batch` over borrowed `Ohlcv :: views`,
// so the hot loop stays free-function/borrowed and supervision
// (`KernelUnit`, `Instrumented`) stays at the control-plane boundary, never
// per bar. What this layer adds is the hand-rolled Salsa fragment the
// workload actually needs: revision stamps plus backdating on whole-series
// caches, composed over the canonical `DependencyGraph`.
//
// The store owns confirmed base bars only (the M4 gate decides what counts
// as confirmed before entry); ascending order is enforced here because the
// bucket math assumes it. Parallel fan-out over independent keys belongs to
// the batch/backtest callers via `ResultPool`, never to per-bar spawns.

/// Reserved kernel id naming a resampled OHLCV frame node inside the store
/// graph. Frames materialize from base bars in the pre-pass, never through
/// `batch`, so this id never resolves in an `IndicatorRegistry` — it is a
/// graph-level name for the resample edge, matched in exactly one predicate
/// ([`SeriesKey::is_frame`]).
pub const FRAME_KERNEL: &str = "@frame";

/// Identity of one cached series: bar span, kernel id, and the canonical
/// fingerprint of the parameters it was computed with. Small and
/// clone-cheap; `Ord` makes graph order deterministic (min-heap tie-break).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SeriesKey {
    /// Bar span in milliseconds (positive; the single ms table).
    pub tf_ms: i64,
    /// Kernel id, or [`FRAME_KERNEL`] for a resampled-frame node.
    pub kernel: IndicatorId,
    /// Canonical parameter fingerprint (`ParamSet :: fingerprint`).
    pub params_hash: u64,
    /// When true the kernel reads the frame's volume leg as every input
    /// leg: serves Pine series-expression kernels (`sma(volume)`) through
    /// the same `batch` over borrowed views, with no second frame.
    pub volume_source: bool,
}

impl SeriesKey {
    /// Name one series; non-positive spans refuse fail-closed.
    pub fn new(tf_ms: i64, kernel: &str, params: &ParamSet) -> Result<Self, KernelError> {
        if tf_ms <= 0 {
            return Err(KernelError::InvalidInput(format!(
                "series span must be positive, got {tf_ms}"
            )));
        }
        Ok(Self {
            tf_ms,
            kernel: kernel.to_owned(),
            params_hash: params.fingerprint(),
            volume_source: false,
        })
    }

    /// Mark this key volume-sourced (see [`SeriesKey::volume_source`]).
    #[must_use]
    pub fn with_volume_source(mut self) -> Self {
        self.volume_source = true;
        self
    }

    /// Name the resampled-frame node for one span (default-params
    /// fingerprint: frames take no parameters).
    pub fn frame_key(tf_ms: i64) -> Result<Self, KernelError> {
        Self::new(tf_ms, FRAME_KERNEL, &ParamSet::new())
    }

    /// Whether this key names a resampled frame rather than a series.
    #[must_use]
    pub fn is_frame(&self) -> bool {
        self.kernel == FRAME_KERNEL
    }
}

impl fmt::Display for SeriesKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}ms/{}#{:016x}",
            self.tf_ms, self.kernel, self.params_hash
        )?;
        if self.volume_source {
            f.write_str("~vol")?;
        }
        Ok(())
    }
}

/// One cached series: whole-window values plus the revision bookkeeping
/// that makes backdating work.
#[derive(Debug, Clone)]
pub struct SeriesCache {
    /// Per-bar outputs, one entry per frame bar (`None` reads na).
    pub values: Vec<Option<f64>>,
    /// Store revision at the last compute whose values differed.
    pub revision: u64,
    /// Content hash of [`values`](Self::values) at the last compute: the
    /// backdate record — equal hashes keep the revision, differing hashes
    /// bump it. Replaces the whole-window `values !=` memcmp (identical
    /// bitwise content backdates, including identical NaN payloads the
    /// memcmp's `NaN != NaN` always re-bumped on).
    pub values_hash: u64,
    /// Kernel warmup floor for the parameters at compute time.
    pub warmup: usize,
    /// Inputs this compute ran against: the frame edge plus every kernel
    /// dep, each with the revision seen. Freshness is uniform — a cache is
    /// fresh iff every recorded revision still reads current.
    pub inputs: Vec<(SeriesKey, u64)>,
}

/// Hash stamped float content: fixed-width little-endian words (stamps,
/// then every leg as bits in a fixed order) hashed once through the shared
/// FNV-1a helper — the one content-compare behind frame and series
/// backdating, following the parameter-fingerprint byte idiom. `None`
/// values map to a fixed sentinel word; every distinct bit pattern hashes
/// distinctly, like every distinct float.
fn content_hash(stamps: &[i64], words: &[u64]) -> u64 {
    let mut bytes = Vec::with_capacity(8 * (stamps.len() + words.len()));
    for stamp in stamps {
        bytes.extend_from_slice(&stamp.to_le_bytes());
    }
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    common_core::hash::fnv1a64(&bytes)
}

/// `None`-valued output as a hash word: all-bits-set (a NaN payload no real
/// output leg carries — outputs resolve through the `fin_or_none` na-policy,
/// so `Some` never holds a non-finite payload by the time it is hashed).
const NONE_WORD: u64 = u64::MAX;

/// One resampled OHLCV frame: confirmed buckets only, extended
/// incrementally while the base grows by append, rematerialized when the
/// base is replaced or `now` regresses. Revision bumps only when the
/// buckets actually change (frame-level backdating).
struct Frame {
    stamps: Vec<i64>,
    ohlcv: OwnedOhlcv,
    revision: u64,
    /// Base generation this frame was built from: equal generations mean
    /// append-only growth, so the stored buckets are a prefix of any newer
    /// projection and only the tail needs folding.
    generation: u64,
    /// Base length this frame covers: equal lengths within one generation
    /// mean identical content, so the skip path costs one comparison.
    base_len: usize,
    now_ms: i64,
}

/// Shared multi-timeframe store: owned confirmed base bars, lazily
/// materialized resampled frames per span, per-key series caches, and the
/// monotonic revision counter. Owned values throughout — per-run isolated,
/// no globals, so parallel backtests each hold their own store.
pub struct MtfStore {
    base: Vec<protopyne_types::Bar>,
    closed: Vec<ClosedBar>,
    closed_rev: u64,
    frames: HashMap<i64, Frame>,
    caches: HashMap<SeriesKey, SeriesCache>,
    revision: u64,
    base_rev: u64,
    /// Base generation: bumped only by [`set_base`](Self::set_base), never
    /// by [`append_bar`](Self::append_bar). Equal generations across an
    /// `ensure` mean the base only grew, so frames extend incrementally;
    /// a changed generation means replacement, so frames rebuild.
    generation: u64,
}

impl MtfStore {
    /// Create an empty store; load the tape with [`set_base`](Self::set_base).
    #[must_use]
    pub fn new() -> Self {
        Self {
            base: Vec::new(),
            closed: Vec::new(),
            closed_rev: u64::MAX,
            frames: HashMap::new(),
            caches: HashMap::new(),
            revision: 0,
            base_rev: 0,
            generation: 0,
        }
    }

    /// Replace the base tape: every row runs the one ordering rule
    /// (`validate_new_bar`, shared with the streaming feed, the CSV loader,
    /// and the history adapter). Empty tapes refuse, like the loader.
    /// Frames and caches are NOT cleared — the next `ensure` rematerializes
    /// and backdates whatever recomputes identically.
    pub fn set_base(&mut self, bars: &[protopyne_types::Bar]) -> Result<(), KernelError> {
        if bars.is_empty() {
            return Err(KernelError::InvalidInput(
                "store base must not be empty".to_owned(),
            ));
        }
        let mut prev = None;
        for bar in bars {
            protopyne_types::validate_new_bar(prev, bar)
                .map_err(|err| KernelError::InvalidInput(err.to_string()))?;
            prev = Some(bar);
        }
        self.base = bars.to_vec();
        self.base_rev = self.base_rev.wrapping_add(1);
        self.generation = self.generation.wrapping_add(1);
        Ok(())
    }

    /// Append one confirmed bar: must advance past the tape tail, else the
    /// tape refuses (gaps stay representable — only non-advancing stamps
    /// refuse, exactly like the loader). Same generation: the next `ensure`
    /// extends frames from their stored tail instead of reprojecting.
    pub fn append_bar(&mut self, bar: &protopyne_types::Bar) -> Result<(), KernelError> {
        protopyne_types::validate_new_bar(self.base.last(), bar)
            .map_err(|err| KernelError::InvalidInput(err.to_string()))?;
        self.base.push(bar.clone());
        self.base_rev = self.base_rev.wrapping_add(1);
        Ok(())
    }

    /// Number of base bars loaded.
    #[must_use]
    pub fn base_len(&self) -> usize {
        self.base.len()
    }

    /// Whether no base bars are loaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.base.is_empty()
    }

    /// Current store revision (bumps only on changed frames/caches).
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Borrow one cached series, if computed.
    #[must_use]
    pub fn cache(&self, key: &SeriesKey) -> Option<&SeriesCache> {
        self.caches.get(key)
    }

    /// Borrow one frame's bucket-open stamps, if materialized.
    #[must_use]
    pub fn frame_stamps(&self, tf_ms: i64) -> Option<&[i64]> {
        self.frames.get(&tf_ms).map(|frame| frame.stamps.as_slice())
    }

    /// Borrow one materialized frame's OHLCV legs, if present. Readers that
    /// need legs beyond the kernel caches (context folds, report math) take
    /// them here rather than re-resampling.
    #[must_use]
    pub fn frame(&self, tf_ms: i64) -> Option<&OwnedOhlcv> {
        self.frames.get(&tf_ms).map(|frame| &frame.ohlcv)
    }

    /// Revision currently read for one input edge: frame revisions from the
    /// frame table, series revisions from the caches.
    fn current_rev(&self, key: &SeriesKey) -> Option<u64> {
        if key.is_frame() {
            self.frames.get(&key.tf_ms).map(|frame| frame.revision)
        } else {
            self.caches.get(key).map(|cache| cache.revision)
        }
    }

    /// Fill every requested series plus its transitive deps, in topo order,
    /// and return the computed series keys in that order. Frames
    /// materialize first (confirmed buckets at `now_ms` only — the forming
    /// tail never enters a frame, so an append that completes no new bucket
    /// leaves unrelated spans untouched); kernels then run through
    /// `batch` over borrowed views in dependency order. Identical recompute
    /// backdates (revision kept, downstream not re-run); changed values bump
    /// the store revision. Duplicate request keys dedupe (first params
    /// win); requested frame keys are accepted no-ops.
    ///
    /// Dependency parameters resolve through
    /// [`build_graph`](crate::mtf::build_graph): a dep key shared with a
    /// direct request reuses the request's parameters (first request wins),
    /// so any `SeriesKey` in the transitive closure computes at most once per
    /// revision. Unknown kernels, unknown deps, and cycles refuse with a
    /// reason — never a partial fill.
    pub fn ensure(
        &mut self,
        registry: &IndicatorRegistry,
        requests: &[(SeriesKey, ParamSet)],
        now_ms: i64,
        lookahead: Lookahead,
    ) -> Result<Vec<SeriesKey>, KernelError> {
        lookahead.require_off()?;
        if self.base.is_empty() {
            return Err(KernelError::InvalidInput(
                "store has no base bars loaded".to_owned(),
            ));
        }
        let default_params = ParamSet::new();
        // First-wins owned pairs: duplicate requests keep the first
        // parameters, frames stay accepted no-ops.
        let mut pairs: Vec<(SeriesKey, ParamSet)> = Vec::with_capacity(requests.len());
        for (key, params) in requests {
            if key.tf_ms <= 0 {
                return Err(KernelError::InvalidInput(format!(
                    "series span must be positive, got {}",
                    key.tf_ms
                )));
            }
            if key.is_frame() {
                continue;
            }
            if !pairs.iter().any(|(seen, _)| seen == key) {
                pairs.push((key.clone(), params.clone()));
            }
        }
        let keys: Vec<SeriesKey> = pairs.iter().map(|(key, _)| key.clone()).collect();

        let (graph, resolved) = build_graph(registry, &pairs)?;
        // The shared `From<GraphError>` reads pine-flavored; the store names
        // its own mapping so a cycle here never reports as an import failure.
        let order = order_for(&graph, &keys)
            .map_err(|err| KernelError::InvalidInput(format!("mtf store graph failed: {err}")))?;

        if self.closed_rev != self.base_rev {
            self.closed = self.base.iter().map(ClosedBar::from_proto_bar).collect();
            self.closed_rev = self.base_rev;
        }
        let mut spans: Vec<i64> = order.iter().map(|key| key.tf_ms).collect();
        spans.sort_unstable();
        spans.dedup();
        for tf_ms in spans {
            self.materialize_frame(tf_ms, now_ms)?;
        }

        let mut ordered = Vec::new();
        for node in &order {
            if node.is_frame() {
                continue;
            }
            let deps = graph.deps_of(node).unwrap_or(&[]).to_vec();
            let fresh = match self.caches.get(node) {
                Some(cache) => {
                    cache.inputs.len() == deps.len()
                        && cache
                            .inputs
                            .iter()
                            .all(|(dep, rev)| self.current_rev(dep) == Some(*rev))
                }
                None => false,
            };
            if !fresh {
                let kernel = registry.get(&node.kernel).ok_or_else(|| {
                    KernelError::UnknownIndicator(format!("unknown kernel '{}'", node.kernel))
                })?;
                // NOT-A-LADDER: every ordered node carries resolved params (requests
                // first-wins, then dep-discovered); the default below is an
                // unreachable fail-safe, never a silently resolved default.
                let params = resolved.get(node).unwrap_or(&default_params);
                let frame = self.frames.get(&node.tf_ms).ok_or_else(|| {
                    KernelError::InvalidInput(format!("no frame materialized for {node}"))
                })?;
                let views;
                let data = if node.volume_source {
                    views = Ohlcv {
                        open: &frame.ohlcv.volume,
                        high: &frame.ohlcv.volume,
                        low: &frame.ohlcv.volume,
                        close: &frame.ohlcv.volume,
                        volume: &frame.ohlcv.volume,
                    };
                    &views
                } else {
                    &frame.ohlcv.views()
                };
                let result = kernel.batch(data, params)?;
                let warmup = kernel.warmup(params)?;
                let previous = self.caches.get(node).map(|cache| cache.revision);
                let words: Vec<u64> = result
                    .values
                    .iter()
                    .map(|slot| match slot {
                        Some(value) => value.to_bits(),
                        None => NONE_WORD,
                    })
                    .collect();
                let values_hash = content_hash(&[], &words);
                let changed = match self.caches.get(node) {
                    Some(cache) => cache.values_hash != values_hash,
                    None => true,
                };
                let revision = if changed {
                    self.revision = self.revision.wrapping_add(1);
                    self.revision
                } else {
                    previous.unwrap_or(self.revision)
                };
                let mut inputs = Vec::with_capacity(deps.len());
                for dep in &deps {
                    let rev = self.current_rev(dep).ok_or_else(|| {
                        KernelError::InvalidInput(format!("no revision recorded for {dep}"))
                    })?;
                    inputs.push((dep.clone(), rev));
                }
                self.caches.insert(
                    node.clone(),
                    SeriesCache {
                        values: result.values,
                        revision,
                        values_hash,
                        warmup,
                        inputs,
                    },
                );
            }
            ordered.push(node.clone());
        }
        Ok(ordered)
    }

    /// Bring one span's frame current at `now_ms`: skip untouched when the
    /// generation, covered base length, and `now` all match (one comparison,
    /// zero folds); extend the stored tail while the base only grew by
    /// append; rebuild from scratch on replacement, first build, or
    /// regressed `now` (the fence never regresses in production — a
    /// regressed `now` takes the slow path, never a wrong one). Converted
    /// [`ClosedBar`] legs ride the existing per-`base_rev` cache, built once
    /// per base change and reused across spans.
    fn materialize_frame(&mut self, tf_ms: i64, now_ms: i64) -> Result<(), KernelError> {
        let grid = NonZeroI64::new(tf_ms).ok_or_else(|| {
            KernelError::InvalidInput(format!("series span must be positive, got {tf_ms}"))
        })?;
        if self.closed_rev != self.base_rev {
            self.closed = self.base.iter().map(ClosedBar::from_proto_bar).collect();
            self.closed_rev = self.base_rev;
        }
        if let Some(frame) = self.frames.get(&tf_ms) {
            if frame.generation == self.generation
                && frame.base_len <= self.closed.len()
                && frame.now_ms <= now_ms
            {
                if frame.base_len == self.closed.len() && frame.now_ms == now_ms {
                    return Ok(());
                }
                if self.extend_frame(tf_ms, now_ms, grid)? {
                    return Ok(());
                }
            }
        }
        self.rebuild_frame(tf_ms, now_ms)
    }

    /// Extend one frame's stored tail: fold only base bars at or after the
    /// last stored bucket-open (a bar inside the last bucket recomputes it;
    /// later bars open new ones), keep the confirmed ones, and splice them
    /// over the stored tail. Returns whether the extend applied: a stored
    /// tail with nothing confirmed now is unreachable while `now` advances
    /// (confirmed buckets stay confirmed), so that case declines and the
    /// caller rebuilds. One bucket `==` decides backdate-vs-bump — bounded
    /// by the tail width, never a full-window compare; new buckets past the
    /// old tail are new by construction. Unchanged extends keep the
    /// revision, so downstream series stay fresh and skip their recompute.
    fn extend_frame(
        &mut self,
        tf_ms: i64,
        now_ms: i64,
        grid: NonZeroI64,
    ) -> Result<bool, KernelError> {
        let last_stamp = self
            .frames
            .get(&tf_ms)
            .and_then(|frame| frame.stamps.last().copied());
        let rebase_from = match last_stamp {
            Some(stamp) => self.closed.partition_point(|bar| bar.ts_ms < stamp),
            None => 0,
        };
        let tail_slice = &self.closed[rebase_from..];
        let mut tail = Vec::new();
        for (idx, first, last) in bucket_bar_ranges(tail_slice, tf_ms)? {
            let ts = idx * tf_ms;
            if last_stamp.is_some_and(|open| ts < open) {
                continue;
            }
            if !fully_formed(ts, grid, now_ms) {
                continue;
            }
            tail.push(fold_bucket_range(tail_slice, first, last, ts));
        }
        let frame = self.frames.get_mut(&tf_ms).expect("extended frame present");
        let old_last = frame.stamps.last().map(|ts| {
            let at = frame.stamps.len() - 1;
            ClosedBar {
                ts_ms: *ts,
                o: frame.ohlcv.open[at],
                h: frame.ohlcv.high[at],
                l: frame.ohlcv.low[at],
                c: frame.ohlcv.close[at],
                volume: frame.ohlcv.volume[at],
            }
        });
        let changed = match (&old_last, tail.first()) {
            // No confirmed tail before or after: markers advance, content stands.
            (None, None) => false,
            // Newly confirmed buckets where none stood: wholesale replace.
            (None, Some(_)) => true,
            // Stored tail with nothing confirmed now: declined above by
            // contract — report unhandled so the caller rebuilds.
            (Some(_), None) => return Ok(false),
            // Recomputed last bucket plus zero or more new buckets: any new
            // bucket is a change; otherwise the one `==` decides.
            (Some(old), Some(new)) => tail.len() != 1 || *new != *old,
        };
        if changed {
            if old_last.is_some() {
                frame.stamps.pop();
                frame.ohlcv.open.pop();
                frame.ohlcv.high.pop();
                frame.ohlcv.low.pop();
                frame.ohlcv.close.pop();
                frame.ohlcv.volume.pop();
            }
            for bar in &tail {
                frame.stamps.push(bar.ts_ms);
                frame.ohlcv.open.push(bar.o);
                frame.ohlcv.high.push(bar.h);
                frame.ohlcv.low.push(bar.l);
                frame.ohlcv.close.push(bar.c);
                frame.ohlcv.volume.push(bar.volume);
            }
            self.revision = self.revision.wrapping_add(1);
            frame.revision = self.revision;
        }
        frame.base_len = self.closed.len();
        frame.now_ms = now_ms;
        Ok(true)
    }

    /// Rebuild one frame from the full base tape: project every bucket, hash
    /// the retained legs against the new legs, backdate on equal hashes and
    /// bump-and-replace on differ. This path already pays O(history) for
    /// projection, so the two streaming hash passes (one transient words
    /// buffer each, freed on return) replace the five-leg early-exit memcmp
    /// with bounded uniform work and a fixed-size backdate record.
    fn rebuild_frame(&mut self, tf_ms: i64, now_ms: i64) -> Result<(), KernelError> {
        let projected = project_bars(&self.closed, tf_ms, Lookahead::Off, Some(now_ms))?;
        let mut stamps = Vec::with_capacity(projected.len());
        let mut ohlcv = OwnedOhlcv {
            open: Vec::with_capacity(projected.len()),
            high: Vec::with_capacity(projected.len()),
            low: Vec::with_capacity(projected.len()),
            close: Vec::with_capacity(projected.len()),
            volume: Vec::with_capacity(projected.len()),
        };
        for bar in &projected {
            stamps.push(bar.ts_ms);
            ohlcv.open.push(bar.o);
            ohlcv.high.push(bar.h);
            ohlcv.low.push(bar.l);
            ohlcv.close.push(bar.c);
            ohlcv.volume.push(bar.volume);
        }
        fn frame_words(legs: &OwnedOhlcv) -> Vec<u64> {
            let mut words = Vec::with_capacity(legs.open.len() * 5);
            for leg in [&legs.open, &legs.high, &legs.low, &legs.close, &legs.volume] {
                words.extend(leg.iter().map(|value| value.to_bits()));
            }
            words
        }
        let new_hash = content_hash(&stamps, &frame_words(&ohlcv));
        let changed = match self.frames.get(&tf_ms) {
            Some(frame) => content_hash(&frame.stamps, &frame_words(&frame.ohlcv)) != new_hash,
            None => true,
        };
        let revision = if changed {
            self.revision = self.revision.wrapping_add(1);
            self.revision
        } else {
            self.frames
                .get(&tf_ms)
                .map_or(self.revision, |frame| frame.revision)
        };
        self.frames.insert(
            tf_ms,
            Frame {
                stamps,
                ohlcv,
                revision,
                generation: self.generation,
                base_len: self.closed.len(),
                now_ms,
            },
        );
        Ok(())
    }
}

impl Default for MtfStore {
    fn default() -> Self {
        Self::new()
    }
}

impl fluent_wvr::validate::Validate for MtfStore {
    type Err = KernelError;

    fn validate(&self) -> Result<(), Self::Err> {
        let mut prev = None;
        for bar in &self.base {
            protopyne_types::validate_new_bar(prev, bar)
                .map_err(|err| KernelError::InvalidInput(err.to_string()))?;
            prev = Some(bar);
        }
        Ok(())
    }
}

/// Register every requested key plus its transitive parametric deps: each
/// series key depends on its frame edge plus the kernel's
/// [`deps_with_params`](crate::indicator::Indicator::deps_with_params)
/// resolved to `(same span, dep id, real params, the request's leg
/// selection)`; frame keys provide themselves with no deps. Unknown kernels
/// and unknown deps refuse with a reason. Ordering itself stays the canonical
/// graph's job — this function only declares nodes and edges, never sorts.
///
/// The returned map carries every node's parameters (requests first-wins,
/// then dep-discovered in traversal order) so the compute step reads each
/// dep's real params — there is exactly one dep-resolution rule, and no
/// default-params fallback anywhere on this path.
pub fn build_graph(
    registry: &IndicatorRegistry,
    requests: &[(SeriesKey, ParamSet)],
) -> Result<(DependencyGraph<SeriesKey>, HashMap<SeriesKey, ParamSet>), KernelError> {
    let mut graph = DependencyGraph::new();
    let mut params_of: HashMap<SeriesKey, ParamSet> = HashMap::new();
    for (key, params) in requests {
        params_of
            .entry(key.clone())
            .or_insert_with(|| params.clone());
    }
    let mut stack: Vec<SeriesKey> = requests.iter().map(|(key, _)| key.clone()).collect();
    while let Some(key) = stack.pop() {
        if key.tf_ms <= 0 {
            return Err(KernelError::InvalidInput(format!(
                "series span must be positive, got {}",
                key.tf_ms
            )));
        }
        if graph.contains(&key) {
            continue;
        }
        if key.is_frame() {
            graph.register(&key, &[], std::slice::from_ref(&key))?;
            continue;
        }
        let kernel = registry.get(&key.kernel).ok_or_else(|| {
            KernelError::UnknownIndicator(format!("unknown kernel '{}'", key.kernel))
        })?;
        // NOT-A-LADDER: every node enters the stack with its params seeded above (requests)
        // or inserted at discovery below (deps); the empty fallback is an
        // unreachable fail-safe, never a silently resolved default.
        let node_params = params_of.get(&key).cloned().unwrap_or_else(ParamSet::new);
        let frame = SeriesKey::frame_key(key.tf_ms)?;
        let mut deps = vec![frame.clone()];
        for (dep_id, dep_params) in kernel.deps_with_params(&node_params) {
            if registry.get(&dep_id).is_none() {
                return Err(KernelError::UnknownIndicator(format!(
                    "unknown kernel dep '{dep_id}' of '{}'",
                    key.kernel
                )));
            }
            let mut dep_key = SeriesKey::new(key.tf_ms, &dep_id, &dep_params)?;
            if key.volume_source {
                dep_key = dep_key.with_volume_source();
            }
            params_of.entry(dep_key.clone()).or_insert(dep_params);
            deps.push(dep_key);
        }
        graph.register(&key, &deps, std::slice::from_ref(&key))?;
        stack.extend(deps);
    }
    Ok((graph, params_of))
}

/// Order requested keys (plus their transitive deps) dependency-first: the
/// one sorter, delegated straight to
/// [`topo_sort_from`](DependencyGraph::topo_sort_from). Cycles refuse
/// through the graph's own error — mapped by callers, never swallowed.
pub fn order_for(
    graph: &DependencyGraph<SeriesKey>,
    requests: &[SeriesKey],
) -> Result<Vec<SeriesKey>, GraphError> {
    graph.topo_sort_from(requests)
}

#[cfg(test)]
#[path = "../tests/mtf.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/mtf_store.rs"]
mod mtf_store_tests;

#[cfg(test)]
#[path = "../tests/mtf_dedup.rs"]
mod mtf_dedup_tests;

#[cfg(test)]
#[path = "../tests/mtf_incremental.rs"]
mod mtf_incremental_tests;
