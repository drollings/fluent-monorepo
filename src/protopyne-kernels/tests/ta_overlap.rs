use super::*;
use crate::compat::compat_for;
use crate::indicator::{builtin_registry, OwnedOhlcv};
use crate::parity::{assert_expr_parity, expr_fixture};
use crate::ta_overlap::{
    BbandsIndicator, DonchianIndicator, IchimokuIndicator, MidpointIndicator, MidpriceIndicator,
    PsarIndicator, TrimaIndicator,
};
use polars::prelude::{col, lit, RollingFnParams, RollingOptionsFixedWindow, RollingVarParams};
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

/// Deterministic trend with periodic gap-ups: one source of truth feeding
/// both the kernel legs and the expression frame.
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

fn std_pop_opts(n: usize) -> RollingOptionsFixedWindow {
    RollingOptionsFixedWindow {
        window_size: n,
        min_periods: n,
        weights: None,
        center: false,
        fn_params: Some(RollingFnParams::Var(RollingVarParams { ddof: 0 })),
    }
}

fn bbands_params() -> ParamSet {
    ParamSet::new()
        .with_int("length", 14)
        .with_float("nbdevup", 2.0)
        .with_float("nbdevdn", 2.0)
}

fn len_params() -> ParamSet {
    ParamSet::new().with_int("length", 14)
}

#[test]
fn bbands_matches_expr_on_spy_slice() {
    let bars = spy_bars(60);
    let data = OwnedOhlcv::from_bars(&bars);
    let bbands = BbandsIndicator::new();
    let result = bbands
        .batch(&data.views(), &bbands_params())
        .expect("valid");
    let sma = col("close").rolling_mean(strict(14));
    let std = col("close").rolling_std(std_pop_opts(14));
    let middle = expr_fixture(&bars, sma.clone().alias("middle")).expect("middle evaluates");
    let upper =
        expr_fixture(&bars, (sma.clone() + std.clone() * lit(2.0)).alias("upper")).expect("upper");
    let lower = expr_fixture(&bars, (sma - std * lit(2.0)).alias("lower")).expect("lower");
    assert_expr_parity(&result.values, &middle, 1e-9, "bbands middle").expect("middle parity");
    assert_expr_parity(
        result.secondary("upper").expect("upper leg"),
        &upper,
        1e-9,
        "bbands upper",
    )
    .expect("upper parity");
    assert_expr_parity(
        result.secondary("lower").expect("lower leg"),
        &lower,
        1e-9,
        "bbands lower",
    )
    .expect("lower parity");
}

#[test]
fn bbands_matches_symmetric_bb_bit_for_bit() {
    // Same denominator (population) and same loop: symmetric bands must equal
    // the legacy kernel exactly, locking the shared arithmetic.
    let bars = spy_bars(60);
    let data = OwnedOhlcv::from_bars(&bars);
    let bbands = BbandsIndicator::new();
    let got = bbands
        .batch(&data.views(), &bbands_params())
        .expect("valid");
    let bb = crate::volatility::BbIndicator::new();
    let want = bb
        .batch(
            &data.views(),
            &ParamSet::new()
                .with_int("length", 14)
                .with_float("mult", 2.0),
        )
        .expect("valid");
    assert_eq!(got.values, want.values);
    assert_eq!(
        got.secondary("upper").expect("upper"),
        want.secondary("upper").expect("upper")
    );
    assert_eq!(
        got.secondary("lower").expect("lower"),
        want.secondary("lower").expect("lower")
    );
}

#[test]
fn bbands_update_agrees_with_batch_on_spy() {
    let bars = spy_bars(60);
    let data = OwnedOhlcv::from_bars(&bars);
    let ps = bbands_params();
    let bbands = BbandsIndicator::new();
    let result = bbands.batch(&data.views(), &ps).expect("valid");
    let mut state = IndicatorState::new();
    let mut primary = Vec::new();
    let mut upper = Vec::new();
    let mut lower = Vec::new();
    for tick in ticks_of(&bars) {
        let full = bbands.update_full(&mut state, tick, &ps).expect("valid");
        primary.push(full.primary);
        upper.push(full.secondary[0].value);
        lower.push(full.secondary[1].value);
    }
    assert_eq!(result.values, primary);
    assert_eq!(result.secondary("upper").expect("upper"), &upper);
    assert_eq!(result.secondary("lower").expect("lower"), &lower);
}

