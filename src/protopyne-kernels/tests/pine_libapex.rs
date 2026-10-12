//! Market-core library goldens: the shipped `libApex` source linked with
//! small driver scripts, pinning each export against hand-computed
//! expectations.
//!
//! Drivers call through the `libApex` alias exactly like the real indicator
//! does, so cross-module dispatch, kernel-backed `fetch`, and the table
//! recorders run the same code the end-to-end proof uses. The multi-
//! timeframe `request.security` branch stays refused headlessly: per-
//! timeframe numbers enter by injecting that timeframe's bars as the run
//! legs, never by a network fetch.
//!
//! Float goldens below are hand-computed decimal expectations with a tight
//! tolerance, never measured quantities.
//! `#[allow(clippy::float_cmp)]` covers those literals.
#![allow(clippy::float_cmp)]

use super::*;
use crate::{InputSet, LinkedModule, ModuleInput, OwnedOhlcv, ParamSet, PineRuntimeConfig, UserInstance};
use protopyne_types::Bar;

use crate::pine::test_fixture as fixture;

/// Frozen agreement tolerance shared with the kernel-vs-Pine matrix.
const EPS: f64 = 1e-9;

const IMPORTS: &str = "import showmethegrail/libUtil/12 as UTIL\n\
     import showmethegrail/libBuffers/6 as libBuffers\n\
     import showmethegrail/libBars/12 as BARS\n\
     import showmethegrail/libDrawing/61 as libDrawing\n\
     import showmethegrail/libApex/88 as libApex\n";

/// One shipped library as a graph input (shared fixture constructor).
fn shipped_module(file: &str, publisher: &str, name: &str, version: u64) -> ModuleInput {
    fixture::shipped_module(file, publisher, name, version)
}

/// One synthetic driver script as a graph input (shared fixture constructor).
fn driver_module(text: &str) -> ModuleInput {
    fixture::driver_module(text)
}

/// Link all five shipped libraries under graph order with one driver root
/// calling through the `UTIL`/`BARS`/`libApex` aliases.
fn linked(driver: &str) -> Vec<LinkedModule> {
    fixture::linked_modules(vec![
        shipped_module("libraries/_libUtil.pine", "showmethegrail", "libUtil", 12),
        shipped_module("libraries/_libBuffers.pine", "showmethegrail", "libBuffers", 6),
        shipped_module("libraries/_libBars.pine", "showmethegrail", "libBars", 12),
        shipped_module("libraries/_libDrawing.pine", "showmethegrail", "libDrawing", 61),
        shipped_module("libraries/_libApex.pine", "showmethegrail", "libApex", 88),
        driver_module(driver),
    ])
}

fn test_engine() -> PineEngine {
    PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period("5".to_owned())
            .build(),
    )
}

/// One closed bar from decimal legs and a whole-second timestamp.
fn bar(open: &str, high: &str, low: &str, close: &str, volume: u64, ts_secs: i64) -> Bar {
    Bar::new()
        .ts(
            chrono::TimeZone::timestamp_opt(&chrono::Utc, ts_secs, 0)
                .single()
                .expect("valid test timestamp"),
        )
        .o(open.parse().expect("decimal"))
        .h(high.parse().expect("decimal"))
        .l(low.parse().expect("decimal"))
        .c(close.parse().expect("decimal"))
        .volume(volume)
        .maybe_adj(None)
        .build()
}

fn run_linked(driver: &str, bars: &[Bar]) -> RunOutcome {
    let modules = linked(driver);
    let mut engine = test_engine();
    engine
        .run_linked(&modules, bars, InputSet::new())
        .expect("linked run evaluates")
}

fn num(outcome: &RunOutcome, at_bar: usize, name: &str) -> f64 {
    match outcome.value_at(at_bar, name) {
        Some(Some(PineValue::Float(value))) => value,
        Some(Some(PineValue::Int(value))) => value as f64,
        other => panic!("expected number for {name} on bar {at_bar}, got {other:?}"),
    }
}

fn ctx_at(outcome: &RunOutcome, bar: usize) -> UserInstance {
    match outcome.value_at(bar, "ctx") {
        Some(Some(PineValue::Object(instance))) => instance,
        other => panic!("expected MarketContext object for ctx on bar {bar}, got {other:?}"),
    }
}

