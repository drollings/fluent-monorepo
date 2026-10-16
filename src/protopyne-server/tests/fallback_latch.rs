//! FallbackLatch and TapeServeStats calibration.
//!
//! The transition edges decide when the gateway warns, so they are pinned
//! with control groups that must NOT fire: a sustained fallback warns once, a
//! recovery logs once, an alternating read warns per entry, and a server with
//! no tape (`NoTape`) never warns at all. The counters the same reads record
//! are pinned exact.

use super::*;
use crate::service::TapeServeStats;
use protopyne_store::StoreError;

/// Walk a latch through `sequence`, asserting each returned event and that
/// exactly `expected_warns` entries into fallback were crossed.
fn walk(sequence: &[(Option<TapeMissKind>, LatchEvent)], expected_warns: usize) {
    let mut latch = FallbackLatch::default();
    let mut warns = 0usize;
    for (miss, expected) in sequence {
        let event = latch.observe(*miss);
        assert_eq!(event, *expected, "latch event for {miss:?}");
        if event == LatchEvent::EnteredFallback {
            warns += 1;
        }
    }
    assert_eq!(warns, expected_warns, "fallback-entry warn count");
}

#[test]
fn consecutive_misses_warn_exactly_once() {
    walk(
        &[
            (Some(TapeMissKind::Denied), LatchEvent::EnteredFallback),
            (Some(TapeMissKind::Denied), LatchEvent::None),
            (Some(TapeMissKind::Store), LatchEvent::None),
        ],
        1,
    );
}

#[test]
fn alternating_serve_and_miss_warn_once_per_entry() {
    walk(
        &[
            (
                Some(TapeMissKind::UnknownSymbol),
                LatchEvent::EnteredFallback,
            ),
            (None, LatchEvent::Recovered),
            (Some(TapeMissKind::Denied), LatchEvent::EnteredFallback),
            (None, LatchEvent::Recovered),
        ],
        2,
    );
}

#[test]
fn recovery_logs_exactly_once() {
    let mut latch = FallbackLatch::default();
    assert_eq!(
        latch.observe(Some(TapeMissKind::Store)),
        LatchEvent::EnteredFallback
    );
    assert_eq!(latch.observe(None), LatchEvent::Recovered);
    assert_eq!(
        latch.observe(None),
        LatchEvent::None,
        "a steady serve does not re-log"
    );
}

#[test]
fn no_tape_sequences_never_warn() {
    walk(
        &[
            (Some(TapeMissKind::NoTape), LatchEvent::None),
            (Some(TapeMissKind::NoTape), LatchEvent::None),
            (None, LatchEvent::None),
            (Some(TapeMissKind::NoTape), LatchEvent::None),
        ],
        0,
    );
}

#[test]
fn counters_tally_each_outcome_exactly() {
    let stats = TapeServeStats::default();
    stats.record(&TapeServe::Served(Vec::new()));
    stats.record(&TapeServe::Missing(TapeMiss::NoTape));
    stats.record(&TapeServe::Missing(TapeMiss::Denied));
    stats.record(&TapeServe::Missing(TapeMiss::UnknownSymbol));
    stats.record(&TapeServe::Missing(TapeMiss::UnsupportedTimeframe));
    stats.record(&TapeServe::Missing(TapeMiss::Store(StoreError::Config(
        "x".to_owned(),
    ))));
    stats.record(&TapeServe::Missing(TapeMiss::Denied));
    let snapshot = stats.snapshot();
    assert_eq!(snapshot.served, 1);
    assert_eq!(snapshot.no_tape, 1);
    assert_eq!(snapshot.denied, 2);
    assert_eq!(snapshot.unknown_symbol, 1);
    assert_eq!(snapshot.unsupported_timeframe, 1);
    assert_eq!(snapshot.store_error, 1);
    assert_eq!(snapshot.last_miss, Some(TapeMissKind::Denied));
}