#[test]
fn bbands_warmup_is_length_and_refuses_non_sma_matype() {
    let bbands = BbandsIndicator::new();
    assert_eq!(bbands.warmup(&bbands_params()).expect("valid"), 14);
    let bars = trend_bars(20);
    let data = OwnedOhlcv::from_bars(&bars);
    let result = bbands
        .batch(&data.views(), &bbands_params())
        .expect("valid");
    assert!(result.values[..13].iter().all(Option::is_none));
    assert!(result.values[13].is_some());
    // Only SMA smoothing exists; any other moving-average type refuses.
    let bad = ParamSet::new()
        .with_int("length", 14)
        .with_float("nbdevup", 2.0)
        .with_float("nbdevdn", 2.0)
        .with_float("matype", 1.0);
    assert!(bbands.batch(&data.views(), &bad).is_err());
    // Absent matype defaults to SMA.
    assert!(bbands.batch(&data.views(), &bbands_params()).is_ok());
}

#[test]
fn midpoint_matches_expr_and_warms_up() {
    let bars = spy_bars(60);
    let data = OwnedOhlcv::from_bars(&bars);
    let midpoint = MidpointIndicator::new();
    let ps = len_params();
    let result = midpoint.batch(&data.views(), &ps).expect("valid");
    let expr = ((col("close").rolling_max(strict(14)) + col("close").rolling_min(strict(14)))
        / lit(2.0))
    .alias("midpoint");
    let want = expr_fixture(&bars, expr).expect("midpoint evaluates");
    assert_expr_parity(&result.values, &want, 1e-9, "midpoint").expect("parity");
    assert_eq!(midpoint.warmup(&ps).expect("valid"), 14);
    assert!(result.values[..13].iter().all(Option::is_none));
    let mut state = IndicatorState::new();
    let folded: Vec<Option<f64>> = ticks_of(&bars)
        .into_iter()
        .map(|t| midpoint.update(&mut state, t, &ps).expect("valid"))
        .collect();
    assert_eq!(result.values, folded);
}

#[test]
fn midprice_reads_high_low_legs_with_gap() {
    let bars = trend_bars(30);
    let data = OwnedOhlcv::from_bars(&bars);
    let midprice = MidpriceIndicator::new();
    let ps = len_params();
    let result = midprice.batch(&data.views(), &ps).expect("valid");
    let expr = ((col("high").rolling_max(strict(14)) + col("low").rolling_min(strict(14)))
        / lit(2.0))
    .alias("midprice");
    let want = expr_fixture(&bars, expr).expect("midprice evaluates");
    assert_expr_parity(&result.values, &want, 1e-9, "midprice").expect("parity");
    assert_eq!(midprice.warmup(&ps).expect("valid"), 14);
    let mut state = IndicatorState::new();
    let folded: Vec<Option<f64>> = ticks_of(&bars)
        .into_iter()
        .map(|t| midprice.update(&mut state, t, &ps).expect("valid"))
        .collect();
    assert_eq!(result.values, folded);
}

