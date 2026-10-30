//! Kernel-expression cross-checks for the task-indicator goldens: every
//! numeric indicator output that reduces to a kernel expression asserts
//! against an independent `polars::Expr` reference through
//! [`assert_expr_parity`](crate::parity::assert_expr_parity) at the frozen
//! kernel eps (`1e-9`, the `compat.rs` rows' eps — never widened). The
//! expressions are check-only (formulation informed by the `polars_ta`
//! Python reference, written in Rust over
//! [`frame_of_bars`](crate::research::frame_of_bars)); kernels stay the
//! single numeric authority.
//!
//! DVP runs no interpreted suite by human verdict (2026-10-12: prune every
//! PineScript DVP run, keep native Rust only), so its legs assert
//! kernel-against-reference instead: the registered kernel's `batch` over
//! the same bars must reproduce the reference (`trend_ma`, `vol_sma`,
//! `stddev`). The RSIMD engine-against-reference legs were retired with the
//! external removal of `indicators/_ApexRSIMD.pine` (see the pin-free ADR):
//! an interpreted run cannot be fixture-pinned without the script bytes,
//! so the engine-bridge half is covered by the synthetic closure runs in
//! the draw-capture suite instead. The kernels seed every EMA with the
//! first-window mean while
//! the reference `ewm_mean` seeds with the first value, so a geometric
//! seed transient (`(1 - alpha)^k`) rides the head of every real-history
//! run: seeded legs pin the steady-state tail on the longest (1-minute)
//! history at `1e-9` — the tail opens at each leg's computed
//! [`seed_horizon`] — plus a bounded seed envelope over the head, which
//! together prove the recurrence matches and only the seed differs.
//!
//! Engine-against-reference for interpreted legs is covered by the
//! synthetic closure runs in the draw-capture suite (no external script
//! bytes required). Wilder-seeded chains (`_atr`-style: the kernels seed
//! Wilder smoothings with the first-window mean while `ewm_mean` seeds
//! with the first value) admit no whole-series `1e-9` reference —
//! matching the reference there would be a regression, not a fix — so
//! reducible DVP legs pin seeded tails plus bounded head envelopes only.
//!
//! Test path note: this file rides the `pine::eval` unit module (closure
//! runs live in `eval.rs`), so the runnable filter is `pine::eval`, not
//! the file stem.

use crate::parity::{assert_expr_parity, expr_fixture};
use crate::{OwnedOhlcv, ParamSet};
use polars::prelude::*;
use protopyne_types::Bar;

use crate::pine::test_fixture as fixture;

/// Chart timeframes under proof: engine period plus the history file that
/// supplies the chart bars.
const CHARTS: &[(&str, &str)] = &[
    ("1", "historical/SPY_1M.csv"),
    ("5", "historical/SPY_5M.csv"),
    ("15", "historical/SPY_15M.csv"),
];

fn strict(n: usize) -> RollingOptionsFixedWindow {
    RollingOptionsFixedWindow {
        window_size: n,
        min_periods: n,
        weights: None,
        center: false,
        fn_params: None,
    }
}

fn ewm_span(n: usize) -> EWMOptions {
    EWMOptions {
        alpha: 2.0 / (n as f64 + 1.0),
        adjust: false,
        bias: false,
        min_periods: n,
        ignore_nulls: true,
    }
}

fn pop_opts(n: usize) -> RollingOptionsFixedWindow {
    RollingOptionsFixedWindow {
        window_size: n,
        min_periods: n,
        weights: None,
        center: false,
        fn_params: Some(RollingFnParams::Var(RollingVarParams { ddof: 0 })),
    }
}

fn sma_volume_expr(n: usize) -> Expr {
    col("volume").rolling_mean(strict(n)).alias("sma_vol")
}

fn ema_close_expr(n: usize) -> Expr {
    col("close").ewm_mean(ewm_span(n)).alias("ema")
}

fn stdev_close_expr(n: usize) -> Expr {
    (col("close").rolling_std(pop_opts(n)) * lit(1.0)).alias("stddev")
}

/// Seed-convergence horizon for an EMA leg of `length` over a series
/// with `prefix_nulls` leading absences: the seed opens at the first
/// NaN-free window (`prefix_nulls + length - 1` — asserted where claimed)
/// with a gap bounded by the head envelope, then dies geometrically
/// (`(1 - alpha)^k`, `alpha = 2/(n+1)`). Horizons: `ema(8)` opens at 90,
/// staged `ema(10)` over `mom(10)` at 123, `ema(21)` at 238, `ema(34)` at
/// 386 — every leg converges inside the 1-minute history (436 bars), so
/// expression tails run on the longest history only. The smoothing
/// recurrence is timeframe-independent (same code path, same seed rule),
/// and the engine-against-batch hop covers every timeframe exactly, so
/// one history proves the formula while the bridge hop plus the goldens
/// pin every number.
fn seed_horizon(length: usize, prefix_nulls: usize) -> usize {
    let decay = 1.0 - 2.0 / (length as f64 + 1.0);
    let first_defined = prefix_nulls + length - 1;
    first_defined + ((1e-9f64 / HEAD_ENVELOPE).ln() / decay.ln()).ceil() as usize
}

