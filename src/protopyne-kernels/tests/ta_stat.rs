use super::*;
use crate::compat::compat_for;
use crate::indicator::bits_of;
use crate::indicator::{builtin_registry, OwnedOhlcv};
use crate::parity::{assert_expr_parity, expr_fixture};
use crate::ta_stat::{CorrelIndicator, StddevIndicator, VarIndicator};
use polars::prelude::{
    col, lit, when, IntoLazy, RollingFnParams, RollingOptionsFixedWindow, RollingVarParams,
};
use protopyne_types::Bar;
use std::path::PathBuf;

fn dec(x: f64) -> rust_decimal::Decimal {
    rust_decimal::Decimal::from_f64_retain(x).expect("finite test price")
}

fn bar_at(open: f64, high: f64, low: f64, close: f64, volume: u64, index: usize) -> Bar {
    use chrono::TimeZone as _;
    Bar::new()
        .ts(chrono::Utc
            .with_ymd_and_hms(
                2026,
                1,
                2,
                9 + (index / 3600) as u32,
                ((index / 60) % 60) as u32,
                (index % 60) as u32,
            )
            .single()
            .expect("valid timestamp"))
        .o(dec(open))
        .h(dec(high))
        .l(dec(low))
        .c(dec(close))
        .volume(volume)
        .maybe_adj(None)
        .build()
}

fn trend_bars(n: usize) -> Vec<Bar> {
    let mut bars = Vec::with_capacity(n);
    let mut prev_close = 100.0;
    for i in 0..n {
        let drift = i as f64 * 0.7 + ((i * 13) % 5) as f64 * 0.9;
        let close = 100.0 + drift;
        let gap = if i > 0 && i % 10 == 0 { 2.5 } else { 0.0 };
        let open = if i == 0 {
            close
        } else {
            prev_close + gap + 0.1
        };
        let high = open.max(close) + 0.8;
        let low = open.min(close) - 0.8;
        bars.push(bar_at(open, high, low, close, 1_000 + i as u64 * 37, i));
        prev_close = close;
    }
    bars
}

fn spy_bars(count: usize) -> Vec<Bar> {
    let path: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_path(path)
        .expect("vendored CSV present");
    let mut bars = Vec::new();
    for record in reader.records().take(count) {
        let row = record.expect("readable row");
        let num = |idx: usize| {
            row.get(idx)
                .expect("column")
                .parse::<f64>()
                .expect("number")
        };
        let epoch: i64 = row.get(0).expect("time").parse().expect("epoch");
        let ts = chrono::DateTime::from_timestamp(epoch, 0).expect("valid epoch");
        bars.push(
            Bar::new()
                .ts(ts)
                .o(dec(num(1)))
                .h(dec(num(2)))
                .l(dec(num(3)))
                .c(dec(num(4)))
                .volume(row.get(5).expect("vol").parse().expect("volume"))
                .maybe_adj(None)
                .build(),
        );
    }
    bars
}

fn ticks_of(bars: &[Bar]) -> Vec<BarTick> {
    use rust_decimal::prelude::ToPrimitive as _;
    bars.iter()
        .map(|b| BarTick {
            open: b.o.to_f64().unwrap_or(f64::NAN),
            high: b.h.to_f64().unwrap_or(f64::NAN),
            low: b.l.to_f64().unwrap_or(f64::NAN),
            close: b.c.to_f64().unwrap_or(f64::NAN),
            volume: b.volume as f64,
        })
        .collect()
}

