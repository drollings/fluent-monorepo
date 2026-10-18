//! Read planning: resolve what was asked against what is stored, purely.
//!
//! [`ReadPlan::resolve_for`] is a pure function over a tape lookup, a
//! fetched coverage, and the request — no I/O, no clock, no gating. The
//! impure read path fetches the coverage first, plans against it, then
//! fetches exactly the planned spans: a plan that resolves is a promise the
//! subsequent fetch keeps, because both read the same coverage.
//!
//! The fields stay private behind accessors so a plan cannot exist
//! unresolved: there is no literal that builds one, only `resolve_for`.
//!
//! The stamp-window helpers below are the one coverage path the served read
//! walks: [`coverage_windows`] derives the fetch window plus the margined
//! stamp window from the tape's extent, and [`coverage_from_stamps`]
//! rebuilds coverage from the stamps inside that window — O(window), never
//! O(tape).

use protopyne_types::{Timeframe, Validate};

use crate::coverage::Coverage;
use crate::error::{PlanError, StoreError};
use crate::fold::{bucket_start, FoldPolicy};
use crate::key::{SymbolId, TapeKey};
use crate::range::TimeRange;
use crate::store::ReadRequest;

/// Where one planned span is served from. A single tier is the honest
/// description of this deployment; the enum is the seam a second tier
/// would fill without an API break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanOrigin {
    /// Served from the committed tape.
    Stored,
}

/// One servable fragment of the requested window: the stored span to
/// fetch, where it comes from, and whether its buckets are whole.
#[derive(Debug, Clone, PartialEq)]
pub struct SpanPlan {
    span: TimeRange,
    origin: SpanOrigin,
    complete: bool,
}

impl SpanPlan {
    /// The stored span to fetch.
    #[must_use]
    pub fn span(&self) -> TimeRange {
        self.span
    }

    /// Where the span is served from.
    #[must_use]
    pub fn origin(&self) -> SpanOrigin {
        self.origin
    }

    /// Whether the span's buckets are whole. A truncated first bucket's
    /// `open` is not the bucket open; a `false` here is the plan reporting
    /// that, and callers must surface it rather than answering with a
    /// partial bucket presented as whole.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.complete
    }
}

/// A resolved read: the tape at its stored spacing, the requested spacing,
/// the stored spans to fetch, and the coverage the resolution read.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadPlan {
    key: TapeKey,
    timeframe: Timeframe,
    spans: Vec<SpanPlan>,
    covered: Coverage,
}

impl ReadPlan {
    /// The tape to read, at its stored spacing.
    #[must_use]
    pub fn key(&self) -> TapeKey {
        self.key
    }

    /// The requested spacing: equal to the key's when no fold is involved.
    #[must_use]
    pub fn timeframe(&self) -> Timeframe {
        self.timeframe
    }

    /// The stored spans to fetch, ascending.
    #[must_use]
    pub fn spans(&self) -> &[SpanPlan] {
        &self.spans
    }

    /// The coverage this plan resolved against.
    #[must_use]
    pub fn covered(&self) -> &Coverage {
        &self.covered
    }

    /// Resolve a request against fetched coverage: pure, no I/O.
    ///
    /// The tape lookup is what the engine read from its market index: the
    /// durable id plus the tape's stored spacing. Callers that hold the
    /// market row pass both; there is no second spelling of this decision.
    ///
    /// Refusals, in order: the request's own shape (`BlankSymbol`,
    /// `InvertedRange`), an unregistered symbol, a spacing the tape cannot
    /// serve (finer than stored, or a non-multiple — a source bar must nest
    /// inside exactly one target bucket), an open-ended read with no limit,
    /// an empty tape (there is no coverage to plan against; the repair
    /// verdict for it is `Empty`, not a read), and a wanted window torn by
    /// an interior hole. A tail past the tape's end is staleness, not a
    /// hole: it resolves, and the spans report the shortfall by ending
    /// where the coverage ends.
    /// Resolve a request against fetched coverage: pure, no I/O.
    ///
    /// The tape lookup is what the engine read from its market index: the
    /// durable id plus the tape's stored spacing. Callers that hold the
    /// market row pass both; there is no second spelling of this decision.
    ///
    /// Refusals, in order: the request's own shape (`BlankSymbol`,
    /// `InvertedRange`), an unregistered symbol, a spacing the tape cannot
    /// serve (finer than stored, or a non-multiple — a source bar must nest
    /// inside exactly one target bucket), an open-ended read with no limit,
    /// an empty tape (there is no coverage to plan against; the repair
    /// verdict for it is `Empty`, not a read), and a wanted window torn by
    /// an interior hole. A tail past the tape's end is staleness, not a
    /// hole: it resolves, and the spans report the shortfall by ending
    /// where the coverage ends.
    pub fn resolve_for(
        tape: Option<(SymbolId, Timeframe)>,
        coverage: Option<Coverage>,
        request: &ReadRequest,
    ) -> Result<Self, PlanError> {
        request.validate()?;
        let (symbol_id, stored) =
            tape.ok_or_else(|| PlanError::UnknownSymbol(request.symbol.clone()))?;
        if !request.timeframe.is_whole_multiple_of(stored) {
            return Err(PlanError::UnsupportedTimeframe {
                symbol: request.symbol.clone(),
                timeframe: request.timeframe,
            });
        }
        let wanted = wanted_window(request, coverage.as_ref())?;
        let Some(covered) = coverage else {
            return Err(PlanError::UncoveredRange {
                span: wanted,
                want: request.timeframe,
            });
        };
        if let Some(hole) = covered.holes(&wanted, request.timeframe).into_iter().next() {
            return Err(PlanError::UncoveredRange {
                span: hole,
                want: request.timeframe,
            });
        }
        let folding = request.timeframe != stored;
        // `wanted_window` bounds every window it returns; the refusal below
        // is unreachable, kept total rather than panicking.
        let (Some(want_start), Some(want_end)) = (wanted.start_ms, wanted.end_ms) else {
            return Err(PlanError::UnboundedUnlimited);
        };
        let fetch = widen(
            want_start,
            want_end,
            request.timeframe,
            stored,
            request.policy,
        );
        let mut spans = Vec::new();
        for run in covered.runs() {
            let Some(span) = run.intersect(&fetch) else {
                continue;
            };
            let complete = match (folding, request.policy) {
                (false, _) => true,
                (true, FoldPolicy::TruncateEdges) => false,
                (true, FoldPolicy::ExtendToFullBuckets) => {
                    buckets_whole(&covered, &span, request.timeframe)
                }
            };
            spans.push(SpanPlan {
                span,
                origin: SpanOrigin::Stored,
                complete,
            });
        }
        Ok(Self {
            key: TapeKey {
                symbol_id,
                timeframe: stored,
            },
            timeframe: request.timeframe,
            spans,
            covered,
        })
    }
}

