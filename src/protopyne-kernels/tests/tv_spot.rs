//! TradingView spot check: `sma` / `ema` / `atr` against an independent
//! reference over the vendored SPY trims.
//!
//! Provenance (fill the export row when a human performs it):
//!
//! - Bars: `tests/data/SPY_{1M,5M,15M}.csv` (436/346/116 rows), the same
//!   vendored trims the freeze goldens run on. Parse is positional
//!   (`time,open,high,low,close,Volume`), mirroring the golden reader.
//! - Reference, leg 1 (spot vectors): hand-computed from the documented
//!   recurrence shapes (SMA seed for EMA/Wilder, `k = 2/(n+1)`, Wilder
//!   `(x + prev*(n-1))/n`, true range with `high - low` on the first bar),
//!   then verified value-by-value against an independent textbook
//!   implementation (scratch, never shipped). Frozen 2026-10-13: kernels
//!   agreed with the independent reference at frozen eps on every value,
//!   so no divergence row was opened.
//! - Reference, leg 2 (full trims): the textbook recurrences re-implemented
//!   inline in this file (`naive_*`), deliberately written without the
//!   kernel math helpers so a shared-helper bug cannot hide on both sides.
//!   First-3/last-3 pins freeze the textbook side too: editing the naive
//!   code to match changed kernels trips the pins. Frozen 2026-10-13
//!   alongside the spot vectors.
//! - TradingView export: PENDING operator action. Nothing here is
//!   TradingView output; do not cite these rows as externally verified.
//!   The export (SMA/EMA/ATR over the same trims, with export date, symbol,
//!   session, and tolerance rationale) closes the pending row in the
//!   divergence log when it lands.
//!
//! Tolerance rationale: both sides implement the same closed-form `f64`
//! recurrence, so agreement tighter than `1e-9` is expected and anything
//! wider signals a formula or seeding difference — never rounding. `eps`
//! stays frozen: divergences become divergence-log rows, never wider eps.
//! `na` policy mirrors the kernels: `None` while `i + 1 < period`, and every
//! recorded value is finite (`fin_or_none` would have read `None`).

use super::*;
use crate::{ema::EmaIndicator, sma::SmaIndicator, volatility::AtrIndicator};
use std::path::PathBuf;

const EPS: f64 = 1e-9;

/// Test-local trim reader: one parameterized copy of the golden reader's
/// positional parse (the production CSV loader lives downstream in server).
fn load_trim(name: &str) -> OwnedOhlcv {
    let path: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join(name);
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_path(path)
        .expect("vendored trim present");
    let mut data = OwnedOhlcv::default();
    for record in reader.records() {
        let row = record.expect("readable row");
        let num = |i: usize| {
            row.get(i)
                .expect("column present")
                .parse::<f64>()
                .expect("parseable number")
        };
        data.open.push(num(1));
        data.high.push(num(2));
        data.low.push(num(3));
        data.close.push(num(4));
        data.volume.push(num(5));
    }
    data
}

/// Spot closes: ramp up, ramp down, flat, jumps — 24 bars so period 5 leaves
/// 20 defined values exercising seeds, slides, gap-driven true ranges, and a
/// flat stretch reading exactly the bar spread.
const SPOT_CLOSE: [f64; 24] = [
    100.0, 101.0, 102.0, 103.0, 104.0, //
    103.0, 102.0, 101.0, 100.0, 99.0, //
    99.0, 99.0, 99.0, 99.0, 99.0, //
    105.0, 104.0, 106.0, 103.0, 107.0, //
    110.0, 108.0, 112.0, 109.0,
];

/// Spot legs: constant 1.0 spread (`high = close + 0.5`, `low = close -
/// 0.5`), so flat bars read true range 1.0 and jumps read gap-driven.
fn spot_ohlcv() -> OwnedOhlcv {
    let mut data = OwnedOhlcv::default();
    for &c in &SPOT_CLOSE {
        data.open.push(c);
        data.high.push(c + 0.5);
        data.low.push(c - 0.5);
        data.close.push(c);
        data.volume.push(1_000.0);
    }
    data
}

