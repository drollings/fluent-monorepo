//! Bar folding: coarser spacings derived on read, never stored twice.
//!
//! The store holds one spacing per symbol; every coarser spacing is folded
//! from it at read time over absolute, UTC-anchored buckets. The anchor is
//! the Unix epoch for every sub-weekly spacing and the Monday of the
//! epoch's week for `W1` — pinned constants, not inherited behavior — so
//! daylight rules never enter and folding is DST-immune by construction.
//!
//! [`fold_bars`] folds a bar slice and nothing else: which span to fetch
//! (widened to full buckets or not) and how to report the edges is the read
//! plan's decision ([`crate::plan`]), because edge treatment needs the
//! requested window and a bar slice does not carry one. Bucket math here is
//! absolute, so the input spacing never enters either — any bars nesting
//! inside one target bucket fold correctly, and a source bar straddling a
//! bucket boundary is the planner's refusal, never this function's guess.
//!
//! Measured agreement, frozen in `tests/data/fold_goldens.json`: 39 bucket
//! boundaries hold exactly (integers, no tolerance), covering the epoch,
//! month and year lines, both 2024 daylight instants, the Monday week line
//! in both directions, and pre-epoch stamps; `bucket_start` is idempotent
//! over a 15-spacing by 22-stamp sweep (330 fixed points). A bar-complete
//! run of 346 real minute bars folds to 70 five-minute buckets with no gaps,
//! removing one folded bar yields exactly the gap naming it, and folding the
//! whole 436-bar file reproduces the one genuine hole the vendor's own
//! five-minute grid shows. Against the 116-bucket vendor fifteen-minute
//! tape, the 23 buckets fully covered by minute bars agree exactly on open,
//! high, low, close, and volume; the other 93 sit outside the minute span
//! or in its torn tail and are recorded as incomparable, not rounded up.

use chrono::DateTime;
use protopyne_types::{Bar, BucketAccum, Timeframe};

use crate::error::StoreError;
use crate::range::TimeRange;

// The anchor vocabulary lives with the timeframe it anchors
// ([`Timeframe::bucket_start_ms`]); these re-exports keep every existing
// path compiling unchanged while the rule has one home.
pub use protopyne_types::{FOLD_ORIGIN_MS, WEEK_FOLD_ORIGIN_MS};

// The complete-bucket rule lives in [`protopyne_types`]: re-exported here
// so every existing path keeps compiling while the rule has one home.
pub use protopyne_types::FoldEdges;

/// The fold anchor for a spacing. Thin delegation to
/// [`Timeframe::fold_origin_ms`], kept so no caller changes.
#[must_use]
pub fn fold_origin_ms(tf: Timeframe) -> i64 {
    tf.fold_origin_ms()
}

/// The bucket a stamp belongs to. Thin delegation to
/// [`Timeframe::bucket_start_ms`], kept so no caller changes.
#[must_use]
pub fn bucket_start(tf: Timeframe, ts_ms: i64) -> i64 {
    tf.bucket_start_ms(ts_ms)
}

/// How a read treats bucket edges. The policy rides the read request and is
/// consumed by the planner and the read path — [`fold_bars`] itself folds a
/// bar slice range-free, so it takes no policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FoldPolicy {
    /// Widen the fetch to enclosing bucket boundaries, fold, then narrow
    /// back to the request — so a returned `open` is always the true bucket
    /// open. The default: a partial-bucket edge is a fetch problem, solved
    /// by fetching more, never by answering with a truncated bucket.
    #[default]
    ExtendToFullBuckets,
    /// Fold exactly the fetched span with no widening. Edge buckets may be
    /// partial; the plan reports them through `complete = false`, which
    /// callers must surface. Diagnostics only: a truncated first bucket's
    /// `open` is not the bucket open.
    TruncateEdges,
}

/// Fold stored bars into `tf` buckets: `open` is the first bar's open,
/// `close` the last bar's close, `high`/`low` the extremes, `volume` the
/// saturating sum, stamped at the bucket start.
///
/// Input must arrive strictly ascending — the store's read boundary
/// guarantees it, and anything else refuses rather than folding a shuffled
/// series into plausible-looking buckets. Every input bar and every folded
/// bar passes the shared `Bar :: validate`: fold output answers to the same
/// rules as stored bars. Folded bars carry no adjustment; adjustment-aware
/// reads are out of scope, and folds never invent what the tape did not
/// hold.
///
/// This is [`fold_bars_edges`] under [`FoldEdges::KeepPartial`]:
/// byte-identical output for every existing caller.
pub fn fold_bars(bars: &[Bar], tf: Timeframe) -> Result<Vec<Bar>, StoreError> {
    fold_bars_edges(bars, tf, FoldEdges::KeepPartial)
}

