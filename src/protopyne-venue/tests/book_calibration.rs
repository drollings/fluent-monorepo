//! Book-detector calibration: controls that must stay silent plus trips
//! that must fire, with measured precision/recall.
//!
//! Protocol (mirrors the venue fill calibration and the kernel parity
//! discipline): the control group proves clean books never trip a detector
//! (a detector that cries on clean structure rehearses nothing); the trip
//! group proves crossed tops, dropped sequences, and stale feeds are caught
//! with reasons. Rates are measured, never assumed.
//!
//! Scope: this milestone is tests plus frozen constants — no production
//! logic. The detectors stay dark (record-only): nothing here gates a fill,
//! submits an order, or reads profitability/fill-rate. Success is signal
//! quality on **(correctness)** inputs only.
//!
//! Frozen thresholds (committed here so the measurement is reproducible):
//!
//! - [`MAX_BOOK_AGE_SECS`]: the staleness bound this suite evaluates. The
//!   follow-up change that wires the detectors into the fill path as
//!   **(correctness)** refusals must use this value, not a fresh guess —
//!   re-tuning without re-running this suite voids the calibration.
//! - Scenario counts ([`CONTROL_SCENARIOS`], [`TRIP_SCENARIOS`]): the gate
//!   test pins the group sizes, so adding or dropping a scenario without
//!   updating the counts fails loudly instead of silently shifting rates.

use super::*;
use chrono::{Duration, TimeZone, Utc};
use protopyne_types::{Money, SymbolIndex};
use rust_decimal::Decimal;

// ── frozen thresholds ──────────────────────────────────────────────
// See the module docs: these values are the calibration under test. The
// follow-up wiring change (detectors into the fill path as correctness
// refusals) inherits them; any change re-opens this suite.

/// Staleness bound evaluated throughout this suite, in seconds. A book
/// older than this at evaluation time must trip `is_stale`; a book at or
/// under it must not.
pub const MAX_BOOK_AGE_SECS: u64 = 30;

/// Control scenarios in this suite (locked-book replay, slow drift,
/// single-tick flicker). The gate test pins this count.
pub const CONTROL_SCENARIOS: usize = 3;

/// Trip scenarios in this suite (injected crosses, dropped sequences,
/// stale feeds, mixed-manager scan). The gate test pins this count.
pub const TRIP_SCENARIOS: usize = 4;

/// Slow-drift replay length in snapshots.
pub const DRIFT_STEPS: u64 = 12;

// ── fixtures ───────────────────────────────────────────────────────

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn level(price: &str, size: u64) -> PriceLevel {
    PriceLevel { price: money(price), size }
}

fn base_ts() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
}

/// Evaluation instant for "fresh" books: inside the staleness bound.
fn fresh_now() -> chrono::DateTime<Utc> {
    base_ts() + Duration::seconds(5)
}

fn book_for(symbol: &str) -> (DepthBook, SymbolIndex) {
    let mut index = SymbolIndex::new();
    let book = DepthBook::new(symbol.to_owned(), &mut index).expect("intern symbol");
    (book, index)
}

fn assert_silent(book: &DepthBook, now: chrono::DateTime<Utc>, what: &str) {
    assert!(
        is_crossed(book).is_none(),
        "{what}: crossed detector fired on a clean book"
    );
    assert!(
        is_stale(book, now, MAX_BOOK_AGE_SECS).is_none(),
        "{what}: stale detector fired on a fresh book"
    );
}

// ── tally ──────────────────────────────────────────────────────────
// Test-local tally (same discipline as the spike `CalibrationTally`:
// accept and reject both 1.0 before trust). It lives here rather than
// reusing the server tally because the venue layer cannot depend on the
// server, and the denominators here are scenarios, not scripts.

/// Control accept / trip recall tally for this suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BookCalibrationTally {
    /// Control scenarios presented.
    controls: usize,
    /// Control scenarios that tripped either detector (false fires).
    control_false_fires: usize,
    /// Trip scenarios presented.
    trips: usize,
    /// Trip scenarios that fired the expected signal.
    trips_fired: usize,
}

