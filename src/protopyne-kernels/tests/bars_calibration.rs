//! Calibration suite for the bar triggers: frozen thresholds with
//! must-fire sets, must-not-fire controls, and recorded precision/recall.
//!
//! Thresholds never widen to fit data: the exact pins in
//! `thresholds_frozen` fail the build on drift, and every trigger table
//! below asserts its verdicts against hand-computed expectations, never
//! measured quantities.
//! `#[allow(clippy::float_cmp)]` covers the exact decimal literals.
#![allow(clippy::float_cmp)]

use super::*;
use crate::indicator::BarTick;
use protopyne_types::Validate;

/// Frozen agreement tolerance shared with the Pine goldens.
const EPS: f64 = 1e-9;

/// Minute-aligned base for gap-matrix stamps.
const T0: i64 = 1_699_999_980_000;

const LTF: i64 = 60_000;
const FETCH: i64 = 60_000;
const CTF: i64 = 300_000;
const HTF: i64 = 86_400_000;

fn assert_close(got: f64, want: f64, what: &str) {
    assert!(
        (got - want).abs() <= EPS * want.abs().max(1.0),
        "{what}: got {got}, want {want}"
    );
}

fn test_config() -> BarsConfig {
    BarsConfig::new(LTF, FETCH, CTF, HTF)
}

/// Load a ring over `config` whose retained candles carry the given
/// volumes (each row meaningful, so every row lands). The reference equals
/// the row sum, so no remainder top-up fires and the ring total is exactly
/// the sum.
fn ring_with_volumes_in(config: &BarsConfig, volumes: &[f64]) -> BarRing {
    let mut ring = BarRing::new(config).expect("config valid");
    let rows: Vec<LtfRow> = volumes
        .iter()
        .enumerate()
        .map(|(index, volume)| LtfRow {
            ts_ms: index as i64 * LTF,
            o: 100.0,
            h: 101.0,
            l: 99.0,
            c: 100.5,
            volume: *volume,
        })
        .collect();
    let total: f64 = volumes.iter().sum();
    let outcome = ring.reconcile(&rows, total, 100.0);
    assert_eq!(outcome.remainder, None, "exact reference tops up nothing");
    assert_close(ring.total_volume(), total, "ring total equals row sum");
    ring
}

fn ring_with_volumes(volumes: &[f64]) -> BarRing {
    ring_with_volumes_in(&test_config(), volumes)
}

/// Precision and recall of predicted flags against hand labels. Empty
/// positive sides read 1.0 (nothing to miss, nothing to misfire).
fn precision_recall(predicted: &[bool], expected: &[bool]) -> (f64, f64) {
    assert_eq!(predicted.len(), expected.len(), "label count matches");
    let (mut true_pos, mut false_pos, mut false_neg) = (0, 0, 0);
    for (got, want) in predicted.iter().zip(expected.iter()) {
        match (got, want) {
            (true, true) => true_pos += 1,
            (true, false) => false_pos += 1,
            (false, true) => false_neg += 1,
            (false, false) => {}
        }
    }
    let precision = if true_pos + false_pos == 0 {
        1.0
    } else {
        f64::from(true_pos) / f64::from(true_pos + false_pos)
    };
    let recall = if true_pos + false_neg == 0 {
        1.0
    } else {
        f64::from(true_pos) / f64::from(true_pos + false_neg)
    };
    (precision, recall)
}

#[test]
fn thresholds_frozen() {
    // Pine-inherited floors, pinned millisecond-for-millisecond to the
    // shipped library: any widening fails the milestone, not the data.
    assert_eq!(VOLUME_EPSILON, 0.0001);
    assert_eq!(DRIFT_TOLERANCE_PCT, 0.1);
    assert_eq!(NEUTRAL_BUY_RATIO, 0.5);
    assert_eq!(BarsConfig::DEFAULT_SPIKE_THRESHOLD, 2.0);
    assert_eq!(BarsConfig::DEFAULT_DRIFT_THRESHOLD, 4.0);
    let fresh = test_config();
    assert_eq!(fresh.spike_threshold, BarsConfig::DEFAULT_SPIKE_THRESHOLD);
    assert_eq!(fresh.drift_threshold, BarsConfig::DEFAULT_DRIFT_THRESHOLD);
    assert!(!fresh.fill_gaps, "sparse stays the locked default");
}

