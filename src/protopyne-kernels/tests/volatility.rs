use super::*;
use crate::indicator::builtin_registry;
use crate::parity::{assert_expr_parity, expr_fixture};
use crate::volatility::{
    AtrIndicator, BbIndicator, KcIndicator, NatrIndicator, SupertrendIndicator, TrIndicator,
};
use polars::prelude::{col, lit, when, EWMOptions, Expr};
use protopyne_types::Bar;
use std::path::PathBuf;

fn approx(got: &[Option<f64>], want: &[Option<f64>], eps: f64) {
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(want.iter()) {
        match (g, w) {
            (None, None) => {}
            (Some(g), Some(w)) => assert!((g - w).abs() <= eps, "{g} vs {w}"),
            _ => panic!("na mismatch: {g:?} vs {w:?}"),
        }
    }
}

fn ohlcv<'a>(high: &'a [f64], low: &'a [f64], close: &'a [f64]) -> Ohlcv<'a> {
    Ohlcv {
        open: &[],
        high,
        low,
        close,
        volume: &[],
    }
}

fn ticks(high: &[f64], low: &[f64], close: &[f64]) -> Vec<BarTick> {
    high.iter()
        .zip(low.iter())
        .zip(close.iter())
        .map(|((&h, &l), &c)| BarTick {
            open: c,
            high: h,
            low: l,
            close: c,
            volume: 0.0,
        })
        .collect()
}

#[test]
fn true_range_uses_prev_close_gap_by_hand() {
    let tr = TrIndicator::new();
    let data = ohlcv(&[10.0, 11.0], &[9.0, 9.5], &[8.0, 10.0]);
    let result = tr.batch(&data, &ParamSet::new()).expect("valid");
    // Bar 0: 10 - 9 = 1. Bar 1: max(1.5, |11-8|, |9.5-8|) = 3.
    assert_eq!(result.values, vec![Some(1.0), Some(3.0)]);
    let mut state = IndicatorState::new();
    let mut folded = Vec::new();
    for tick in ticks(&[10.0, 11.0], &[9.0, 9.5], &[8.0, 10.0]) {
        folded.push(
            tr.update(&mut state, tick, &ParamSet::new())
                .expect("valid"),
        );
    }
    assert_eq!(result.values, folded);
}

#[test]
fn atr_smooths_ranges_by_hand() {
    let atr = AtrIndicator::new();
    // Ranges [1, 3, 2] (see tr test plus a 2-point third bar), length 2:
    // seed = 2, then (2 + 2)/2 = 2.
    let data = ohlcv(&[10.0, 11.0, 12.0], &[9.0, 9.5, 10.0], &[8.0, 10.0, 11.0]);
    let ps = ParamSet::new().with_int("length", 2);
    let result = atr.batch(&data, &ps).expect("valid");
    assert_eq!(result.values, vec![None, Some(2.0), Some(2.0)]);
    let mut state = IndicatorState::new();
    let mut folded = Vec::new();
    for tick in ticks(&[10.0, 11.0, 12.0], &[9.0, 9.5, 10.0], &[8.0, 10.0, 11.0]) {
        folded.push(atr.update(&mut state, tick, &ps).expect("valid"));
    }
    assert_eq!(result.values, folded);
}

fn trending_legs(bars: usize) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let close: Vec<f64> = (0..bars)
        .map(|i| 100.0 + i as f64 * 0.05 + ((i * 37 % 11) as f64) * 0.13)
        .collect();
    let high: Vec<f64> = close.iter().map(|c| c + 0.4).collect();
    let low: Vec<f64> = close.iter().map(|c| c - 0.4).collect();
    (high, low, close)
}

fn max_abs_diff(a: &[Option<f64>], b: &[Option<f64>]) -> f64 {
    a.iter()
        .zip(b.iter())
        .map(|pair| match pair {
            (Some(x), Some(y)) => (x - y).abs(),
            (None, None) => 0.0,
            _ => f64::INFINITY,
        })
        .fold(0.0, f64::max)
}