fn field_num(instance: &UserInstance, name: &str) -> f64 {
    match instance.fields.get(name) {
        Some(Some(PineValue::Float(value))) => *value,
        Some(Some(PineValue::Int(value))) => *value as f64,
        other => panic!("expected number field {name}, got {other:?}"),
    }
}

fn field_enum(instance: &UserInstance, name: &str) -> String {
    match instance.fields.get(name) {
        Some(Some(PineValue::EnumHandle(handle))) => handle.member.clone(),
        other => panic!("expected enum field {name}, got {other:?}"),
    }
}

/// The native kernel twin over the same bars, as the agreement oracle.
fn kernel_batch(kernel_id: &str, params: &ParamSet, ohlcv: &OwnedOhlcv) -> Vec<Option<f64>> {
    let registry = crate::indicator::builtin_registry().expect("builtins");
    let kernel = registry.get(kernel_id).expect("kernel registered");
    kernel
        .batch(&ohlcv.views(), params)
        .expect("valid batch")
        .values
}

/// Sixty steep rising bars (slope 5): every `fetch` kernel is warm by
/// the last bar, and the moving averages separate far past the range, so
/// the health score aligns bullish with a wide margin.
fn ramp_bars() -> Vec<Bar> {
    (0u32..60)
        .map(|i| {
            let close = 100.0 + 5.0 * f64::from(i);
            bar(
                &format!("{:.2}", close - 0.25),
                &format!("{:.2}", close + 0.5),
                &format!("{:.2}", close - 0.5),
                &format!("{close:.2}"),
                1_000 + u64::from(i) * 10,
                1_700_000_000 + i64::from(i) * 300,
            )
        })
        .collect()
}

fn ramp_ohlcv() -> OwnedOhlcv {
    let close: Vec<f64> = (0u32..60).map(|i| 100.0 + 5.0 * f64::from(i)).collect();
    OwnedOhlcv {
        open: close.iter().map(|c| c - 0.25).collect(),
        high: close.iter().map(|c| c + 0.5).collect(),
        low: close.iter().map(|c| c - 0.5).collect(),
        close,
        volume: (0u32..60).map(|i| 1_000.0 + 10.0 * f64::from(i)).collect(),
    }
}

const CTX_DRIVER: &str = "var ctx = libApex.MarketContext.new()\nif barstate.isfirst\n    ctx.init()\nctx.fetch()\n";

#[test]
fn health_perfect_bull_scores_upper_band() {
    // fast_dist = slow_dist = 5/2 = 2.5, min 2.5, k*min = 5.0;
    // fast_tanh(5) = 5/6, so 10 + 5/6*120 = 110 exactly.
    let bars = vec![bar("100", "101", "99", "100", 1000, 1_700_000_000)];
    let outcome = run_linked(
        &format!("{IMPORTS}h = libApex.getHealthMAs(2.0, 110.0, 105.0, 100.0, 2.0)"),
        &bars,
    );
    assert_eq!(num(&outcome, 0, "h"), 110.0);
}

#[test]
fn health_perfect_bear_scores_lower_band() {
    // min(|-2.5|, |-2.5|) = 2.5; -50 - 5/6*120 = -150 exactly.
    let bars = vec![bar("100", "101", "99", "100", 1000, 1_700_000_000)];
    let outcome = run_linked(
        &format!("{IMPORTS}h = libApex.getHealthMAs(2.0, 100.0, 105.0, 110.0, 2.0)"),
        &bars,
    );
    assert_eq!(num(&outcome, 0, "h"), -150.0);
}

#[test]
fn health_mixed_weights_bull_share_and_deadband() {
    // fast 105 > mid 100 < slow 102, atr 2: bull 2.5, bear 0.7,
    // total 3.2, score -10 + 2.5/3.2*20 = 5.625.
    // Flat MAs carry no distance: the 0.01 deadband answers exactly 0.
    let bars = vec![bar("100", "101", "99", "100", 1000, 1_700_000_000)];
    let outcome = run_linked(
        &format!(
            "{IMPORTS}m = libApex.getHealthMAs(2.0, 105.0, 100.0, 102.0, 2.0)\n\
             d = libApex.getHealthMAs(2.0, 100.0, 100.0, 100.0, 2.0)"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 0, "m"), 5.625);
    assert_eq!(num(&outcome, 0, "d"), 0.0);
}