/// Seed-transient envelope over the head: the SMA-seed/first-value-seed
/// gap opens at window scale (single-digit prices on ~700-scale data)
/// and can only shrink through the shared recurrence, so a unit bound
/// proves same-recurrence while the tail proves steady-state identity.
const HEAD_ENVELOPE: f64 = 1.0;

/// One registered kernel's `batch` over bars as floats: the bridge-fidelity
/// oracle. `volume_leg` batches over the volume column (SMA-of-volume
/// legs) instead of close.
fn kernel_batch(
    bars: &[Bar],
    kernel: &str,
    params: &ParamSet,
    volume_leg: bool,
) -> Vec<Option<f64>> {
    let data = OwnedOhlcv::from_bars(bars);
    let registry = crate::indicator::builtin_registry().expect("builtins");
    let views;
    let vol_ohlcv;
    let ohlcv_ref = if volume_leg {
        vol_ohlcv = OwnedOhlcv {
            open: data.open.clone(),
            high: data.high.clone(),
            low: data.low.clone(),
            close: data.volume.clone(),
            volume: data.volume.clone(),
        };
        views = vol_ohlcv.views();
        &views
    } else {
        views = data.views();
        &views
    };
    registry
        .get(kernel)
        .unwrap_or_else(|| panic!("{kernel} registered"))
        .batch(ohlcv_ref, params)
        .expect("valid batch")
        .values
}

/// Steady-state tail of a seeded EMA leg against its reference plus the
/// bounded head envelope: proves the recurrence matches and only the seed
/// differs (see the module docstring). `expected` is the caller-staged
/// reference series (close-EMA or staged momentum smoothing); the tail
/// opens at the leg's [`seed_horizon`], so histories shorter than the
/// horizon refuse loudly instead of asserting vacuously.
fn assert_seeded_leg(
    actual: &[Option<f64>],
    expected: &[Option<f64>],
    length: usize,
    prefix_nulls: usize,
    context: &str,
) {
    assert_eq!(actual.len(), expected.len(), "{context}: equal length");
    let horizon = seed_horizon(length, prefix_nulls);
    assert!(
        actual.len() > horizon,
        "{context}: history outruns the seed horizon {horizon}"
    );
    let first_defined = prefix_nulls + length - 1;
    assert!(
        actual[first_defined].is_some(),
        "{context}: seed opens at bar {first_defined}"
    );
    let head_gap = actual[..horizon]
        .iter()
        .zip(expected[..horizon].iter())
        .filter_map(|(a, e)| match (a, e) {
            (Some(x), Some(y)) => Some((x - y).abs()),
            _ => None,
        })
        .fold(0.0f64, f64::max);
    assert!(
        head_gap < HEAD_ENVELOPE,
        "{context}: head gap {head_gap} exceeds the seed envelope"
    );
    assert_expr_parity(&actual[horizon..], &expected[horizon..], 1e-9, context)
        .expect("seeded-leg tail parity");
}

#[test]
fn dvp_trend_ma_matches_ema_leg() {
    // Default trend length 34: `ta.ema(close, 34)` — kernel tail against
    // the reference on the longest history (the only one outrunning the
    // 386-bar horizon); no interpreted run exists.
    let bars = fixture::history_bars("historical/SPY_1M.csv");
    let params = ParamSet::new().with_int("length", 34);
    let actual = kernel_batch(&bars, "ema", &params, false);
    let expected = expr_fixture(&bars, ema_close_expr(34)).expect("ema evaluates");
    assert_seeded_leg(&actual, &expected, 34, 0, "dvp trend ma 1m");
}

#[test]
fn dvp_vol_sma_matches_sma_expr() {
    // `ta.sma(volume, 20)`: kernel-against-reference on every timeframe
    // (strict rolling mean, no seeding) — no interpreted run exists.
    let params = ParamSet::new().with_int("length", 20);
    for (chart, file) in CHARTS {
        let bars = fixture::history_bars(file);
        let actual = kernel_batch(&bars, "sma", &params, true);
        let expected = expr_fixture(&bars, sma_volume_expr(20)).expect("volume sma evaluates");
        assert_expr_parity(&actual, &expected, 1e-9, &format!("dvp vol sma {chart}m"))
            .expect("vol sma parity");
    }
}

#[test]
fn dvp_stddev_matches_stdev_expr() {
    // `ta.stdev(close, 20)`: population deviation (`biased` default) —
    // kernel-against-reference on every timeframe, no interpreted run.
    let params = ParamSet::new()
        .with_int("length", 20)
        .with_float("nbdev", 1.0);
    for (chart, file) in CHARTS {
        let bars = fixture::history_bars(file);
        let actual = kernel_batch(&bars, "stddev", &params, false);
        let expected = expr_fixture(&bars, stdev_close_expr(20)).expect("stdev evaluates");
        assert_expr_parity(&actual, &expected, 1e-9, &format!("dvp stddev {chart}m"))
            .expect("stddev parity");
    }
}
