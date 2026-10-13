//! Honest delta estimation: every buy-volume split carries the method that
//! produced it and the confidence its coverage earns. Ticks win where they
//! cover the bucket (`TickSigned`); candle shape fills only uncovered
//! buckets and degenerate input, always labeled `Low`. The neutral split is
//! the declared prior for directionless input, never a measurement.
//!
//! Numeric outputs are unchanged (wrappers delegate to the labeled
//! constructors); these tests pin the labels and the coverage rule.
//! `#[allow(clippy::float_cmp)]` covers the exact decimal literals (splits
//! asserted here are exact by construction: halves and whole takes).
#![allow(clippy::float_cmp)]

use super::*;
use crate::indicator::BarTick;

const T0: i64 = 1_699_999_980_000;
const MIN: i64 = 60_000;

fn test_config() -> BarsConfig {
    BarsConfig::new(MIN, MIN, 5 * MIN, 86_400_000)
}

fn row(ts_ms: i64, o: f64, h: f64, l: f64, c: f64, volume: f64) -> LtfRow {
    LtfRow { ts_ms, o, h, l, c, volume }
}

fn print(ts_ms: i64, price: f64, volume: f64) -> RawTick {
    RawTick { ts_ms, price, volume }
}

fn fresh_ring() -> BarRing {
    BarRing::new(&test_config()).expect("config validates")
}

fn provenance(ring: &BarRing, index: usize) -> (DeltaMethod, DeltaConfidence) {
    ring.buy_provenance(index).expect("every stored candle is labeled")
}

#[test]
fn mixed_coverage_tape_labels_per_bucket() {
    let mut ring = fresh_ring();
    let rows = vec![
        row(T0, 100.0, 101.0, 99.5, 100.8, 100.0),
        row(T0 + MIN, 100.8, 101.5, 100.0, 101.2, 200.0),
        row(T0 + 2 * MIN, 101.2, 102.0, 100.9, 101.8, 150.0),
    ];
    // Buckets 0 (two directed prints) and 2 (one thin print) are covered;
    // bucket 1 has no prints at all.
    let ticks = vec![
        print(T0 + 5_000, 100.0, 10.0),
        print(T0 + 10_000, 100.5, 20.0),
        print(T0 + 2 * MIN + 5_000, 101.0, 30.0),
    ];
    let outcome = ring.reconcile_with_ticks(&rows, &ticks, 450.0, 100.0);
    assert_eq!(outcome.rebuilt, 3, "three rows rebuild");
    assert!(outcome.remainder.is_none(), "exact reference tops up nothing");
    // Bucket 0: first print seeds neutral (5.0), second takes all (20.0).
    assert_eq!(provenance(&ring, 0), (DeltaMethod::TickSigned, DeltaConfidence::High));
    assert_eq!(ring.get(0).expect("candle").buy_volume, 25.0);
    // Bucket 1: shape fills the uncovered bucket, labeled Low.
    assert_eq!(provenance(&ring, 1), (DeltaMethod::ShapeFallback, DeltaConfidence::Low));
    assert_eq!(
        ring.get(1).expect("candle").buy_volume,
        estimate_delta_volume(100.8, 101.5, 100.0, 101.2, 200.0)
    );
    // Bucket 2: one thin print signs the whole split at reduced confidence.
    assert_eq!(provenance(&ring, 2), (DeltaMethod::TickSigned, DeltaConfidence::Reduced));
    assert_eq!(ring.get(2).expect("candle").buy_volume, 30.0);
}

#[test]
fn degenerate_flat_inputs_read_neutral_low() {
    let mut ring = fresh_ring();
    // Flat prints carry no direction: the neutral prior at low confidence.
    let flat_row = row(T0, 100.0, 100.0, 100.0, 100.0, 30.0);
    // Dust range fails structural validation: neutral prior, low confidence.
    let dust_row = row(T0 + MIN, 100.0, 100.0 + 1e-9, 100.0, 100.0, 50.0);
    let ticks = vec![
        print(T0 + 5_000, 100.0, 10.0),
        print(T0 + 10_000, 100.0, 10.0),
        print(T0 + 15_000, 100.0, 10.0),
    ];
    let outcome = ring.reconcile_with_ticks(&[flat_row, dust_row], &ticks, 80.0, 100.0);
    assert_eq!(outcome.rebuilt, 2);
    assert!(outcome.remainder.is_none(), "exact reference tops up nothing");
    assert_eq!(provenance(&ring, 0), (DeltaMethod::TickSigned, DeltaConfidence::Low));
    assert_eq!(ring.get(0).expect("candle").buy_volume, 15.0, "neutral split of 30");
    assert_eq!(provenance(&ring, 1), (DeltaMethod::ShapeFallback, DeltaConfidence::Low));
    assert_eq!(ring.get(1).expect("candle").buy_volume, 25.0, "neutral split of 50");
}