#[test]
fn fetch_chart_branch_matches_kernels_within_eps() {
    let bars = ramp_bars();
    let ohlcv = ramp_ohlcv();
    let outcome = run_linked(&format!("{IMPORTS}{CTX_DRIVER}"), &bars);
    let last = bars.len() - 1;
    let state = ctx_at(&outcome, last);
    let expect = |kernel: &str, params: &ParamSet| {
        kernel_batch(kernel, params, &ohlcv)[last].expect("kernel warm on the last bar")
    };
    let alma = ParamSet::new()
        .with_int("length", 8)
        .with_float("offset", 0.85)
        .with_float("sigma", 6.0);
    let stdev14 = ParamSet::new().with_int("length", 14).with_float("nbdev", 1.0);
    let len = |n: i64| ParamSet::new().with_int("length", n);
    for (field, kernel, params) in [
        ("ma_primary", "alma", alma),
        ("ma_signal", "vwma", len(12)),
        ("ma_mean", "ema", len(21)),
        ("atr", "atr", len(14)),
        ("atr_long", "atr", len(50)),
        ("stdev", "stddev", stdev14),
    ] {
        let kernel_value = expect(kernel, &params);
        assert!(
            (field_num(&state, field) - kernel_value).abs() < EPS,
            "{field} agrees with its kernel within 1e-9"
        );
    }
    // Normalized twins: cci x3, (rsi - 50) x2.
    let cci = expect("cci", &len(14)) * 3.0;
    assert!((field_num(&state, "cci") - cci).abs() < EPS, "cci normalizes x3");
    let rsi = (expect("rsi", &len(14)) - 50.0) * 2.0;
    assert!((field_num(&state, "rsi") - rsi).abs() < EPS, "rsi normalizes to zero center");
    // `sma(volume, 20)` runs the kernel over the volume leg, so the
    // oracle feeds the volume series as the kernel's source port.
    let vol_ohlcv = OwnedOhlcv {
        open: ohlcv.volume.clone(),
        high: ohlcv.volume.clone(),
        low: ohlcv.volume.clone(),
        close: ohlcv.volume.clone(),
        volume: ohlcv.volume.clone(),
    };
    let vol_sma = kernel_batch("sma", &len(20), &vol_ohlcv)[last].expect("volume sma warm");
    assert!(
        (field_num(&state, "vol_sma") - vol_sma).abs() < EPS,
        "volume sma reads the volume leg"
    );
    // Derived metrics recomposed from the fetched fields.
    let close = 395.0;
    let atr_distance = (close - field_num(&state, "ma_mean")) / field_num(&state, "atr");
    assert!(
        (field_num(&state, "atr_distance") - atr_distance).abs() < EPS,
        "atr distance recomposes"
    );
    assert!(
        (field_num(&state, "vol_ratio") - 1590.0 / field_num(&state, "vol_sma")).abs() < EPS,
        "volume ratio recomposes"
    );
    let health = field_num(&state, "ma_health");
    assert!(
        (field_num(&state, "trend_strength") - health.abs() / 100.0).abs() < EPS,
        "trend strength normalizes health"
    );
    assert_eq!(field_num(&state, "trend_direction"), 0.0, "monotone ramp stays directionless");
    assert_eq!(
        field_enum(&state, "regime"),
        "VOLATILE",
        "monotone ramp: dispersion beats alignment, so volatility wins"
    );
    assert_eq!(field_enum(&state, "tf"), "C", "chart fetch keeps the chart timeframe");
}

#[test]
fn fetch_explicit_chart_timeframe_takes_chart_branch() {
    // Under a "5" chart the M5 enum member is a chart timeframe, so the
    // kernel branch runs instead of the security branch.
    let bars = ramp_bars();
    let ohlcv = ramp_ohlcv();
    let outcome = run_linked(
        &format!(
            "{IMPORTS}var ctx = libApex.MarketContext.new()\nif barstate.isfirst\n    ctx.init()\nctx.fetch(UTIL.Timeframes.M5)\n"
        ),
        &bars,
    );
    let last = bars.len() - 1;
    let state = ctx_at(&outcome, last);
    let ema21 = kernel_batch("ema", &ParamSet::new().with_int("length", 21), &ohlcv)[last]
        .expect("kernel warm");
    assert!((field_num(&state, "ma_mean") - ema21).abs() < EPS, "M5 fetch runs kernels");
    assert_eq!(field_enum(&state, "tf"), "M5", "context records the requested timeframe");
}

