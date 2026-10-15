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
use protopyne_types::{Bar, Timeframe};

use crate::error::StoreError;
use crate::range::TimeRange;

/// Fold anchor for every sub-weekly spacing: the Unix epoch itself.
pub const FOLD_ORIGIN_MS: i64 = 0;

/// Fold anchor for weekly bars: 1969-12-29 00:00:00 UTC, the Monday of the
/// epoch's week. The epoch itself is a Thursday, and a Thursday-anchored
/// weekly bar is confusing to hand an operator.
pub const WEEK_FOLD_ORIGIN_MS: i64 = -259_200_000;

/// The fold anchor for a spacing. Only `W1` is special; any future fixed-ms
/// spacing anchors at the epoch with no code change.
#[must_use]
pub fn fold_origin_ms(tf: Timeframe) -> i64 {
    match tf {
        Timeframe::W1 => WEEK_FOLD_ORIGIN_MS,
        _ => FOLD_ORIGIN_MS,
    }
}

/// The bucket a stamp belongs to: the greatest anchor-aligned grid line at
/// or before the stamp.
///
/// Pure Euclidean floor division on UTC milliseconds — negative stamps
/// floor toward the earlier bucket, never toward zero — saturating instead
/// of overflowing past the representable range. Idempotent: feeding a
/// bucket start back in returns it unchanged, which is what makes
/// incremental folding over a live tape converge instead of drift.
#[must_use]
pub fn bucket_start(tf: Timeframe, ts_ms: i64) -> i64 {
    let origin = fold_origin_ms(tf);
    let step = tf.duration_ms();
    origin.saturating_add(
        ts_ms
            .saturating_sub(origin)
            .div_euclid(step)
            .saturating_mul(step),
    )
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
pub fn fold_bars(bars: &[Bar], tf: Timeframe) -> Result<Vec<Bar>, StoreError> {
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
    let mut folded: Vec<Bar> = Vec::new();
    let mut open_bucket: Option<Bucket> = None;
    for bar in bars {
        let start = bucket_start(tf, bar.ts.timestamp_millis());
        match open_bucket.as_mut() {
            Some(bucket) if bucket.start_ms == start => bucket.absorb(bar),
            _ => {
                if let Some(bucket) = open_bucket.take() {
                    folded.push(bucket.finish()?);
                }
                open_bucket = Some(Bucket::first(start, bar));
            }
        }
    }
    if let Some(bucket) = open_bucket.take() {
        folded.push(bucket.finish()?);
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
