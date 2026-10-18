//! Delta calibration gate: earn trust for every buy-volume number before
//! any of them may feed strategy conviction, caching, or persistence.
//!
//! Two tables, matching the confidence/correctness split:
//!
//! * Producer-confidence (controls): flat auction prints, dust-range
//!   candles, single-print buckets, and NaN-only buckets must never produce
//!   a directional split. Quiet rate 1.0 or the gate is red.
//! * Task-value (measured accuracy): `TickSigned` and `ShapeFallback` mean
//!   absolute share error against trade-signed references, each under its
//!   frozen bound.
//!
//! Reference provenance (read before citing a number): the recorded tick
//! windows under `tests/data/ticks/` carry no aggressor side, so accuracy
//! references are *constructed* synthetic tapes whose per-print truth is
//! known by construction (closed-form markets below, no fixture file to
//! drift). Recorded tapes anchor the structural properties only (clamp,
//! labels). A real trade-signed feed would tighten this; until one exists
//! the bounds below are the trust ceiling, per the honest-backtest rule.
//!
//! Coverage-to-confidence table (authority: [`DeltaConfidence`] docs; this
//! suite gates that no other mapping leaks in):
//!
//! | Coverage                              | Method          | Confidence |
//! |---------------------------------------|-----------------|------------|
//! | Directed prints, 2 or more in bucket  | `TickSigned`    | `High`     |
//! | One lone directed print               | `TickSigned`    | `Reduced`  |
//! | Flat/NaN prints, shape, top-up, gaps  | either          | `Low`      |
//!
//! Measured-error table (bounds a priori, set before measuring; measured
//! values pinned exactly so any estimator change trips the build):
//!
//! | Method            | Measured share MAE | Frozen bound | Verdict |
//! |-------------------|--------------------|--------------|---------|
//! | `TickSigned`      | 0.06               | 0.10         | Green   |
//! | `ShapeFallback`   | 0.078888888889     | 0.15         | Green   |
//!
//! Measured 2026-10-12 over the closed-form markets below (5 signed buckets
//! with two crossed prints each in buckets 2-3; 6 shape candles including
//! the adversarial doji). The tick value is exactly (0+0+0.1+0.2+0)/5 by
//! construction; the shape value prices the doji blindness at 0.4 inside a
//! six-candle mean.
//!
//! Bound rationale: above 10pp mean bucket error a tick-signed split is
//! noise, not signal, for conviction use; above 15pp mean candle error a
//! shape split is unusable even as a weak prior. Bounds never widen to fit
//! data — a miss is a finding (estimator untrusted), not a retune.
//!
//! `#[allow(clippy::float_cmp)]` covers the exact decimal literals: control
//! splits asserted here are exact by construction (halves, whole takes,
//! clamped formula values), and measured MAE pins are deterministic
//! closed-form arithmetic.
#![allow(clippy::float_cmp)]

use super::*;
use std::path::PathBuf;

const T0: i64 = 1_699_999_980_000;
const MIN: i64 = 60_000;

/// A priori trust ceiling for tick-signed bucket splits: mean absolute buy-
/// share error over the signed market. Set before measuring (see rationale
/// above); never widened without a milestone note.
const TICK_SHARE_MAE_BOUND: f64 = 0.10;
/// A priori trust ceiling for shape-estimated candle splits: mean absolute
/// buy-share error over the shape market, adversarial doji included.
const SHAPE_SHARE_MAE_BOUND: f64 = 0.15;

/// Tight pin tolerance for deterministic closed-form measurements: any
/// estimator or fold change trips the build, like the frozen goldens.
const PIN_EPS: f64 = 1e-9;

fn test_config() -> BarsConfig {
    BarsConfig::new(MIN, MIN, 5 * MIN, 86_400_000)
}

fn fresh_ring() -> BarRing {
    BarRing::new(&test_config()).expect("config validates")
}

fn assert_close(got: f64, want: f64, what: &str) {
    assert!(
        (got - want).abs() <= PIN_EPS * want.abs().max(1.0),
        "{what}: got {got}, want {want}"
    );
}