/// The span a read must fetch over: the wanted window widened to enclosing
/// bucket boundaries for a folded extend-policy read. This is exactly the
/// span [`resolve`](ReadPlan::resolve) intersects coverage against,
/// factored out so the stamp fetch covers what the planner will see — the
/// planner re-derives the same bounds internally, idempotently, over the
/// same inputs.
pub(crate) fn fetch_span(
    request: &ReadRequest,
    stored: Timeframe,
    wanted: &TimeRange,
) -> Result<TimeRange, PlanError> {
    // `wanted_window` bounds every window it returns; the refusal below is
    // unreachable, kept total rather than panicking.
    let (Some(start), Some(end)) = (wanted.start_ms, wanted.end_ms) else {
        return Err(PlanError::UnboundedUnlimited);
    };
    Ok(widen(start, end, request.timeframe, stored, request.policy))
}

/// A bar limit as a millisecond span at `step_ms`, saturating instead of
/// overflowing: an absurd limit degrades to an unbounded span, not a wrap.
fn limit_span_ms(limit: usize, step_ms: i64) -> u64 {
    let bars = u64::try_from(limit).unwrap_or(u64::MAX);
    let step = u64::try_from(step_ms).unwrap_or(0);
    bars.saturating_mul(step)
}

/// The bounded window a request asks for. Every open end needs a limit to
/// close it: an end-anchored read resolves to a tail, a start-anchored read
/// to a head, and a fully open read with a limit to the watermark-anchored
/// latest-N window — the query the scan-everything cliff refused to bound.
/// Without a limit there is no window, only a refusal.
pub(crate) fn wanted_window(
    request: &ReadRequest,
    coverage: Option<&Coverage>,
) -> Result<TimeRange, PlanError> {
    let step = request.timeframe.duration_ms();
    let span_ms = request.limit.map(|limit| limit_span_ms(limit, step));
    match (request.range.start_ms, request.range.end_ms, span_ms) {
        (Some(start), Some(end), _) => Ok(TimeRange {
            start_ms: Some(start),
            end_ms: Some(end),
        }),
        (None, Some(end), Some(span)) => Ok(TimeRange::tail(span, end)),
        (Some(start), None, Some(span)) => {
            let span_i64 = i64::try_from(span).unwrap_or(i64::MAX);
            Ok(TimeRange {
                start_ms: Some(start),
                end_ms: Some(start.saturating_add(span_i64)),
            })
        }
        (None, None, Some(span)) => {
            let Some(covered) = coverage else {
                // No watermark to anchor a latest-N window against, and no
                // coverage to plan from: the whole unbounded ask is
                // uncovered.
                return Err(PlanError::UncoveredRange {
                    span: TimeRange::unbounded(),
                    want: request.timeframe,
                });
            };
            Ok(TimeRange::tail(
                span,
                covered.last.ts_ms.saturating_add(step),
            ))
        }
        _ => Err(PlanError::UnboundedUnlimited),
    }
}