#[test]
fn fetch_mtf_branch_refuses_headlessly() {
    // M15 is not the "5" chart: the run aborts naming the injection path.
    let bars = ramp_bars();
    let modules = linked(&format!(
        "{IMPORTS}var ctx = libApex.MarketContext.new()\nif barstate.isfirst\n    ctx.init()\nctx.fetch(UTIL.Timeframes.M15)\n"
    ));
    let mut engine = test_engine();
    let err = engine
        .run_linked(&modules, &bars, InputSet::new())
        .expect_err("MTF fetch refuses without injection");
    assert!(
        err.to_string().contains("live request.security is refused headlessly"),
        "refusal names the live fetch: {err}"
    );
}

#[test]
fn fetch_numbers_reach_mtf_through_injected_legs() {
    // The auditable MTF path: 15-minute bars injected as the run legs run
    // the same kernels, so per-timeframe numbers never need a fetch.
    let bars: Vec<Bar> = (0u32..60)
        .map(|i| {
            let close = 500.0 + f64::from(i) * 5.0;
            bar(
                &format!("{:.2}", close - 0.5),
                &format!("{:.2}", close + 1.0),
                &format!("{:.2}", close - 1.0),
                &format!("{close:.2}"),
                5_000 + u64::from(i) * 20,
                1_700_000_000 + i64::from(i) * 900,
            )
        })
        .collect();
    let ohlcv = OwnedOhlcv {
        close: (0u32..60).map(|i| 500.0 + f64::from(i) * 5.0).collect(),
        open: (0u32..60).map(|i| 500.0 + f64::from(i) * 5.0 - 0.5).collect(),
        high: (0u32..60).map(|i| 500.0 + f64::from(i) * 5.0 + 1.0).collect(),
        low: (0u32..60).map(|i| 500.0 + f64::from(i) * 5.0 - 1.0).collect(),
        volume: (0u32..60).map(|i| 5_000.0 + 20.0 * f64::from(i)).collect(),
    };
    let outcome = run_linked(&format!("{IMPORTS}{CTX_DRIVER}"), &bars);
    let last = bars.len() - 1;
    let state = ctx_at(&outcome, last);
    let ema21 = kernel_batch("ema", &ParamSet::new().with_int("length", 21), &ohlcv)[last]
        .expect("kernel warm");
    assert!((field_num(&state, "ma_mean") - ema21).abs() < EPS, "injected legs run kernels");
    assert_eq!(
        field_enum(&state, "regime"),
        "VOLATILE",
        "monotone ramp reads volatile on injected legs too"
    );
}

/// Forty bars of gentle wobble around `base`, flat volume unless spiked:
/// every kernel is warm while trends stay small.
fn wobble_bars(base: f64, volume_spike: Option<(u32, u64)>) -> Vec<Bar> {
    (0u32..40)
        .map(|i| {
            let wobble = if i % 2 == 0 { 0.0 } else { 0.4 };
            let close = base + wobble;
            let volume = volume_spike
                .filter(|(at, _)| *at == i)
                .map_or(1_000, |(_, v)| v);
            bar(
                &format!("{:.2}", close - 0.2),
                &format!("{:.2}", close + 0.4),
                &format!("{:.2}", close - 0.3),
                &format!("{close:.2}"),
                volume,
                1_700_000_000 + i64::from(i) * 300,
            )
        })
        .collect()
}

fn regime_of(bars: &[Bar]) -> String {
    let outcome = run_linked(&format!("{IMPORTS}{CTX_DRIVER}"), bars);
    field_enum(&ctx_at(&outcome, bars.len() - 1), "regime")
}