#[test]
fn drift_report_separates_measurement_from_decision() {
    let ring = ring_with_volumes(&[400.0, 600.0]);
    let report = ring.drift_report(900.0);
    assert_close(
        report.measured_drift_pct.unwrap_or(f64::NAN),
        100.0 * 100.0 / 900.0,
        "measured drift",
    );
    assert_eq!(report.threshold_pct, BarsConfig::DEFAULT_DRIFT_THRESHOLD);
    assert_eq!(report.decision, DriftDecision::Pull);
    assert!(report.pull_needed());
    assert!(!report.healthy());
    // Same measurement, looser verdict: the decision derives from the
    // threshold, the measurement stands on its own.
    let loose = BarsConfig {
        drift_threshold: 20.0,
        ..test_config()
    };
    let loose_ring = ring_with_volumes_in(&loose, &[400.0, 600.0]);
    let loose_report = loose_ring.drift_report(900.0);
    assert_close(
        loose_report.measured_drift_pct.unwrap_or(f64::NAN),
        report.measured_drift_pct.unwrap_or(f64::NAN),
        "measurement ignores the threshold",
    );
    assert_eq!(loose_report.threshold_pct, 20.0);
    assert_eq!(loose_report.decision, DriftDecision::Accept);
    assert!(!loose_report.pull_needed());
    // Aligned ring: zero measured, accept verdict, healthy.
    let aligned = ring_with_volumes(&[400.0, 600.0]);
    let aligned_report = aligned.drift_report(1000.0);
    assert_eq!(aligned_report.measured_drift_pct, Some(0.0));
    assert_eq!(aligned_report.decision, DriftDecision::Accept);
    assert!(aligned_report.healthy());
}

#[test]
fn pull_must_fire_set_precision_one() {
    // (ring volumes, reference, hand-computed drift): every row trips the
    // Pine `onConfirm` pull branch (`drift > 4.0`).
    let cases: Vec<(Vec<f64>, f64, f64)> = vec![
        (vec![400.0, 600.0], 900.0, 100.0 * 100.0 / 900.0),
        (vec![400.0, 600.0], 1100.0, 100.0 * 100.0 / 1100.0),
        (vec![400.0, 600.0], 950.0, 100.0 * 50.0 / 950.0),
        (vec![400.0, 600.0], 500.0, 100.0),
        (vec![400.0, 600.0], 2000.0, 50.0),
    ];
    let mut fired = Vec::new();
    for (volumes, reference, want_drift) in &cases {
        let mut ring = ring_with_volumes(volumes);
        let report = ring.drift_report(*reference);
        assert_close(
            report.measured_drift_pct.unwrap_or(f64::NAN),
            *want_drift,
            "hand drift",
        );
        fired.push(report.decision == DriftDecision::Pull);
        let confirm = ring.confirm(&ConfirmedBar {
            bar_open_ms: 0,
            o: 100.0,
            h: 101.0,
            l: 99.0,
            c: 100.5,
            volume: *reference,
        });
        assert!(confirm.confirmed, "first confirm lands");
        assert!(confirm.pull_needed, "drifted confirm flags the pull");
        assert!(!confirm.healthy, "drifted confirm reads unhealthy");
    }
    let (precision, _) = precision_recall(&fired, &vec![true; cases.len()]);
    assert_eq!(precision, 1.0, "must-fire set fires in full");
}

