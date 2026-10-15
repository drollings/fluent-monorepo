//! Merge refusal: what a restatement may not do to a forming bar.
//!
//! `merge` composes two readings of the same instant. It never re-derives
//! OHLC validity — it hands the result to `Bar::validate` and refuses with
//! the shared reason — and it refuses *before* touching what the emitter
//! holds, so a malformed restatement cannot half-apply.

use chrono::{DateTime, Utc};

use super::*;
use crate::commit::test_tapes::{bar, ohlc, stamp};

/// The reading a merge produced that no OHLC invariant allows: the high sits
/// below the open, so `Bar::validate` refuses it.
fn high_below_open() -> Bar {
    ohlc(0, "100", "99", "98", "98")
}

#[test]
fn a_merge_that_breaks_the_ohlc_invariants_is_refused() {
    let held = bar(0, "100");
    let refusal = held
        .merge(&high_below_open())
        .expect_err("a print whose high sits below its open cannot merge");
    assert!(
        matches!(refusal, SnapshotError::Inconsistent { .. }),
        "the refusal names inconsistency, got {refusal:?}"
    );
    let SnapshotError::Inconsistent { reason } = refusal else {
        unreachable!("refusal variant already matched")
    };
    assert!(
        reason.contains("high"),
        "the reason comes from the shared bar validator, not a private copy: \
         {reason}"
    );
}

#[test]
fn a_union_fold_alone_would_have_absorbed_the_garbage_print() {
    // The control group for the check above. Folding without validating the
    // incoming reading first yields `open 100 / high 100 / low 98 / close 98`
    // — structurally valid, and silently wrong. This is why the incoming
    // reading is refused before it is folded rather than after.
    let held = bar(0, "100");
    let garbage = high_below_open();
    let absorbed = Bar {
        ts: held.ts,
        o: held.o,
        h: held.h.max(garbage.h),
        l: held.l.min(garbage.l),
        c: garbage.c,
        volume: garbage.volume,
        adj: None,
    };
    assert!(
        absorbed.validate().is_ok(),
        "the union of a valid bar with garbage is itself valid — which is \
         exactly why validating only the result is not enough"
    );
    assert!(
        held.merge(&garbage).is_err(),
        "the fold is guarded instead, so the garbage never reaches the bar"
    );
}

#[test]
fn a_refused_merge_leaves_the_held_bar_untouched() {
    let mut emitter = ClosedEmitter::<Bar>::new();
    emitter.observe(bar(0, "100")).expect("opens");
    let before = emitter.held().expect("bar is held").clone();

    let refusal = emitter
        .observe(high_below_open())
        .expect_err("an inconsistent restatement is refused");

    assert!(
        matches!(refusal, SnapshotError::Inconsistent { .. }),
        "the observe path reports the same refusal the trait does"
    );
    assert_eq!(
        emitter.held(),
        Some(&before),
        "a refused restatement leaves the forming bar exactly as it was"
    );
    assert_eq!(
        emitter.take_closed(),
        Vec::new(),
        "a refusal closes nothing"
    );
    assert_eq!(emitter.take_late(), Vec::new(), "a refusal queues nothing");
    assert_eq!(emitter.counts().revised, 0, "a refusal is not a revision");
}

#[test]
fn merging_two_different_instants_is_a_key_conflict() {
    let held = bar(0, "100");
    let other = bar(5, "101");
    let refusal = held
        .merge(&other)
        .expect_err("two instants are never one bar");
    assert_eq!(
        refusal,
        SnapshotError::KeyConflict {
            held: held.ts.to_rfc3339(),
            newer: other.ts.to_rfc3339(),
        },
        "the refusal names both stamps, so the caller can say which two"
    );
}

#[test]
fn a_valid_restatement_merges_open_from_the_first_and_close_from_the_last() {
    let held = bar(0, "100");
    // Same instant, open unchanged, close moved, high extended to cover it.
    let later = ohlc(0, "100", "104", "100", "104");

    let merged = held.merge(&later).expect("a consistent restatement merges");
    assert_eq!(merged.ts, held.ts, "the bar keeps its own stamp");
    assert_eq!(merged.o, held.o, "the open is the first print's open");
    assert_eq!(merged.c, later.c, "the close is the latest print's close");
    assert_eq!(merged.h, later.h, "the high covers everything seen");
    assert_eq!(merged.l, held.l, "the low covers everything seen");
    assert_eq!(
        merged.volume, later.volume,
        "volume is the venue's running total, so the latest print wins"
    );
}

#[test]
fn a_restatement_can_move_the_close_down_and_the_low_with_it() {
    let held = ohlc(0, "100", "104", "98", "103");
    let later = ohlc(0, "100", "103", "97", "99");

    let merged = held.merge(&later).expect("a downward restatement merges");
    assert_eq!(merged.c, "99".parse().expect("valid decimal"));
    assert_eq!(
        merged.l,
        "97".parse().expect("valid decimal"),
        "a low that moved down extends the range, it does not shrink it"
    );
    assert_eq!(
        merged.h,
        "104".parse().expect("valid decimal"),
        "the high never shrinks: it covered a price that was really printed"
    );
}

#[test]
fn a_consistent_restatement_reaches_the_held_bar_through_the_emitter() {
    let mut emitter = ClosedEmitter::<Bar>::new();
    emitter.observe(bar(0, "100")).expect("opens");

    let arrival = emitter
        .observe(ohlc(0, "100", "101", "99", "100"))
        .expect("this restatement is consistent");
    assert_eq!(arrival, Arrival::Revised(held_stamp()));

    assert_eq!(
        emitter.held().map(|b| b.h),
        Some("101".parse().expect("valid decimal")),
        "the merged high is what the emitter now holds"
    );
    assert_eq!(emitter.counts().revised, 1);
}

fn held_stamp() -> DateTime<Utc> {
    stamp(0)
}