#[test]
fn atr_windowed_plus_warmup_converges_to_full_history_over_visible_slice() {
    // Same recursive-seeding transient as EMA (ATR smooths through the
    // recursive RMA): the declared warmup buys convergence, not coincidence —
    // see the EMA pin for the mechanism. The terminal keeps full-history
    // evaluation (today's exact values, no regression).
    let atr = AtrIndicator::new();
    let ps = ParamSet::new().with_int("length", 14);
    let warmup = atr.warmup(&ps).expect("declared warmup");
    let (high, low, close) = trending_legs(500);
    let frame = || ohlcv(&high, &low, &close);
    let full = atr.batch(&frame(), &ps).expect("full history");
    let visible_start: usize = 400;
    let windowed_at = |extra: usize| {
        let eval_start = visible_start.saturating_sub(warmup + extra);
        atr.batch(
            &ohlcv(
                &high[eval_start..],
                &low[eval_start..],
                &close[eval_start..],
            ),
            &ps,
        )
        .expect("windowed")
    };
    let one = windowed_at(0);
    let four = windowed_at(3 * warmup);
    let end = close.len() - visible_start;
    let diff_one = max_abs_diff(&one.values[warmup..][..end], &full.values[visible_start..]);
    let diff_four = max_abs_diff(
        &four.values[3 * warmup + warmup..][..end],
        &full.values[visible_start..],
    );
    assert!(
        diff_one <= 0.05,
        "declared-warmup transient stays small on the fixture: {diff_one}"
    );
    assert!(
        diff_four * 8.0 <= diff_one,
        "tripling the warmup shrinks the transient geometrically: {diff_four} vs {diff_one}"
    );
    let first = (one.values[warmup].unwrap() - full.values[visible_start].unwrap()).abs();
    assert!(
        (first - diff_one).abs() <= 1e-12,
        "the transient peaks at the window head: {first} vs max {diff_one}"
    );
}

#[test]
fn bollinger_bands_use_population_stdev_by_hand() {
    let bb = BbIndicator::new();
    // mean 3, biased dev sqrt(2/3): sample stdev (1.0) would widen the bands.
    let data = [1.0, 2.0, 3.0, 4.0, 5.0];
    let ps = ParamSet::new()
        .with_int("length", 3)
        .with_float("mult", 2.0);
    let result = bb.batch(&Ohlcv::close_only(&data), &ps).expect("valid");
    let dev = (2.0_f64 / 3.0).sqrt();
    approx(&result.values[2..3], &[Some(2.0)], 1e-9);
    let upper = result.secondary("upper").expect("upper leg");
    let lower = result.secondary("lower").expect("lower leg");
    approx(&upper[2..3], &[Some(2.0 + 2.0 * dev)], 1e-9);
    approx(&lower[2..3], &[Some(2.0 - 2.0 * dev)], 1e-9);
    let mut state = IndicatorState::new();
    let mut primary = Vec::new();
    let mut upper_fold = Vec::new();
    let mut lower_fold = Vec::new();
    for &c in &data {
        let full = bb
            .update_full(&mut state, BarTick::close_only(c), &ps)
            .expect("valid");
        primary.push(full.primary);
        upper_fold.push(full.secondary[0].value);
        lower_fold.push(full.secondary[1].value);
    }
    assert_eq!(result.values, primary);
    assert_eq!(upper, &upper_fold);
    assert_eq!(lower, &lower_fold);
}

#[test]
fn keltner_channels_pair_basis_with_smoothed_range_by_hand() {
    let kc = KcIndicator::new();
    // Ranges all 2 (high-low with no gaps), length 3, mult 1:
    // basis[2] = 2, span[2] = 2 -> upper 4, lower 0.
    let data = ohlcv(
        &[2.0, 3.0, 4.0, 5.0, 6.0],
        &[0.0, 1.0, 2.0, 3.0, 4.0],
        &[1.0, 2.0, 3.0, 4.0, 5.0],
    );
    let ps = ParamSet::new()
        .with_int("length", 3)
        .with_float("mult", 1.0);
    let result = kc.batch(&data, &ps).expect("valid");
    approx(&result.values[2..3], &[Some(2.0)], 1e-9);
    let upper = result.secondary("upper").expect("upper leg");
    let lower = result.secondary("lower").expect("lower leg");
    approx(&upper[2..3], &[Some(4.0)], 1e-9);
    approx(&lower[2..3], &[Some(0.0)], 1e-9);
}

fn natr_bar(open: f64, high: f64, low: f64, close: f64, index: usize) -> Bar {
    use chrono::TimeZone as _;
    let dec = |x: f64| rust_decimal::Decimal::from_f64_retain(x).expect("finite test price");
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
        .volume(1_000_u64)
        .maybe_adj(None)
        .build()
}

