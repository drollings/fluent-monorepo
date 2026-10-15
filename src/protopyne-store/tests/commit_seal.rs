//! Sealing: the one way a held bar becomes committed without a later bar
//! proving it closed, and the session clock that decides when.
//!
//! Three facts are pinned here. A seal commits the held bar exactly once, so
//! a caller cannot double-count it. A session-end seal is driven by
//! `TradingSession`'s ET clock, never by wall-clock arithmetic, so it cannot
//! drift from the session the rest of the platform reads — the daylight
//! control group proves that by holding the UTC wall clock fixed and letting
//! the answer change. And the emitter keeps ownership of the bar it is
//! holding, so no caller can persist a bar the emitter can still revise.

use chrono::{DateTime, Duration, TimeZone as _, Utc};

use super::*;
use crate::commit::test_tapes::{bar, revised, stamp};

/// `0930-1600`, the regular session every session-end seal is driven from.
fn regular_session() -> TradingSession {
    TradingSession::parse("0930-1600").expect("regular session string parses")
}

/// A flat bar stamped at `ts`.
fn at(ts: DateTime<Utc>) -> Bar {
    Bar::new()
        .ts(ts)
        .o("100".parse().expect("valid decimal"))
        .h("100".parse().expect("valid decimal"))
        .l("100".parse().expect("valid decimal"))
        .c("100".parse().expect("valid decimal"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .build()
}

fn utc(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
        .single()
        .expect("valid instant")
}

#[test]
fn seal_commits_the_held_bar_exactly_once() {
    let mut emitter = ClosedEmitter::<Bar>::new();
    emitter.observe(bar(0, "100")).expect("opens");
    assert_eq!(emitter.held(), Some(&bar(0, "100")), "nothing sealed yet");

    assert_eq!(
        emitter.seal(SealReason::Shutdown),
        Some(bar(0, "100")),
        "the seal hands over the bar that was forming"
    );
    assert_eq!(
        emitter.held(),
        None,
        "the seal gives the emitter nothing to hold"
    );
    assert_eq!(
        emitter.seal(SealReason::Shutdown),
        None,
        "a second seal is a no-op: the bar is committed, not re-committed"
    );
    assert_eq!(
        emitter.take_closed(),
        Vec::new(),
        "a sealed bar is handed to the caller, not re-queued"
    );
}

#[test]
fn every_seal_reason_commits_and_is_recorded() {
    for reason in [
        SealReason::SessionEnd,
        SealReason::Shutdown,
        SealReason::Explicit,
    ] {
        let mut emitter = ClosedEmitter::<Bar>::new();
        emitter.observe(bar(0, "100")).expect("opens");
        assert_eq!(emitter.seal(reason), Some(bar(0, "100")));
        assert_eq!(
            emitter.sealed_by(),
            Some(reason),
            "why a bar was sealed is observable, not decorative"
        );
    }
}

#[test]
fn sealing_an_empty_emitter_commits_nothing() {
    let mut emitter = ClosedEmitter::<Bar>::new();
    assert_eq!(emitter.seal(SealReason::Explicit), None);
    assert_eq!(emitter.sealed_by(), None);
    assert_eq!(emitter.counts().sealed, 0);
}

#[test]
fn session_end_seal_waits_until_the_bar_is_outside_the_session() {
    let session = regular_session();
    let mut emitter = ClosedEmitter::<Bar>::new();

    // 15:57 and 15:58 ET on a winter Monday: both inside 0930-1600.
    emitter.observe(at(utc(2026, 1, 5, 20, 57))).expect("opens");
    let arrival = emitter
        .observe(at(utc(2026, 1, 5, 20, 58)))
        .expect("closes");
    assert_eq!(
        arrival,
        Arrival::Closed {
            at: utc(2026, 1, 5, 20, 57)
        }
    );

    assert_eq!(
        emitter.seal_session_end(session),
        None,
        "15:58 ET is inside the session: the bar is still forming"
    );

    // 16:01 ET: past the close, so the forming bar is committed.
    emitter
        .observe(at(utc(2026, 1, 5, 21, 1)))
        .expect("closes the in-session bar");
    assert_eq!(
        emitter.seal_session_end(session),
        Some(at(utc(2026, 1, 5, 21, 1))),
        "16:01 ET is outside the session: the forming bar is committed"
    );
    assert_eq!(emitter.sealed_by(), Some(SealReason::SessionEnd));
    assert_eq!(
        emitter.seal_session_end(session),
        None,
        "the seal already fired; a second pass commits nothing"
    );
}

#[test]
fn session_end_seal_reads_the_daylight_clock_not_a_fixed_offset() {
    let session = regular_session();
    // The identical UTC wall-clock minute, one week either side of the
    // November transition. Under EDT it lands at 16:59 ET — past the close;
    // under EST it lands at 15:59 ET — still inside. A closer with its own
    // fixed offset gets exactly one of these two wrong, and gets it wrong
    // silently.
    let before = utc(2024, 11, 1, 20, 59);
    let after = utc(2024, 11, 4, 20, 59);
    assert_eq!(
        before.time().to_string(),
        after.time().to_string(),
        "both stamps carry the same UTC wall-clock minute"
    );

    let mut before_emitter = ClosedEmitter::<Bar>::new();
    before_emitter.observe(at(before)).expect("opens");
    assert_eq!(
        before_emitter.seal_session_end(session),
        Some(at(before)),
        "16:59 ET under EDT is past the close"
    );

    let mut after_emitter = ClosedEmitter::<Bar>::new();
    after_emitter.observe(at(after)).expect("opens");
    assert_eq!(
        after_emitter.seal_session_end(session),
        None,
        "15:59 ET under EST is still inside the session"
    );
}

#[test]
fn session_end_seal_is_repeatable_and_binds_no_lock_across_the_clock() {
    // Purity control: the same emitter and the same session, asked twice in a
    // row with a session-end seal in between, answer from the emitter's own
    // state and nothing else.
    let session = regular_session();
    let mut emitter = ClosedEmitter::<Bar>::new();
    emitter.observe(at(utc(2026, 1, 5, 21, 30))).expect("opens");
    assert_eq!(
        emitter.seal_session_end(session),
        Some(at(utc(2026, 1, 5, 21, 30)))
    );
    assert_eq!(emitter.seal(SealReason::Explicit), None);
    assert_eq!(emitter.counts().closed, 0);
}

#[test]
fn held_borrows_so_a_caller_cannot_persist_a_bar_still_being_revised() {
    let mut emitter = ClosedEmitter::<Bar>::new();
    emitter.observe(bar(0, "100")).expect("opens");

    // `held` lends; it does not hand over. What the caller can read is still
    // the emitter's, and it moves the moment a restatement arrives.
    let borrowed: &Bar = emitter.held().expect("bar is held");
    assert_eq!(borrowed.c, "100".parse().expect("valid decimal"));

    emitter.observe(revised(0, "100", "105")).expect("revises");

    assert_eq!(
        emitter.held().map(|b| b.c),
        Some("105".parse().expect("valid decimal")),
        "a caller holding a borrow cannot have committed the old value"
    );
    assert_eq!(
        emitter.take_closed(),
        Vec::new(),
        "a revision commits nothing: the bar has not closed"
    );
    assert_eq!(
        emitter.held().map(|b| b.ts),
        Some(stamp(0)),
        "the restatement did not move the bar's own stamp"
    );
}

#[test]
fn a_late_arrival_seen_before_a_seal_is_never_committed_by_the_seal() {
    let mut emitter = ClosedEmitter::<Bar>::new();
    emitter.observe(at(utc(2026, 1, 5, 21, 30))).expect("opens");
    emitter
        .observe(at(utc(2026, 1, 5, 20, 55)))
        .expect("queues the late bar");

    assert_eq!(
        emitter.seal(SealReason::Shutdown),
        Some(at(utc(2026, 1, 5, 21, 30))),
        "the seal commits the forming bar, not the straggler"
    );
    assert_eq!(
        emitter.take_late(),
        vec![at(utc(2026, 1, 5, 20, 55))],
        "the straggler stays a hole signal after the seal"
    );
}

#[test]
fn a_gap_in_the_observed_stamps_is_visible_after_the_seal() {
    // The lifecycle hands the caller two queues — committed and late — and
    // those are exactly the two inputs `BarWindow::push` needs to report a
    // hole. This asserts the handoff exists without re-deriving gap math.
    let mut emitter = ClosedEmitter::<Bar>::new();
    emitter.observe(at(utc(2026, 1, 5, 20, 57))).expect("opens");
    emitter
        .observe(at(utc(2026, 1, 5, 20, 58)))
        .expect("closes");
    emitter
        .observe(at(utc(2026, 1, 5, 20, 59)))
        .expect("closes");
    emitter
        .observe(at(utc(2026, 1, 5, 20, 55)))
        .expect("queues the straggler");
    emitter.seal(SealReason::Explicit);

    let mut committed = emitter.take_closed();
    committed.sort_by_key(|b| b.ts);
    let mut late = emitter.take_late();
    late.sort_by_key(|b| b.ts);

    assert_eq!(
        committed,
        vec![at(utc(2026, 1, 5, 20, 57)), at(utc(2026, 1, 5, 20, 58)),],
        "the last bar is committed by the seal, the rest by closure"
    );
    assert_eq!(
        late,
        vec![at(utc(2026, 1, 5, 20, 55))],
        "the straggler is retained as the hole signal"
    );
    assert_eq!(
        late[0].ts + Duration::minutes(2),
        committed[0].ts,
        "the committed series really does have a hole in it"
    );
}
