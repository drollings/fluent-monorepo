//! The freshness-calibration harness: corpus loading plus the accept/reject
//! tally.
//!
//! One home for "read the labelled corpus, run every case at a candidate
//! bound, count what fired", shared by the calibration gate and the
//! stability gate so the two can never disagree about what was measured. The
//! tally mirrors the kernels' `ParityDashboard` shape — rows plus rates plus
//! a single green predicate — rather than inventing a second calibration
//! idiom: recall is the accept rate over the must-fire corpus, and the
//! control false-fire rate is the exact analog of the dashboard's.
//!
//! Included once from `coverage.rs`; consumer suites `use` it, never include
//! it again.
//!
//! `Case :: note` and `Case :: symbol` are record-keeping: they are read by
//! the engineer auditing the corpus and by `Debug`, not by the tally, so
//! dead-code is allowed here rather than threaded through fake uses.

#![allow(dead_code)]

use std::collections::BTreeMap;

use protopyne_types::Timeframe;
use serde::Deserialize;

use crate::commit::test_tapes::at_ms;
use crate::coverage::{classify, Coverage, FreshnessPolicy, GapClass};

/// The labelled corpus as recorded on disk.
#[derive(Debug, Clone, Deserialize)]
pub struct Corpus {
    /// The bound the measurement froze. The gate asserts the crate default
    /// equals this, so the two cannot drift.
    pub frozen_max_lag_bars: u64,
    /// The measured `(recall, control false-fire)` per candidate bound. The
    /// gate asserts the computation still produces these numbers, so a later
    /// change that shifts the measurement fails loudly instead of silently
    /// re-freezing.
    pub measured: BTreeMap<String, RecordedMeasure>,
    /// Tapes that genuinely require repair.
    pub positive: Vec<Case>,
    /// Healthy tapes that must be left alone.
    pub control: Vec<Case>,
}

/// One recorded sweep-table cell.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
pub struct RecordedMeasure {
    /// Fraction of must-fire cases classified non-`Fresh`.
    pub recall: f64,
    /// Fraction of must-not-fire cases classified non-`Fresh`.
    pub control_false_fire: f64,
}

/// One labelled tape.
#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    /// What the case proves.
    pub name: String,
    /// Why this tape belongs on its side of the corpus.
    pub note: String,
    /// Symbol the tape is labelled with.
    pub symbol: String,
    /// Bar spacing of the tape.
    pub timeframe: Timeframe,
    /// First stamp of the tape grid, in epoch milliseconds.
    pub t0_ms: i64,
    /// Which grid bars the tape holds.
    pub grid: Grid,
    /// Per-case hole tolerance; the sweep varies the lag bound, not this.
    #[serde(default)]
    pub max_gap_bars: u64,
    /// The instant the verdict is taken at.
    pub now: NowSpec,
    /// The verdict the case must produce.
    pub expect: ExpectClass,
    /// Torn cases additionally pin their hole count.
    #[serde(default)]
    pub expect_holes: Option<usize>,
}

/// Which bars of the grid the tape holds: `count` bars from `t0_ms` stepping
/// by the timeframe, minus the omitted positions. An empty grid is an empty
/// tape.
#[derive(Debug, Clone, Deserialize)]
pub struct Grid {
    /// Bars on the grid.
    pub count: u64,
    /// Grid positions with no bar, ascending.
    #[serde(default)]
    pub omit: Vec<u64>,
}

/// The instant the verdict is taken at: a fixed stamp, or a lag behind the
/// tape's last bar. A lag may name the candidate bound under test (`"k"`),
/// which is what makes the boundary case a boundary case at every candidate
/// instead of only at the frozen one.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum NowSpec {
    /// A lag in whole bars behind the tape's last bar.
    AfterLast {
        /// Bar count, or `"k"` for the candidate bound.
        after_last_bars: LagCount,
    },
    /// A fixed stamp.
    At {
        /// The stamp in epoch milliseconds.
        at_ms: i64,
    },
}

/// A bar lag: an absolute count, or the candidate bound under test.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum LagCount {
    /// An absolute bar count.
    Bars(u64),
    /// The candidate bound; the only accepted spelling is `"k"`.
    Candidate(String),
}

/// The verdict a case must produce.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub enum ExpectClass {
    /// No rows.
    Empty,
    /// The tail trails past the bound.
    Stale,
    /// Current ends, torn middle.
    Torn,
    /// Covered and current.
    Fresh,
}

impl ExpectClass {
    /// The verdict under test, stripped of its payload.
    fn of(class: &GapClass) -> Self {
        match class {
            GapClass::Empty => Self::Empty,
            GapClass::Stale { .. } => Self::Stale,
            GapClass::Torn { .. } => Self::Torn,
            GapClass::Fresh => Self::Fresh,
        }
    }
}