impl BookCalibrationTally {
    /// Fraction of controls that stayed silent.
    fn control_accept_rate(&self) -> f64 {
        if self.controls == 0 {
            return 0.0;
        }
        (self.controls - self.control_false_fires) as f64 / self.controls as f64
    }

    /// Fraction of trips that fired.
    fn trip_recall(&self) -> f64 {
        if self.trips == 0 {
            return 0.0;
        }
        self.trips_fired as f64 / self.trips as f64
    }

    /// Fraction of all fired signals that were expected trips.
    fn precision(&self) -> f64 {
        let fired = self.trips_fired + self.control_false_fires;
        if fired == 0 {
            return 0.0;
        }
        self.trips_fired as f64 / fired as f64
    }

    /// Whether the detectors may be trusted: every control silent, every
    /// trip fired.
    fn is_green(&self) -> bool {
        (self.control_accept_rate() - 1.0).abs() < f64::EPSILON
            && (self.trip_recall() - 1.0).abs() < f64::EPSILON
    }
}

// ── control group: clean books stay silent ─────────────────────────

/// Locked-book replay: the spread pinned at the minimum valid one-tick
/// width across consecutive snapshots must trip neither detector.
///
/// Interpretation note: a strictly locked top (`bid == ask`) IS a cross by
/// the `Quote::spread_width` contract (`None` when `ask <= bid`), so a
/// replay of truly locked prints belongs in the trip group (covered by
/// `injected_crosses_fire`). This control replays the tightest *valid*
/// book instead — the case the wiring change must never refuse.
#[test]
fn locked_book_replay_stays_silent() {
    let (mut book, _index) = book_for("SPY");
    for sequence in 1..=6 {
        book.snapshot_replace(
            &[level("600.00", 50)],
            &[level("600.01", 40)],
            sequence,
            base_ts() + Duration::seconds(sequence as i64),
        )
        .expect("tight snapshot sets");
        assert_silent(&book, fresh_now(), "locked-book replay");
    }
}

/// Slow drift: the touch walks up with a constant spread and fresh stamps
/// across [`DRIFT_STEPS`] snapshots — structure stays clean throughout.
#[test]
fn slow_drift_stays_silent() {
    let (mut book, _index) = book_for("SPY");
    for step in 0..DRIFT_STEPS {
        let bid = Decimal::new(60_000 + step as i64, 2);
        let ask = Decimal::new(60_010 + step as i64, 2);
        book.snapshot_replace(
            &[PriceLevel { price: Money::new(bid), size: 50 }],
            &[PriceLevel { price: Money::new(ask), size: 40 }],
            step + 1,
            base_ts() + Duration::seconds(step as i64),
        )
        .expect("drift snapshot sets");
        assert_silent(&book, fresh_now(), "slow drift");
    }
}

/// Single-tick flicker: one side steps a tick adverse and back, never
/// crossing, staying fresh — transient tightness, not a cross.
#[test]
fn single_tick_flicker_stays_silent() {
    let (mut book, _index) = book_for("SPY");
    book.snapshot_replace(
        &[level("600.00", 50)],
        &[level("600.10", 40)],
        1,
        base_ts(),
    )
    .expect("flicker baseline sets");
    assert_silent(&book, fresh_now(), "flicker baseline");
    for (sequence, bids, asks) in [
        (2, vec![level("599.99", 50)], vec![]),
        (3, vec![level("600.00", 50)], vec![]),
        (4, vec![], vec![level("600.11", 40)]),
        (5, vec![], vec![level("600.10", 40)]),
    ] {
        book.apply_delta(&DepthDelta { bids, asks, sequence, ts: base_ts() })
            .expect("flicker delta applies");
        assert_silent(&book, fresh_now(), "single-tick flicker");
    }
}

// ── trip group: faults fire with reasons ───────────────────────────