#[test]
fn trima_matches_nested_sma_expr_with_length_warmup() {
    // Sub-periods ceil(14/2) = 7 and floor(14/2) + 1 = 8: the first valid
    // output lands at index 13, so warmup is the length itself.
    let bars = spy_bars(60);
    let data = OwnedOhlcv::from_bars(&bars);
    let trima = TrimaIndicator::new();
    let ps = len_params();
    let result = trima.batch(&data.views(), &ps).expect("valid");
    let expr = col("close")
        .rolling_mean(strict(7))
        .rolling_mean(strict(8))
        .alias("trima");
    let want = expr_fixture(&bars, expr).expect("trima evaluates");
    assert_expr_parity(&result.values, &want, 1e-9, "trima").expect("parity");
    assert_eq!(trima.warmup(&ps).expect("valid"), 14);
    assert!(result.values[..13].iter().all(Option::is_none));
    assert!(result.values[13].is_some());
    let mut state = IndicatorState::new();
    let folded: Vec<Option<f64>> = ticks_of(&bars)
        .into_iter()
        .map(|t| trima.update(&mut state, t, &ps).expect("valid"))
        .collect();
    assert_eq!(result.values, folded);
}

#[test]
fn kama_has_no_kernel_and_refuses_with_reason() {
    // The reference raises unconditionally for KAMA: no smoothing is
    // invented here. Lookup fails closed instead of miscomputing.
    let registry = builtin_registry().expect("builtins register");
    assert!(registry.get("kama").is_none());
    assert!(compat_for("kama").is_none());
}

fn donchian_bars() -> Vec<Bar> {
    let legs = [
        (10.0, 8.0, 9.0),
        (12.0, 9.0, 11.0),
        (11.0, 8.0, 10.0),
        (13.0, 10.0, 12.0),
        (12.0, 9.0, 10.5),
    ];
    legs.into_iter()
        .enumerate()
        .map(|(i, (h, l, c))| bar_at(c, h, l, c, 1_000, i))
        .collect()
}

#[test]
fn donchian_matches_hand_channel() {
    let bars = donchian_bars();
    let data = OwnedOhlcv::from_bars(&bars);
    let donchian = DonchianIndicator::new();
    let ps = ParamSet::new().with_int("length", 3);
    assert_eq!(donchian.warmup(&ps).expect("valid"), 3);
    let result = donchian.batch(&data.views(), &ps).expect("valid");
    assert!(
        result.values[..2].iter().all(Option::is_none),
        "window warms"
    );
    let upper = result.secondary("upper").expect("upper leg");
    let lower = result.secondary("lower").expect("lower leg");
    for (got, want) in result.values[2..].iter().zip([10.0, 10.5, 10.5]) {
        assert_eq!(*got, Some(want), "hand middle");
    }
    for (got, want) in upper[2..].iter().zip([12.0, 13.0, 13.0]) {
        assert_eq!(*got, Some(want), "hand upper");
    }
    for (got, want) in lower[2..].iter().zip([8.0, 8.0, 8.0]) {
        assert_eq!(*got, Some(want), "hand lower");
    }
    let mut state = IndicatorState::new();
    for (tick, ((want_mid, want_up), want_low)) in ticks_of(&bars)
        .into_iter()
        .zip(result.values.iter().zip(upper.iter()).zip(lower.iter()))
    {
        let tick_out = donchian.update_full(&mut state, tick, &ps).expect("valid");
        assert_eq!(&tick_out.primary, want_mid, "middle agreement");
        let got_up = tick_out
            .secondary
            .iter()
            .find(|v| v.name == "upper")
            .and_then(|v| v.value);
        let got_low = tick_out
            .secondary
            .iter()
            .find(|v| v.name == "lower")
            .and_then(|v| v.value);
        assert_eq!(got_up, *want_up, "upper agreement");
        assert_eq!(got_low, *want_low, "lower agreement");
    }
}

#[test]
fn donchian_orders_bands_and_holds_defined_through_clean_runs() {
    let donchian = DonchianIndicator::new();
    let ps = ParamSet::new().with_int("length", 20);
    for (bars, name) in [(trend_bars(60), "trend"), (spy_bars(60), "spy")] {
        let data = OwnedOhlcv::from_bars(&bars);
        let result = donchian.batch(&data.views(), &ps).expect("valid");
        let upper = result.secondary("upper").expect("upper leg");
        let lower = result.secondary("lower").expect("lower leg");
        assert!(
            result.values[..19].iter().all(Option::is_none),
            "window warms on {name}"
        );
        for (i, ((m, u), l)) in result
            .values
            .iter()
            .zip(upper.iter())
            .zip(lower.iter())
            .enumerate()
        {
            match (m, u, l) {
                (Some(m), Some(u), Some(l)) => {
                    assert!(u >= m && m >= l, "band order at {i} on {name}");
                    assert!(i >= 19, "defined only past warmup on {name}");
                }
                _ => assert!(i < 60, "holes only from short input"),
            }
        }
    }
}

