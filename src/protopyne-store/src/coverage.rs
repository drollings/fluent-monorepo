//! What the tape holds, and whether it is enough.
//!
//! The store answers one question about its contents — *what did the tape
//! actually hold over this span, and where is it incomplete?* — and this
//! module is that answer's vocabulary. [`Coverage`] says where the tape
//! covers; [`classify`] says what that coverage means for a repair decision.
//!
//! Two rules hold throughout. First, the classifier's input is
//! `Option<Coverage>`, never a swallowed error: `Some` means the store
//! answered, `None` means the store answered "no rows", and a store that
//! could not be reached produces `Err(StoreError :: …)` at the caller and
//! never reaches [`classify`]. A transport failure and an empty table both
//! reading as "no data" is what triggers a multi-year backfill on the next
//! repair; the signature makes that confusion unrepresentable. Second, the
//! classification is a **task-value** statement — repair or not — never a
//! confidence reading, so `GapClass` has no "maybe" and no score.
//!
//! Session awareness lives at construction, not in the classifier. A tape
//! whose session is simply closed holds no bars for the closed stretch, and
//! no classifier can tell "closed" from "missing" by staring at the bars.
//! The caller that knows the session calendar refines the covered runs before
//! building coverage ([`Coverage::from_runs`]); the raw grid scan
//! ([`Coverage::from_bars`]) reports session gaps as the discontinuities they
//! are. What the weekend means is decided by which `now_ms` is passed in —
//! a closed session produces no lag because no bars were due — not by a flag
//! on this type.

use fluent_wvr::prelude::*;
use protopyne_types::{Bar, Timeframe, Validate};

use crate::range::TimeRange;

/// One observed stamp on the tape's high-water mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Watermark {
    /// The stamp in epoch milliseconds.
    pub ts_ms: i64,
}

/// Where the tape covers, as maximal half-open bar spans plus the counts that
/// interpret them.
///
/// The fields are read access, not construction surface: build through
/// [`Coverage::from_bars`] or [`Coverage::from_runs`]. The first and last
/// stamps bound the tape; `bars` is the observed row count; `runs` are the
/// maximal covered spans in ascending order, each `[first_stamp,
/// last_stamp + step)` so a hole between two runs is exactly the missing
/// bars' span.
///
/// A deviation from the usual shape, stated plainly: this type cannot be
/// `Copy`. Locating an interior hole needs the run boundaries, and the three
/// pinned fields cannot name them. Dropping `Copy` is what keeps [`holes`]
/// honest — a `Copy` coverage could name how far the tape reaches but never
/// where it is torn, and the repair loop needs the second fact.
///
/// [`holes`]: Coverage::holes
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coverage {
    /// First observed stamp.
    pub first: Watermark,
    /// Last observed stamp.
    pub last: Watermark,
    /// Observed row count.
    pub bars: u64,
    /// Maximal covered bar spans, ascending and non-overlapping.
    runs: Vec<TimeRange>,
}

impl Coverage {
    /// Build from the bars actually observed, scanning grid continuity at
    /// the bar spacing.
    ///
    /// Consecutive bars exactly one step apart continue a run; any other
    /// distance starts a new one. This is session-oblivious: an overnight or
    /// weekend stretch with no bars is a discontinuity here. Callers that
    /// hold a session calendar build the runs themselves and use
    /// [`Coverage::from_runs`] instead. `None` when there is nothing to
    /// describe — the caller maps that to an absent `Coverage`, which
    /// [`classify`] reads as [`GapClass::Empty`]. The stamps are mapped out
    /// of the bars and the run walk below runs over them, so the two
    /// constructors cannot drift apart.
    #[must_use]
    pub fn from_bars(bars: &[Bar], tf: Timeframe) -> Option<Self> {
        Self::from_stamps(
            &bars
                .iter()
                .map(|bar| bar.ts.timestamp_millis())
                .collect::<Vec<_>>(),
            tf,
        )
    }