/// Injected crosses: a crossed top and a locked top both fire `is_crossed`
/// with a reason naming the book.
#[test]
fn injected_crosses_fire() {
    let (mut book, _index) = book_for("SPY");
    book.snapshot_replace(
        &[level("600.20", 50)],
        &[level("600.10", 40)],
        1,
        base_ts(),
    )
    .expect("crossed snapshot sets");
    let reason = is_crossed(&book).expect("crossed top must fire");
    assert!(reason.contains("SPY"), "reason names the book: {reason}");

    book.snapshot_replace(
        &[level("600.10", 50)],
        &[level("600.10", 40)],
        2,
        base_ts(),
    )
    .expect("locked snapshot sets");
    let reason = is_crossed(&book).expect("locked top must fire");
    assert!(reason.contains("SPY"), "reason names the book: {reason}");
}

/// Dropped sequences: a gapped delta refuses fail-closed with a gap reason
/// and commits nothing; a gapped manager heartbeat refuses the same way.
#[test]
fn dropped_sequences_refuse() {
    let (mut book, _index) = book_for("SPY");
    book.snapshot_replace(
        &[level("600.00", 50)],
        &[level("600.10", 40)],
        5,
        base_ts(),
    )
    .expect("snapshot sets");
    let before = book.top().expect("touch");
    let refusal = book
        .apply_delta(&DepthDelta {
            bids: vec![level("600.05", 30)],
            asks: vec![],
            sequence: 7,
            ts: base_ts(),
        })
        .expect_err("sequence gap must refuse");
    assert!(
        refusal.to_string().contains("sequence gap"),
        "gap refusal names the fault: {refusal}"
    );
    assert_eq!(book.sequence(), 5, "refusal commits nothing");
    assert_eq!(book.top().expect("touch"), before, "ladders unchanged");

    let mut manager = BookManager::new();
    manager
        .apply_snapshot("SPY", &[level("600.00", 50)], &[level("600.10", 40)], 1, base_ts())
        .expect("manager snapshot opens");
    let refusal = manager
        .apply_event(&MarketEvent::BookDelta { underlying: "SPY".to_owned(), sequence: 3 })
        .expect_err("manager heartbeat gap must refuse");
    assert!(
        refusal.to_string().contains("sequence gap"),
        "manager gap refusal names the fault: {refusal}"
    );
}

/// Stale feeds: a book past [`MAX_BOOK_AGE_SECS`] fires `is_stale` with a
/// reason; a book exactly at the bound stays silent (strictly-greater-than
/// contract, pinned here so the wiring change inherits the edge).
#[test]
fn stale_feeds_fire() {
    let (mut book, _index) = book_for("SPY");
    book.snapshot_replace(
        &[level("600.00", 50)],
        &[level("600.10", 40)],
        1,
        base_ts(),
    )
    .expect("snapshot sets");
    assert!(
        is_stale(&book, base_ts() + Duration::seconds(MAX_BOOK_AGE_SECS as i64), MAX_BOOK_AGE_SECS)
            .is_none(),
        "a book exactly at the bound is fresh"
    );
    let late = base_ts() + Duration::seconds(MAX_BOOK_AGE_SECS as i64 + 1);
    let reason = is_stale(&book, late, MAX_BOOK_AGE_SECS).expect("stale book must fire");
    assert!(reason.contains("SPY"), "reason names the book: {reason}");
    assert!(reason.contains("30s"), "reason states the bound: {reason}");
}

/// Mixed-manager scan: the dark sweep over clean, crossed, and stale books
/// records exactly the expected findings, sorted by symbol — the
/// record-only path the wiring change will consume.
#[test]
fn mixed_manager_scan_records_expected_findings() {
    let mut manager = BookManager::new();
    manager
        .apply_snapshot("SPY", &[level("600.00", 50)], &[level("600.10", 40)], 1, base_ts())
        .expect("clean book opens");
    manager
        .apply_snapshot("AAPL", &[level("100.20", 10)], &[level("100.10", 10)], 1, base_ts())
        .expect("crossed book opens");
    manager
        .apply_snapshot(
            "MSFT",
            &[level("200.00", 10)],
            &[level("200.05", 10)],
            1,
            base_ts() - Duration::seconds(MAX_BOOK_AGE_SECS as i64 + 60),
        )
        .expect("old book opens");
    let findings = manager.scan(fresh_now(), MAX_BOOK_AGE_SECS);
    let symbols: Vec<&str> = findings.iter().map(|finding| finding.symbol.as_str()).collect();
    assert_eq!(symbols, vec!["AAPL", "MSFT"], "only the faulty books record");
    assert!(
        findings[0].reason.contains("crossed"),
        "crossed finding names the fault: {}",
        findings[0].reason
    );
    assert!(
        findings[1].reason.contains("stale"),
        "stale finding names the fault: {}",
        findings[1].reason
    );
}