fn mean_abs_err(errs: &[f64]) -> f64 {
    errs.iter().sum::<f64>() / errs.len() as f64
}

// ---------------------------------------------------------------------------
// Calibration report (dashboard pattern: quiet rate + measured gates)
// ---------------------------------------------------------------------------

/// Delta trust report: control-quiet plus both measured gates. Green is 1.0
/// quiet with both MAEs inside their frozen bounds, mirroring the parity
/// dashboard shape (accept 1.0 plus false-fire 0.0) one module over.
#[derive(Debug, Clone, Copy, PartialEq)]
struct DeltaReport {
    /// No directional split on any directionless control input.
    controls_quiet: bool,
    /// Measured `TickSigned` mean share error.
    tick_mae: f64,
    /// Measured `ShapeFallback` mean share error.
    shape_mae: f64,
}

impl DeltaReport {
    /// Control-quiet rate: 1.0 when every control held, else 0.0.
    fn quiet_rate(&self) -> f64 {
        if self.controls_quiet {
            1.0
        } else {
            0.0
        }
    }

    /// Whether delta output may be trusted: every control quiet and both
    /// measured errors inside their frozen bounds.
    fn is_green(&self) -> bool {
        self.controls_quiet
            && self.tick_mae <= TICK_SHARE_MAE_BOUND
            && self.shape_mae <= SHAPE_SHARE_MAE_BOUND
    }
}

// ---------------------------------------------------------------------------
// Synthetic signed market: per-print truth known by construction
// ---------------------------------------------------------------------------

/// One signed print: price, own volume, and the aggressor side known by
/// construction (the trade-signed reference the estimators are measured
/// against).
#[derive(Debug, Clone, Copy)]
struct SignedPrint {
    price: f64,
    volume: f64,
    is_buy: bool,
}

fn signed(ts_ms: i64, price: f64, volume: f64, is_buy: bool) -> (RawTick, SignedPrint) {
    (
        RawTick {
            ts_ms,
            price,
            volume,
        },
        SignedPrint {
            price,
            volume,
            is_buy,
        },
    )
}

/// True buy share of a signed tape slice.
fn true_share(prints: &[SignedPrint]) -> f64 {
    let total: f64 = prints.iter().map(|print| print.volume).sum();
    let buys: f64 = prints
        .iter()
        .filter(|print| print.is_buy)
        .map(|print| print.volume)
        .sum();
    buys / total
}

/// Closed-bucket row built from its tape slice (self-consistent OHLCV, so
/// the clamp never binds in the accuracy markets — estimator error is
/// isolated from disagreement handling, which the clamp test pins alone).
fn row_of(ts_ms: i64, prints: &[SignedPrint]) -> LtfRow {
    let mut high = f64::NEG_INFINITY;
    let mut low = f64::INFINITY;
    let mut volume = 0.0;
    for print in prints {
        high = high.max(print.price);
        low = low.min(print.price);
        volume += print.volume;
    }
    LtfRow {
        ts_ms,
        o: prints.first().expect("nonempty bucket").price,
        h: high,
        l: low,
        c: prints.last().expect("nonempty bucket").price,
        volume,
    }
}

