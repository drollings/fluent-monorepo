//! Span arithmetic: construction refuses, queries clip.
//!
//! The suite pins three facts. An inverted window is refused with a reason
//! rather than clamped into an empty span. Bounds are half-open, so touching
//! spans share nothing. And intersection keeps the bounds it is given: an
//! absent bound survives, which is what lets a planner clip an open-ended
//! request against a bounded tape.

use super::*;

#[test]
fn an_inverted_window_is_refused_with_both_bounds_named() {
    assert_eq!(
        TimeRange::bounded(9, 4).expect_err("an inverted window cannot exist"),
        PlanError::InvertedRange { start: 9, end: 4 }
    );
    assert_eq!(
        TimeRange::new(Some(9), Some(4)).expect_err("the general constructor refuses too"),
        PlanError::InvertedRange { start: 9, end: 4 }
    );
}

#[test]
fn an_empty_window_is_bounded_but_contains_nothing() {
    let empty = TimeRange::bounded(9, 9).expect("equal bounds are not inverted");
    assert!(empty.is_bounded());
    assert!(!empty.contains(9), "half-open: the end bound is exclusive");
}

#[test]
fn containment_is_half_open_on_both_sides() {
    let span = TimeRange::bounded(10, 20).expect("ordered bounds");
    assert!(span.contains(10), "the start bound is inclusive");
    assert!(span.contains(19));
    assert!(!span.contains(20), "the end bound is exclusive");
    assert!(!span.contains(9));
}

#[test]
fn an_absent_bound_accepts_everything_on_its_side() {
    let tail = TimeRange::to_end(100);
    assert!(!tail.is_bounded());
    assert!(tail.contains(i64::MIN));
    assert!(tail.contains(99));
    assert!(!tail.contains(100));

    let open = TimeRange::unbounded();
    assert!(!open.is_bounded());
    assert!(open.contains(0));
    assert!(open.contains(i64::MAX));
}

#[test]
fn touching_spans_share_nothing() {
    let left = TimeRange::bounded(0, 10).expect("ordered bounds");
    let right = TimeRange::bounded(10, 20).expect("ordered bounds");
    assert!(
        !left.overlaps(&right),
        "the shared point belongs to exactly one span"
    );
    assert!(!right.overlaps(&left));
    assert!(left.intersect(&right).is_none());
}

#[test]
fn overlap_is_symmetric_and_self_overlapping() {
    let span = TimeRange::bounded(0, 10).expect("ordered bounds");
    assert!(span.overlaps(&span));
    let inner = TimeRange::bounded(3, 7).expect("ordered bounds");
    assert!(span.overlaps(&inner));
    assert!(inner.overlaps(&span));
    let disjoint = TimeRange::bounded(11, 20).expect("ordered bounds");
    assert!(!span.overlaps(&disjoint));
    assert!(!disjoint.overlaps(&span));
}

#[test]
fn intersection_keeps_the_bounds_it_is_given() {
    let bounded = TimeRange::bounded(0, 100).expect("ordered bounds");
    let open_tail = TimeRange::to_end(50);
    assert_eq!(
        bounded.intersect(&open_tail),
        Some(TimeRange {
            start_ms: Some(0),
            end_ms: Some(50),
        }),
        "the absent start survives: an open-ended request stays open-ended \
         where the tape does not bound it"
    );
    assert_eq!(
        open_tail.intersect(&bounded),
        Some(TimeRange {
            start_ms: Some(0),
            end_ms: Some(50),
        }),
        "intersection is symmetric"
    );
    assert_eq!(
        TimeRange::unbounded().intersect(&bounded),
        Some(bounded),
        "the unbounded span is the identity of intersection"
    );
    assert_eq!(bounded.intersect(&TimeRange::unbounded()), Some(bounded));
}

#[test]
fn intersection_clips_a_request_to_the_tape() {
    let tape = TimeRange::bounded(1_000, 2_000).expect("ordered bounds");
    let request = TimeRange::bounded(500, 5_000).expect("ordered bounds");
    assert_eq!(
        tape.intersect(&request),
        Some(TimeRange {
            start_ms: Some(1_000),
            end_ms: Some(2_000),
        })
    );
}

#[test]
fn tail_materializes_a_latest_n_window_into_concrete_bounds() {
    assert_eq!(
        TimeRange::tail(500, 2_000),
        TimeRange {
            start_ms: Some(1_500),
            end_ms: Some(2_000),
        }
    );
    assert_eq!(
        TimeRange::tail(0, 2_000),
        TimeRange {
            start_ms: Some(2_000),
            end_ms: Some(2_000),
        },
        "a zero span is an empty window at the anchor, not a refusal"
    );
}

#[test]
fn tail_never_names_a_stamp_before_the_epoch() {
    assert_eq!(
        TimeRange::tail(5_000, 1_000),
        TimeRange {
            start_ms: Some(0),
            end_ms: Some(1_000),
        },
        "the start clamps at zero, mirroring the fetch-bounds widening"
    );
}

#[test]
fn containment_is_bound_by_bound() {
    let outer = TimeRange::bounded(1_000, 2_000).expect("ordered bounds");
    let inner = TimeRange::bounded(1_200, 1_800).expect("ordered bounds");
    assert!(inner.contained_in(&outer));
    assert!(outer.contained_in(&outer), "a span contains itself");
    assert!(!outer.contained_in(&inner));
    let past_end = TimeRange::bounded(1_500, 2_500).expect("ordered bounds");
    assert!(!past_end.contained_in(&outer));
    let touching = TimeRange::bounded(2_000, 3_000).expect("ordered bounds");
    assert!(
        !touching.contained_in(&outer),
        "touching at the bound is not inside"
    );
}

#[test]
fn containment_needs_the_same_absence_on_both_sides() {
    let bounded = TimeRange::bounded(1_000, 2_000).expect("ordered bounds");
    assert!(bounded.contained_in(&TimeRange::unbounded()));
    assert!(
        !TimeRange::unbounded().contained_in(&bounded),
        "an unbounded side is only inside an unbounded side"
    );
    assert!(
        !TimeRange::to_end(2_000).contained_in(&bounded),
        "an open start escapes a bounded start"
    );
    assert!(TimeRange::to_end(1_500).contained_in(&TimeRange::to_end(2_000)));
}