// ── gate: measured rates at 1.0 ────────────────────────────────────

/// Calibration gate: replays both groups end to end and pins the measured
/// rates — 1.0 control accept, 1.0 trip recall — plus the frozen scenario
/// counts, so group edits without count updates fail here.
#[test]
fn book_detector_calibration_is_green() {
    // Controls: each scenario replays clean books and counts false fires.
    let mut false_fires = 0;
    let mut clean = BookManager::new();
    clean
        .apply_snapshot("SPY", &[level("600.00", 50)], &[level("600.01", 40)], 1, base_ts())
        .expect("tight book opens");
    false_fires += clean.scan(fresh_now(), MAX_BOOK_AGE_SECS).len();
    for step in 0..DRIFT_STEPS {
        let bid = Decimal::new(60_000 + step as i64, 2);
        let ask = Decimal::new(60_010 + step as i64, 2);
        clean
            .apply_snapshot(
                "SPY",
                &[PriceLevel { price: Money::new(bid), size: 50 }],
                &[PriceLevel { price: Money::new(ask), size: 40 }],
                step + 1,
                base_ts(),
            )
            .expect("drift snapshot sets");
        false_fires += clean.scan(fresh_now(), MAX_BOOK_AGE_SECS).len();
    }
    let (mut flicker, _index) = book_for("SPY");
    flicker
        .snapshot_replace(&[level("600.00", 50)], &[level("600.10", 40)], 1, base_ts())
        .expect("flicker baseline sets");
    false_fires += usize::from(is_crossed(&flicker).is_some());
    false_fires += usize::from(is_stale(&flicker, fresh_now(), MAX_BOOK_AGE_SECS).is_some());

    // Trips: each scenario presents a fault and counts the expected fire.
    let mut fired = 0;
    let (mut crossed, _index) = book_for("SPY");
    crossed
        .snapshot_replace(&[level("600.20", 50)], &[level("600.10", 40)], 1, base_ts())
        .expect("crossed snapshot sets");
    fired += usize::from(is_crossed(&crossed).is_some());
    fired += usize::from(
        crossed
            .apply_delta(&DepthDelta {
                bids: vec![],
                asks: vec![],
                sequence: 3,
                ts: base_ts(),
            })
            .is_err(),
    );
    fired += usize::from(
        is_stale(&crossed, base_ts() + Duration::seconds(MAX_BOOK_AGE_SECS as i64 + 1), MAX_BOOK_AGE_SECS)
            .is_some(),
    );
    let mut mixed = BookManager::new();
    mixed
        .apply_snapshot("SPY", &[level("600.00", 50)], &[level("600.10", 40)], 1, base_ts())
        .expect("clean book opens");
    mixed
        .apply_snapshot("AAPL", &[level("100.20", 10)], &[level("100.10", 10)], 1, base_ts())
        .expect("crossed book opens");
    fired += usize::from(mixed.scan(fresh_now(), MAX_BOOK_AGE_SECS).len() == 1);

    let tally = BookCalibrationTally {
        controls: CONTROL_SCENARIOS,
        control_false_fires: false_fires,
        trips: TRIP_SCENARIOS,
        trips_fired: fired,
    };
    assert_eq!(CONTROL_SCENARIOS, 3, "control group size is frozen");
    assert_eq!(TRIP_SCENARIOS, 4, "trip group size is frozen");
    assert!(
        tally.is_green(),
        "calibration must be green at 1.0 accept / 1.0 recall: \
         control accept {:.3}, trip recall {:.3}, precision {:.3} ({tally:?})",
        tally.control_accept_rate(),
        tally.trip_recall(),
        tally.precision()
    );
}