#[test]
fn every_write_path_labels_its_candles() {
    // Shape rebuild: every candle labeled.
    let mut ring = fresh_ring();
    ring.reconcile(
        &[row(T0, 100.0, 101.0, 99.5, 100.8, 100.0), row(T0 + MIN, 100.8, 101.5, 100.0, 101.2, 200.0)],
        300.0,
        100.0,
    );
    assert_eq!(ring.len(), 2);
    for index in 0..ring.len() {
        assert_eq!(provenance(&ring, index), (DeltaMethod::ShapeFallback, DeltaConfidence::Low));
    }

    // Mixed tick rebuild: every candle labeled.
    let mut ring = fresh_ring();
    ring.reconcile_with_ticks(
        &[row(T0, 100.0, 101.0, 99.5, 100.8, 100.0), row(T0 + MIN, 100.8, 101.5, 100.0, 101.2, 200.0)],
        &[print(T0 + 5_000, 100.0, 10.0), print(T0 + 10_000, 100.5, 20.0)],
        300.0,
        100.0,
    );
    assert_eq!(ring.len(), 2);
    for index in 0..ring.len() {
        let _ = provenance(&ring, index);
    }
    assert_eq!(provenance(&ring, 0).0, DeltaMethod::TickSigned);
    assert_eq!(provenance(&ring, 1).0, DeltaMethod::ShapeFallback);

    // Live fold into a finalized candle: tick-signed at high confidence.
    let mut ring = fresh_ring();
    ring.on_bar_open(100.0, 100.0, 100.0, 100.0, T0);
    ring.ingest_tick(
        BarTick { open: 100.0, high: 100.5, low: 100.0, close: 100.5, volume: 10.0 },
        T0 + 10_000,
    );
    ring.ingest_tick(
        BarTick { open: 100.0, high: 101.0, low: 100.0, close: 101.0, volume: 30.0 },
        T0 + 20_000,
    );
    ring.confirm_ltf(T0 + 70_000, 101.0, None);
    assert_eq!(ring.len(), 1, "crossed boundary finalizes");
    assert_eq!(provenance(&ring, 0), (DeltaMethod::TickSigned, DeltaConfidence::High));

    // A flat print anywhere in the bar drops the accumulator to low. The
    // first update only seeds tracking, so the flat print is the second.
    let mut ring = fresh_ring();
    ring.on_bar_open(100.0, 100.0, 100.0, 100.0, T0);
    ring.ingest_tick(
        BarTick { open: 100.0, high: 100.0, low: 100.0, close: 100.0, volume: 10.0 },
        T0 + 10_000,
    );
    ring.ingest_tick(
        BarTick { open: 100.0, high: 100.0, low: 100.0, close: 100.0, volume: 20.0 },
        T0 + 15_000,
    );
    ring.ingest_tick(
        BarTick { open: 100.0, high: 101.0, low: 100.0, close: 101.0, volume: 30.0 },
        T0 + 20_000,
    );
    ring.confirm_ltf(T0 + 70_000, 101.0, None);
    assert_eq!(provenance(&ring, 0), (DeltaMethod::TickSigned, DeltaConfidence::Low));

    // Gap placeholders: invented continuity, labeled low.
    let gapped = BarsConfig { fill_gaps: true, ..test_config() };
    let mut ring = BarRing::new(&gapped).expect("config validates");
    ring.on_bar_open(100.0, 100.0, 100.0, 100.0, T0);
    ring.ingest_tick(
        BarTick { open: 100.0, high: 100.5, low: 100.0, close: 100.5, volume: 10.0 },
        T0 + 10_000,
    );
    ring.ingest_tick(
        BarTick { open: 100.0, high: 101.0, low: 100.0, close: 101.0, volume: 30.0 },
        T0 + 20_000,
    );
    ring.confirm_ltf(T0 + 190_000, 101.0, None);
    assert_eq!(ring.len(), 3, "finalized plus two gaps");
    for index in 0..ring.len() {
        let _ = provenance(&ring, index);
    }

    // Remainder top-up carries the prevailing method label.
    let mut ring = fresh_ring();
    ring.reconcile(
        &[row(T0, 100.0, 101.0, 99.5, 100.8, 150.0), row(T0 + MIN, 100.8, 101.5, 100.0, 101.2, 150.0)],
        300.0,
        100.0,
    );
    ring.confirm(&ConfirmedBar {
        bar_open_ms: T0,
        o: 100.0,
        h: 101.5,
        l: 99.5,
        c: 101.2,
        volume: 310.0,
    });
    assert_eq!(ring.len(), 3, "remainder tops up");
    assert_eq!(
        provenance(&ring, 2),
        (DeltaMethod::ShapeFallback, DeltaConfidence::Low),
        "all-shape rebuild prevails shape"
    );
}

