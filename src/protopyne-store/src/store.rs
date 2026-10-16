//! The synchronous candle interface: what the store promises every caller.
//!
//! [`CandleStore`] is deliberately synchronous and object-safe. The
//! file-backed engine is synchronous, so `async fn` on the trait would box
//! a future per read for no benefit — and the concurrency skill forbids the
//! `async_trait` macro that would hide that cost.
//!
//! One timeframe per symbol per store file: rows are keyed `(symbol_id,
//! ts_ms)` with no timeframe column, so two spacings for one symbol would
//! collide on the primary key. Coarser spacings are derived on read by
//! folding, never stored twice.
//!
//! The ordering guarantee lives here, not in any caller: [`CandleStore::
//! read`] returns bars ascending by `ts` with every adjacent pair passing
//! `validate_new_bar`, no matter what order the rows were written in. A
//! runner that trusts its input order inherits safety from this boundary
//! instead of from a file's line order. [`order_and_validate`] is the one
//! spelling of that boundary: every served read proves its output through
//! it, before and after the fold.

use protopyne_types::{Bar, Timeframe, Validate, validate_new_bar};

use crate::catalog::Market;

/// Rows per bulk write chunk: the storage-economy bound the sealed-snapshot
/// writer chunks row groups by. A **cost bound**, not an accuracy threshold —
/// raising it changes IO behavior, never correctness.
pub const DEFAULT_APPEND_CHUNK: usize = 8_192;
use crate::coverage::Coverage;
use crate::error::{PlanError, StoreError};
use crate::fold::FoldPolicy;
use crate::key::{SymbolId, TapeKey};
use crate::plan::SpanPlan;
use crate::range::TimeRange;

/// What to read: a named tape over a millisecond window.
///
/// The symbol travels as a string so an unknown symbol refuses with its
/// name attached ([`PlanError::UnknownSymbol`](crate::error::PlanError)).
/// Open range ends mean "no bound on that side"; bounding an unbounded
/// read is the planner's job, not this request's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadRequest {
    /// The instrument's spelling, as registered.
    pub symbol: String,
    /// The venue the tape prints on, as registered.
    pub venue: String,
    /// The bar spacing to read at. Coarser than the tape's spacing folds
    /// on read; finer or non-multiple spacings refuse.
    pub timeframe: Timeframe,
    /// The window to serve.
    pub range: TimeRange,
    /// Bar cap bounding an open-ended read. A read open on any side with
    /// no limit refuses ([`PlanError::UnboundedUnlimited`]) rather than
    /// scanning the whole tape; with a limit the window resolves to a tail
    /// (end-anchored), a head (start-anchored), or the watermark-anchored
    /// latest-N window.
    pub limit: Option<usize>,
    /// How a folded read treats bucket edges. Direct reads are unaffected:
    /// a stored bar is a whole bar under any policy.
    pub policy: FoldPolicy,
}

impl Validate for ReadRequest {
    type Err = PlanError;

    /// Refuse what no coverage could answer: a request naming no symbol,
    /// and a window ending before it begins. Everything needing the catalog
    /// or the tape — unknown symbols, unsupported spacings, open-ended
    /// reads, uncovered ranges — is the planner's decision, not this
    /// shape's.
    fn validate(&self) -> Result<(), Self::Err> {
        if self.symbol.is_empty() {
            return Err(PlanError::BlankSymbol);
        }
        if let (Some(start), Some(end)) = (self.range.start_ms, self.range.end_ms) {
            if start > end {
                return Err(PlanError::InvertedRange { start, end });
            }
        }
        Ok(())
    }
}

/// The bars a read served, ascending by `ts`.
///
/// `served` is the bounded window this read answered — the request's range
/// with every open end resolved through its limit. How much of that window
/// was actually covered is [`provenance`](Self::provenance)'s to report,
/// not this field's: an empty `bars` over a requested range is an answered
/// read with nothing in it, distinctly from a refused one.
#[derive(Debug, Clone, PartialEq)]
pub struct CandleSeries {
    /// The tape that was read, at its stored spacing.
    pub key: TapeKey,
    /// Committed bars, ascending by `ts`, every adjacent pair
    /// `validate_new_bar`-clean.
    pub bars: Vec<Bar>,
    /// The window this read answered.
    pub served: TimeRange,
    /// How every span of the answer was produced, in ascending order. The
    /// evidence a report's `data_limits` names: which stored spans served
    /// the read, at which origin, and whether each span's buckets are whole.
    pub provenance: Vec<SpanPlan>,
}