#[test]
fn donchian_hole_poisons_only_its_window() {
    // Holes enter at the leg level (`Decimal` bars cannot carry NaN).
    let bars = donchian_bars();
    let mut data = OwnedOhlcv::from_bars(&bars);
    data.high[2] = f64::NAN;
    let mut ticks = ticks_of(&bars);
    ticks[2].high = f64::NAN;
    let donchian = DonchianIndicator::new();
    let ps = ParamSet::new().with_int("length", 3);
    let result = donchian.batch(&data.views(), &ps).expect("valid");
    // Windows ending 2, 3, 4 all cover bar 2; earlier bars stay clean.
    assert!(
        result.values[2..].iter().all(Option::is_none),
        "covered windows poison"
    );
    let mut state = IndicatorState::new();
    for (tick, want) in ticks.into_iter().zip(result.values.iter()) {
        assert_eq!(
            &donchian.update(&mut state, tick, &ps).expect("valid"),
            want,
            "hole agreement"
        );
    }
}

fn ichimoku_params() -> ParamSet {
    ParamSet::new()
        .with_int("tenkan_length", 2)
        .with_int("kijun_length", 3)
        .with_int("senkou_b_length", 4)
}

#[test]
fn ichimoku_matches_hand_legs_undisplaced() {
    // Rising ramp: every midline is hand-computable; spans publish at
    // their computing bar (no forward/backward displacement in data).
    let legs = [
        (10.0, 8.0, 9.0),
        (11.0, 9.0, 10.0),
        (12.0, 10.0, 11.0),
        (13.0, 11.0, 12.0),
        (14.0, 12.0, 13.0),
    ];
    let bars: Vec<Bar> = legs
        .into_iter()
        .enumerate()
        .map(|(i, (h, l, c))| bar_at(c, h, l, c, 1_000, i))
        .collect();
    let data = OwnedOhlcv::from_bars(&bars);
    let ichimoku = IchimokuIndicator::new();
    assert_eq!(ichimoku.warmup(&ichimoku_params()).expect("valid"), 4);
    let result = ichimoku
        .batch(&data.views(), &ichimoku_params())
        .expect("valid");
    let leg = |name: &str| result.secondary(name).expect("leg").to_vec();
    let (kijun, senkou_a, senkou_b, chikou) = (
        leg("kijun"),
        leg("senkou_a"),
        leg("senkou_b"),
        leg("chikou"),
    );
    assert_eq!(result.values[1], Some(9.5), "tenkan[1]");
    assert_eq!(result.values[2], Some(10.5), "tenkan[2]");
    assert_eq!(kijun[2], Some(10.0), "kijun[2]");
    assert_eq!(senkou_a[2], Some(10.25), "senkou_a[2]");
    assert_eq!(senkou_b[3], Some(10.5), "senkou_b[3]");
    assert_eq!(senkou_a[4], Some(12.25), "senkou_a[4]");
    assert_eq!(senkou_b[4], Some(11.5), "senkou_b[4]");
    for (i, (got, close)) in chikou.iter().zip([9.0, 10.0, 11.0, 12.0, 13.0]).enumerate() {
        assert_eq!(*got, Some(close), "chikou rides close at {i}");
    }
    let mut state = IndicatorState::new();
    let series = [&result.values, &kijun, &senkou_a, &senkou_b, &chikou];
    let names = ["tenkan", "kijun", "senkou_a", "senkou_b", "chikou"];
    for (tick, index) in ticks_of(&bars).into_iter().zip(0..bars.len()) {
        let tick_out = ichimoku
            .update_full(&mut state, tick, &ichimoku_params())
            .expect("valid");
        assert_eq!(&tick_out.primary, &series[0][index], "tenkan fold");
        for (name, leg) in names[1..].iter().zip(series[1..].iter()) {
            let got = tick_out
                .secondary
                .iter()
                .find(|v| &v.name == name)
                .and_then(|v| v.value);
            assert_eq!(got, leg[index], "{name} fold at {index}");
        }
    }
}

