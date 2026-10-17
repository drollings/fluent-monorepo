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
//! RSIMD legs assert engine-against-reference (the engine's per-bar bridge
//! must reproduce the reference whole-series): `rsi_momentum_source`,
//! `rsi_momentum_smooth` (staged), `_ma_signal`/`_ma_mean` (seeded tails),
//! `_volume_sma`.
//!
//! DVP runs no interpreted suite by human verdict (2026-10-12: prune every
//! PineScript DVP run, keep native Rust only), so its legs assert
//! kernel-against-reference instead: the registered kernel's `batch` over
//! the same bars must reproduce the reference (`trend_ma`, `vol_sma`,
//! `stddev`). The kernels seed every EMA with the first-window mean while
//! the reference `ewm_mean` seeds with the first value, so a geometric
//! seed transient (`(1 - alpha)^k`) rides the head of every real-history
//! run: seeded legs pin the steady-state tail on the longest (1-minute)
//! history at `1e-9` — the tail opens at each leg's computed
//! [`seed_horizon`] — plus a bounded seed envelope over the head, which
//! together prove the recurrence matches and only the seed differs.
//!
//! Every RSIMD reducible leg is additionally checked engine-against-kernel:
//! the engine's per-bar bridge must reproduce the registered kernel's
//! `batch` over the same bars whole-series at `1e-9` (same seed, same
//! recurrence — this hop isolates bridge fidelity from kernel
//! correctness; the framed kernel-against-reference checks elsewhere
//! carry the other half).
//!
//! Golden-only (pinned by name below, never widened into parity):
//!
//! * Wilder-seeded chains: RSIMD `_atr`, `_atr_long`, `rsi_raw`, `rsi_val`,
//!   `rsi_right`. The kernels seed Wilder smoothings with the
//!   first-window mean while `ewm_mean` seeds with the first value, so the
//!   seed transient decays geometrically (`(1 - 1/n)^k`) and no
//!   whole-series `1e-9` reference exists — e.g. `atr(14)` needs ~380 bars
//!   to converge on SPY-scale data, past the 15M history's end. Matching
//!   the reference there would be a regression, not a fix.
//! * Library-method chains: RSIMD `trend_health` (`libApex.getHealthMAs`
//!   over atr legs).
//! * Pivot/divergence logic: RSIMD `found_pl`, `found_ph`, `is_bull_div`,
//!   `is_bear_div`, `low_level`, `high_level` (occurrence-indexed
//!   confirmation windows, not kernel expressions).
//! * Drawing events and regime labels: golden-pinned only, never
//!   expressions.
//!
//! Test path note: this file rides the `pine::eval` unit module (closure
//! runs live in `eval.rs`), so the runnable filter is `pine::eval`, not
//! the file stem.

use crate::parity::{assert_expr_parity, expr_fixture};
use crate::{
    InputSet, OwnedOhlcv, ParamSet, PineEngine, PineRuntimeConfig, PineValue,
    RunOutcome,
};
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

/// Outputs with an expression check in this file. RSIMD names assert
/// engine-against-reference; DVP names assert kernel-against-reference
/// (no interpreted DVP run exists). Disjoint from [`GOLDEN_ONLY`] by
/// construction (asserted below); a future output joins exactly one list,
/// never neither (asserted bound) and never both.
const EXPR_CHECKED: &[&str] = &[
    "rsi_momentum_source",
    "rsi_momentum_smooth",
    "_ma_signal",
    "_ma_mean",
    "_volume_sma",
    "trend_ma",
    "vol_sma",
    "stddev",
];