#[test]
fn wrappers_stay_byte_identical() {
    // Print-level splits: direction signs, flat/NaN splits neutral.
    assert_eq!(estimate_buy_volume(100.0, 1.0), 100.0, "up takes all");
    assert_eq!(estimate_buy_volume(100.0, -1.0), 0.0, "down takes none");
    assert_eq!(estimate_buy_volume(100.0, 0.0), 50.0, "flat splits neutral");
    assert_eq!(estimate_buy_volume(100.0, f64::NAN), 50.0, "directionless splits neutral");
    assert_eq!(estimate_buy_volume(0.0, 1.0), 0.0, "empty print stays empty");
    // Shape splits: degenerate falls back to neutral.
    assert_eq!(estimate_delta_volume(100.0, 100.0, 100.0, 100.0, 50.0), 25.0, "flat candle neutral");
    assert_eq!(estimate_delta_volume(f64::NAN, 102.0, 99.0, 101.0, 50.0), 25.0, "invalid neutral");
    // One valid shape vector, pinned against the hand computation
    // (body 1/3, close-position 2/3): 0.5 + 1/6 + 0.05 = 0.7166… of 1000.
    let shaped = estimate_delta_volume(100.0, 102.0, 99.0, 101.0, 1000.0);
    let want = 1000.0 * (0.5 + (1.0 / 3.0) * 0.5 + ((2.0 / 3.0) - 0.5) * 0.3);
    assert!((shaped - want).abs() <= 1e-9, "shape vector stable: got {shaped}, want {want}");
    // Labeled constructors agree with the wrappers exactly.
    assert_eq!(estimate_buy_volume_estimated(100.0, 1.0).buy_volume, 100.0);
    assert_eq!(estimate_buy_volume_estimated(100.0, 0.0).buy_volume, 50.0);
    assert_eq!(estimate_delta_volume_estimated(100.0, 100.0, 100.0, 100.0, 50.0).buy_volume, 25.0);
    assert_eq!(estimate_delta_volume_estimated(100.0, 102.0, 99.0, 101.0, 1000.0).buy_volume, shaped);
    // …and carry the honest labels.
    assert_eq!(
        estimate_buy_volume_estimated(100.0, 1.0).method,
        DeltaMethod::TickSigned
    );
    assert_eq!(
        estimate_buy_volume_estimated(100.0, 1.0).confidence,
        DeltaConfidence::High
    );
    assert_eq!(
        estimate_buy_volume_estimated(100.0, 0.0).confidence,
        DeltaConfidence::Low
    );
    assert_eq!(
        estimate_delta_volume_estimated(100.0, 102.0, 99.0, 101.0, 1000.0).method,
        DeltaMethod::ShapeFallback
    );
    assert_eq!(
        estimate_delta_volume_estimated(100.0, 102.0, 99.0, 101.0, 1000.0).confidence,
        DeltaConfidence::Low
    );
}

#[test]
fn choose_source_prefers_coverage() {
    assert_eq!(choose_source(5, 5), ResampleSource::Ticks, "full cover folds ticks");
    assert_eq!(choose_source(4, 5), ResampleSource::Bars, "partial cover reuses bars");
    assert_eq!(choose_source(0, 5), ResampleSource::Bars, "uncovered reuses bars");
    assert_eq!(choose_source(0, 0), ResampleSource::Bars, "nothing to cover reuses bars");
}

#[test]
fn reconcile_without_ticks_matches_plain_reconcile() {
    let rows = vec![
        row(T0, 100.0, 101.0, 99.5, 100.8, 100.0),
        row(T0 + MIN, 100.8, 101.5, 100.0, 101.2, 100.0),
        row(T0 + 2 * MIN, 101.2, 102.0, 100.9, 101.8, 100.0),
    ];
    let mut plain = fresh_ring();
    let left = plain.reconcile(&rows, 310.0, 100.0);
    let mut ticked = fresh_ring();
    let right = ticked.reconcile_with_ticks(&rows, &[], 310.0, 100.0);
    assert_eq!(left.rebuilt, right.rebuilt, "same rebuild count");
    assert_eq!(left.remainder, right.remainder, "same remainder candle");
    assert_eq!(plain.len(), ticked.len(), "same ring length");
    for index in 0..plain.len() {
        assert_eq!(plain.get(index), ticked.get(index), "candle {index} identical");
    }
    // Labels ride along: the tick-free rebuild is all shape, low.
    for index in 0..ticked.len() {
        assert_eq!(provenance(&ticked, index), (DeltaMethod::ShapeFallback, DeltaConfidence::Low));
    }
}