fn natr_ticks_of(bars: &[Bar]) -> Vec<BarTick> {
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

fn natr_spy_bars(count: usize) -> Vec<Bar> {
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
        let dec = |x: f64| rust_decimal::Decimal::from_f64_retain(x).expect("finite");
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

/// Independent reference for the volatility ratio: EWM-seeded smoothing over
/// the bar range, divided by close — the reference formula. The horizontal
/// maximum is a null-safe `when` ladder: shifted legs are null on the first
/// bar, so they enter the ladder as negative infinity (a true range never
/// reads below it) instead of poisoning the comparison.
fn natr_expr(n: usize) -> Expr {
    let prev_close = col("close").shift(lit(1));
    let tr1 = col("high") - col("low");
    let neg_inf = lit(f64::NEG_INFINITY);
    let tr2 = (col("high") - prev_close.clone())
        .abs()
        .fill_null(neg_inf.clone());
    let tr3 = (col("low") - prev_close).abs().fill_null(neg_inf);
    let m12 = when(tr1.clone().gt_eq(tr2.clone()))
        .then(tr1)
        .otherwise(tr2);
    let tr = when(m12.clone().gt_eq(tr3.clone()))
        .then(m12)
        .otherwise(tr3);
    let atr = tr.ewm_mean(EWMOptions {
        alpha: 1.0 / n as f64,
        adjust: false,
        bias: false,
        min_periods: n,
        ignore_nulls: true,
    });
    (atr / col("close")).alias("natr")
}

#[test]
fn natr_flat_zero_is_none_not_zero() {
    // Degenerate zero prices: 0/0 refuses via the division guard instead of
    // fabricating a ratio.
    let bars: Vec<Bar> = (0..30).map(|i| natr_bar(0.0, 0.0, 0.0, 0.0, i)).collect();
    let data = crate::indicator::OwnedOhlcv::from_bars(&bars);
    let ps = ParamSet::new().with_int("length", 14);
    let natr = NatrIndicator::new();
    let result = natr.batch(&data.views(), &ps).expect("valid");
    assert!(result.values.iter().all(Option::is_none));
    let mut state = IndicatorState::new();
    for tick in natr_ticks_of(&bars) {
        assert_eq!(natr.update(&mut state, tick, &ps).expect("valid"), None);
    }
}

#[test]
fn natr_flat_price_is_confident_zero_after_warmup() {
    // A genuinely still market has a true zero ratio — confident and correct,
    // distinct from the degenerate refusal above.
    let bars: Vec<Bar> = (0..30)
        .map(|i| natr_bar(100.0, 100.0, 100.0, 100.0, i))
        .collect();
    let data = crate::indicator::OwnedOhlcv::from_bars(&bars);
    let ps = ParamSet::new().with_int("length", 14);
    let natr = NatrIndicator::new();
    assert_eq!(natr.warmup(&ps).expect("valid"), 14);
    let result = natr.batch(&data.views(), &ps).expect("valid");
    assert!(result.values[..13].iter().all(Option::is_none));
    assert!(result.values[13..].iter().all(|v| *v == Some(0.0)));
}

#[test]
fn natr_matches_expr_once_seeded_and_agrees_on_spy() {
    // The kernel smooths with Wilder seeding while the reference smooths
    // with EWM seeding: the two coincide exactly once their seed states
    // match, so the Expr-parity frame opens with a flat prefix (both seeds read
    // zero) before trending. Seeding, not smoothing, is the documented
    // difference between the two ratio definitions.
    let mut bars: Vec<Bar> = (0..14)
        .map(|i| natr_bar(100.0, 100.0, 100.0, 100.0, i))
        .collect();
    for i in 0..30 {
        let close = 100.0 + i as f64 * 0.9 + ((i * 7) % 4) as f64;
        bars.push(natr_bar(
            close - 0.2,
            close + 0.9,
            close - 0.7,
            close,
            14 + i,
        ));
    }
    let data = crate::indicator::OwnedOhlcv::from_bars(&bars);
    let ps = ParamSet::new().with_int("length", 14);
    let natr = NatrIndicator::new();
    let result = natr.batch(&data.views(), &ps).expect("valid");
    let want = expr_fixture(&bars, natr_expr(14)).expect("natr evaluates");
    assert_expr_parity(&result.values, &want, 1e-9, "natr").expect("parity");
    // Streaming agrees with batch on real dynamics, including secondaries
    // (none here) and warmup shape.
    let spy = natr_spy_bars(60);
    let spy_data = crate::indicator::OwnedOhlcv::from_bars(&spy);
    let spy_result = natr.batch(&spy_data.views(), &ps).expect("valid");
    assert!(spy_result.values[..13].iter().all(Option::is_none));
    let mut state = IndicatorState::new();
    let folded: Vec<Option<f64>> = natr_ticks_of(&spy)
        .into_iter()
        .map(|t| natr.update(&mut state, t, &ps).expect("valid"))
        .collect();
    assert_eq!(spy_result.values, folded);
}

fn supertrend_params() -> ParamSet {
    ParamSet::new()
        .with_int("atr_length", 2)
        .with_float("multiplier", 1.0)
}

#[test]
fn supertrend_matches_hand_ratchet() {
    // Constant TR of 2.0 seeds ATR at the mean; basic bands hug HL2 ± 2
    // and the line ratchets the lower band while bullish.
    let high = vec![10.0, 11.0, 12.0, 11.0];
    let low = vec![8.0, 9.0, 10.0, 9.0];
    let close = vec![9.0, 10.0, 11.0, 9.5];
    let data = ohlcv(&high, &low, &close);
    let st = SupertrendIndicator::new();
    assert_eq!(st.warmup(&supertrend_params()).expect("valid"), 2);
    let result = st.batch(&data, &supertrend_params()).expect("valid");
    let dir = result.secondary("direction").expect("direction leg");
    assert_eq!(result.values.len(), 4);
    assert!(result.values[0].is_none() && dir[0].is_none(), "ATR warms");
    for (got, want) in result.values[1..].iter().zip([8.0, 9.0, 9.0]) {
        assert_eq!(*got, Some(want), "hand line");
    }
    assert!(
        dir[1..].iter().all(|v| *v == Some(1.0)),
        "bullish throughout"
    );
    let mut state = IndicatorState::new();
    for (tick, (want_line, want_dir)) in ticks(&high, &low, &close)
        .into_iter()
        .zip(result.values.iter().zip(dir.iter()))
    {
        let tick_out = st
            .update_full(&mut state, tick, &supertrend_params())
            .expect("valid");
        assert_eq!(&tick_out.primary, want_line, "line agreement");
        let got_dir = tick_out
            .secondary
            .iter()
            .find(|v| v.name == "direction")
            .and_then(|v| v.value);
        assert_eq!(got_dir, *want_dir, "direction agreement");
    }
}

#[test]
fn supertrend_flips_bearish_through_the_lower_band() {
    // Extend the hand fixture with a breakdown: the close under the held
    // lower band flips the line to the upper band.
    let high = vec![10.0, 11.0, 12.0, 11.0, 9.0, 8.0];
    let low = vec![8.0, 9.0, 10.0, 9.0, 7.0, 6.0];
    let close = vec![9.0, 10.0, 11.0, 9.5, 7.5, 6.5];
    let data = ohlcv(&high, &low, &close);
    let st = SupertrendIndicator::new();
    let result = st.batch(&data, &supertrend_params()).expect("valid");
    let dir = result.secondary("direction").expect("direction leg");
    assert_eq!(dir[4], Some(-1.0), "breakdown flips bearish");
    assert_eq!(dir[5], Some(-1.0), "bearish holds");
    let line4 = result.values[4].expect("defined");
    assert!(
        (line4 - 10.25).abs() < 1e-9,
        "flip rides the upper band, got {line4}"
    );
    assert!(
        result.values[5].expect("defined") >= high[5],
        "bearish line stays over price"
    );
    let mut state = IndicatorState::new();
    for (tick, (want_line, want_dir)) in ticks(&high, &low, &close)
        .into_iter()
        .zip(result.values.iter().zip(dir.iter()))
    {
        let tick_out = st
            .update_full(&mut state, tick, &supertrend_params())
            .expect("valid");
        assert_eq!(&tick_out.primary, want_line, "flip agreement");
        let got_dir = tick_out
            .secondary
            .iter()
            .find(|v| v.name == "direction")
            .and_then(|v| v.value);
        assert_eq!(got_dir, *want_dir, "flip direction agreement");
    }
}

#[test]
fn supertrend_refuses_bad_shape() {
    let st = SupertrendIndicator::new();
    assert!(
        st.warmup(
            &ParamSet::new()
                .with_int("atr_length", 0)
                .with_float("multiplier", 3.0)
        )
        .is_err(),
        "zero length refuses"
    );
    assert!(
        st.warmup(
            &ParamSet::new()
                .with_int("atr_length", 10)
                .with_float("multiplier", -1.0)
        )
        .is_err(),
        "negative multiplier refuses"
    );
    assert!(
        st.warmup(
            &ParamSet::new()
                .with_int("atr_length", 10)
                .with_float("multiplier", f64::NAN)
        )
        .is_err(),
        "NaN multiplier refuses"
    );
}

#[test]
fn supertrend_registry_and_matrix_cover_new_row() {
    let registry = builtin_registry().expect("builtins");
    assert!(
        registry.get("supertrend").is_some(),
        "supertrend registered"
    );
    let row = crate::compat::compat_for("supertrend").expect("matrix row");
    assert!((row.eps - 1e-9).abs() == 0.0, "frozen eps");
}
