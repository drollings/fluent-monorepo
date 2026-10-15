//! The gap-classification control group: every direction the classifier can
//! answer, asserted from one table.
//!
//! `classify` is a repair verdict, so the suite proves both halves — the
//! cases that must fire a repair and the cases that must leave a healthy tape
//! alone. The second half is the one that matters in practice: a classifier
//! that fires on healthy tapes trains the operator to ignore it.
//!
//! The table's shared clock is frozen on purpose. Every lag is stated in
//! whole bars against a one-minute spacing, so "three bars behind" is an
//! exact millisecond count and the boundary rows — exactly at the bound is
//! calm, one millisecond past it is not — are not hostage to the wall clock.

use super::*;
use crate::commit::test_tapes::at_ms;

/// One scripted tape and the verdict it must produce.
struct Case {
    /// What the case proves.
    name: &'static str,
    /// Observed stamps in milliseconds, ascending.
    stamps: Vec<i64>,
    /// The `now` the verdict is taken at.
    now_ms: i64,
    /// The policy under test.
    policy: FreshnessPolicy,
    /// The verdict every observation must produce.
    expect: GapClass,
}

/// Minute zero of the table clock, in epoch milliseconds.
const T0: i64 = 1_767_225_900_000;

/// Stamps `count` bars long on the one-minute grid starting at `T0 + start`.
fn grid(start: i64, count: i64) -> Vec<i64> {
    (start..start + count).map(|n| T0 + n * 60_000).collect()
}

fn policy() -> FreshnessPolicy {
    FreshnessPolicy::new().build()
}

/// An explicit lag bound. Mechanism cases state their bound absolutely so
/// they test the classifier's logic, never the frozen default — the default
/// is the calibration's pinned value, asserted where it was frozen.
fn lag(max_lag_bars: u64) -> FreshnessPolicy {
    FreshnessPolicy::new().max_lag_bars(max_lag_bars).build()
}

fn coverage_of(stamps: &[i64]) -> Option<Coverage> {
    let bars: Vec<Bar> = stamps.iter().map(|ts| at_ms(*ts)).collect();
    Coverage::from_bars(&bars, Timeframe::M1)
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "an empty table is empty, not stale and not torn",
            stamps: vec![],
            now_ms: T0,
            policy: policy(),
            expect: GapClass::Empty,
        },
        Case {
            name: "exactly two bars behind is fresh at a bound of two: the boundary is inclusive",
            stamps: grid(0, 10),
            now_ms: T0 + 9 * 60_000 + 2 * 60_000,
            policy: lag(2),
            expect: GapClass::Fresh,
        },
        Case {
            name: "one bar past a bound of two is stale",
            stamps: grid(0, 10),
            now_ms: T0 + 9 * 60_000 + 3 * 60_000,
            policy: lag(2),
            expect: GapClass::Stale {
                behind: TimeRange {
                    start_ms: Some(T0 + 10 * 60_000),
                    end_ms: Some(T0 + 12 * 60_000 + 1),
                },
            },
        },
        Case {
            name: "a tape three bars behind reads stale at a bound of two",
            stamps: grid(0, 10),
            now_ms: T0 + 9 * 60_000 + 3 * 60_000,
            policy: lag(2),
            expect: GapClass::Stale {
                behind: TimeRange {
                    start_ms: Some(T0 + 10 * 60_000),
                    end_ms: Some(T0 + 12 * 60_000 + 1),
                },
            },
        },
        Case {
            name: "a single late print is fresh at a bound of one, not a repair",
            stamps: grid(0, 10),
            now_ms: T0 + 10 * 60_000,
            policy: lag(1),
            expect: GapClass::Fresh,
        },
        Case {
            name: "an interior hole with current ends is torn",
            stamps: [grid(0, 5), grid(6, 4)].concat(),
            now_ms: T0 + 9 * 60_000,
            policy: policy(),
            expect: GapClass::Torn {
                holes: vec![TimeRange {
                    start_ms: Some(T0 + 5 * 60_000),
                    end_ms: Some(T0 + 6 * 60_000),
                }],
            },
        },
        Case {
            name: "two interior holes yield two spans",
            stamps: [grid(0, 3), grid(4, 3), grid(8, 2)].concat(),
            now_ms: T0 + 9 * 60_000,
            policy: policy(),
            expect: GapClass::Torn {
                holes: vec![
                    TimeRange {
                        start_ms: Some(T0 + 3 * 60_000),
                        end_ms: Some(T0 + 4 * 60_000),
                    },
                    TimeRange {
                        start_ms: Some(T0 + 7 * 60_000),
                        end_ms: Some(T0 + 8 * 60_000),
                    },
                ],
            },
        },
        Case {
            name: "a current and complete tape is fresh and never stale",
            stamps: grid(0, 10),
            now_ms: T0 + 9 * 60_000,
            policy: policy(),
            expect: GapClass::Fresh,
        },
        Case {
            name: "a hole of exactly max_gap_bars is tolerated",
            stamps: [grid(0, 5), grid(7, 3)].concat(),
            now_ms: T0 + 9 * 60_000,
            policy: FreshnessPolicy::new().max_gap_bars(2).build(),
            expect: GapClass::Fresh,
        },
        Case {
            name: "a hole one bar past the tolerance tears",
            stamps: [grid(0, 5), grid(8, 2)].concat(),
            now_ms: T0 + 9 * 60_000,
            policy: FreshnessPolicy::new().max_gap_bars(2).build(),
            expect: GapClass::Torn {
                holes: vec![TimeRange {
                    start_ms: Some(T0 + 5 * 60_000),
                    end_ms: Some(T0 + 8 * 60_000),
                }],
            },
        },
        Case {
            name: "a closed session produces no lag: now is where the tape ends",
            stamps: grid(0, 10),
            now_ms: T0 + 9 * 60_000,
            policy: policy(),
            expect: GapClass::Fresh,
        },
    ]
}

