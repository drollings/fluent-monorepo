//! The planner and the runtime agree about where holes are.
//!
//! [`Coverage::holes`] and [`BarWindow::gaps`](protopyne_types::BarWindow)
//! are two answers to the same question — where is this series torn? —
//! computed by different code over different shapes. If they ever disagree,
//! the repair loop fills holes the runtime cannot see, or the runtime flags
//! gaps the repair loop will never close. This suite builds tapes with known
//! holes, asks both, and requires the same answer: every reported gap names
//! the same missing span, and a complete series reports nothing on both
//! sides.
//!
//! The agreement is stated in milliseconds rather than bars so neither side
//! gets to choose the other's units.

use protopyne_types::{BarWindow, Timeframe};

use super::*;
use crate::commit::test_tapes::at_ms;

/// The one-minute grid these tapes are laid out on.
const STEP_MS: i64 = 60_000;
/// Minute zero of the suite clock.
const T0: i64 = 1_767_225_900_000;

fn whole_tape() -> TimeRange {
    TimeRange {
        start_ms: Some(T0),
        end_ms: Some(T0 + 10 * STEP_MS),
    }
}

fn series(stamps: &[i64]) -> Vec<protopyne_types::Bar> {
    stamps.iter().map(|ts| at_ms(*ts)).collect()
}

fn window_of(stamps: &[i64]) -> BarWindow {
    let mut window = BarWindow::new(Timeframe::M1);
    for bar in series(stamps) {
        window
            .push(bar)
            .expect("fixture stamps are ordered and valid");
    }
    window
}

fn coverage_of(stamps: &[i64]) -> Coverage {
    Coverage::from_bars(&series(stamps), Timeframe::M1).expect("nonempty tape")
}

/// Every gap the runtime reports, and every hole the store reports, name the
/// same missing span: the hole starts where the runtime expected the bar and
/// is exactly as wide as the bars the runtime counted missing.
fn assert_agreement(stamps: &[i64]) {
    let window = window_of(stamps);
    let coverage = coverage_of(stamps);
    let holes = coverage.holes(&whole_tape(), Timeframe::M1);
    let gaps = window.gaps();

    assert_eq!(
        holes.len(),
        gaps.len(),
        "the store and the runtime count the same holes: {holes:?} vs {gaps:?}"
    );
    for (hole, gap) in holes.iter().zip(gaps.iter()) {
        assert_eq!(
            hole.start_ms,
            Some(gap.expected_ts.timestamp_millis()),
            "the hole starts where the runtime expected the bar"
        );
        assert_eq!(
            hole.end_ms
                .map(|end| end - hole.start_ms.expect("hole is bounded")),
            Some(gap.missing_bars as i64 * STEP_MS),
            "the hole spans exactly the missing bars"
        );
    }
}

#[test]
fn a_complete_series_has_no_holes_on_either_side() {
    let stamps: Vec<i64> = (0..10).map(|n| T0 + n * STEP_MS).collect();
    assert!(!window_of(&stamps).has_gaps());
    assert!(
        coverage_of(&stamps)
            .holes(&whole_tape(), Timeframe::M1)
            .is_empty()
    );
}

#[test]
fn one_missing_bar_is_named_identically_by_both() {
    let stamps: Vec<i64> = [0, 1, 2, 3, 5, 6, 7, 8, 9]
        .iter()
        .map(|n| T0 + n * STEP_MS)
        .collect();
    assert_agreement(&stamps);
}

#[test]
fn two_missing_bars_in_one_hole_are_one_span() {
    let stamps: Vec<i64> = [0, 1, 2, 6, 7, 8, 9]
        .iter()
        .map(|n| T0 + n * STEP_MS)
        .collect();
    assert_agreement(&stamps);
}

#[test]
fn two_separate_holes_are_two_spans_on_both_sides() {
    let stamps: Vec<i64> = [0, 1, 3, 4, 5, 8, 9]
        .iter()
        .map(|n| T0 + n * STEP_MS)
        .collect();
    assert_agreement(&stamps);
}