/// The signed market: five minute buckets over one tape. Buckets 0, 1, 4
/// are clean trends (tick rule exact); buckets 2 and 3 carry known crossed
/// prints (buyer-initiated prints ticking down on price improvement and the
/// mirror), two misses each. A pre-roll seed print fixes the direction
/// tracker's predecessor so every scored print carries a real delta; the
/// seed's own bucket has no row and is never scored.
fn signed_market() -> (Vec<LtfRow>, Vec<RawTick>, Vec<f64>) {
    // (minute offset, price, volume, is_buy).
    let tape: Vec<(i64, f64, f64, bool)> = vec![
        // Pre-roll seed (bucket -1, unscored): establishes prev 100.0.
        (-30_000, 100.0, 10.0, true),
        // Bucket 0: clean climb, all buys, all upticks.
        (5_000, 100.1, 10.0, true),
        (15_000, 100.2, 10.0, true),
        (25_000, 100.3, 10.0, true),
        (35_000, 100.4, 10.0, true),
        (45_000, 100.5, 10.0, true),
        // Bucket 1: clean slide, all sells, all downticks.
        (65_000, 100.4, 10.0, false),
        (75_000, 100.3, 10.0, false),
        (85_000, 100.2, 10.0, false),
        (95_000, 100.1, 10.0, false),
        (105_000, 100.0, 10.0, false),
        // Bucket 2: truth 0.6, two crossed prints (buy ticking down,
        // sell ticking up). Ten prints on a 5s grid: the 45s span stays
        // inside the minute bucket (a 10s grid would spill 90s of prints
        // across the fence — caught by this test on the first run).
        (122_500, 100.1, 10.0, true),
        (127_500, 100.2, 10.0, true),
        (132_500, 100.1, 10.0, true),
        (137_500, 100.2, 10.0, true),
        (142_500, 100.3, 10.0, true),
        (147_500, 100.2, 10.0, true),
        (152_500, 100.3, 10.0, false),
        (157_500, 100.2, 10.0, false),
        (162_500, 100.1, 10.0, false),
        (167_500, 100.0, 10.0, false),
        // Bucket 3: truth 0.8, two crossed buys ticking down.
        (182_500, 100.1, 10.0, true),
        (187_500, 100.2, 10.0, true),
        (192_500, 100.1, 10.0, true),
        (197_500, 100.2, 10.0, true),
        (202_500, 100.3, 10.0, true),
        (207_500, 100.4, 10.0, true),
        (212_500, 100.3, 10.0, true),
        (217_500, 100.4, 10.0, true),
        (222_500, 100.3, 10.0, false),
        (227_500, 100.2, 10.0, false),
        // Bucket 4: clean climb again.
        (245_000, 100.3, 10.0, true),
        (255_000, 100.4, 10.0, true),
        (265_000, 100.5, 10.0, true),
        (275_000, 100.6, 10.0, true),
        (285_000, 100.7, 10.0, true),
    ];
    let mut ticks = Vec::with_capacity(tape.len());
    let mut signed_all = Vec::with_capacity(tape.len());
    for (offset, price, volume, is_buy) in tape {
        let (tick, print) = signed(T0 + offset, price, volume, is_buy);
        ticks.push(tick);
        signed_all.push(print);
    }
    // Slice per minute bucket; bucket -1 (the seed) is dropped.
    let mut rows = Vec::new();
    let mut truths = Vec::new();
    for (bucket, want_prints) in [5, 5, 10, 10, 5].into_iter().enumerate() {
        let bucket = bucket as i64;
        let start = T0 + bucket * MIN;
        let end = start + MIN;
        let slice: Vec<SignedPrint> = ticks
            .iter()
            .zip(signed_all.iter())
            .filter(|(tick, _)| tick.ts_ms >= start && tick.ts_ms < end)
            .map(|(_, print)| *print)
            .collect();
        assert_eq!(
            slice.len(),
            want_prints,
            "bucket {bucket} holds its prints (no fence spill)"
        );
        truths.push(true_share(&slice));
        rows.push(row_of(start, &slice));
    }
    (rows, ticks, truths)
}