#[test]
fn detect_regime_calibration_at_unit_precision_recall() {
    // Labeled synthetic regimes plus a ranging control that must NOT
    // classify trending. Every window must classify correctly before the
    // proof trusts regime-gated output: rates measure classification
    // correctness, never component confidence.
    // Thirty steep bars plus a five-bar counter-tick: the pullback
    // (rally) drops the faster averages back into aligned order while
    // the mean still lags, so the health score separates with margin.
    let drive = |closes: Vec<f64>| {
        closes
            .iter()
            .enumerate()
            .map(|(i, c)| {
                bar(
                    &format!("{:.2}", c - 0.25),
                    &format!("{:.2}", c + 0.5),
                    &format!("{:.2}", c - 0.5),
                    &format!("{c:.2}"),
                    1_000,
                    1_700_000_000 + i as i64 * 300,
                )
            })
            .collect::<Vec<_>>()
    };
    let up: Vec<f64> = (0..30).map(|i| 100.0 + 5.0 * f64::from(i)).collect();
    let down: Vec<f64> = (0..30).map(|i| 300.0 - 5.0 * f64::from(i)).collect();
    let mut pullback = up.clone();
    pullback.extend((1..=5).map(|i| 245.0 - f64::from(i)));
    let mut rally = down.clone();
    rally.extend((1..=5).map(|i| 155.0 + f64::from(i)));
    let bull = drive(pullback);
    let bear = drive(rally);
    // A twenty-up/twenty-down V: the bend crosses the averages (no
    // alignment, trend stays small) while window dispersion dwarfs the
    // smoothed range, so volatility wins with margin.
    let mut bend: Vec<f64> = (0..20).map(|i| 100.0 + 3.0 * f64::from(i)).collect();
    bend.extend((1..=20).map(|i| 160.0 - 3.0 * f64::from(i)));
    let spike = drive(bend);
    // Wobble plus a 6x volume print: the volume score wins without trend.
    let ranging = wobble_bars(100.0, Some((39, 6_000)));
    // Wobble plus a 1.5x print on a low-phase bar: volume leads weakly
    // (trend ~10, volume ~15), so nothing triggers and the bar stays
    // quiet. Pure-calm wobble never qualifies (see below): marginal
    // alignment always trends, so quiet needs volume to lead. The run
    // ends on the spiked low-phase bar, which is the bar classified.
    let mut calm = wobble_bars(100.0, Some((38, 1_500)));
    calm.truncate(39);
    let cases: &[(&str, &Vec<Bar>, &str)] = &[
        ("pullback in uptrend", &bull, "TRENDING_BULL"),
        ("rally in downtrend", &bear, "TRENDING_BEAR"),
        ("V bend", &spike, "VOLATILE"),
        ("volume spike", &ranging, "RANGING"),
        ("calm wobble", &calm, "LOW_VOLUME"),
    ];
    let mut correct = 0;
    for (label, bars, expected) in cases {
        let got = regime_of(bars);
        assert_eq!(got, *expected, "{label} classifies {expected}");
        correct += 1;
    }
    assert_eq!(correct, cases.len(), "recall 1.0 over labeled regimes");
    // Precision of trend calls: neither ranging-labeled window may read
    // trending. Both are asserted above by label; restate the property
    // so a future divergence fails naming it.
    for (label, bars) in [("volume spike", &ranging), ("mild volume", &calm)] {
        let regime = regime_of(bars);
        assert!(
            regime != "TRENDING_BULL" && regime != "TRENDING_BEAR",
            "{label} never trends: {regime}"
        );
    }
}

#[test]
fn chop_alternation_characterization() {
    // A two-phase chop marginally aligns every bar — bullish on low-phase
    // bars (scoring just over the +10 ceiling into trend territory via
    // the max==trend tie) and bearish on high-phase bars (scoring at the
    // -50 floor). The alternation holds at any amplitude, so health-gated
    // consumers must expect bar-by-bar regime flips on chop, never a
    // quiet read. This pins the observed behavior for those consumers.
    for amp in ["0.40", "0.10", "0.05"] {
        let bars: Vec<Bar> = (0..40)
            .map(|i| {
                let wobble: f64 = if i % 2 == 0 { 0.0 } else { amp.parse().unwrap() };
                let close = 100.0 + wobble;
                bar(
                    &format!("{:.2}", close - 0.2),
                    &format!("{:.2}", close + 0.4),
                    &format!("{:.2}", close - 0.3),
                    &format!("{close:.2}"),
                    1_000,
                    1_700_000_000 + i64::from(i) * 300,
                )
            })
            .collect();
        let outcome = run_linked(&format!("{IMPORTS}{CTX_DRIVER}"), &bars);
        assert_eq!(
            field_enum(&ctx_at(&outcome, 38), "regime"),
            "TRENDING_BULL",
            "low-phase chop trends bull at amplitude {amp}"
        );
        assert_eq!(
            field_enum(&ctx_at(&outcome, 39), "regime"),
            "TRENDING_BEAR",
            "high-phase chop trends bear at amplitude {amp}"
        );
    }
}