#[test]
fn control_group_classifies_every_verdict() {
    let mut saw_fire = false;
    let mut saw_quiet = false;
    for case in cases() {
        let got = classify(
            coverage_of(&case.stamps),
            case.now_ms,
            Timeframe::M1,
            case.policy,
        );
        assert_eq!(got, case.expect, "case: {}", case.name);
        if matches!(got, GapClass::Fresh) {
            saw_quiet = true;
        } else {
            saw_fire = true;
        }
    }
    assert!(saw_fire, "control group asserts no must-fire case");
    assert!(saw_quiet, "control group asserts no must-not-fire case");
}

#[test]
fn staleness_names_the_span_after_the_last_bar() {
    let class = classify(
        coverage_of(&grid(0, 10)),
        T0 + 14 * 60_000,
        Timeframe::M1,
        lag(2),
    );
    let GapClass::Stale { behind } = class else {
        panic!("five bars behind at a bound of two is stale, got {class:?}");
    };
    assert_eq!(behind.start_ms, Some(T0 + 10 * 60_000));
    assert_eq!(behind.end_ms, Some(T0 + 14 * 60_000 + 1));
    assert!(
        behind.contains(T0 + 12 * 60_000),
        "the reported span covers the bars that never arrived"
    );
}

#[test]
fn a_tape_both_stale_and_torn_reports_stale_first() {
    // The tail is what a repair pass fixes first; the interior holes are
    // still there when the tape catches up, and the next pass names them.
    let class = classify(
        coverage_of(&[grid(0, 5), grid(6, 4)].concat()),
        T0 + 14 * 60_000,
        Timeframe::M1,
        lag(2),
    );
    assert!(
        matches!(class, GapClass::Stale { .. }),
        "staleness outranks tornness, got {class:?}"
    );
}

#[test]
fn a_torn_tape_with_a_current_tail_reports_the_hole_not_the_tail() {
    let class = classify(
        coverage_of(&[grid(0, 5), grid(6, 4)].concat()),
        T0 + 9 * 60_000,
        Timeframe::M1,
        policy(),
    );
    let GapClass::Torn { holes } = class else {
        panic!("current ends with an interior hole is torn, got {class:?}");
    };
    assert_eq!(holes.len(), 1);
    assert_eq!(holes[0].start_ms, Some(T0 + 5 * 60_000));
    assert_eq!(holes[0].end_ms, Some(T0 + 6 * 60_000));
}

#[test]
fn the_policy_is_a_validated_component() {
    let unit = policy();
    unit.validate()
        .expect("every non-negative pair is coherent");
    let ctx = fluent_wvr::WorkContext::for_unit(&unit, fluent_wvr::CapabilitySet::new());
    let out = unit.execute(&ctx).expect("a coherent policy executes");
    assert!(out.success);
}

#[test]
fn repair_spans_names_what_to_fill_and_nothing_else() {
    assert!(
        GapClass::Empty.repair_spans().is_empty(),
        "an empty tape has no span to bound a repair: the provider decides"
    );
    assert!(
        GapClass::Fresh.repair_spans().is_empty(),
        "a fresh tape schedules nothing"
    );
    let stale = classify(
        coverage_of(&grid(0, 10)),
        T0 + 14 * 60_000,
        Timeframe::M1,
        lag(2),
    );
    assert_eq!(
        stale.repair_spans(),
        vec![TimeRange {
            start_ms: Some(T0 + 10 * 60_000),
            end_ms: Some(T0 + 14 * 60_000 + 1),
        }],
        "a stale tape repairs the tail"
    );
    let torn = classify(
        coverage_of(&[grid(0, 3), grid(4, 3), grid(8, 2)].concat()),
        T0 + 9 * 60_000,
        Timeframe::M1,
        policy(),
    );
    assert_eq!(
        torn.repair_spans(),
        vec![
            TimeRange {
                start_ms: Some(T0 + 3 * 60_000),
                end_ms: Some(T0 + 4 * 60_000),
            },
            TimeRange {
                start_ms: Some(T0 + 7 * 60_000),
                end_ms: Some(T0 + 8 * 60_000),
            },
        ],
        "a torn tape repairs every hole the policy did not tolerate"
    );
}
