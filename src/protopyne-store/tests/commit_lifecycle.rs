//! The lifecycle control group: both directions of the late-arrival
//! classification, asserted from one table.
//!
//! `Late` fires on exactly one fact — an observation strictly *older* than the
//! bar the emitter currently holds. Every other observation is ordinary
//! streaming: a first sighting, the venue republishing the bar that has not
//! finished yet, or the next instant arriving. A suite that proved only the
//! firing direction would pass an emitter that flagged every observation, so
//! the must-fire and must-not-fire cases are asserted side by side and the
//! whole table fails if either direction drifts.

use chrono::{DateTime, Utc};

use super::*;
use crate::commit::test_tapes::{bar, revised, stamp};

/// What one scripted observation is expected to classify as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// First sighting of this key.
    Opened,
    /// Same instant, more content.
    Revised,
    /// The held bar closed because a strictly later instant arrived.
    Closed,
    /// Strictly older than the held bar.
    Late,
}

impl Expected {
    /// Whether the observation was a late arrival.
    fn fired_late(self) -> bool {
        matches!(self, Self::Late)
    }
}

fn classify(arrival: &Arrival<DateTime<Utc>>) -> Expected {
    match arrival {
        Arrival::Opened(_) => Expected::Opened,
        Arrival::Revised(_) => Expected::Revised,
        Arrival::Closed { .. } => Expected::Closed,
        Arrival::Late { .. } => Expected::Late,
    }
}

/// One scripted tape and the classification every observation must produce.
struct Case {
    /// What the case proves.
    name: &'static str,
    /// The observations, in the order the feed produced them.
    script: Vec<Bar>,
    /// The classification of each observation, positionally.
    expect: Vec<Expected>,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "first sighting ever opens the bar",
            script: vec![bar(0, "100")],
            expect: vec![Expected::Opened],
        },
        Case {
            name: "republishing the forming bar revises it",
            script: vec![bar(0, "100"), revised(0, "100", "101")],
            expect: vec![Expected::Opened, Expected::Revised],
        },
        Case {
            name: "a strictly later instant closes the held bar",
            script: vec![bar(0, "100"), bar(5, "101")],
            expect: vec![Expected::Opened, Expected::Closed],
        },
        Case {
            name: "an instant strictly older than the held bar is late",
            script: vec![bar(5, "100"), bar(0, "99")],
            expect: vec![Expected::Opened, Expected::Late],
        },
        Case {
            name: "a late arrival does not displace the held bar",
            script: vec![bar(5, "100"), bar(0, "99"), bar(10, "101")],
            expect: vec![Expected::Opened, Expected::Late, Expected::Closed],
        },
        Case {
            name: "a repeated republication keeps revising, never going late",
            script: vec![
                bar(0, "100"),
                revised(0, "100", "101"),
                revised(0, "100", "102"),
                bar(5, "103"),
            ],
            expect: vec![
                Expected::Opened,
                Expected::Revised,
                Expected::Revised,
                Expected::Closed,
            ],
        },
    ]
}

/// Run one scripted tape and return what the emitter reported per observation.
fn observe_script(script: &[Bar]) -> Vec<Arrival<DateTime<Utc>>> {
    let mut emitter = ClosedEmitter::<Bar>::new();
    script
        .iter()
        .cloned()
        .map(|next| emitter.observe(next).expect("fixture bar merges cleanly"))
        .collect()
}

#[test]
fn control_group_classifies_every_direction() {
    let mut saw_fire = false;
    let mut saw_quiet = false;
    for case in cases() {
        let got: Vec<Expected> = observe_script(&case.script).iter().map(classify).collect();
        assert_eq!(got, case.expect, "case: {}", case.name);
        for expected in &case.expect {
            if expected.fired_late() {
                saw_fire = true;
            } else {
                saw_quiet = true;
            }
        }
    }
    // Both directions must actually be represented, or the table has stopped
    // testing the distinction it exists to protect.
    assert!(saw_fire, "control group asserts no must-fire case");
    assert!(saw_quiet, "control group asserts no must-not-fire case");
}

