//! Signed remainder: the reference-vs-ring volume difference carries a sign,
//! and each sign takes its own arm.
//!
//! - Shortfall (reference above the ring): append a top-up candle (the old
//!   rule, unchanged — pinned by `top_up_appends_on_shortfall`).
//! - Overshoot (reference below the ring): trim the newest candle(s) down
//!   toward the reference, newest first, preserving each candle's buy share
//!   and provenance labels; never append a fabricated positive candle.
//!
//! Behavior-change note (reviewed): before the signed rule, overshoot took
//! `abs()` and appended a positive candle, pushing the total further from
//! the reference — the native code mirrored the shipped Pine
//! `_addRemainderCandle` verbatim. The signed rule deliberately diverges
//! from the Pine oracle here (scale down instead of fabricate); freeze
//! windows covering overshoot regenerate with this reason recorded.

use super::*;

const MIN: i64 = 60_000;

fn test_config() -> BarsConfig {
    BarsConfig::new(MIN, MIN, 5 * MIN, 86_400_000)
}

fn row(ts_ms: i64, o: f64, h: f64, l: f64, c: f64, volume: f64) -> LtfRow {
    LtfRow { ts_ms, o, h, l, c, volume }
}

fn rows_total_1000() -> Vec<LtfRow> {
    let base = 1_699_999_980_000;
    vec![
        row(base, 100.0, 101.0, 99.0, 100.5, 500.0),
        row(base + MIN, 100.5, 102.0, 100.0, 101.5, 500.0),
    ]
}

#[test]
fn top_up_appends_on_shortfall() {
    let mut ring = BarRing::new(&test_config()).expect("valid config");
    let outcome = ring.reconcile(&rows_total_1000(), 1_200.0, 100.0);
    assert_eq!(ring.len(), 3, "shortfall appends one top-up candle");
    assert_close(ring.total_volume(), 1_200.0, "total meets the reference");
    let top = outcome.remainder.expect("shortfall reports the top-up");
    assert_close(top.volume, 200.0, "top-up carries the shortfall");
}

#[test]
fn overshoot_trims_instead_of_appending() {
    // REGENERATED with the signed rule (reason in the file header): overshoot
    // used to take `abs()` and append a positive candle (total 1,200 from a
    // 1,000 ring against an 800 reference); now it trims newest-first.
    let mut ring = BarRing::new(&test_config()).expect("valid config");
    let outcome = ring.reconcile(&rows_total_1000(), 800.0, 100.0);
    assert_eq!(ring.len(), 2, "signed rule appends nothing on overshoot");
    assert_close(ring.total_volume(), 800.0, "total meets the reference");
    let top = outcome.remainder.expect("signed rule reports the trim");
    assert_close(top.volume, 300.0, "newest trimmed by the overshoot");
}

#[test]
fn overshoot_trims_newest_after_signed_rule() {
    // NEW RULE (red before the signed rule lands): trim, never append.
    let mut ring = BarRing::new(&test_config()).expect("valid config");
    let outcome = ring.reconcile(&rows_total_1000(), 800.0, 100.0);
    assert_eq!(ring.len(), 2, "signed rule appends nothing on overshoot");
    assert_close(ring.total_volume(), 800.0, "total meets the reference");
    let trimmed = outcome.remainder.expect("signed rule reports the trim");
    assert_close(trimmed.volume, 300.0, "newest trimmed by the overshoot");
    assert_close(trimmed.c, 101.5, "trim keeps the newest candle identity");
    // Report-read identity: the reported trim is the stored newest candle,
    // so its buy share survives the trim instead of being re-estimated.
    let read = ring.get(1).expect("newest readable");
    assert_close(read.volume, 300.0, "stored newest matches the report");
    assert_close(
        read.buy_volume,
        trimmed.buy_volume,
        "stored buy matches the report",
    );
    assert_close(
        ring.total_buy_volume(),
        ring.get(0).expect("older readable").buy_volume + read.buy_volume,
        "totals stay the sum of stored splits",
    );
}

#[test]
fn overshoot_bigger_than_newest_cascades() {
    // NEW RULE (red before the signed rule lands): an overshoot past the
    // newest candle zeroes it (kept as a zero-volume placeholder, like gap
    // fills) and cascades to the next-newest.
    let mut ring = BarRing::new(&test_config()).expect("valid config");
    let outcome = ring.reconcile(&rows_total_1000(), 300.0, 100.0);
    assert_eq!(ring.len(), 2, "cascade keeps every timestamp");
    assert_close(ring.total_volume(), 300.0, "total meets the reference");
    let newest = ring.get(1).expect("newest readable");
    assert_close(newest.volume, 0.0, "newest zeroed, not removed");
    let older = ring.get(0).expect("older readable");
    assert_close(older.volume, 300.0, "cascade trims the next-newest");
    assert!(outcome.remainder.is_some(), "cascade reports the trim");
}

#[test]
fn dust_undershoot_tops_up_nothing() {
    let mut ring = BarRing::new(&test_config()).expect("valid config");
    let over = ring.reconcile(&rows_total_1000(), 1_000.0 - VOLUME_EPSILON / 2.0, 100.0);
    assert!(over.remainder.is_none(), "dust overshoot trims nothing");
    assert_eq!(ring.len(), 2, "dust leaves the ring alone");
    let under = ring.reconcile(&rows_total_1000(), 1_000.0 + VOLUME_EPSILON / 2.0, 100.0);
    assert!(under.remainder.is_none(), "dust shortfall tops up nothing");
}

#[test]
fn empty_ring_negative_reference_tops_up_nothing() {
    let mut ring = BarRing::new(&test_config()).expect("valid config");
    let outcome = ring.reconcile(&[], -50.0, 100.0);
    assert!(outcome.remainder.is_none(), "nothing to trim on an empty ring");
    assert!(ring.is_empty(), "empty ring stays empty");
}

fn assert_close(got: f64, want: f64, what: &str) {
    assert!(
        (got - want).abs() <= 1e-9,
        "{what}: got {got}, want {want}"
    );
}