#[test]
fn pull_must_not_fire_controls_zero_false_fires() {
    // Aligned rings, zero-volume gaps in sparse mode, sub-epsilon
    // remainders, the exact-threshold knife-edge, and non-finite input:
    // the Pine pull branch stays quiet on every row.
    let mut false_fires = 0;
    // Aligned ring: zero drift, remainder path, healthy.
    let mut aligned = ring_with_volumes(&[400.0, 600.0]);
    let report = aligned.drift_report(1000.0);
    if report.pull_needed() {
        false_fires += 1;
    }
    let confirm = aligned.confirm(&ConfirmedBar {
        bar_open_ms: 0,
        o: 100.0,
        h: 101.0,
        l: 99.0,
        c: 100.5,
        volume: 1000.0,
    });
    assert_eq!(confirm.remainder, None, "exact reference tops up nothing");
    if confirm.pull_needed {
        false_fires += 1;
    }
    assert!(confirm.healthy, "aligned confirm reads healthy");
    // Zero-volume gap in sparse mode: empty ring against a zero reference
    // measures zero drift (the Pine `expected > 0` guard), never a pull.
    let empty = BarRing::new(&test_config()).expect("config valid");
    if empty.drift_report(0.0).pull_needed() {
        false_fires += 1;
    }
    // Reference overshoots by less than epsilon: no pull, no remainder.
    let mut dust = ring_with_volumes(&[400.0, 600.0]);
    let dust_confirm = dust.confirm(&ConfirmedBar {
        bar_open_ms: 0,
        o: 100.0,
        h: 101.0,
        l: 99.0,
        c: 100.5,
        volume: 1000.00005,
    });
    if dust_confirm.pull_needed || dust_confirm.remainder.is_some() {
        false_fires += 1;
    }
    // Exactly at the threshold: Pine `>` is strict, so 4.0 takes the
    // remainder path, never the pull — yet Pine `<` is strict too, so the
    // boundary reads unhealthy (suspicious, below the pull line).
    let mut edge = ring_with_volumes(&[440.0, 600.0]);
    if edge.drift_report(1000.0).pull_needed() {
        false_fires += 1;
    }
    let edge_confirm = edge.confirm(&ConfirmedBar {
        bar_open_ms: 0,
        o: 100.0,
        h: 101.0,
        l: 99.0,
        c: 100.5,
        volume: 1000.0,
    });
    if edge_confirm.pull_needed {
        false_fires += 1;
    }
    assert!(!edge_confirm.healthy, "exact threshold heals nothing");
    // Zero reference with volume aboard: Pine guards to 0.0, never pulls.
    let quirk = ring_with_volumes(&[60.0, 40.0]);
    let quirk_report = quirk.drift_report(0.0);
    assert_eq!(
        quirk_report.measured_drift_pct,
        Some(0.0),
        "Pine zero-guard"
    );
    if quirk_report.pull_needed() {
        false_fires += 1;
    }
    // Non-finite reference: unmeasurable, so no pull — but unhealthy, the
    // same way Pine `na` drift refuses both branches' confidence.
    let mut unmeasurable = ring_with_volumes(&[400.0, 600.0]);
    let nan_report = unmeasurable.drift_report(f64::NAN);
    assert_eq!(nan_report.measured_drift_pct, None);
    if nan_report.pull_needed() {
        false_fires += 1;
    }
    assert!(!nan_report.healthy(), "unmeasurable reads unhealthy");
    let nan_confirm = unmeasurable.confirm(&ConfirmedBar {
        bar_open_ms: 0,
        o: 100.0,
        h: 101.0,
        l: 99.0,
        c: 100.5,
        volume: f64::NAN,
    });
    if nan_confirm.pull_needed {
        false_fires += 1;
    }
    assert!(!nan_confirm.healthy, "unmeasurable confirm reads unhealthy");
    assert_eq!(false_fires, 0, "control set stays fully quiet");
}

#[test]
fn spike_precision_and_recall_are_one() {
    // Hand labels from the Pine rule: spike iff newest/expected exceeds
    // `1 + spike_threshold` strictly (3.0 at the frozen default).
    // Expected newest volume is `htf_ref * ltf/htf`; the table pins
    // `expected = 100` via `htf_ref = 144_000`.
    let htf_ref = 144_000.0;
    let cases: Vec<(f64, f64, bool)> = vec![
        (400.0, htf_ref, true),
        (301.0, htf_ref, true),
        (300.0, htf_ref, false),
        (100.0, htf_ref, false),
        (50.0, htf_ref, false),
        (400.0, 0.0, false),
        (400.0, -5.0, false),
        (400.0, f64::NAN, false),
    ];
    let mut predicted = Vec::new();
    let mut expected = Vec::new();
    for (newest, reference, label) in &cases {
        let ring = ring_with_volumes(&[200.0, *newest]);
        predicted.push(ring.is_spike(*reference));
        expected.push(*label);
    }
    // Empty ring never spikes, whatever the reference.
    let empty = BarRing::new(&test_config()).expect("config valid");
    predicted.push(empty.is_spike(htf_ref));
    expected.push(false);
    let (precision, recall) = precision_recall(&predicted, &expected);
    assert_eq!(precision, 1.0, "no false spike flags");
    assert_eq!(recall, 1.0, "no missed spike flags");
}