#[test]
fn holes_are_clipped_to_the_requested_window() {
    let stamps: Vec<i64> = [0, 1, 2, 3, 5, 6, 7, 8, 9]
        .iter()
        .map(|n| T0 + n * STEP_MS)
        .collect();
    let coverage = coverage_of(&stamps);
    // A request fully inside the covered extent but straddling the hole still
    // names it; a request that ends before the hole never sees it.
    let straddling = coverage.holes(
        &TimeRange {
            start_ms: Some(T0 + 2 * STEP_MS),
            end_ms: Some(T0 + 6 * STEP_MS),
        },
        Timeframe::M1,
    );
    assert_eq!(straddling.len(), 1);
    assert_eq!(straddling[0].start_ms, Some(T0 + 4 * STEP_MS));

    let before = coverage.holes(
        &TimeRange {
            start_ms: Some(T0),
            end_ms: Some(T0 + 4 * STEP_MS),
        },
        Timeframe::M1,
    );
    assert!(before.is_empty(), "nothing missing inside a covered window");
}

#[test]
fn intersect_is_the_bounding_overlap_not_the_holes() {
    let stamps: Vec<i64> = [0, 1, 2, 3, 5, 6, 7, 8, 9]
        .iter()
        .map(|n| T0 + n * STEP_MS)
        .collect();
    let coverage = coverage_of(&stamps);
    assert_eq!(
        coverage.intersect(&whole_tape()),
        Some(TimeRange {
            start_ms: Some(T0),
            end_ms: Some(T0 + 9 * STEP_MS + 1),
        })
    );
    assert_eq!(
        coverage.intersect(&TimeRange {
            start_ms: Some(T0 + 20 * STEP_MS),
            end_ms: Some(T0 + 30 * STEP_MS),
        }),
        None,
        "a disjoint request intersects nothing"
    );
}

#[test]
fn a_session_closed_stretch_is_an_uncovered_fact_not_a_hidden_hole() {
    // Friday's bars and Monday's bars described as one coverage: the weekend
    // between them is uncovered, and the type says so rather than smoothing
    // it over. "Uncovered" is an observation fact; whether it means "hole"
    // or "closed" is the session calendar's call, made by the repair loop
    // before it acts — the classifier itself never sees the weekend, because
    // on Saturday the tape holds Friday's bars and `now` is the last instant
    // bars were due.
    let friday = TimeRange {
        start_ms: Some(T0),
        end_ms: Some(T0 + 5 * STEP_MS),
    };
    let monday = TimeRange {
        start_ms: Some(T0 + 3 * 86_400_000),
        end_ms: Some(T0 + 3 * 86_400_000 + 5 * STEP_MS),
    };
    let both = Coverage::from_runs(&[friday, monday], 10).expect("two runs describe a tape");
    assert_eq!(both.bars, 10);
    assert_eq!(both.first.ts_ms, T0);
    let request = TimeRange {
        start_ms: Some(T0),
        end_ms: Some(T0 + 3 * 86_400_000 + 5 * STEP_MS),
    };
    assert_eq!(
        both.holes(&request, Timeframe::M1),
        vec![TimeRange {
            start_ms: Some(T0 + 5 * STEP_MS),
            end_ms: Some(T0 + 3 * 86_400_000),
        }],
        "the weekend reports uncovered; the calendar decides it needs no repair"
    );

    // The operational reading, on Saturday: the tape holds Friday's bars,
    // and `now` is the last stamp bars were due — no lag, no holes, no repair.
    let friday_only = Coverage::from_runs(&[friday], 5).expect("one run describes Friday");
    assert_eq!(
        classify(
            Some(friday_only),
            T0 + 4 * STEP_MS,
            Timeframe::M1,
            FreshnessPolicy::new().build()
        ),
        GapClass::Fresh,
        "a closed session produces no lag because no bars were due"
    );
}