fn sar_params() -> ParamSet {
    ParamSet::new()
        .with_float("start", 0.02)
        .with_float("increment", 0.02)
        .with_float("maximum", 0.2)
}

use rust_decimal::prelude::ToPrimitive as _;

fn rising_bars(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let close = 100.0 + i as f64 * 0.5;
            bar_at(close - 0.2, close + 1.0, close - 1.0, close, 1_000, i)
        })
        .collect()
}

#[test]
fn sar_seeds_and_trails_a_clean_uptrend() {
    let bars = rising_bars(30);
    let data = OwnedOhlcv::from_bars(&bars);
    let sar = PsarIndicator::new();
    assert_eq!(sar.warmup(&sar_params()).expect("valid"), 2);
    let result = sar.batch(&data.views(), &sar_params()).expect("valid");
    let trend = result.secondary("trend").expect("trend leg");
    assert!(
        result.values[0].is_none() && trend[0].is_none(),
        "first bar seeds nothing"
    );
    // Seed: uptrend off rising closes, stop at the prior low (99.0).
    assert_eq!(result.values[1], Some(99.0), "seed");
    assert_eq!(trend[1], Some(1.0), "seed trend");
    for (i, ((s, t), _)) in result
        .values
        .iter()
        .zip(trend.iter())
        .zip(bars.iter())
        .enumerate()
        .skip(1)
    {
        let (s, t) = (s.expect("defined"), t.expect("defined"));
        assert!((t - 1.0).abs() == 0.0, "trend holds up at {i}");
        let low = 100.0 + i as f64 * 0.5 - 1.0;
        assert!(s <= low, "trail rides under price at {i}: {s} vs {low}");
    }
    assert!(
        result.values[29].expect("defined") > result.values[1].expect("defined"),
        "rising extremes lift the trail"
    );
    let mut state = IndicatorState::new();
    for (tick, (want_sar, want_trend)) in ticks_of(&bars)
        .into_iter()
        .zip(result.values.iter().zip(trend.iter()))
    {
        let tick_out = sar
            .update_full(&mut state, tick, &sar_params())
            .expect("valid");
        assert_eq!(&tick_out.primary, want_sar, "sar agreement");
        let got_trend = tick_out
            .secondary
            .iter()
            .find(|v| v.name == "trend")
            .and_then(|v| v.value);
        assert_eq!(got_trend, *want_trend, "trend agreement");
    }
}