fn strict(n: usize) -> RollingOptionsFixedWindow {
    RollingOptionsFixedWindow {
        window_size: n,
        min_periods: n,
        weights: None,
        center: false,
        fn_params: None,
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

fn stddev_expr(n: usize, nbdev: f64) -> polars::prelude::Expr {
    (col("close").rolling_std(pop_opts(n)) * lit(nbdev)).alias("stddev")
}

fn var_expr(n: usize, nbdev: f64) -> polars::prelude::Expr {
    (col("close").rolling_var(pop_opts(n)) * lit(nbdev)).alias("var")
}

/// Staged correlation reference: no `rolling_corr` expression exists on this
/// side, so the ddof-invariant moment form is staged from strict rolling
/// sums of shift-demeaned legs. The shift is exact-safe (same-scale prices)
/// and correlation is shift-invariant, so this is the reference's
/// `rolling_corr` without the raw-moment form's catastrophic cancellation on
/// large-magnitude prices (which drifted ~1e-8 off the two-pass value on
/// SPY-scale data) and without the un-stageable per-window demeaning a
/// two-pass transcription would need.
fn correl_reference(bars: &[Bar], n: usize) -> Vec<Option<f64>> {
    use crate::research::frame_of_bars;
    use polars::prelude::DataType;
    let nf = n as f64;
    let staged = frame_of_bars(bars)
        .expect("frame")
        .lazy()
        .with_columns([
            (col("high") - col("high").first()).alias("__hx"),
            (col("low") - col("low").first()).alias("__hy"),
        ])
        .with_columns([
            col("__hx").rolling_sum(strict(n)).alias("__sx"),
            col("__hy").rolling_sum(strict(n)).alias("__sy"),
            (col("__hx") * col("__hy"))
                .rolling_sum(strict(n))
                .alias("__sxy"),
            (col("__hx") * col("__hx"))
                .rolling_sum(strict(n))
                .alias("__sxx"),
            (col("__hy") * col("__hy"))
                .rolling_sum(strict(n))
                .alias("__syy"),
        ])
        .collect()
        .expect("staged frame");
    assert_eq!(staged.height(), bars.len());
    let num = col("__sxy") * lit(nf) - col("__sx") * col("__sy");
    let den = ((col("__sxx") * lit(nf) - col("__sx") * col("__sx"))
        * (col("__syy") * lit(nf) - col("__sy") * col("__sy")))
    .sqrt();
    let selected = staged
        .lazy()
        .select([when(den.clone().eq(lit(0.0)))
            .then(lit(f64::NAN))
            .otherwise(num / den)
            .alias("correl")])
        .collect()
        .expect("select");
    let cast = selected
        .column("correl")
        .expect("column")
        .cast(&DataType::Float64)
        .expect("cast");
    let chunked = cast.f64().expect("f64 view");
    (0..chunked.len())
        .map(|i| chunked.get(i).and_then(crate::indicator::fin_or_none))
        .collect()
}

fn stddev_params(n: usize, nbdev: f64) -> ParamSet {
    ParamSet::new()
        .with_int("length", n as i64)
        .with_float("nbdev", nbdev)
}

#[test]
fn stddev_matches_population_expr() {
    let stddev = StddevIndicator::new();
    let ps = stddev_params(5, 2.0);
    assert_eq!(stddev.warmup(&ps).expect("valid"), 5);
    for (bars, name) in [(trend_bars(60), "trend"), (spy_bars(60), "spy")] {
        let data = OwnedOhlcv::from_bars(&bars);
        let result = stddev.batch(&data.views(), &ps).expect("valid");
        assert!(result.values[..4].iter().all(Option::is_none));
        let want = expr_fixture(&bars, stddev_expr(5, 2.0)).expect("fixture");
        assert_expr_parity(&result.values, &want, 1e-9, &format!("stddev-{name}")).expect("parity");
        let mut state = IndicatorState::new();
        for (tick, want) in ticks_of(&bars).into_iter().zip(result.values.iter()) {
            assert_eq!(
                &stddev.update(&mut state, tick, &ps).expect("valid"),
                want,
                "stddev agreement"
            );
        }
    }
}

#[test]
fn stddev_locks_population_denominator() {
    // The reference divides by `n`, not `n - 1`: on a 3-bar window the two
    // denominators differ by exactly sqrt(3/2), and the kernel must track
    // the population side — the fork guard the milestone asks for, with the
    // corrected (population) polarity.
    let bars = trend_bars(10);
    let data = OwnedOhlcv::from_bars(&bars);
    let ps = stddev_params(3, 1.0);
    let result = StddevIndicator::new()
        .batch(&data.views(), &ps)
        .expect("valid");
    let pop = expr_fixture(&bars, stddev_expr(3, 1.0)).expect("fixture");
    assert_expr_parity(&result.values, &pop, 1e-9, "stddev-pop").expect("parity");
    let ratio = (3.0f64 / 2.0).sqrt();
    for (got, p) in result.values.iter().zip(pop.iter()) {
        match (got, p) {
            (Some(got), Some(p)) => {
                assert!((got - p).abs() <= 1e-9);
                let sample = p * ratio;
                assert!(
                    (got - sample).abs() > 1e-9,
                    "kernel must not drift to the sample denominator"
                );
            }
            (None, None) => {}
            _ => panic!("stddev null shape must match the population reference"),
        }
    }
    // Flat market: zero dispersion is a confident zero, not a refusal.
    let flat: Vec<Bar> = (0..20)
        .map(|i| bar_at(10.0, 10.0, 10.0, 10.0, 1_000, i))
        .collect();
    let data = OwnedOhlcv::from_bars(&flat);
    let result = StddevIndicator::new()
        .batch(&data.views(), &ps)
        .expect("valid");
    assert!(result.values[..2].iter().all(Option::is_none));
    assert!(result.values[2..].iter().all(|v| *v == Some(0.0)));
}

#[test]
fn var_matches_population_expr_and_squares_stddev() {
    let var = VarIndicator::new();
    let ps = stddev_params(5, 1.5);
    assert_eq!(var.warmup(&ps).expect("valid"), 5);
    for (bars, name) in [(trend_bars(60), "trend"), (spy_bars(60), "spy")] {
        let data = OwnedOhlcv::from_bars(&bars);
        let result = var.batch(&data.views(), &ps).expect("valid");
        let want = expr_fixture(&bars, var_expr(5, 1.5)).expect("fixture");
        assert_expr_parity(&result.values, &want, 1e-9, &format!("var-{name}")).expect("parity");
        let mut state = IndicatorState::new();
        for (tick, want) in ticks_of(&bars).into_iter().zip(result.values.iter()) {
            assert_eq!(
                &var.update(&mut state, tick, &ps).expect("valid"),
                want,
                "var agreement"
            );
        }
    }
    // Variance is the squared deviation within tolerance (not bit-identical:
    // sqrt-then-square re-rounds, which is why the kernel divides directly).
    let bars = trend_bars(30);
    let data = OwnedOhlcv::from_bars(&bars);
    let unit = stddev_params(5, 1.0);
    let dev = StddevIndicator::new()
        .batch(&data.views(), &unit)
        .expect("valid");
    let variance = VarIndicator::new()
        .batch(&data.views(), &unit)
        .expect("valid");
    for (v, d) in variance.values.iter().zip(dev.values.iter()) {
        match (v, d) {
            (Some(v), Some(d)) => assert!((v - d * d).abs() <= 1e-9, "var tracks squared stddev"),
            (None, None) => {}
            _ => panic!("var/stddev null shapes must agree"),
        }
    }
}

#[test]
fn correl_matches_staged_reference() {
    let correl = CorrelIndicator::new();
    let ps = ParamSet::new().with_int("length", 5);
    assert_eq!(correl.warmup(&ps).expect("valid"), 5);
    for (bars, name) in [(trend_bars(60), "trend"), (spy_bars(60), "spy")] {
        let data = OwnedOhlcv::from_bars(&bars);
        let result = correl.batch(&data.views(), &ps).expect("valid");
        assert!(result.values[..4].iter().all(Option::is_none));
        let want = correl_reference(&bars, 5);
        assert_expr_parity(&result.values, &want, 1e-9, &format!("correl-{name}")).expect("parity");
        assert!(
            result.values[4..]
                .iter()
                .all(|v| matches!(v, Some(x) if *x >= -1.0 && *x <= 1.0)),
            "correlation bounds on {name}"
        );
        let mut state = IndicatorState::new();
        for (tick, want) in ticks_of(&bars).into_iter().zip(result.values.iter()) {
            assert_eq!(
                &correl.update(&mut state, tick, &ps).expect("valid"),
                want,
                "correl agreement"
            );
        }
    }
    // Constant legs carry no direction: honest `None` throughout, on both paths.
    let flat: Vec<Bar> = (0..20)
        .map(|i| bar_at(10.0, 10.0, 10.0, 10.0, 1_000, i))
        .collect();
    let data = OwnedOhlcv::from_bars(&flat);
    let result = correl.batch(&data.views(), &ps).expect("valid");
    assert!(result.values.iter().all(Option::is_none));
    let mut state = IndicatorState::new();
    for tick in ticks_of(&flat) {
        assert_eq!(correl.update(&mut state, tick, &ps).expect("valid"), None);
    }
}

#[test]
fn raise_stubs_have_no_kernels() {
    // BETA, the LINEARREG family, and TSF raise unconditionally in the
    // reference: lookup fails closed instead of inventing them.
    let registry = builtin_registry().expect("builtins");
    for id in [
        "beta",
        "linearreg",
        "linearreg_angle",
        "linearreg_intercept",
        "linearreg_slope",
        "tsf",
    ] {
        assert!(registry.get(id).is_none(), "{id} must have no kernel");
        assert!(compat_for(id).is_none(), "{id} must have no matrix row");
    }
}

#[test]
fn stat_registry_and_matrix_cover_all_three() {
    let registry = builtin_registry().expect("builtins");
    for id in ["stddev", "var", "correl"] {
        assert!(registry.get(id).is_some(), "{id} registered");
        let row = compat_for(id).unwrap_or_else(|| panic!("{id} matrix row"));
        assert!((row.eps - 1e-9).abs() == 0.0, "{id} frozen eps");
    }
}

fn fold_stddev(closes: &[f64], params: &ParamSet) -> Vec<Option<f64>> {
    let sd = StddevIndicator::new();
    let mut state = IndicatorState::new();
    closes
        .iter()
        .map(|close| {
            sd.update(&mut state, BarTick::close_only(*close), params)
                .expect("update runs")
        })
        .collect()
}

#[test]
fn buffered_update_matches_batch_on_constant_and_subnormal_inputs() {
    // Bit-exactness pin for the scratch migration, over the dispersion
    // representative (two-pass deviation arithmetic must agree exactly,
    // including a zero-variance flat line).
    let params = stddev_params(14, 1.0);
    for closes in [
        vec![42.0; 40],
        (1..=40).map(|i| f64::from(i) * 1e-310).collect::<Vec<_>>(),
    ] {
        let sd = StddevIndicator::new();
        let batch = sd
            .batch(&Ohlcv::close_only(&closes), &params)
            .expect("batch runs")
            .values;
        assert_eq!(bits_of(&fold_stddev(&closes, &params)), bits_of(&batch));
    }
}