/// Outputs pinned by the golden only, with the reason per group in the
/// module docstring. Every name must bind on the RSIMD run (asserted
/// below), so a typo fails instead of passing vacuously.
const GOLDEN_ONLY: &[&str] = &[
    "rsi_raw",
    "rsi_val",
    "rsi_right",
    "_atr",
    "_atr_long",
    "trend_health",
    "found_pl",
    "found_ph",
    "is_bull_div",
    "is_bear_div",
    "low_level",
    "high_level",
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

fn mom_close_expr(n: usize) -> Expr {
    (col("close") - col("close").shift(lit(n as i64))).alias("mom")
}

fn stdev_close_expr(n: usize) -> Expr {
    (col("close").rolling_std(pop_opts(n)) * lit(1.0)).alias("stddev")
}

/// One engine snapshot series as floats: `na` slots read `None`, matching
/// the reference's null mapping. Anything else is a shape defect, never a
/// silent skip.
fn float_series(outcome: &RunOutcome, name: &str) -> Vec<Option<f64>> {
    outcome
        .snapshots
        .iter()
        .map(|snapshot| match snapshot.values.get(name) {
            None | Some(None) => None,
            Some(Some(PineValue::Float(v))) => Some(*v),
            Some(Some(PineValue::Int(v))) => Some(*v as f64),
            Some(Some(other)) => panic!("series {name} holds no number, got {other:?}"),
        })
        .collect()
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

/// Engine series against the kernel batch, whole-series at frozen eps:
/// same seed, same recurrence, so any drift is bridge error, never seed.
fn assert_bridge_matches_batch(
    actual: &[Option<f64>],
    bars: &[Bar],
    kernel: &str,
    params: &ParamSet,
    volume_leg: bool,
    context: &str,
) {
    let expected = kernel_batch(bars, kernel, params, volume_leg);
    assert_expr_parity(actual, &expected, 1e-9, context).expect("bridge fidelity");
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

/// Engine for one chart timeframe: SPY scope with zero warmup bars, the
/// same evaluated-bar semantics the server headless proof runs under.
fn engine_for(period: &str) -> PineEngine {
    PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period(period.to_owned())
            .warmup_bars(0)
            .build(),
    )
}

/// Link RSIMD once and run it over one full chart history with no
/// injections: divergence detection runs on chart legs.
fn run_rsimd(chart: &str, file: &str) -> (Vec<Bar>, RunOutcome) {
    let bars = fixture::history_bars(file);
    let modules = fixture::linked_closure("ApexRSIMD");
    let outcome = engine_for(chart)
        .run_linked(&modules, &bars, InputSet::new())
        .expect("RSIMD evaluates with zero refusals");
    (bars, outcome)
}

#[test]
#[ignore = "requires trimmed CSV history fixtures no longer shipped in the tree"]
fn rsimd_momentum_source_matches_mom_expr() {
    // Default momentum period 10: `ta.mom(close, 10)` is pure
    // shift-subtract with `None` over the first 10 bars on both sides.
    let params = ParamSet::new().with_int("length", 10);
    for (chart, file) in CHARTS {
        let (bars, outcome) = run_rsimd(chart, file);
        let actual = float_series(&outcome, "rsi_momentum_source");
        let expected = expr_fixture(&bars, mom_close_expr(10)).expect("mom evaluates");
        assert_expr_parity(&actual, &expected, 1e-9, &format!("rsimd mom {chart}m"))
            .expect("momentum source parity");
        assert_bridge_matches_batch(
            &actual,
            &bars,
            "mom",
            &params,
            false,
            &format!("rsimd mom bridge {chart}m"),
        );
    }
}

#[test]
#[ignore = "requires trimmed CSV history fixtures no longer shipped in the tree"]
fn rsimd_signal_mean_match_ema_legs() {
    // `ta.ema(close, 8/21)`: expression tails on the longest history (the
    // only one outrunning every horizon), bridge hops on all timeframes.
    let (bars, outcome) = run_rsimd("1", "historical/SPY_1M.csv");
    for (name, length) in [("_ma_signal", 8), ("_ma_mean", 21)] {
        let actual = float_series(&outcome, name);
        let expected = expr_fixture(&bars, ema_close_expr(length)).expect("ema evaluates");
        assert_seeded_leg(&actual, &expected, length, 0, &format!("rsimd {name} 1m"));
    }
    for (chart, file) in CHARTS {
        let (bars, outcome) = run_rsimd(chart, file);
        for (name, length) in [("_ma_signal", 8), ("_ma_mean", 21)] {
            assert_bridge_matches_batch(
                &float_series(&outcome, name),
                &bars,
                "ema",
                &ParamSet::new().with_int("length", i64::from(length)),
                false,
                &format!("rsimd {name} bridge {chart}m"),
            );
        }
    }
}

#[test]
#[ignore = "requires trimmed CSV history fixtures no longer shipped in the tree"]
fn rsimd_volume_sma_matches_sma_expr() {
    // `ta.sma(volume, 50)`: strict rolling mean over the volume leg.
    let params = ParamSet::new().with_int("length", 50);
    for (chart, file) in CHARTS {
        let (bars, outcome) = run_rsimd(chart, file);
        let actual = float_series(&outcome, "_volume_sma");
        let expected =
            expr_fixture(&bars, sma_volume_expr(50)).expect("volume sma evaluates");
        assert_expr_parity(&actual, &expected, 1e-9, &format!("rsimd volume sma {chart}m"))
            .expect("volume sma parity");
        assert_bridge_matches_batch(
            &actual,
            &bars,
            "sma",
            &params,
            true,
            &format!("rsimd volume sma bridge {chart}m"),
        );
    }
}

#[test]
#[ignore = "requires trimmed CSV history fixtures no longer shipped in the tree"]
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
#[ignore = "requires trimmed CSV history fixtures no longer shipped in the tree"]
fn dvp_vol_sma_matches_sma_expr() {
    // `ta.sma(volume, 20)`: kernel-against-reference on every timeframe
    // (strict rolling mean, no seeding) — no interpreted run exists.
    let params = ParamSet::new().with_int("length", 20);
    for (chart, file) in CHARTS {
        let bars = fixture::history_bars(file);
        let actual = kernel_batch(&bars, "sma", &params, true);
        let expected =
            expr_fixture(&bars, sma_volume_expr(20)).expect("volume sma evaluates");
        assert_expr_parity(&actual, &expected, 1e-9, &format!("dvp vol sma {chart}m"))
            .expect("vol sma parity");
    }
}

#[test]
#[ignore = "requires trimmed CSV history fixtures no longer shipped in the tree"]
fn dvp_stddev_matches_stdev_expr() {
    // `ta.stdev(close, 20)`: population deviation (`biased` default) —
    // kernel-against-reference on every timeframe, no interpreted run.
    let params = ParamSet::new()
        .with_int("length", 20)
        .with_float("nbdev", 1.0);
    for (chart, file) in CHARTS {
        let bars = fixture::history_bars(file);
        let actual = kernel_batch(&bars, "stddev", &params, false);
        let expected =
            expr_fixture(&bars, stdev_close_expr(20)).expect("stdev evaluates");
        assert_expr_parity(&actual, &expected, 1e-9, &format!("dvp stddev {chart}m"))
            .expect("stddev parity");
    }
}

/// Staged reference for the smoothed momentum leg: the single-select
/// harness cannot nest `ewm_mean` through an alias, so momentum is staged
/// as a frame column first and only the smoothing pass is selected — the
/// same evaluation the harness performs, with identical null/NaN mapping.
fn momentum_smooth_reference(bars: &[Bar]) -> Vec<Option<f64>> {
    use crate::research::frame_of_bars;
    let staged = frame_of_bars(bars)
        .expect("frame")
        .lazy()
        .with_columns([(col("close") - col("close").shift(lit(10_i64))).alias("__mom")])
        .with_columns([col("__mom").ewm_mean(ewm_span(10)).alias("__smooth")])
        .collect()
        .expect("staged frame");
    assert_eq!(staged.height(), bars.len());
    let column = staged
        .column("__smooth")
        .expect("smooth column")
        .cast(&DataType::Float64)
        .expect("f64 cast");
    let chunked = column.f64().expect("f64 view");
    (0..chunked.len())
        .map(|i| {
            chunked.get(i).and_then(crate::indicator::fin_or_none)
        })
        .collect()
}

#[test]
#[ignore = "requires trimmed CSV history fixtures no longer shipped in the tree"]
fn rsimd_momentum_smooth_matches_staged_expr() {
    // Default momentum smoothing EMA(10) over `ta.mom(close, 10)`:
    // expression tail on the longest history, since the na-prefixed
    // smoothing carries the same seed transient as every EMA leg.
    let (bars, outcome) = run_rsimd("1", "historical/SPY_1M.csv");
    let actual = float_series(&outcome, "rsi_momentum_smooth");
    let expected = momentum_smooth_reference(&bars);
    assert_seeded_leg(&actual, &expected, 10, 10, "rsimd mom smooth 1m");
}

#[test]
#[ignore = "requires trimmed CSV history fixtures no longer shipped in the tree"]
fn task_parity_coverage_names_every_output_once() {
    // The two lists partition the proof: disjoint, and every golden-only
    // name binds on the RSIMD run — binding is a name check, not a value
    // check. DVP names live in `EXPR_CHECKED` (kernel-against-reference)
    // and bind through the kernel registry, never through an engine run.
    for name in EXPR_CHECKED {
        assert!(
            !GOLDEN_ONLY.contains(name),
            "{name} is checked and golden-only at once"
        );
    }
    let (_, rsimd) = run_rsimd("1", "historical/SPY_1M.csv");
    let rsimd_keys: std::collections::BTreeSet<&str> = rsimd
        .snapshots
        .iter()
        .flat_map(|snapshot| snapshot.values.keys().map(String::as_str))
        .collect();
    for name in GOLDEN_ONLY {
        assert!(
            rsimd_keys.contains(*name),
            "{name} binds on neither indicator"
        );
    }
}