// Frozen reference vectors (record-actuals pass 2026-10-13): hand-computed
// from the documented recurrences (seeds and first steps checked by hand —
// SMA5/EMA5 seed 102.0, EMA5[5] 307/3 in `f64` op order, ATR5 seed 1.4,
// flat-bar true range 1.0, jump-bar true range 6.5), then verified
// value-by-value against the independent textbook implementation. Full
// 24-slot vectors, `None` over the 4-bar warmup.
const SPOT_SMA5: [Option<f64>; 24] = [
    None,
    None,
    None,
    None,
    Some(102.0),
    Some(102.6),
    Some(102.8),
    Some(102.6),
    Some(102.0),
    Some(101.0),
    Some(100.2),
    Some(99.6),
    Some(99.2),
    Some(99.0),
    Some(99.0),
    Some(100.2),
    Some(101.2),
    Some(102.6),
    Some(103.4),
    Some(105.0),
    Some(106.0),
    Some(106.8),
    Some(108.0),
    Some(109.2),
];
const SPOT_EMA5: [Option<f64>; 24] = [
    None,
    None,
    None,
    None,
    Some(102.0),
    Some(102.33333333333334),
    Some(102.22222222222224),
    Some(101.81481481481484),
    Some(101.2098765432099),
    Some(100.47325102880662),
    Some(99.98216735253776),
    Some(99.65477823502518),
    Some(99.43651882335013),
    Some(99.2910125489001),
    Some(99.19400836593341),
    Some(101.12933891062228),
    Some(102.08622594041486),
    Some(103.39081729360991),
    Some(103.26054486240662),
    Some(104.50702990827108),
    Some(106.3380199388474),
    Some(106.89201329256494),
    Some(108.59467552837663),
    Some(108.72978368558442),
];
const SPOT_ATR5: [Option<f64>; 24] = [
    None,
    None,
    None,
    None,
    Some(1.4),
    Some(1.42),
    Some(1.436),
    Some(1.4487999999999999),
    Some(1.45904),
    Some(1.4672319999999999),
    Some(1.3737856),
    Some(1.29902848),
    Some(1.239222784),
    Some(1.1913782272),
    Some(1.15310258176),
    Some(2.2224820654079998),
    Some(2.0779856523264),
    Some(2.16238852186112),
    Some(2.429910817488896),
    Some(2.843928653991117),
    Some(2.9751429231928936),
    Some(2.880114338554315),
    Some(3.2040914708434522),
    Some(3.263273176674762),
];

/// Independent textbook SMA: direct fold without the kernel math helpers.
fn naive_sma(values: &[f64], period: usize) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            if i + 1 < period {
                return None;
            }
            let window = &values[i + 1 - period..=i];
            let mut sum = 0.0;
            for v in window {
                sum += v;
            }
            Some(sum / period as f64)
        })
        .collect()
}

/// Independent textbook EMA: SMA seed, then `close*k + prev*(1-k)`.
fn naive_ema(values: &[f64], period: usize) -> Vec<Option<f64>> {
    let factor = 2.0 / (period as f64 + 1.0);
    let mut prev: Option<f64> = None;
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            if i + 1 < period {
                return None;
            }
            let next = if let Some(p) = prev {
                values[i] * factor + p * (1.0 - factor)
            } else {
                let window = &values[i + 1 - period..=i];
                let mut sum = 0.0;
                for v in window {
                    sum += v;
                }
                sum / period as f64
            };
            prev = Some(next);
            Some(next)
        })
        .collect()
}

/// Independent textbook true range: `high - low` on bar 0, Pine's
/// three-way max thereafter.
fn naive_tr(high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    high.iter()
        .enumerate()
        .map(|(i, &h)| {
            let range = h - low[i];
            if i == 0 {
                range
            } else {
                let prev = close[i - 1];
                range.max((h - prev).abs()).max((low[i] - prev).abs())
            }
        })
        .collect()
}

/// Independent textbook ATR: Wilder smoothing over the textbook ranges.
fn naive_atr(high: &[f64], low: &[f64], close: &[f64], period: usize) -> Vec<Option<f64>> {
    let ranges = naive_tr(high, low, close);
    let mut prev: Option<f64> = None;
    ranges
        .iter()
        .enumerate()
        .map(|(i, _)| {
            if i + 1 < period {
                return None;
            }
            let next = if let Some(p) = prev {
                (ranges[i] + p * (period as f64 - 1.0)) / period as f64
            } else {
                let window = &ranges[i + 1 - period..=i];
                let mut sum = 0.0;
                for v in window {
                    sum += v;
                }
                sum / period as f64
            };
            prev = Some(next);
            Some(next)
        })
        .collect()
}

fn assert_series_match(got: &[Option<f64>], want: &[Option<f64>], what: &str) {
    assert_eq!(got.len(), want.len(), "length drift in {what}");
    for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
        match (g, w) {
            (None, None) => {}
            (Some(g), Some(w)) => {
                assert!((g - w).abs() <= EPS, "{what}[{i}]: {g} vs {w} (eps {EPS})");
            }
            _ => panic!("{what}[{i}]: na drift: {g:?} vs {w:?}"),
        }
    }
}

fn params(length: i64) -> ParamSet {
    ParamSet::new().with_int("length", length)
}

#[test]
fn spot_sma5_matches_reference() {
    let data = spot_ohlcv();
    let result = SmaIndicator::new()
        .batch(&data.views(), &params(5))
        .expect("valid params");
    assert_series_match(&result.values, &SPOT_SMA5, "spot sma(5)");
}