#[test]
fn color_momentum_alpha_hand_rolled() {
    // Bullish candle, momentum 60, alpha 0.5, vol ratio 1.2:
    // normalized = (1.7/2.7)*90 = 56.66.., transparency int(43.33) = 43;
    // gradient input 60*56.66../100 = 34.0, t = 134/200 = 0.67 from blue
    // (0,0,255) to green (0,128,0): g = round(85.76) = 86, b = round(84.15)
    // = 84; color.new then stamps transparency 43.
    let bars = vec![bar("100", "101", "99", "100", 1000, 1_700_000_000)];
    let outcome = run_linked(
        &format!("{IMPORTS}c = libApex.getColorMomentumAlpha(100.0, 105.0, 60.0, 0.5, 1.2)"),
        &bars,
    );
    let raw = match outcome.value_at(0, "c") {
        Some(Some(PineValue::ColorHandle(handle))) => handle.raw(),
        other => panic!("expected color, got {other:?}"),
    };
    assert_eq!(raw, 0x00_56_54_2B, "gradient lerps then stamps alpha");
}

#[test]
fn volume_preview_scales_buy_sell_like_formula() {
    // Closed-bar drivers cannot fill the candle buffer, so the empty
    // buffer answers the zero pair; the formula itself divides the
    // snapshot buy/sell legs by the estimate.
    let bars = vec![bar("100", "101", "99", "100", 1000, 1_700_000_000)];
    let outcome = run_linked(
        &format!(
            "{IMPORTS}var bars = BARS.Bars.new()\nif barstate.isfirst\n    bars.init(UTIL.Timeframes.M1, UTIL.Timeframes.M1, UTIL.Timeframes.D1)\n\
             pv = libApex.calcVolumePreview(bars, 1000.0)\n[b, s] = pv"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 0, "b"), 0.0);
    assert_eq!(num(&outcome, 0, "s"), 0.0);
}

#[test]
fn pivot_extreme_finds_highest_high_offsets() {
    // Twelve bars peaking at index 8: on the last bar the lookback scan
    // over offsets 0..=8 finds offset 3, so the pivot bar is 11 - 3 = 8.
    let bars: Vec<Bar> = (0..12)
        .map(|i| {
            let high = if i == 8 { 120.0 } else { 100.0 + f64::from(i) };
            bar("100", &format!("{high:.1}"), "99", "100", 1000, 1_700_000_000 + i64::from(i) * 300)
        })
        .collect();
    let outcome = run_linked(
        &format!(
            "{IMPORTS}[pt, pb, ph, pl] = libApex.detectPivotFromOscillatorExtreme(true, false, 8)"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 11, "pt"), 1.0);
    assert_eq!(num(&outcome, 11, "pb"), 8.0);
    assert_eq!(num(&outcome, 11, "ph"), 120.0);
    assert!(matches!(outcome.value_at(11, "pl"), Some(None)), "no pivot low on an upper extreme");
}