#[test]
fn sar_reverses_on_a_break_and_holds_bearish() {
    let mut bars = rising_bars(12);
    for i in 0..6 {
        let close = 105.5 - i as f64 * 3.0;
        bars.push(bar_at(
            close + 0.2,
            close + 1.0,
            close - 1.0,
            close,
            1_000,
            12 + i,
        ));
    }
    let data = OwnedOhlcv::from_bars(&bars);
    let sar = PsarIndicator::new();
    let result = sar.batch(&data.views(), &sar_params()).expect("valid");
    let trend = result.secondary("trend").expect("trend leg");
    let flips: Vec<usize> = trend
        .iter()
        .zip(trend.iter().skip(1))
        .enumerate()
        .filter(|(_, (a, b))| matches!((a, b), (Some(1.0), Some(-1.0))))
        .map(|(i, _)| i + 1)
        .collect();
    assert_eq!(
        flips.len(),
        1,
        "exactly one bull-to-bear flip, got {flips:?}"
    );
    let flip = flips[0];
    assert!(flip >= 12, "flip fires inside the breakdown");
    for (i, ((s, t), bar)) in result
        .values
        .iter()
        .zip(trend.iter())
        .zip(bars.iter())
        .enumerate()
        .skip(flip)
    {
        let (s, t) = (s.expect("defined"), t.expect("defined"));
        assert!((t + 1.0).abs() == 0.0, "trend holds down at {i}");
        let high = bar.h.to_f64().expect("finite");
        assert!(s >= high, "bearish trail rides over price at {i}");
    }
    let mut state = IndicatorState::new();
    for (tick, (want_sar, want_trend)) in ticks_of(&bars)
        .into_iter()
        .zip(result.values.iter().zip(trend.iter()))
    {
        let tick_out = sar
            .update_full(&mut state, tick, &sar_params())
            .expect("valid");
        assert_eq!(&tick_out.primary, want_sar, "reversal sar agreement");
        let got_trend = tick_out
            .secondary
            .iter()
            .find(|v| v.name == "trend")
            .and_then(|v| v.value);
        assert_eq!(got_trend, *want_trend, "reversal trend agreement");
    }
}

#[test]
fn sar_hole_pauses_and_resumes_without_failing() {
    let bars = rising_bars(10);
    let mut data = OwnedOhlcv::from_bars(&bars);
    data.high[5] = f64::NAN;
    let mut ticks = ticks_of(&bars);
    ticks[5].high = f64::NAN;
    let sar = PsarIndicator::new();
    let result = sar.batch(&data.views(), &sar_params()).expect("valid");
    assert_eq!(result.values.len(), bars.len());
    assert!(result.values[5].is_none(), "hole emits None");
    assert!(
        result.secondary("trend").expect("trend")[5].is_none(),
        "hole trend None"
    );
    assert!(
        result.values[4].is_some() && result.values[6].is_some(),
        "trail resumes"
    );
    let mut state = IndicatorState::new();
    for (tick, want) in ticks.into_iter().zip(result.values.iter()) {
        assert_eq!(
            &sar.update(&mut state, tick, &sar_params()).expect("valid"),
            want,
            "hole agreement"
        );
    }
}

#[test]
fn sar_refuses_bad_acceleration() {
    let sar = PsarIndicator::new();
    let bad = [
        ParamSet::new()
            .with_float("start", 0.0)
            .with_float("increment", 0.02)
            .with_float("maximum", 0.2),
        ParamSet::new()
            .with_float("start", 0.02)
            .with_float("increment", -0.01)
            .with_float("maximum", 0.2),
        ParamSet::new()
            .with_float("start", 0.3)
            .with_float("increment", 0.02)
            .with_float("maximum", 0.2),
    ];
    for ps in bad {
        assert!(sar.warmup(&ps).is_err(), "bad shape refuses");
    }
}

fn bits_of(values: &[Option<f64>]) -> Vec<Option<u64>> {
    values.iter().map(|v| v.map(f64::to_bits)).collect()
}

#[test]
fn buffered_update_matches_batch_on_constant_and_subnormal_inputs() {
    // Bit-exactness pin for the scratch migration, over the band-midpoint
    // representative (mean plus scaled deviation must agree exactly).
    let bbands = BbandsIndicator::new();
    let params = bbands_params();
    for closes in [
        vec![42.0; 40],
        (1..=40).map(|i| f64::from(i) * 1e-310).collect::<Vec<_>>(),
    ] {
        let batch = bbands
            .batch(&Ohlcv::close_only(&closes), &params)
            .expect("batch runs")
            .values;
        let mut state = IndicatorState::new();
        let folded: Vec<Option<f64>> = closes
            .iter()
            .map(|close| {
                bbands
                    .update_full(&mut state, BarTick::close_only(*close), &params)
                    .expect("update runs")
                    .primary
            })
            .collect();
        assert_eq!(
            bits_of(&folded),
            bits_of(&batch),
            "bbands reproduces batch bit-for-bit"
        );
    }
}