#[test]
fn spot_ema5_matches_reference() {
    let data = spot_ohlcv();
    let result = EmaIndicator::new()
        .batch(&data.views(), &params(5))
        .expect("valid params");
    assert_series_match(&result.values, &SPOT_EMA5, "spot ema(5)");
}

#[test]
fn spot_atr5_matches_reference() {
    let data = spot_ohlcv();
    let result = AtrIndicator::new()
        .batch(&data.views(), &params(5))
        .expect("valid params");
    assert_series_match(&result.values, &SPOT_ATR5, "spot atr(5)");
}

// Frozen pins (record-actuals pass 2026-10-13): first-3/last-3 defined
// values per (trim, kernel) at period 14, pinning the textbook side.
const PIN_TRIM_ROWS: [(&str, usize); 3] = [
    ("SPY_1M.csv", 436),
    ("SPY_5M.csv", 346),
    ("SPY_15M.csv", 116),
];
const PIN_FIRST3: [[f64; 3]; 9] = [
    [692.7728571428571, 692.8721428571428, 692.9621428571428],
    [692.7728571428571, 692.8631428571429, 692.9387238095238],
    [0.2996428571428688, 0.2932397959183727, 0.2901512390670603],
    [688.3248571428572, 688.3755714285714, 688.4355714285714],
    [688.3248571428572, 688.3802095238096, 688.453514920635],
    [0.23292857142856097, 0.237005102040804, 0.2536475947521771],
    [688.7449285714285, 688.8514285714285, 688.9428571428572],
    [688.7449285714285, 688.8722714285715, 689.0013019047619],
    [0.7786428571428375, 0.8115969387754927, 0.855054300291526],
];
const PIN_LAST3: [[f64; 3]; 9] = [
    [694.2707142857143, 694.2864285714286, 694.2978571428572],
    [694.3054792048065, 694.318081977499, 694.3290043804992],
    [
        0.04914480342961005,
        0.047063031756065174,
        0.046558529487780324,
    ],
    [694.3407142857143, 694.3407142857143, 694.3314285714285],
    [694.289095040876, 694.3012157020926, 694.3143869418136],
    [
        0.09300705676599108,
        0.08636369556842029,
        0.08448057445639448,
    ],
    [694.1192857142858, 694.1492857142857, 694.1864285714286],
    [694.2177007295528, 694.2393406322792, 694.2607618813087],
    [0.34628041773803353, 0.33226038789960094, 0.312813217335348],
];

/// Full trims: kernels agree with the inline textbook recurrences at frozen
/// eps, warmup prefixes stay `None`, every recorded value stays finite, and
/// the first-3/last-3 pins freeze the textbook side against silent edits.
#[test]
fn trim_series_agree_with_textbook() {
    let kernels: [(&str, ParamSet); 3] = [
        ("sma", params(14)),
        ("ema", params(14)),
        ("atr", params(14)),
    ];
    for (series, (name, rows)) in PIN_TRIM_ROWS.iter().enumerate() {
        let data = load_trim(name);
        assert_eq!(data.close.len(), *rows, "row-count pin for {name}");
        let views = data.views();
        let textbook = [
            naive_sma(&data.close, 14),
            naive_ema(&data.close, 14),
            naive_atr(&data.high, &data.low, &data.close, 14),
        ];
        for (leg, (id, ps)) in kernels.iter().enumerate() {
            let kernel = match *id {
                "sma" => SmaIndicator::new().batch(&views, ps).expect("valid"),
                "ema" => EmaIndicator::new().batch(&views, ps).expect("valid"),
                _ => AtrIndicator::new().batch(&views, ps).expect("valid"),
            };
            let what = format!("{name}:{id}(14)");
            assert_series_match(&kernel.values, &textbook[leg], &what);
            // Warmup honesty: exactly the 13-bar prefix is `None`.
            assert!(
                kernel.values[..13].iter().all(Option::is_none),
                "warmup None prefix drift in {what}"
            );
            assert!(
                kernel.values.iter().flatten().all(|v| v.is_finite()),
                "non-finite value inside Some in {what}"
            );
            // Frozen pins: first-3/last-3 defined values (indexes 13..16 and
            // the tail), so the textbook side cannot drift silently.
            let pin = series * 3 + leg;
            for (k, &want) in PIN_FIRST3[pin].iter().enumerate() {
                let got = kernel.values[13 + k].expect("defined past warmup");
                assert!(
                    (got - want).abs() <= EPS,
                    "first pin drift in {what}[{}]: {got} vs {want}",
                    13 + k
                );
            }
            let tail = kernel.values.len() - 3;
            for (k, &want) in PIN_LAST3[pin].iter().enumerate() {
                let got = kernel.values[tail + k].expect("defined tail");
                assert!(
                    (got - want).abs() <= EPS,
                    "last pin drift in {what}[{}]: {got} vs {want}",
                    tail + k
                );
            }
        }
    }
}