    /// Build from stamps alone, walking exactly the run rule
    /// [`from_bars`](Self::from_bars) walks — no decimal decode, no `Bar`
    /// materialization. The read path feeds this the stamps of the window it
    /// will actually serve, so coverage planning stops fetching the whole
    /// tape per read. `None` exactly when `from_bars` answers `None`: no
    /// stamps, no coverage.
    #[must_use]
    pub fn from_stamps(stamps: &[i64], tf: Timeframe) -> Option<Self> {
        let step = tf.duration_ms();
        let first = stamps.first()?;
        let (mut run_first, mut run_last) = (*first, *first);
        let mut runs = Vec::new();
        for stamp in &stamps[1..] {
            if *stamp == run_last + step {
                run_last = *stamp;
            } else {
                runs.push(run_span(run_first, run_last, step));
                run_first = *stamp;
                run_last = *stamp;
            }
        }
        runs.push(run_span(run_first, run_last, step));
        let last = stamps.last().expect("a nonempty slice has a last stamp");
        Some(Self {
            first: Watermark { ts_ms: *first },
            last: Watermark { ts_ms: *last },
            bars: stamps.len() as u64,
            runs,
        })
    }

    /// Build from the covered runs the caller already refined — typically
    /// with a session calendar, so closed stretches are not runs and do not
    /// read as holes.
    ///
    /// Runs are expected ascending, non-overlapping, and bounded; `bar_count`
    /// is the observed row count independent of the spans. `None` when there
    /// are no runs to describe.
    #[must_use]
    pub fn from_runs(runs: &[TimeRange], bar_count: u64) -> Option<Self> {
        let first = runs.first()?.start_ms?;
        let last = runs.last()?.end_ms?.saturating_sub(1);
        Some(Self {
            first: Watermark { ts_ms: first },
            last: Watermark { ts_ms: last },
            bars: bar_count,
            runs: runs.to_vec(),
        })
    }

    /// Tape-wide bounds with window-scoped runs and count: `first_ms` and
    /// `last_ms` describe the whole tape (read from the bounds query);
    /// `bars` counts the stamps observed inside the fetch window and `runs`
    /// are maximal within it. This is the read path's planning input —
    /// never an audit answer: [`holes`] stays exact for wants inside the
    /// fetch window (the only wants the plan resolves), while a want
    /// reaching past the window would read its uncovered margin as a hole,
    /// so callers must only plan wants the window covers. The tape-wide row
    /// count is deliberately absent — `COUNT(*)` walks the symbol, which is
    /// O(tape) per read — and nothing on the read path reads one.
    ///
    /// [`holes`]: Coverage::holes
    #[must_use]
    pub fn from_extent_and_runs(
        first_ms: i64,
        last_ms: i64,
        bars: u64,
        runs: Vec<TimeRange>,
    ) -> Self {
        Self {
            first: Watermark { ts_ms: first_ms },
            last: Watermark { ts_ms: last_ms },
            bars,
            runs,
        }
    }

    /// The maximal covered bar spans, ascending.
    #[must_use]
    pub fn runs(&self) -> &[TimeRange] {
        &self.runs
    }

    /// The bounding overlap of the tape with a request: the window a planner
    /// should read. Interior holes are the [`holes`] question, not this one.
    ///
    /// [`holes`]: Coverage::holes
    #[must_use]
    pub fn intersect(&self, want: &TimeRange) -> Option<TimeRange> {
        let extent = TimeRange {
            start_ms: Some(self.first.ts_ms),
            end_ms: Some(self.last.ts_ms.saturating_add(1)),
        };
        extent.intersect(want)
    }

