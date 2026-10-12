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

use crate::bars::{ClosedBar, project_bars};
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
pub fn interleave_sorted_by<T: Clone>(
    a: &[T],
    b: &[T],
    ms: impl Fn(&T) -> i64,
) -> Vec<T> {
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
pub fn pairs_from_sorted<T: Clone>(
    a: &[T],
    b: &[T],
    ms: impl Fn(&T) -> i64,
) -> Vec<(T, T)> {
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
/// [`Validate`](protopyne_types::Validate), bridged through
/// [`check_for_execute`](protopyne_types::check_for_execute) at execution),
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

impl protopyne_types::Validate for BucketSpec {
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
        protopyne_types::check_for_execute(self, "bucket spec invalid")?;
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
        write!(f, "{}ms/{}#{:016x}", self.tf_ms, self.kernel, self.params_hash)?;
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
    /// Kernel warmup floor for the parameters at compute time.
    pub warmup: usize,
    /// Inputs this compute ran against: the frame edge plus every kernel
    /// dep, each with the revision seen. Freshness is uniform — a cache is
    /// fresh iff every recorded revision still reads current.
    pub inputs: Vec<(SeriesKey, u64)>,
}

/// One resampled OHLCV frame: confirmed buckets only, rematerialized when
/// the base tape or `now` moves, revision-bumped only when the buckets
/// actually change (frame-level backdating).
struct Frame {
    stamps: Vec<i64>,
    ohlcv: OwnedOhlcv,
    revision: u64,
    base_rev: u64,
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
        }
    }

    /// Replace the base tape: every row runs the one ordering rule
    /// (`validate_new_bar`, shared with the streaming feed, the CSV loader,
    /// and the history adapter). Empty tapes refuse, like the loader.
    /// Frames and caches are NOT cleared — the next `ensure` rematerializes
    /// and backdates whatever recomputes identically.
    pub fn set_base(&mut self, bars: &[protopyne_types::Bar]) -> Result<(), KernelError> {
        if bars.is_empty() {
            return Err(KernelError::InvalidInput("store base must not be empty".to_owned()));
        }
        let mut prev = None;
        for bar in bars {
            protopyne_types::validate_new_bar(prev, bar)
                .map_err(|err| KernelError::InvalidInput(err.to_string()))?;
            prev = Some(bar);
        }
        self.base = bars.to_vec();
        self.base_rev = self.base_rev.wrapping_add(1);
        Ok(())
    }

    /// Append one confirmed bar: must advance past the tape tail, else the
    /// tape refuses (gaps stay representable — only non-advancing stamps
    /// refuse, exactly like the loader).
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
    /// Lifted dependency keys run default parameters (`ParamSet :: new`):
    /// the kernel spec names ordering deps, not their parameters, so a dep
    /// shares its cache entry with an explicit default-params request and
    /// computes separately from any custom-params request for the same
    /// kernel. Unknown kernels, unknown deps, and cycles refuse with a
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
        let mut keys = Vec::with_capacity(requests.len());
        let mut params_of: HashMap<&SeriesKey, &ParamSet> = HashMap::new();
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
            params_of.entry(key).or_insert(params);
            if !keys.contains(key) {
                keys.push(key.clone());
            }
        }

        let graph = build_graph(registry, &keys)?;
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
            match self.frames.entry(tf_ms) {
                std::collections::hash_map::Entry::Vacant(slot) => {
                    self.revision = self.revision.wrapping_add(1);
                    let revision = self.revision;
                    slot.insert(Frame {
                        stamps,
                        ohlcv,
                        revision,
                        base_rev: self.base_rev,
                        now_ms,
                    });
                }
                std::collections::hash_map::Entry::Occupied(slot)
                    if slot.get().base_rev == self.base_rev
                        && slot.get().now_ms == now_ms => {}
                std::collections::hash_map::Entry::Occupied(mut slot)
                    if slot.get().stamps == stamps
                        && slot.get().ohlcv.open == ohlcv.open
                        && slot.get().ohlcv.high == ohlcv.high
                        && slot.get().ohlcv.low == ohlcv.low
                        && slot.get().ohlcv.close == ohlcv.close
                        && slot.get().ohlcv.volume == ohlcv.volume =>
                {
                    let frame = slot.get_mut();
                    frame.base_rev = self.base_rev;
                    frame.now_ms = now_ms;
                }
                std::collections::hash_map::Entry::Occupied(mut slot) => {
                    self.revision = self.revision.wrapping_add(1);
                    let revision = self.revision;
                    slot.insert(Frame {
                        stamps,
                        ohlcv,
                        revision,
                        base_rev: self.base_rev,
                        now_ms,
                    });
                }
            }
        }

        let mut ordered = Vec::new();
        for node in &order {
            if node.is_frame() {
                continue;
            }
            let deps = graph.deps_of(node).unwrap_or(&[]).to_vec();
            let fresh = match self.caches.get(node) {
                Some(cache) => cache.inputs.len() == deps.len()
                    && cache
                        .inputs
                        .iter()
                        .all(|(dep, rev)| self.current_rev(dep) == Some(*rev)),
                None => false,
            };
            if !fresh {
                let kernel = registry.get(&node.kernel).ok_or_else(|| {
                    KernelError::UnknownIndicator(format!("unknown kernel '{}'", node.kernel))
                })?;
                let params = params_of.get(node).copied().unwrap_or(&default_params);
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
                let changed = match self.caches.get(node) {
                    Some(cache) => cache.values != result.values,
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
                        warmup,
                        inputs,
                    },
                );
            }
            ordered.push(node.clone());
        }
        Ok(ordered)
    }
}

impl Default for MtfStore {
    fn default() -> Self {
        Self::new()
    }
}

impl protopyne_types::Validate for MtfStore {
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

/// Register every requested key plus its transitive deps: each series key
/// depends on its frame edge plus the kernel `depends` lifted to
/// `(same span, dep id, default params)`; frame keys provide themselves
/// with no deps. Unknown kernels and unknown deps refuse with a reason.
/// Ordering itself stays the canonical graph's job — this function only
/// declares nodes and edges, never sorts.
pub fn build_graph(
    registry: &IndicatorRegistry,
    requests: &[SeriesKey],
) -> Result<DependencyGraph<SeriesKey>, KernelError> {
    let mut graph = DependencyGraph::new();
    let mut stack: Vec<SeriesKey> = requests.to_vec();
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
        let frame = SeriesKey::frame_key(key.tf_ms)?;
        let mut deps = vec![frame.clone()];
        for dep_id in kernel.spec().depends.clone() {
            if registry.get(&dep_id).is_none() {
                return Err(KernelError::UnknownIndicator(format!(
                    "unknown kernel dep '{dep_id}' of '{}'",
                    key.kernel
                )));
            }
            deps.push(SeriesKey {
                tf_ms: key.tf_ms,
                kernel: dep_id,
                params_hash: ParamSet::new().fingerprint(),
                volume_source: false,
            });
        }
        graph.register(&key, &deps, std::slice::from_ref(&key))?;
        stack.extend(deps);
    }
    Ok(graph)
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