/// Which side of the corpus a row was measured on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Must fire.
    Positive,
    /// Must not fire.
    Control,
}

/// One measured row: the case, its side, what it produced at the candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct GapRow {
    /// The case name, as recorded.
    pub name: String,
    /// Which corpus the case came from.
    pub side: Side,
    /// The recorded verdict.
    pub expected: ExpectClass,
    /// The measured verdict.
    pub actual: ExpectClass,
    /// Holes reported when torn.
    pub holes: usize,
}

/// The accept/reject tally at one candidate bound.
#[derive(Debug, Clone, PartialEq)]
pub struct GapTally {
    /// One row per corpus case, positives first, in recorded order.
    pub rows: Vec<GapRow>,
}

impl GapTally {
    /// Measure the whole corpus at candidate bound `k`.
    #[must_use]
    pub fn for_k(corpus: &Corpus, k: u64) -> Self {
        let mut rows = Vec::with_capacity(corpus.positive.len() + corpus.control.len());
        for (side, case) in corpus
            .positive
            .iter()
            .map(|case| (Side::Positive, case))
            .chain(corpus.control.iter().map(|case| (Side::Control, case)))
        {
            let coverage = coverage_for(case);
            let now_ms = now_for(case, coverage.as_ref(), k);
            let policy = FreshnessPolicy::new()
                .max_lag_bars(k)
                .max_gap_bars(case.max_gap_bars)
                .build();
            let class = classify(coverage, now_ms, case.timeframe, policy);
            let holes = match &class {
                GapClass::Torn { holes } => holes.len(),
                _ => 0,
            };
            rows.push(GapRow {
                name: case.name.clone(),
                side,
                expected: case.expect,
                actual: ExpectClass::of(&class),
                holes,
            });
        }
        Self { rows }
    }

    /// Fraction of must-fire cases classified non-`Fresh`.
    #[must_use]
    pub fn recall(&self) -> f64 {
        rate(
            self.rows.iter().filter(|row| row.side == Side::Positive),
            |row| row.actual != ExpectClass::Fresh,
        )
    }

    /// Fraction of must-not-fire cases classified `Fresh`.
    #[must_use]
    pub fn control_precision(&self) -> f64 {
        rate(
            self.rows.iter().filter(|row| row.side == Side::Control),
            |row| row.actual == ExpectClass::Fresh,
        )
    }

    /// Fraction of must-not-fire cases that fired.
    #[must_use]
    pub fn false_fire_rate(&self) -> f64 {
        1.0 - self.control_precision()
    }

    /// Whether the classifier may be trusted at this bound: every must-fire
    /// fired and no must-not-fire did.
    #[must_use]
    pub fn is_green(&self) -> bool {
        (self.recall() - 1.0).abs() < f64::EPSILON && self.false_fire_rate() == 0.0
    }
}

fn rate<'a>(
    rows: impl Iterator<Item = &'a GapRow>,
    mut accept: impl FnMut(&GapRow) -> bool,
) -> f64 {
    let mut total = 0_usize;
    let mut hits = 0_usize;
    for row in rows {
        total += 1;
        if accept(row) {
            hits += 1;
        }
    }
    if total == 0 {
        return 0.0;
    }
    hits as f64 / total as f64
}

/// Build the tape a case describes.
#[must_use]
pub fn coverage_for(case: &Case) -> Option<Coverage> {
    let step = case.timeframe.duration_ms();
    let bars: Vec<protopyne_types::Bar> = (0..case.grid.count)
        .filter(|i| !case.grid.omit.contains(i))
        .map(|i| at_ms(case.t0_ms.saturating_add(i as i64 * step)))
        .collect();
    Coverage::from_bars(&bars, case.timeframe)
}

/// Resolve the verdict instant: a fixed stamp, or the tape's last bar plus
/// the recorded lag — the candidate bound when the case names `"k"`.
pub fn now_for(case: &Case, coverage: Option<&Coverage>, candidate_k: u64) -> i64 {
    match &case.now {
        NowSpec::At { at_ms } => *at_ms,
        NowSpec::AfterLast { after_last_bars } => {
            let coverage =
                coverage.unwrap_or_else(|| panic!("case '{}' lags an empty tape", case.name));
            let step = case.timeframe.duration_ms();
            let lag = match after_last_bars {
                LagCount::Bars(n) => *n,
                LagCount::Candidate(spelling) => {
                    assert_eq!(
                        spelling, "k",
                        "case '{}' names its lag {spelling:?}: the only relative lag is \"k\"",
                        case.name
                    );
                    candidate_k
                }
            };
            coverage.last.ts_ms.saturating_add(lag as i64 * step)
        }
    }
}

/// Read the recorded corpus.
pub fn load_corpus() -> Corpus {
    serde_json::from_str(include_str!("../data/gap_fixtures.json"))
        .expect("gap_fixtures.json parses")
}