    /// The parts of `want` the tape does not cover, as maximal sub-spans.
    ///
    /// An unbounded side of `want` is clipped to the tape's extent: a hole is
    /// only defined between two known bounds, and the stretch past the tape's
    /// last bar is staleness, not a hole — that is [`classify`]'s question,
    /// not this method's.
    ///
    /// The `tf` parameter names the grid the run convention was built on; the
    /// geometry itself is exact run geometry in milliseconds rather than an
    /// origin-relative bar count. `tf` is the frame the caller divides hole
    /// widths by when it applies [`FreshnessPolicy::max_gap_bars`].
    #[must_use]
    pub fn holes(&self, want: &TimeRange, _tf: Timeframe) -> Vec<TimeRange> {
        let lo = match want.start_ms {
            Some(start) => start.max(self.first.ts_ms),
            None => self.first.ts_ms,
        };
        let hi = match want.end_ms {
            Some(end) => end.min(self.last.ts_ms.saturating_add(1)),
            None => self.last.ts_ms.saturating_add(1),
        };
        if lo >= hi {
            return Vec::new();
        }
        let mut holes = Vec::new();
        let mut cursor = lo;
        for run in &self.runs {
            let (Some(run_start), Some(run_end)) = (run.start_ms, run.end_ms) else {
                continue;
            };
            if run_end <= cursor {
                continue;
            }
            if run_start > cursor {
                holes.push(TimeRange {
                    start_ms: Some(cursor),
                    end_ms: Some(run_start.min(hi)),
                });
            }
            cursor = cursor.max(run_end);
            if cursor >= hi {
                break;
            }
        }
        if cursor < hi {
            holes.push(TimeRange {
                start_ms: Some(cursor),
                end_ms: Some(hi),
            });
        }
        holes
    }
}

/// One bar-span run: `[first_stamp, last_stamp + step)`.
fn run_span(first_ms: i64, last_ms: i64, step: i64) -> TimeRange {
    TimeRange {
        start_ms: Some(first_ms),
        end_ms: Some(last_ms.saturating_add(step)),
    }
}

/// What the tape's coverage means for a repair decision. Every variant is a
/// verdict — repair or not — never a confidence reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GapClass {
    /// The store holds no rows at all.
    Empty,
    /// The tail trails the clock past the policy's lag bound.
    Stale {
        /// The span after the tape's last bar, `[last + step, now]`.
        behind: TimeRange,
    },
    /// The ends are current but the middle is torn.
    Torn {
        /// Every hole wider than the policy tolerates, ascending.
        holes: Vec<TimeRange>,
    },
    /// Covered and current: leave it alone.
    Fresh,
}

impl GapClass {
    /// The spans a repair pass should fill, ascending. Empty for the two
    /// verdicts that need no repair. The repair worker consumes this; it
    /// never re-derives which spans to fill from the variant.
    #[must_use]
    pub fn repair_spans(&self) -> Vec<TimeRange> {
        match self {
            Self::Empty | Self::Fresh => Vec::new(),
            Self::Stale { behind } => vec![*behind],
            Self::Torn { holes } => holes.clone(),
        }
    }
}

/// The measured lag bound: bars a tape may trail the clock before it reads
/// stale. See the `max_lag_bars` field documentation for the measurement
/// behind this value. The calibration gate asserts the recorded fixture and
/// this constant agree, so neither can drift without the other failing.
pub const DEFAULT_MAX_LAG_BARS: u64 = 1;

/// Repair bounds a tape is held to. Configurable from the schema-driven
/// settings form rather than a hardcoded constant, so the operator's
/// thresholds and the classifier's thresholds are the same value read from
/// one place.
#[derive(Debug, Clone, Copy, bon::Builder, FieldAccess, Describable)]
#[builder(start_fn = new)]
pub struct FreshnessPolicy {
    /// Bars the tape may trail the clock before it reads stale. The boundary
    /// is inclusive: exactly this far behind is still [`GapClass::Fresh`].
    ///
    /// The default is frozen by measurement, not chosen: over the labelled
    /// gap corpus (`tests/data/gap_fixtures.json`, six must-fire tapes
    /// against five must-not-fire), `1` is the only bound in `0..=6` with
    /// recall `1.0` and control false-fire `0.0`. At `0` the single-late-print
    /// control fires (false-fire `0.2`); at `2` the two-bar H4 tail sits
    /// exactly on the inclusive boundary and stays silent (recall `0.83`);
    /// past `2` the three-bar M1 tail goes quiet too (recall `0.67`). The
    /// inherited default of `2` did not survive the measurement, and the
    /// change is recorded here rather than smoothed over.
    #[field(
        desc = "Bars the tape may trail the clock before it reads stale",
        min = 0
    )]
    #[builder(default = DEFAULT_MAX_LAG_BARS)]
    pub max_lag_bars: u64,
    /// Whole missing bars tolerated inside an otherwise current tape. A gap
    /// of exactly this many missing bars is still [`GapClass::Fresh`]; zero
    /// tolerates nothing but sub-step slivers, which are not missing bars.
    #[field(
        desc = "Whole missing bars tolerated inside an otherwise current tape",
        min = 0
    )]
    #[builder(default = 0)]
    pub max_gap_bars: u64,
}