/// Fold stored bars into `tf` buckets under an explicit edge policy (see
/// [`FoldEdges`]). Validation, bucketing, and per-bucket arithmetic match
/// [`fold_bars`] exactly; only edge emission differs.
pub fn fold_bars_edges(
    bars: &[Bar],
    tf: Timeframe,
    edges: FoldEdges,
) -> Result<Vec<Bar>, StoreError> {
    let mut last_ts: Option<i64> = None;
    for bar in bars {
        bar.validate()
            .map_err(|e| StoreError::InvalidBar(e.to_string()))?;
        let ts = bar.ts.timestamp_millis();
        if last_ts.is_some_and(|prev| ts <= prev) {
            return Err(StoreError::InvalidBar(format!(
                "fold input must arrive strictly ascending, saw {ts} after {}",
                last_ts.expect("a previous stamp was recorded")
            )));
        }
        last_ts = Some(ts);
    }
    let step_ms = tf.duration_ms();
    let mut folded: Vec<Bar> = Vec::new();
    // Completeness rides alongside the arithmetic, never in a per-bucket
    // stamp vector: one accumulator per open bucket, O(1) per bar.
    let mut open_bucket: Option<(Bucket, BucketAccum)> = None;
    let emit = |bucket: Bucket, accum: BucketAccum, folded: &mut Vec<Bar>| {
        let whole = match edges {
            FoldEdges::KeepPartial => true,
            FoldEdges::CompleteOnly => accum.is_complete(step_ms),
        };
        if whole {
            folded.push(bucket.finish()?);
        }
        Ok::<_, StoreError>(())
    };
    for bar in bars {
        let ts = bar.ts.timestamp_millis();
        let start = tf.bucket_start_ms(ts);
        match open_bucket.as_mut() {
            Some((bucket, accum)) if bucket.start_ms == start => {
                bucket.absorb(bar);
                accum.absorb(ts);
            }
            _ => {
                if let Some((bucket, accum)) = open_bucket.take() {
                    emit(bucket, accum, &mut folded)?;
                }
                let mut accum = BucketAccum::new(start);
                accum.absorb(ts);
                open_bucket = Some((Bucket::first(start, bar), accum));
            }
        }
    }
    if let Some((bucket, accum)) = open_bucket.take() {
        emit(bucket, accum, &mut folded)?;
    }
    Ok(folded)
}

/// Keep the folded buckets overlapping `window`: the narrow-back half of
/// the extend policy. Buckets are half-open spans, so a bucket merely
/// touching the window's edge is outside it. The read path calls this after
/// folding a widened fetch; diagnostics under `TruncateEdges` narrow the
/// same way, so both policies answer exactly the requested window and
/// differ only in how the edge buckets were built.
pub(crate) fn narrow_to_window(folded: Vec<Bar>, window: &TimeRange, tf: Timeframe) -> Vec<Bar> {
    let step = tf.duration_ms();
    folded
        .into_iter()
        .filter(|bar| {
            let start = bar.ts.timestamp_millis();
            TimeRange {
                start_ms: Some(start),
                end_ms: Some(start.saturating_add(step)),
            }
            .overlaps(window)
        })
        .collect()
}

/// One bucket under construction: the running first/last/extremes/sum.
struct Bucket {
    start_ms: i64,
    open: rust_decimal::Decimal,
    high: rust_decimal::Decimal,
    low: rust_decimal::Decimal,
    close: rust_decimal::Decimal,
    volume: u64,
}

impl Bucket {
    fn first(start_ms: i64, bar: &Bar) -> Self {
        Self {
            start_ms,
            open: bar.o,
            high: bar.h,
            low: bar.l,
            close: bar.c,
            volume: bar.volume,
        }
    }

    fn absorb(&mut self, bar: &Bar) {
        self.high = self.high.max(bar.h);
        self.low = self.low.min(bar.l);
        self.close = bar.c;
        self.volume = self.volume.saturating_add(bar.volume);
    }

    fn finish(self) -> Result<Bar, StoreError> {
        let ts = DateTime::from_timestamp_millis(self.start_ms).ok_or_else(|| {
            StoreError::InvalidBar(format!("bucket start {} out of range", self.start_ms))
        })?;
        let folded = Bar::new()
            .ts(ts)
            .o(self.open)
            .h(self.high)
            .l(self.low)
            .c(self.close)
            .volume(self.volume)
            .maybe_adj(None)
            .build();
        folded
            .validate()
            .map_err(|e| StoreError::InvalidBar(e.to_string()))?;
        Ok(folded)
    }
}

#[cfg(test)]
#[path = "../tests/fold_basic.rs"]
mod tests_fold_basic;

#[cfg(test)]
#[path = "../tests/fold_edges.rs"]
mod tests_fold_edges;

#[cfg(test)]
#[path = "../tests/common/fold.rs"]
pub(crate) mod test_fold;

#[cfg(test)]
#[path = "../tests/fold_calibration.rs"]
mod tests_fold_calibration;

#[cfg(test)]
#[path = "../tests/fold_calibration_negative.rs"]
mod tests_fold_calibration_negative;

#[cfg(test)]
#[path = "../tests/fold_calibration_stability.rs"]
mod tests_fold_calibration_stability;