/// How an append resolves a row that is already stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WritePolicy {
    /// Keep the stored row; count the collision. Stream append, backfill,
    /// and repair stay concurrently safe without a cursor or a lock
    /// between them, because re-delivery is an observable conflict count
    /// rather than a silent overwrite.
    InsertIfAbsent,
    /// Overwrite the stored row with the batch's row. Corrections to
    /// already-committed rows are deliberately *not* reachable this way —
    /// they go through [`CandleStore::repair_one`], the explicit,
    /// audit-logged path — so this policy serves re-seeds and restatements,
    /// where the caller names the span and owns the overwrite. The sequence number is the caller's idempotency
    /// token (repair pass, seed generation); it is carried, not compared,
    /// because the primary key already decides what collides.
    UpsertBySeq {
        /// The caller's idempotency token for this write.
        seq: u64,
    },
}

/// What one append did, counted rather than discarded: a re-delivered
/// batch reports `inserted == 0, conflicts == n`, which is an observable
/// fact about the world, not a silent no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AppendOutcome {
    /// Rows newly stored.
    pub inserted: usize,
    /// Rows that collided with a stored row.
    pub conflicts: usize,
    /// Rows the price-scale policy refused. Counted, never stored, never
    /// rounded, never truncated: a caller that ignores a nonzero `rejected`
    /// has a hole, and the coverage classifier will find it.
    pub rejected: usize,
}

/// Sort ascending and prove it: every adjacent pair passes the shared
/// monotonicity check. This is the boundary every reader inherits — a tape
/// that was written shuffled still reads back as a valid series.
pub(crate) fn order_and_validate(mut bars: Vec<Bar>) -> Result<Vec<Bar>, StoreError> {
    bars.sort_by_key(|bar| bar.ts);
    let mut last: Option<&Bar> = None;
    for bar in &bars {
        validate_new_bar(last, bar)
            .map_err(|e| StoreError::InvalidBar(format!("stored series out of order: {e}")))?;
        last = Some(bar);
    }
    Ok(bars)
}

#[cfg(test)]
#[path = "../tests/store_ordering.rs"]
mod tests_ordering;

#[cfg(test)]
#[path = "../tests/store_coverage.rs"]
mod tests_coverage;

/// The synchronous, object-safe candle interface.
pub trait CandleStore: Send + Sync + std::fmt::Debug {
    /// Every registered market, in id order.
    fn markets(&self) -> Result<Vec<Market>, StoreError>;
    /// Resolve `(symbol, venue)` to its durable id. Reads, never creates:
    /// unknown is `Ok(None)`, not an error.
    fn resolve(&self, symbol: &str, venue: &str) -> Result<Option<SymbolId>, StoreError>;
    /// Where one tape covers. `Ok(None)` means the store answered "no
    /// rows"; a store that could not be reached answers `Err`, never
    /// `Ok(None)`.
    fn coverage(&self, key: TapeKey) -> Result<Option<Coverage>, StoreError>;
    /// Store a batch of committed bars under `policy`. Every representable
    /// row lands atomically — the log batch commits or refuses as one, so a
    /// write failure stores none of it. Rows the market's price scale
    /// cannot represent are refused per-row before anything is logged and
    /// counted in [`AppendOutcome::rejected`]; the representable remainder
    /// still lands. Bars are validated before the write, so a malformed
    /// batch refuses without storing its good prefix.
    fn append(
        &self,
        key: TapeKey,
        batch: &[Bar],
        policy: WritePolicy,
    ) -> Result<AppendOutcome, StoreError>;
    /// Overwrite one committed row, explicitly. The caller names the row it
    /// means — `key` plus `ts_ms`, which must equal the bar's own stamp —
    /// and the overwrite is audit-logged. This is the only path that
    /// corrects a committed row: corrections are deliberately not reachable
    /// through [`append`](CandleStore::append), so no restatement can
    /// silently rewrite history. Refuses when no row is stored at
    /// `(key, ts_ms)`, when the stamps disagree, and when the replacement
    /// bar is invalid or unrepresentable at the market's scale.
    fn repair_one(&self, key: TapeKey, ts_ms: i64, bar: &Bar) -> Result<(), StoreError>;
    /// Serve a window of one tape: bars ascending by `ts`, every adjacent
    /// pair `validate_new_bar`-clean, regardless of write order.
    fn read(&self, req: &ReadRequest) -> Result<CandleSeries, StoreError>;
}