impl Validate for FreshnessPolicy {
    type Err = String;

    fn validate(&self) -> Result<(), Self::Err> {
        let _ = self;
        // Every non-negative pair is a coherent policy: usefulness is the
        // calibration's measured question, not a structural refusal.
        Ok(())
    }
}

impl WorkUnit for FreshnessPolicy {
    fn name(&self) -> &str {
        "store_freshness_policy"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        protopyne_types::check_for_execute(self, "freshness policy invalid")?;
        Ok(protopyne_types::valid_output("freshness policy valid"))
    }
}

impl_component!(FreshnessPolicy);

/// Say what a tape's coverage means: repair, or leave it alone.
///
/// `None` is an empty table, reported as [`GapClass::Empty`] — distinctly
/// from "the store could not be reached", which never gets here. The tail is
/// judged first: a tape that is both stale and torn reports
/// [`GapClass::Stale`], because the tail is what a repair pass fixes first.
/// Weekend handling is the caller's choice of `now_ms` — no bars were due
/// while the session was closed, so a closed session produces no lag — not a
/// flag on this function.
#[must_use]
pub fn classify(
    coverage: Option<Coverage>,
    now_ms: i64,
    tf: Timeframe,
    policy: FreshnessPolicy,
) -> GapClass {
    let Some(cov) = coverage else {
        return GapClass::Empty;
    };
    let step = tf.duration_ms();
    let behind_bars = now_ms
        .saturating_sub(cov.last.ts_ms)
        .max(0)
        .saturating_div(step);
    // A bound past `i64::MAX` bars means "never stale": saturate rather than
    // refuse, so an absurd policy degrades to infinite patience, not a panic.
    let max_lag_bars: i64 = policy.max_lag_bars.try_into().unwrap_or(i64::MAX);
    if behind_bars > max_lag_bars {
        return GapClass::Stale {
            behind: TimeRange {
                start_ms: Some(cov.last.ts_ms.saturating_add(step)),
                end_ms: Some(now_ms.saturating_add(1)),
            },
        };
    }
    let extent = TimeRange {
        start_ms: Some(cov.first.ts_ms),
        end_ms: Some(cov.last.ts_ms.saturating_add(1)),
    };
    let holes: Vec<TimeRange> = cov
        .holes(&extent, tf)
        .into_iter()
        .filter(|hole| {
            let (Some(start), Some(end)) = (hole.start_ms, hole.end_ms) else {
                return true;
            };
            let missing_bars = end.saturating_sub(start).saturating_div(step);
            let tolerated: i64 = policy.max_gap_bars.try_into().unwrap_or(i64::MAX);
            missing_bars > tolerated
        })
        .collect();
    if holes.is_empty() {
        GapClass::Fresh
    } else {
        GapClass::Torn { holes }
    }
}

#[cfg(test)]
#[path = "../tests/coverage_stamps.rs"]
mod tests_stamps;

#[cfg(test)]
#[path = "../tests/gap_classify.rs"]
mod tests_classify;

#[cfg(test)]
#[path = "../tests/coverage_holes.rs"]
mod tests_holes;

#[cfg(test)]
#[path = "../tests/common/calibration.rs"]
pub(crate) mod test_calibration;

#[cfg(test)]
#[path = "../tests/gap_calibration.rs"]
mod tests_calibration;

#[cfg(test)]
#[path = "../tests/gap_calibration_stability.rs"]
mod tests_calibration_stability;