#[test]
fn a_republished_open_bar_never_produces_a_late_arrival() {
    let mut emitter = ClosedEmitter::<Bar>::new();
    emitter.observe(bar(0, "100")).expect("opens");
    for close in ["101", "102", "103"] {
        let arrival = emitter.observe(revised(0, "100", close)).expect("revises");
        assert_eq!(
            arrival,
            Arrival::Revised(stamp(0)),
            "a same-instant republication is a revision, not a hole"
        );
    }
    assert!(
        emitter.take_late().is_empty(),
        "no republication may reach the late queue"
    );
}

#[test]
fn closing_reports_the_key_of_the_bar_that_closed() {
    let mut emitter = ClosedEmitter::<Bar>::new();
    emitter.observe(bar(0, "100")).expect("opens");
    let arrival = emitter.observe(bar(5, "101")).expect("closes");
    assert_eq!(arrival, Arrival::Closed { at: stamp(0) });
    assert_eq!(
        emitter.take_closed(),
        vec![bar(0, "100")],
        "the closed bar is handed on by value, never by borrow"
    );
}

#[test]
fn a_late_arrival_names_itself_and_the_bar_it_lost_to() {
    let mut emitter = ClosedEmitter::<Bar>::new();
    emitter.observe(bar(5, "100")).expect("opens");
    let arrival = emitter.observe(bar(0, "99")).expect("queues the late bar");

    assert_eq!(
        arrival,
        Arrival::Late {
            at: stamp(0),
            newest_open: Some(stamp(5)),
        },
        "a late arrival is a hole in the committed series, and the emitter \
         says which bar it lost to"
    );
    assert_eq!(
        emitter.take_late(),
        vec![bar(0, "99")],
        "a late arrival is retained as a gap signal, never dropped"
    );
}

#[test]
fn a_late_arrival_leaves_the_held_bar_holding() {
    let mut emitter = ClosedEmitter::<Bar>::new();
    emitter.observe(bar(5, "100")).expect("opens");
    emitter.observe(bar(0, "99")).expect("queues the late bar");

    assert_eq!(
        emitter.held(),
        Some(&bar(5, "100")),
        "the newest open bar is unaffected by an older observation"
    );
    assert_eq!(emitter.counts().closed, 0, "nothing closed");
    assert_eq!(emitter.counts().late, 1);
}

#[test]
fn a_late_arrival_names_the_bar_it_lost_to_after_a_close() {
    // A straggler that arrives after the emitter has already closed one bar
    // is still out of order. It is queued as a hole, and `newest_open` says
    // plainly that it did not displace the bar that is forming.
    let mut emitter = ClosedEmitter::<Bar>::new();
    emitter.observe(bar(0, "100")).expect("opens");
    emitter.observe(bar(5, "101")).expect("closes");
    let arrival = emitter.observe(bar(1, "100.5")).expect("queues");
    assert_eq!(
        arrival,
        Arrival::Late {
            at: stamp(1),
            newest_open: Some(stamp(5)),
        }
    );
}

#[test]
fn merging_a_restatement_keeps_the_first_open_and_the_latest_close() {
    let mut emitter = ClosedEmitter::<Bar>::new();
    emitter.observe(bar(0, "100")).expect("opens");
    emitter.observe(revised(0, "100", "104")).expect("revises");

    let held = emitter.held().expect("bar is held");
    assert_eq!(held.o, "100".parse().expect("valid decimal"));
    assert_eq!(held.c, "104".parse().expect("valid decimal"));
    assert_eq!(held.h, "104".parse().expect("valid decimal"));
    assert_eq!(
        held.ts,
        stamp(0),
        "a restatement does not move the bar's own stamp"
    );
}