/// Widen a wanted window to enclosing bucket boundaries for a folded read
/// under [`FoldPolicy::ExtendToFullBuckets`], so the fetch covers whole
/// edge buckets. Anything else — a direct read, or diagnostics under
/// `TruncateEdges` — fetches exactly what was asked.
fn widen(
    start_ms: i64,
    end_ms: i64,
    want: Timeframe,
    stored: Timeframe,
    policy: FoldPolicy,
) -> TimeRange {
    if want == stored || policy == FoldPolicy::TruncateEdges {
        return TimeRange {
            start_ms: Some(start_ms),
            end_ms: Some(end_ms),
        };
    }
    let step = want.duration_ms();
    let floor = bucket_start(want, start_ms);
    let end_floor = bucket_start(want, end_ms);
    let ceil = if end_floor == end_ms {
        end_ms
    } else {
        end_floor.saturating_add(step)
    };
    TimeRange {
        start_ms: Some(floor),
        end_ms: Some(ceil),
    }
}

/// Whether every folded bucket overlapping `span` sits entirely inside the
/// covered runs — the per-span form of the extend policy's promise that a
/// returned `open` is the true bucket open. A bucket reaching past a run
/// into staleness or across a margin hole is partial, and the span covering
/// it reports so.
fn buckets_whole(covered: &Coverage, span: &TimeRange, want: Timeframe) -> bool {
    let (Some(start), Some(end)) = (span.start_ms, span.end_ms) else {
        return false;
    };
    if start >= end {
        return true;
    }
    let step = want.duration_ms();
    let mut bucket = bucket_start(want, start);
    while bucket < end {
        let bucket_end = bucket.saturating_add(step);
        let whole = covered.runs().iter().any(|run| {
            run.start_ms.is_some_and(|run_start| run_start <= bucket)
                && run.end_ms.is_some_and(|run_end| run_end >= bucket_end)
        });
        if !whole {
            return false;
        }
        bucket = bucket_end;
    }
    true
}

/// One tape's bounds from its stamps alone: first stamp, last stamp.
/// `None` is an empty tape — answered, not failed.
///
/// Deliberately no row count: counting would walk every stamp, which would
/// put the read path back at O(tape) per read. The watermark anchor and
/// hole clipping need bounds, never a count; the audit path counts the
/// bars it validates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TapeExtent {
    /// First stored stamp.
    pub(crate) first_ms: i64,
    /// Last stored stamp.
    pub(crate) last_ms: i64,
}

/// The stamp-fetch window for a request against a known extent: pure over
/// the request, so every read layer derives the same bounds before touching
/// the tape. Returns the fetch window the plan resolves against plus the
/// margined stamp window — one stored step each side, so a run crossing
/// the fetch edge has its straddling pair inside the stamp fetch and the
/// walk sees the same continuity the whole-tape walk sees.
pub(crate) fn coverage_windows(
    stored: Timeframe,
    extent: &TapeExtent,
    req: &ReadRequest,
) -> Result<(TimeRange, TimeRange), StoreError> {
    // The row count is unused below: `wanted_window` reads only the last
    // stamp, so the bounds carry a zero count rather than a tape-wide
    // count that would cost O(tape) per read.
    let bounds = Coverage::from_extent_and_runs(extent.first_ms, extent.last_ms, 0, Vec::new());
    let wanted = wanted_window(req, Some(&bounds))?;
    let fetch = fetch_span(req, stored, &wanted)?;
    let step = stored.duration_ms();
    let stamp_window = TimeRange {
        start_ms: fetch.start_ms.map(|start| start.saturating_sub(step)),
        end_ms: fetch.end_ms.map(|end| end.saturating_add(step)),
    };
    Ok((fetch, stamp_window))
}

/// Runs from window stamps clipped to the fetch window, over the tape-wide
/// extent bounds: the planning input every read resolves against. Clipped
/// runs equal whole-tape runs intersected with the fetch — run adjacency
/// is local, and the stamp margin keeps both straddling pairs visible — so
/// holes, spans, and bucket-wholeness resolve identically. The bar count
/// is the stamps observed inside the fetch window, matching the runs'
/// scope; nothing on the read path reads a tape-wide count.
pub(crate) fn coverage_from_stamps(
    extent: &TapeExtent,
    fetch: &TimeRange,
    stamps: &[i64],
    stored: Timeframe,
) -> Coverage {
    let bars = stamps
        .iter()
        .filter(|stamp| fetch.contains(**stamp))
        .count();
    let runs = Coverage::from_stamps(stamps, stored)
        .map(|coverage| coverage.runs().to_vec())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|run| run.intersect(fetch))
        .collect::<Vec<_>>();
    Coverage::from_extent_and_runs(extent.first_ms, extent.last_ms, bars as u64, runs)
}

#[cfg(test)]
#[path = "../tests/plan_resolve.rs"]
mod tests_plan_resolve;

#[cfg(test)]
#[path = "../tests/plan_unbounded.rs"]
mod tests_plan_unbounded;

#[cfg(test)]
#[path = "../tests/warmup_roundtrip.rs"]
mod tests_warmup_roundtrip;

#[cfg(test)]
#[path = "../tests/plan_read_end_to_end.rs"]
mod tests_plan_read;