#[test]
fn gap_sparse_dense_matrix() {
    // Pine doc example at LTF=1m: a trade at :00, silence, a trade at :05 —
    // four skipped buckets. Sparse stores only the traded buckets; dense
    // materializes exact `O=H=L=C=last_close, v=0` placeholders.
    for fill_gaps in [false, true] {
        let config = BarsConfig {
            fill_gaps,
            ..test_config()
        };
        let mut ring = BarRing::new(&config).expect("config valid");
        ring.on_bar_open(100.0, 100.0, 100.0, 100.0, T0);
        let legs = |price: f64, cum: f64| BarTick {
            open: 100.0,
            high: 100.0,
            low: 100.0,
            close: price,
            volume: cum,
        };
        ring.ingest_tick(legs(100.0, 0.0), T0 + 5_000);
        ring.ingest_tick(legs(100.0, 50.0), T0 + 10_000);
        ring.ingest_tick(legs(101.0, 80.0), T0 + 305_000);
        let outcome = ring.confirm_ltf(T0 + 305_000, 101.0, None);
        assert_eq!(outcome.boundaries_crossed, 5, "five buckets crossed");
        assert_eq!(outcome.gaps_added, 4, "four skipped buckets");
        assert_eq!(ring.gap_count(), 4, "gap counter matches");
        if fill_gaps {
            assert_eq!(outcome.finalized.len(), 5, "trade plus four placeholders");
            for (at, candle) in outcome.finalized.iter().skip(1).enumerate() {
                assert_eq!(
                    candle.ts_ms,
                    T0 + (at as i64 + 1) * LTF,
                    "grid-aligned placeholder"
                );
                assert_eq!(candle.o, 101.0, "placeholder opens at last close");
                assert_eq!(candle.h, 101.0, "placeholder high equals last close");
                assert_eq!(candle.l, 101.0, "placeholder low equals last close");
                assert_eq!(candle.c, 101.0, "placeholder closes at last close");
                assert_eq!(candle.volume, 0.0, "placeholder carries no volume");
                assert_eq!(candle.buy_volume, 0.0, "placeholder carries no buy flow");
            }
            assert_eq!(ring.len(), 5, "dense ring holds the placeholders");
        } else {
            assert_eq!(outcome.finalized.len(), 1, "sparse stores the trade only");
            assert_eq!(ring.len(), 1, "sparse ring holds no placeholders");
            assert!(
                ring.iter().all(|candle| candle.volume > 0.0),
                "no zero-volume rows"
            );
        }
    }
}

#[test]
fn rollback_and_fence_control_group() {
    // Producer-confidence control: a rollback resets tracking, and the next
    // aligned confirm still accepts (confidence intact, no spurious pull);
    // the single-confirmation fence refuses repeats without side effects.
    let mut ring = BarRing::new(&test_config()).expect("config valid");
    ring.on_bar_open(100.0, 100.0, 100.0, 100.0, T0);
    let legs = |price: f64, high: f64, low: f64, cum: f64| BarTick {
        open: 100.0,
        high,
        low,
        close: price,
        volume: cum,
    };
    ring.ingest_tick(legs(100.0, 100.0, 100.0, 0.0), T0 + 5_000);
    ring.ingest_tick(legs(101.0, 101.0, 100.0, 60.0), T0 + 10_000);
    ring.ingest_tick(legs(100.5, 101.0, 100.0, 20.0), T0 + 15_000);
    assert_eq!(ring.rollback_count(), 1, "volume decrease rolls back");
    let bar = ConfirmedBar {
        bar_open_ms: T0,
        o: 100.0,
        h: 101.0,
        l: 100.0,
        c: 100.5,
        volume: 0.0,
    };
    let first = ring.confirm(&bar);
    assert!(first.confirmed, "post-rollback confirm lands");
    assert!(!first.pull_needed, "zero-reference guard stays quiet");
    assert_eq!(ring.pull_count(), 0, "no pull served");
    let second = ring.confirm(&bar);
    assert!(!second.confirmed, "fence refuses the repeat");
    assert!(!second.pull_needed, "repeat flags nothing");
    assert_eq!(ring.pull_count(), 0, "repeat has no side effects");
    ring.validate().expect("ring stays coherent");
}

#[test]
fn drift_reports_never_cache() {
    // The verdict is recomputed on every call: repeated reports agree and
    // leave every counter, total, and validation verdict untouched.
    let ring = ring_with_volumes(&[400.0, 600.0]);
    let first = ring.drift_report(900.0);
    let second = ring.drift_report(900.0);
    let third = ring.drift_report(950.0);
    assert_eq!(first, second, "same input repeats identically");
    assert_eq!(third.decision, DriftDecision::Pull, "new input re-derives");
    assert_close(
        third.measured_drift_pct.unwrap_or(f64::NAN),
        100.0 * 50.0 / 950.0,
        "fresh measurement",
    );
    assert_eq!(ring.pull_count(), 1, "reports serve no pulls");
    assert_eq!(ring.fetch_check_count(), 0, "reports run no fetch checks");
    assert_close(ring.total_volume(), 1000.0, "totals untouched");
    ring.validate().expect("ring stays coherent");
}