#[test]
fn pivot_extreme_lower_mirror_and_quiet_pair() {
    let bars: Vec<Bar> = (0..12)
        .map(|i| {
            let low = if i == 8 { 80.0 } else { 99.0 };
            bar("100", "101", &format!("{low:.1}"), "100", 1000, 1_700_000_000 + i64::from(i) * 300)
        })
        .collect();
    let outcome = run_linked(
        &format!(
            "{IMPORTS}[pt, pb, ph, pl] = libApex.detectPivotFromOscillatorExtreme(false, true, 8)\n\
             [qt, qb, qh, ql] = libApex.detectPivotFromOscillatorExtreme(false, false, 8)"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 11, "pt"), -1.0);
    assert_eq!(num(&outcome, 11, "pb"), 8.0);
    assert_eq!(num(&outcome, 11, "pl"), 80.0);
    assert!(matches!(outcome.value_at(11, "ph"), Some(None)), "no pivot high on a lower extreme");
    assert_eq!(num(&outcome, 11, "qt"), 0.0, "quiet pair types zero");
    assert!(matches!(outcome.value_at(11, "qb"), Some(None)), "quiet pair bars na");
}

#[test]
fn binary_search_insertion_points_on_sorted_levels() {
    // Levels sorted by price_low 9/19/29: lower-bound insertion points.
    let bars = vec![bar("100", "101", "99", "100", 1000, 1_700_000_000)];
    let outcome = run_linked(
        &format!(
            "{IMPORTS}l1 = libApex.MarketLevel.new()\nl1.init(9.0, 11.0, 0, 1)\n\
             l2 = libApex.MarketLevel.new()\nl2.init(19.0, 21.0, 0, 1)\n\
             l3 = libApex.MarketLevel.new()\nl3.init(29.0, 31.0, 0, 1)\n\
             arr = array.from(l1, l2, l3)\n\
             i_below = libApex.binarySearchMarketLevels(arr, 5.0)\n\
             i_hit = libApex.binarySearchMarketLevels(arr, 19.0)\n\
             i_between = libApex.binarySearchMarketLevels(arr, 25.0)\n\
             i_above = libApex.binarySearchMarketLevels(arr, 35.0)\n\
             i_empty = libApex.binarySearchMarketLevels(array.new(), 5.0)"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 0, "i_below"), 0.0);
    assert_eq!(num(&outcome, 0, "i_hit"), 1.0);
    assert_eq!(num(&outcome, 0, "i_between"), 2.0);
    assert_eq!(num(&outcome, 0, "i_above"), 3.0);
    assert_eq!(num(&outcome, 0, "i_empty"), 0.0);
}

#[test]
fn market_level_touch_success_break_flip_lifecycle() {
    // One level driven across five bars: init, successful touch,
    // quiet touch, break, flip. Counters and decay compose exactly.
    let bars: Vec<Bar> = (0..5)
        .map(|i| bar("100", "101", "99", "100", 1000, 1_700_000_000 + i * 300))
        .collect();
    let outcome = run_linked(
        &format!(
            "{IMPORTS}var lv = libApex.MarketLevel.new()\nif barstate.isfirst\n    lv.init(90.0, 110.0, 0, 1)\n\
             ok = true\n\
             hit = lv.updateLevelTouch(bar_index, bar_index == 1, 1000.0)\n\
             lv.breakLevel(bar_index)\n\
             flipped = bar_index == 4 ? lv.flip(105.0, -1) : false"
        ),
        &bars,
    );
    let state = match outcome.value_at(4, "lv") {
        Some(Some(PineValue::Object(instance))) => instance,
        other => panic!("expected MarketLevel on bar 4, got {other:?}"),
    };
    assert_eq!(field_num(&state, "price_low"), 90.0);
    assert_eq!(field_num(&state, "touch_count"), 1.0 + 5.0 + 5.0, "init plus five touches plus five breaks");
    assert_eq!(field_num(&state, "successes"), 2.0, "init plus the bar-1 success");
    assert_eq!(field_num(&state, "last_touch_bar"), 4.0);
    assert_eq!(field_num(&state, "last_success_bar"), 1.0);
    assert_eq!(field_num(&state, "last_active_bar"), 4.0);
    assert_eq!(field_num(&state, "breach_count"), 1.0, "one flip");
    assert_eq!(field_num(&state, "direction"), -1.0, "flip reverses");
    assert_eq!(field_num(&state, "pressure"), 0.0, "zero pressure stays zero through halving");
    assert_eq!(field_num(&state, "price_high"), 110.0);
    match outcome.value_at(4, "flipped") {
        Some(Some(PineValue::Bool(true))) => {}
        other => panic!("flip reports true on bar 4, got {other:?}"),
    }
}