/// Tick-signed mean share error over the signed market, plus the per-bucket
/// errors for hand-verification.
fn tick_signed_errors() -> Vec<f64> {
    let (rows, ticks, truths) = signed_market();
    let total: f64 = rows.iter().map(|row| row.volume).sum();
    let mut ring = fresh_ring();
    let outcome = ring.reconcile_with_ticks(&rows, &ticks, total, 100.0);
    assert_eq!(outcome.rebuilt, rows.len(), "market rebuilds whole");
    assert!(
        outcome.remainder.is_none(),
        "exact reference tops up nothing"
    );
    rows.iter()
        .zip(truths.iter())
        .enumerate()
        .map(|(index, (row, truth))| {
            let candle = ring.get(index).expect("rebuilt candle");
            assert_eq!(
                ring.buy_provenance(index).expect("every candle labeled").0,
                DeltaMethod::TickSigned,
                "bucket {index} covered by the tape"
            );
            (candle.buy_volume / row.volume - truth).abs()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Shape market: candles with known truth (adversarial doji included)
// ---------------------------------------------------------------------------

/// (row, true buy share): strong trends the formula should track, mild
/// trends with hand-computed ratios, and one directionless candle with
/// extreme truth — the case shape estimation is blind to, kept in the
/// market so the bound prices it instead of hiding it.
fn shape_market() -> Vec<(LtfRow, f64)> {
    vec![
        // Strong climb, all buys: 0.5 + 0.5 + 0.15 clamps to 0.98.
        (
            LtfRow {
                ts_ms: T0,
                o: 100.0,
                h: 102.0,
                l: 100.0,
                c: 102.0,
                volume: 100.0,
            },
            1.0,
        ),
        // Strong slide, all sells: 0.5 - 0.5 - 0.15 clamps to 0.02.
        (
            LtfRow {
                ts_ms: T0 + MIN,
                o: 102.0,
                h: 102.0,
                l: 100.0,
                c: 100.0,
                volume: 100.0,
            },
            0.0,
        ),
        // Doji, balanced truth: body 0, close mid-range reads 0.5.
        (
            LtfRow {
                ts_ms: T0 + 2 * MIN,
                o: 100.0,
                h: 101.0,
                l: 99.0,
                c: 100.0,
                volume: 100.0,
            },
            0.5,
        ),
        // Mild climb, truth 0.7: 0.5 + 1/6 + 0.05.
        (
            LtfRow {
                ts_ms: T0 + 3 * MIN,
                o: 100.0,
                h: 101.0,
                l: 99.5,
                c: 100.5,
                volume: 100.0,
            },
            0.7,
        ),
        // Mild slide, truth 0.3: 0.5 - 1/6 - 0.05.
        (
            LtfRow {
                ts_ms: T0 + 4 * MIN,
                o: 101.0,
                h: 102.0,
                l: 99.0,
                c: 100.0,
                volume: 100.0,
            },
            0.3,
        ),
        // Flat directionless candle, truth 0.9: shape reads 0.5 and errs
        // 0.4 — the documented blindness, priced into the bound.
        (
            LtfRow {
                ts_ms: T0 + 5 * MIN,
                o: 100.0,
                h: 100.5,
                l: 99.5,
                c: 100.0,
                volume: 100.0,
            },
            0.9,
        ),
    ]
}

fn shape_errors() -> Vec<f64> {
    shape_market()
        .iter()
        .map(|(row, truth)| {
            let split = estimate_delta_volume_estimated(row.o, row.h, row.l, row.c, row.volume);
            assert_eq!(
                split.method,
                DeltaMethod::ShapeFallback,
                "shape labels itself"
            );
            assert_eq!(
                split.confidence,
                DeltaConfidence::Low,
                "shape never claims more"
            );
            (split.buy_volume / row.volume - truth).abs()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Controls: directionless input must never produce a directional split
// ---------------------------------------------------------------------------

/// Flat auction prints across three buckets: every split neutral at low
/// confidence, zero directional volume anywhere.
#[test]
fn controls_flat_auction_prints_stay_neutral() {
    let rows = vec![
        LtfRow {
            ts_ms: T0,
            o: 100.0,
            h: 100.0,
            l: 100.0,
            c: 100.0,
            volume: 30.0,
        },
        LtfRow {
            ts_ms: T0 + MIN,
            o: 100.0,
            h: 100.0,
            l: 100.0,
            c: 100.0,
            volume: 30.0,
        },
        LtfRow {
            ts_ms: T0 + 2 * MIN,
            o: 100.0,
            h: 100.0,
            l: 100.0,
            c: 100.0,
            volume: 30.0,
        },
    ];
    let mut ticks = vec![RawTick {
        ts_ms: T0 - 30_000,
        price: 100.0,
        volume: 10.0,
    }];
    for bucket in 0..3 {
        for offset in [5_000, 10_000, 15_000] {
            ticks.push(RawTick {
                ts_ms: T0 + bucket * MIN + offset,
                price: 100.0,
                volume: 10.0,
            });
        }
    }
    let mut ring = fresh_ring();
    let outcome = ring.reconcile_with_ticks(&rows, &ticks, 90.0, 100.0);
    assert_eq!(outcome.rebuilt, 3);
    assert!(outcome.remainder.is_none());
    for index in 0..3 {
        let candle = ring.get(index).expect("rebuilt candle");
        assert_eq!(candle.buy_volume, 15.0, "bucket {index} splits neutral");
        assert_eq!(
            ring.buy_provenance(index),
            Some((DeltaMethod::TickSigned, DeltaConfidence::Low)),
            "bucket {index} carries no direction"
        );
    }
    // Print level: flat and NaN deltas split neutral, never directional.
    assert_eq!(estimate_buy_volume_estimated(10.0, 0.0).buy_volume, 5.0);
    assert_eq!(
        estimate_buy_volume_estimated(10.0, f64::NAN).buy_volume,
        5.0
    );
    assert_eq!(
        estimate_buy_volume_estimated(10.0, 0.0).confidence,
        DeltaConfidence::Low
    );
    assert_eq!(
        estimate_buy_volume_estimated(10.0, f64::NAN).confidence,
        DeltaConfidence::Low
    );
}

/// Dust-range and degenerate candles: the structural guard refuses them, so
/// every split is the neutral prior at low confidence.
#[test]
fn controls_dust_candles_stay_neutral() {
    // Guard truth table: valid candles pass, dust/degenerate fail.
    assert!(
        validate_ohlcv(100.0, 101.0, 99.0, 100.5, 50.0),
        "well-formed passes"
    );
    assert!(
        !validate_ohlcv(100.0, 100.0, 100.0, 100.0, 50.0),
        "flat range fails"
    );
    assert!(
        !validate_ohlcv(100.0, 100.0 + 1e-9, 100.0, 100.0, 50.0),
        "dust range fails"
    );
    assert!(
        !validate_ohlcv(f64::NAN, 102.0, 99.0, 101.0, 50.0),
        "NaN leg fails"
    );
    assert!(
        !validate_ohlcv(100.0, 101.0, 99.0, 100.5, 0.0),
        "empty volume fails"
    );
    let rows = vec![
        LtfRow {
            ts_ms: T0,
            o: 100.0,
            h: 100.0,
            l: 100.0,
            c: 100.0,
            volume: 40.0,
        },
        LtfRow {
            ts_ms: T0 + MIN,
            o: 100.0,
            h: 100.0 + 1e-9,
            l: 100.0,
            c: 100.0,
            volume: 60.0,
        },
    ];
    let mut ring = fresh_ring();
    ring.reconcile(&rows, 100.0, 100.0);
    for index in 0..2 {
        let candle = ring.get(index).expect("rebuilt candle");
        assert_eq!(
            candle.buy_volume,
            candle.volume * 0.5,
            "candle {index} neutral"
        );
        assert_eq!(
            ring.buy_provenance(index),
            Some((DeltaMethod::ShapeFallback, DeltaConfidence::Low)),
            "candle {index} claims nothing"
        );
    }
}

/// Single-print buckets: one lone print borrows its delta from a neighbor,
/// so confidence never reaches `High` — directed reads `Reduced`, flat
/// reads `Low` with a neutral split.
#[test]
fn controls_single_print_buckets_never_high() {
    let rows = vec![
        LtfRow {
            ts_ms: T0,
            o: 100.0,
            h: 101.0,
            l: 100.0,
            c: 101.0,
            volume: 10.0,
        },
        LtfRow {
            ts_ms: T0 + MIN,
            o: 101.0,
            h: 101.0,
            l: 101.0,
            c: 101.0,
            volume: 10.0,
        },
    ];
    let ticks = vec![
        RawTick {
            ts_ms: T0 - 30_000,
            price: 100.0,
            volume: 10.0,
        },
        RawTick {
            ts_ms: T0 + 5_000,
            price: 101.0,
            volume: 10.0,
        },
        RawTick {
            ts_ms: T0 + MIN + 5_000,
            price: 101.0,
            volume: 10.0,
        },
    ];
    let mut ring = fresh_ring();
    ring.reconcile_with_ticks(&rows, &ticks, 20.0, 100.0);
    assert_eq!(
        ring.buy_provenance(0),
        Some((DeltaMethod::TickSigned, DeltaConfidence::Reduced)),
        "lone directed print never reads High"
    );
    assert_eq!(
        ring.get(0).expect("candle").buy_volume,
        10.0,
        "uptick takes all"
    );
    assert_eq!(
        ring.buy_provenance(1),
        Some((DeltaMethod::TickSigned, DeltaConfidence::Low)),
        "lone flat print carries no direction"
    );
    assert_eq!(
        ring.get(1).expect("candle").buy_volume,
        5.0,
        "flat splits neutral"
    );
}

/// NaN prints are skipped by the fold without advancing the direction
/// tracker: a NaN-only bucket is uncovered (shape fallback), and the print
/// after the gap still signs against its last finite predecessor.
#[test]
fn controls_nan_prints_donate_no_direction() {
    let rows = vec![
        LtfRow {
            ts_ms: T0,
            o: 100.0,
            h: 101.0,
            l: 100.0,
            c: 100.5,
            volume: 10.0,
        },
        LtfRow {
            ts_ms: T0 + MIN,
            o: 100.5,
            h: 101.0,
            l: 100.0,
            c: 100.5,
            volume: 50.0,
        },
        LtfRow {
            ts_ms: T0 + 2 * MIN,
            o: 100.5,
            h: 100.0,
            l: 100.0,
            c: 100.0,
            volume: 10.0,
        },
    ];
    let ticks = vec![
        RawTick {
            ts_ms: T0 - 30_000,
            price: 100.0,
            volume: 10.0,
        },
        RawTick {
            ts_ms: T0 + 5_000,
            price: 100.5,
            volume: 10.0,
        },
        RawTick {
            ts_ms: T0 + MIN + 5_000,
            price: f64::NAN,
            volume: 25.0,
        },
        RawTick {
            ts_ms: T0 + MIN + 10_000,
            price: f64::NAN,
            volume: 25.0,
        },
        RawTick {
            ts_ms: T0 + 2 * MIN + 5_000,
            price: 100.0,
            volume: 10.0,
        },
    ];
    let mut ring = fresh_ring();
    ring.reconcile_with_ticks(&rows, &ticks, 70.0, 100.0);
    assert_eq!(
        ring.buy_provenance(0),
        Some((DeltaMethod::TickSigned, DeltaConfidence::Reduced)),
        "one finite print signs thin"
    );
    assert_eq!(
        ring.buy_provenance(1),
        Some((DeltaMethod::ShapeFallback, DeltaConfidence::Low)),
        "NaN-only bucket falls back to shape"
    );
    assert_eq!(
        ring.get(1).expect("candle").buy_volume,
        25.0,
        "shape-neutral split of 50"
    );
    // The post-gap print signs down against 100.5 (NaN never became the
    // predecessor): takes none.
    assert_eq!(ring.get(2).expect("candle").buy_volume, 0.0);
    assert_eq!(
        ring.buy_provenance(2),
        Some((DeltaMethod::TickSigned, DeltaConfidence::Reduced))
    );
}

// ---------------------------------------------------------------------------
// Measured gates: tick-signed and shape accuracy vs signed truth
// ---------------------------------------------------------------------------

/// Tick-signed splits measured against the signed market: clean buckets
/// exact, crossed buckets off by exactly their crossed share, mean inside
/// the frozen bound. The measured value is pinned so any fold or estimator
/// change trips the build.
#[test]
fn tick_signed_mae_within_frozen_bound() {
    let errors = tick_signed_errors();
    assert_eq!(errors.len(), 5, "five scored buckets");
    assert_close(errors[0], 0.0, "clean climb exact");
    assert_close(errors[1], 0.0, "clean slide exact");
    assert_close(errors[2], 0.1, "two crossed prints in ten err 0.1");
    assert_close(errors[3], 0.2, "two crossed buys in ten err 0.2");
    assert_close(errors[4], 0.0, "clean climb exact");
    let measured = mean_abs_err(&errors);
    // Frozen 2026-10-12: exactly (0+0+0.1+0.2+0)/5 by market construction.
    // Any fold, clamp, or estimator change trips this pin.
    assert_close(measured, 0.06, "tick-signed share MAE frozen");
    assert!(
        measured <= TICK_SHARE_MAE_BOUND,
        "tick-signed MAE {measured} inside frozen {TICK_SHARE_MAE_BOUND}"
    );
}

/// Shape splits measured against candles with known truth: trends track,
/// mild candles read near truth, the adversarial doji errs 0.4 — priced
/// into the bound, never hidden. Measured value pinned exactly.
#[test]
fn shape_fallback_mae_within_frozen_bound() {
    let errors = shape_errors();
    assert_eq!(errors.len(), 6, "six scored candles");
    assert_close(errors[0], 0.02, "clamped strong climb");
    assert_close(errors[1], 0.02, "clamped strong slide");
    assert_close(errors[2], 0.0, "balanced doji");
    assert_close(
        errors[3],
        0.5 + 1.0 / 6.0 + 0.05 - 0.7,
        "mild climb hand ratio",
    );
    assert_close(
        errors[4],
        0.3 - (0.5 - 1.0 / 6.0 - 0.05),
        "mild slide hand ratio",
    );
    assert_close(errors[5], 0.4, "adversarial doji blindness");
    let measured = mean_abs_err(&errors);
    // Frozen 2026-10-12: (0.02+0.02+0+0.0167+0.0167+0.4)/6 — the 0.4 is the
    // adversarial doji, priced in, never hidden. Any shape-formula change
    // trips this pin.
    assert_close(measured, 0.078_888_888_888_888_88, "shape share MAE frozen");
    assert!(
        measured <= SHAPE_SHARE_MAE_BOUND,
        "shape MAE {measured} inside frozen {SHAPE_SHARE_MAE_BOUND}"
    );
}

/// Print/candle disagreement: tick splits clamp to `[0, row volume]` so a
/// late or auction-heavy tape can never invent or erase shares.
#[test]
fn clamp_holds_under_print_candle_disagreement() {
    let rows = vec![
        LtfRow {
            ts_ms: T0,
            o: 100.0,
            h: 101.0,
            l: 100.0,
            c: 101.0,
            volume: 60.0,
        },
        LtfRow {
            ts_ms: T0 + MIN,
            o: 101.0,
            h: 101.0,
            l: 100.0,
            c: 100.0,
            volume: 200.0,
        },
    ];
    let ticks = vec![
        RawTick {
            ts_ms: T0 - 30_000,
            price: 100.0,
            volume: 10.0,
        },
        RawTick {
            ts_ms: T0 + 5_000,
            price: 100.5,
            volume: 50.0,
        },
        RawTick {
            ts_ms: T0 + 10_000,
            price: 101.0,
            volume: 50.0,
        },
        RawTick {
            ts_ms: T0 + MIN + 5_000,
            price: 100.5,
            volume: 20.0,
        },
        RawTick {
            ts_ms: T0 + MIN + 10_000,
            price: 100.0,
            volume: 20.0,
        },
    ];
    let mut ring = fresh_ring();
    ring.reconcile_with_ticks(&rows, &ticks, 260.0, 100.0);
    // Bucket 0 signs 100 buy against a 60-share row: clamped to the row.
    assert_eq!(ring.get(0).expect("candle").buy_volume, 60.0);
    // Bucket 1 signs none against a 200-share row: floor holds.
    assert_eq!(ring.get(1).expect("candle").buy_volume, 0.0);
    for index in 0..2 {
        let candle = ring.get(index).expect("candle");
        assert!(
            (0.0..=candle.volume).contains(&candle.buy_volume),
            "candle {index} split inside its volume"
        );
    }
}

// ---------------------------------------------------------------------------
// Recorded tapes: structural properties on real prints (no truth to score)
// ---------------------------------------------------------------------------

fn load_tick_window(name: &str) -> Vec<RawTick> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/ticks")
        .join(name);
    let content = std::fs::read_to_string(&path).expect("tick window loads");
    let mut lines = content.lines();
    assert_eq!(
        lines.next().expect("window header"),
        "timestamp_ms,price,volume,is_out_of_order,original_timestamp_ms",
        "window columns pinned"
    );
    lines
        .map(|line| {
            let mut cells = line.split(',');
            RawTick {
                ts_ms: cells.next().expect("stamp").parse().expect("int stamp"),
                price: cells.next().expect("price").parse().expect("price"),
                volume: cells.next().expect("volume").parse().expect("volume"),
            }
        })
        .collect()
}

/// Every recorded window reconciled through tick coverage: each stored
/// candle labeled, each split inside its row volume. Real tapes carry no
/// aggressor side, so this pins structure, never accuracy.
#[test]
fn recorded_windows_stay_clamped_and_labeled() {
    for name in [
        "window_quiet.csv",
        "window_sparse.csv",
        "window_pull.csv",
        "window_pull_remainder.csv",
        "window_out_of_order.csv",
    ] {
        let ticks = load_tick_window(name);
        assert!(!ticks.is_empty(), "{name} carries prints");
        // Bucket prints per minute; rows follow the tape's own OHLCV.
        let mut buckets: std::collections::BTreeMap<i64, Vec<&RawTick>> =
            std::collections::BTreeMap::new();
        for tick in &ticks {
            buckets
                .entry(tick.ts_ms.div_euclid(MIN))
                .or_default()
                .push(tick);
        }
        let rows: Vec<LtfRow> = buckets
            .values()
            .map(|prints| {
                let slice: Vec<SignedPrint> = prints
                    .iter()
                    .map(|tick| SignedPrint {
                        price: tick.price,
                        volume: tick.volume,
                        is_buy: true,
                    })
                    .collect();
                row_of(prints[0].ts_ms.div_euclid(MIN) * MIN, &slice)
            })
            .collect();
        let total: f64 = rows.iter().map(|row| row.volume).sum();
        let mut ring = fresh_ring();
        let outcome = ring.reconcile_with_ticks(&rows, &ticks, total, 100.0);
        assert!(
            outcome.remainder.is_none(),
            "{name}: exact reference tops up nothing"
        );
        assert_eq!(ring.len(), rows.len(), "{name}: every row lands");
        for index in 0..ring.len() {
            let candle = ring.get(index).expect("rebuilt candle");
            assert!(
                ring.buy_provenance(index).is_some(),
                "{name} candle {index} labeled"
            );
            assert!(
                (0.0..=candle.volume).contains(&candle.buy_volume),
                "{name} candle {index} split inside its volume"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Gate: the report assembles quiet + measured and reads green
// ---------------------------------------------------------------------------

/// The published verdict: controls quiet at 1.0 with both measured errors
/// inside their frozen bounds. Blocking use: until this is green, no delta
/// number may feed strategy conviction, caching, or persistence.
#[test]
fn delta_report_is_green() {
    // Controls hold exactly (proven by the control tests above; the report
    // re-checks the cheapest one so the verdict never assembles blind).
    let flat = estimate_buy_volume_estimated(10.0, 0.0);
    let controls_quiet = flat.buy_volume == 5.0 && flat.confidence == DeltaConfidence::Low;
    let report = DeltaReport {
        controls_quiet,
        tick_mae: mean_abs_err(&tick_signed_errors()),
        shape_mae: mean_abs_err(&shape_errors()),
    };
    assert_eq!(report.quiet_rate(), 1.0, "controls fully quiet");
    assert!(
        report.tick_mae <= TICK_SHARE_MAE_BOUND,
        "tick gate holds in the report"
    );
    assert!(
        report.shape_mae <= SHAPE_SHARE_MAE_BOUND,
        "shape gate holds in the report"
    );
    assert!(
        report.is_green(),
        "delta output may be trusted within its bounds"
    );
}
