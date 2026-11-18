//! Millisecond spans with open ends: what was asked for, what is covered,
//! what is missing.
//!
//! A [`TimeRange`] is half-open, `[start, end)`. Either bound may be absent,
//! and absence means "no bound on that side", never zero. An open-ended read
//! with no limit is refused by the planner rather than silently scanning the
//! whole tape; the range type itself stays honest about what was asked.
//!
//! Every span the store names — requested windows, covered runs, holes,
//! staleness — is one of these, so there is exactly one span vocabulary to
//! keep consistent. Inverted construction refuses with the shared plan error
//! rather than clamping.

use crate::error::PlanError;

/// A half-open millisecond span with optional bounds.
///
/// Stamps name instants; spans name coverage over them. A covered bar stamped
/// `t` occupies the stamp-space point `t`, and a run of contiguous bars from
/// `first` to `last` spans `[first, last + 1)`: the `+1` is what makes a
/// single covered stamp a non-empty half-open span. Bar-span ranges — the
/// runs [`Coverage`](crate::coverage::Coverage) keeps — extend the end by the
/// bar spacing instead, so a hole between two runs is exactly the missing
/// bars' span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct TimeRange {
    /// Lower bound, inclusive. `None` means no lower bound.
    pub start_ms: Option<i64>,
    /// Upper bound, exclusive. `None` means no upper bound.
    pub end_ms: Option<i64>,
}

impl TimeRange {
    /// Build a span, refusing an inverted window fail-closed rather than
    /// clamping it into an empty one.
    pub fn new(start_ms: Option<i64>, end_ms: Option<i64>) -> Result<Self, PlanError> {
        if let (Some(start), Some(end)) = (start_ms, end_ms) {
            if start > end {
                return Err(PlanError::InvertedRange { start, end });
            }
        }
        Ok(Self { start_ms, end_ms })
    }

    /// A fully bounded span.
    pub fn bounded(start_ms: i64, end_ms: i64) -> Result<Self, PlanError> {
        Self::new(Some(start_ms), Some(end_ms))
    }

    /// Everything up to `end_ms`: the shape of a "latest N" read with its
    /// lower bound still to be supplied by a limit.
    #[must_use]
    pub fn to_end(end_ms: i64) -> Self {
        Self {
            start_ms: None,
            end_ms: Some(end_ms),
        }
    }

    /// A span with no bounds on either side.
    #[must_use]
    pub fn unbounded() -> Self {
        Self {
            start_ms: None,
            end_ms: None,
        }
    }

    /// The last `span_ms` ending at `end_ms`: `[end_ms - span_ms, end_ms)`.
    ///
    /// A tail materializes a "latest N" window into concrete bounds once, at
    /// plan time. Carrying a bar count or an offset instead would re-resolve
    /// against a tape that may have grown since — a cursor over a growing
    /// dataset. Callers convert bars to milliseconds through
    /// `Timeframe :: duration_ms` (the single span table); this constructor
    /// stays in milliseconds so it never needs a second table. The start
    /// saturates and clamps at zero, mirroring `adjust_fetch_bounds`: stamps
    /// before the epoch are not a window the tape can hold, and an anchor
    /// at or before zero yields an empty window rather than an inverted one.
    #[must_use]
    pub fn tail(span_ms: u64, end_ms: i64) -> Self {
        let span = i64::try_from(span_ms).unwrap_or(i64::MAX);
        let start = end_ms.saturating_sub(span).max(0).min(end_ms);
        Self {
            start_ms: Some(start),
            end_ms: Some(end_ms),
        }
    }

    /// Whether both bounds are present.
    #[must_use]
    pub fn is_bounded(&self) -> bool {
        self.start_ms.is_some() && self.end_ms.is_some()
    }

    /// Whether the stamp falls in `[start, end)`. An absent bound accepts
    /// everything on its side.
    #[must_use]
    pub fn contains(&self, ts_ms: i64) -> bool {
        self.start_ms.is_none_or(|start| ts_ms >= start)
            && self.end_ms.is_none_or(|end| ts_ms < end)
    }

    /// Whether the two spans share at least one millisecond. Touching at a
    /// single bound (`[0, 10)` and `[10, 20)`) does not overlap: the shared
    /// point belongs to exactly one of them.
    #[must_use]
    pub fn overlaps(&self, other: &TimeRange) -> bool {
        let disjoint = match (self.end_ms, other.start_ms) {
            (Some(end), Some(start)) => end <= start,
            _ => false,
        } || match (other.end_ms, self.start_ms) {
            (Some(end), Some(start)) => end <= start,
            _ => false,
        };
        !disjoint
    }

    /// Whether this span sits entirely inside `outer`. An absent bound on
    /// `outer` accepts everything on its side; an absent bound on `self`
    /// needs the same absence on `outer` — an unbounded side is only
    /// "inside" an unbounded side.
    #[must_use]
    pub fn contained_in(&self, outer: &TimeRange) -> bool {
        let start_ok = match (self.start_ms, outer.start_ms) {
            (Some(start), Some(lo)) => start >= lo,
            (None, Some(_)) => false,
            _ => true,
        };
        let end_ok = match (self.end_ms, outer.end_ms) {
            (Some(end), Some(hi)) => end <= hi,
            (None, Some(_)) => false,
            _ => true,
        };
        start_ok && end_ok
    }

    /// The shared part of the two spans, or `None` when they are disjoint.
    /// Absent bounds survive the intersection: the result is only as bounded
    /// as both inputs.
    #[must_use]
    pub fn intersect(&self, other: &TimeRange) -> Option<TimeRange> {
        if !self.overlaps(other) {
            return None;
        }
        let start_ms = match (self.start_ms, other.start_ms) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        let end_ms = match (self.end_ms, other.end_ms) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        Some(TimeRange { start_ms, end_ms })
    }
}

#[cfg(test)]
#[path = "../tests/range_span.rs"]
mod tests;