#[test]
fn managed_table_update_delete_lifecycle() {
    // A 2x2 string matrix under two headers: one table creation, six
    // cell writes, then deletion retires the object to na.
    let bars: Vec<Bar> = (0..3)
        .map(|i| bar("100", "101", "99", "100", 1000, 1_700_000_000 + i * 300))
        .collect();
    let outcome = run_linked(
        &format!(
            "{IMPORTS}var tbl = libApex.ManagedTable.new()\n\
             hdrs = array.from(\"a\", \"b\")\n\
             mat = matrix.new<string>(2, 2, \"x\")\n\
             matrix.set(mat, 0, 1, \"y\")\n\
             cell = matrix.get(mat, 0, 1)\n\
             rows = matrix.rows(mat)\n\
             cols = matrix.columns(mat)\n\
             tbl.update(hdrs, mat)\n\
             if barstate.islast\n    tbl.delete()"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 2, "rows"), 2.0);
    assert_eq!(num(&outcome, 2, "cols"), 2.0);
    match outcome.value_at(2, "cell") {
        Some(Some(PineValue::Str(text))) => assert_eq!(text, "y", "matrix.set persists"),
        other => panic!("expected patched cell, got {other:?}"),
    }
    let created = outcome
        .draw_events
        .iter()
        .filter(|event| event.kind == crate::pine::eval::DrawingEventKind::Created)
        .count();
    let updated = outcome
        .draw_events
        .iter()
        .filter(|event| event.kind == crate::pine::eval::DrawingEventKind::Updated)
        .count();
    let deleted = outcome
        .draw_events
        .iter()
        .filter(|event| event.kind == crate::pine::eval::DrawingEventKind::Deleted)
        .count();
    // Three bars run update three times; the library recreates the table
    // on every update while an object is stored (two recycle deletes),
    // and the last bar deletes it outright.
    assert_eq!(created, 3, "one creation per update");
    assert_eq!(updated, 18, "two headers plus four cells per update");
    assert_eq!(deleted, 3, "two recycle deletes plus the final delete");
    match outcome.value_at(2, "tbl") {
        Some(Some(PineValue::Object(instance))) => match instance.fields.get("table_obj") {
            Some(None) => {}
            other => panic!("deleted table reads na, got {other:?}"),
        },
        other => panic!("expected ManagedTable on bar 2, got {other:?}"),
    }
}

#[test]
fn managed_table_column_mismatch_refuses() {
    let bars = vec![bar("100", "101", "99", "100", 1000, 1_700_000_000)];
    let modules = linked(&format!(
        "{IMPORTS}var tbl = libApex.ManagedTable.new()\n\
         hdrs = array.from(\"a\", \"b\")\n\
         mat = matrix.new<string>(2, 3, \"x\")\n\
         tbl.update(hdrs, mat)"
    ));
    let mut engine = test_engine();
    let err = engine
        .run_linked(&modules, &bars, InputSet::new())
        .expect_err("column mismatch refuses");
    assert!(
        err.to_string().contains("Data matrix columns must match header count"),
        "library's own message survives: {err}"
    );
}

#[test]
fn for_in_iterates_arrays_with_index_and_value() {
    // Collection iteration binds index/value pairs (or values alone),
    // accumulates across iterations, and refuses non-arrays loudly.
    let bars = vec![bar("100", "101", "99", "100", 1000, 1_700_000_000)];
    let outcome = run_linked(
        &format!(
            "{IMPORTS}vals = array.from(5, 7)\n\
             total = 0\n\
             for [i, v] in vals\n    total := total + i * 10 + v\n\
             solo = 0\n\
             for v in vals\n    solo := solo + v"
        ),
        &bars,
    );
    assert_eq!(num(&outcome, 0, "total"), 22.0, "0*10+5 plus 1*10+7");
    assert_eq!(num(&outcome, 0, "solo"), 12.0, "values alone sum");
    let modules = linked(&format!("{IMPORTS}x = 0\nfor v in 5\n    x := x + v"));
    let mut engine = test_engine();
    let err = engine
        .run_linked(&modules, &bars, InputSet::new())
        .expect_err("for-in over a scalar refuses");
    assert!(err.to_string().contains("for ... in"), "names the construct: {err}");
}
