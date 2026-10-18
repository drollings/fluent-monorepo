//! Regime-trigger calibration: labels are task-value (classification
//! correctness), guards are producer-confidence (input validity) —
//! asserted separately, never substituted. Thresholds stay frozen;
//! widening one to pass fails the milestone.
//!
//! Shapes mirror the Pine `detect_regime_calibration` suite so the native
//! view earns the same labels from the same tapes. The two controls pin
//! quiet: calm wobble stays untrending, and single-bar spikes (volume and
//! price) never latch — a transient excursion may read hot on the spike
//! bar itself (parity math), but the washout must return to the pre-spike
//! label.
//! `#[allow(clippy::float_cmp)]` covers the hand-computed literals.
#![allow(clippy::float_cmp)]

use super::*;
use protopyne_types::Bar;
use protopyne_types::Timeframe;

/// One closed bar from decimal legs and a whole-second timestamp.
fn bar(open: &str, high: &str, low: &str, close: &str, volume: u64, ts_secs: i64) -> Bar {
    Bar::new()
        .ts(chrono::TimeZone::timestamp_opt(&chrono::Utc, ts_secs, 0)
            .single()
            .expect("valid test timestamp"))
        .o(open.parse().expect("decimal"))
        .h(high.parse().expect("decimal"))
        .l(low.parse().expect("decimal"))
        .c(close.parse().expect("decimal"))
        .volume(volume)
        .maybe_adj(None)
        .build()
}

/// Thirty steep bars plus a five-bar counter-move (Pine `drive` shape).
fn drive(closes: &[f64]) -> Vec<Bar> {
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
        .collect()
}

/// Forty bars of gentle two-phase wobble around `base`, flat volume unless
/// spiked at one bar (Pine `wobble_bars` shape).
fn wobble(base: f64, volume_spike: Option<(u32, u64)>, truncate_to: Option<usize>) -> Vec<Bar> {
    let mut bars: Vec<Bar> = (0u32..40)
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
        .collect();
    if let Some(n) = truncate_to {
        bars.truncate(n);
    }
    bars
}

fn native_regime(bars: &[Bar], at: usize) -> MarketContext {
    let registry = crate::indicator::builtin_registry().expect("builtins");
    let mut store = MtfStore::new();
    store.set_base(bars).expect("base loads");
    let key = MarketContextKey::new(Timeframe::M5);
    let now = bars.last().expect("nonempty").ts.timestamp_millis() + 300_000;
    fetch_all(
        &mut store,
        &registry,
        std::slice::from_ref(&key),
        &std::collections::HashMap::new(),
        now,
        Lookahead::Off,
    )
    .expect("fetch fills");
    let frame_len = store.frame_stamps(key.tf_ms()).expect("frame").len();
    assert!(
        at < frame_len,
        "probe bar {at} inside {frame_len} confirmed buckets"
    );
    update(&store, &key, at, None).expect("context reads")
}

fn last_regime(bars: &[Bar]) -> MarketRegime {
    // Confirmed-bucket count equals bar count on the aligned 5M grid, so
    // the last frame bar is the readout bar.
    let registry = crate::indicator::builtin_registry().expect("builtins");
    let mut store = MtfStore::new();
    store.set_base(bars).expect("base loads");
    let key = MarketContextKey::new(Timeframe::M5);
    let now = bars.last().expect("nonempty").ts.timestamp_millis() + 300_000;
    let contexts = fetch_all(
        &mut store,
        &registry,
        &[key],
        &std::collections::HashMap::new(),
        now,
        Lookahead::Off,
    )
    .expect("fetch fills");
    contexts.into_iter().next().expect("one context").regime
}

fn is_trending(regime: MarketRegime) -> bool {
    matches!(
        regime,
        MarketRegime::TrendingBull | MarketRegime::TrendingBear
    )
}

#[test]
fn regime_precision_recall_at_unit() {
    // Hand-labeled tapes (Pine calibration shapes): pullback in an
    // uptrend, rally in a downtrend, V bend, volume-spike wobble, calm
    // wobble. Rates measure classification correctness, never confidence.
    let up: Vec<f64> = (0..30).map(|i| 100.0 + 5.0 * f64::from(i)).collect();
    let down: Vec<f64> = (0..30).map(|i| 300.0 - 5.0 * f64::from(i)).collect();
    let mut pullback = up.clone();
    pullback.extend((1..=5).map(|i| 245.0 - f64::from(i)));
    let mut rally = down.clone();
    rally.extend((1..=5).map(|i| 155.0 + f64::from(i)));
    let bull = drive(&pullback);
    let bear = drive(&rally);
    let mut bend: Vec<f64> = (0..20).map(|i| 100.0 + 3.0 * f64::from(i)).collect();
    bend.extend((1..=20).map(|i| 160.0 - 3.0 * f64::from(i)));
    let volatile = drive(&bend);
    let ranging = wobble(100.0, Some((39, 6_000)), None);
    let calm = wobble(100.0, Some((38, 1_500)), Some(39));
    let cases: &[(&str, &Vec<Bar>, MarketRegime)] = &[
        ("pullback in uptrend", &bull, MarketRegime::TrendingBull),
        ("rally in downtrend", &bear, MarketRegime::TrendingBear),
        ("V bend", &volatile, MarketRegime::Volatile),
        ("volume spike", &ranging, MarketRegime::Ranging),
        ("calm wobble", &calm, MarketRegime::LowVolume),
    ];
    let mut correct = 0;
    let mut trend_predicted = 0;
    let mut trend_right = 0;
    for (label, bars, expected) in cases {
        let got = last_regime(bars);
        assert_eq!(got, *expected, "{label} classifies {expected:?}");
        correct += 1;
        if is_trending(got) {
            trend_predicted += 1;
            if is_trending(*expected) {
                trend_right += 1;
            }
        }
    }
    assert_eq!(correct, cases.len(), "recall 1.0 over labeled regimes");
    assert_eq!(
        trend_right, trend_predicted,
        "trend-call precision 1.0 (no false trends)"
    );
    assert_eq!(trend_predicted, 2, "both trend labels predicted trending");
}

#[test]
fn guards_are_confidence_not_correctness() {
    // Forty zero-volume bars: the volume guard (`vol_sma > 0`) blocks the
    // ratio update — the field carries its default instead of inventing a
    // value — while trend inputs stay computable. Guard behavior and label
    // correctness asserted in separate statements, never substituted.
    let zeros: Vec<Bar> = (0u32..40)
        .map(|i| {
            let close = 100.0 + f64::from(i % 2) * 0.4;
            bar(
                &format!("{:.2}", close - 0.2),
                &format!("{:.2}", close + 0.4),
                &format!("{:.2}", close - 0.3),
                &format!("{close:.2}"),
                0,
                1_700_000_000 + i64::from(i) * 300,
            )
        })
        .collect();
    let ctx = native_regime(&zeros, 39);
    assert_eq!(
        ctx.vol_ratio,
        Some(0.0),
        "blocked guard carries the default, never na"
    );
    assert_eq!(ctx.vol, Some(0.0), "zero volume still reports present");
    assert_eq!(
        ctx.vol_sma,
        Some(0.0),
        "sma over zeros computes zero (guard reads it, not na)"
    );
    assert!(
        ctx.ma_health.is_some(),
        "trend inputs stay computable without volume"
    );
}

#[test]
fn single_bar_spikes_never_latch() {
    // Flat forty with a 6x volume print at bar 30 (sma warm): the volume
    // score fires arithmetically (4.8 → 48), but the flat tape leaves the
    // volatility surface absent — and the propagating max voids the tie,
    // exactly as the warming probe pins for the oracle. So the spike bar
    // must NOT read RANGING here (no false volume win on absent siblings),
    // and the washout must return to the pre-spike label (no latch).
    let flat = |volume: u64| {
        (0u32..40)
            .map(|i| {
                bar(
                    "100",
                    "100",
                    "100",
                    "100",
                    if i == 30 { volume } else { 1_000 },
                    1_700_000_000 + i64::from(i) * 300,
                )
            })
            .collect::<Vec<_>>()
    };
    let bars = flat(6_000);
    let pre = native_regime(&bars, 29).regime;
    let spike = native_regime(&bars, 30).regime;
    let washout = native_regime(&bars, 39).regime;
    assert!(
        !is_trending(pre),
        "pre-spike flat stays untrending: {pre:?}"
    );
    assert_eq!(
        spike,
        MarketRegime::LowVolume,
        "absent sibling voids the volume tie"
    );
    assert_eq!(
        washout, pre,
        "washout returns to the pre-spike label (no latch)"
    );
    // Price spike without volume change: recursive kernels (EMA-21,
    // ATR-14) retain ghosts for dozens of bars, and the ghost *ratio* can
    // still tie at the washout horizon — the same arithmetic in Pine reads
    // identically, so washout-equality is unassertable here by construction.
    // What the control pins instead: the spike never leaks into the
    // participation surface (vol_ratio identically 1.0 throughout), it
    // registers on the spike bar itself, and the ATR excursion strictly
    // decays (ghosts fade, never latch permanently).
    let jabbed: Vec<Bar> = (0u32..90)
        .map(|i| {
            let close = if i == 30 { "120.00" } else { "100.00" };
            let high = if i == 30 { "121.00" } else { "100.00" };
            bar(
                "100.00",
                high,
                "100.00",
                close,
                1_000,
                1_700_000_000 + i64::from(i) * 300,
            )
        })
        .collect();
    let pre_price = native_regime(&jabbed, 29);
    let spike_price = native_regime(&jabbed, 30);
    let washout_price = native_regime(&jabbed, 89);
    for (label, ctx) in [
        ("pre", &pre_price),
        ("spike", &spike_price),
        ("washout", &washout_price),
    ] {
        assert_eq!(
            ctx.vol_ratio,
            Some(1.0),
            "{label}: volume surface untouched by a price spike"
        );
    }
    assert_eq!(
        spike_price.regime,
        MarketRegime::TrendingBear,
        "spike bar registers"
    );
    // Oracle-pinned spike-bar health (-70.168…): the lagging fast average
    // stacks bearish under the print while dispersion trails — verified
    // against the engine on this exact tape, frozen here.
    match spike_price.ma_health {
        Some(health) => assert!(
            (health - -70.16806722688966).abs() <= 1e-9,
            "spike-bar health frozen: {health}"
        ),
        None => panic!("spike-bar health present"),
    }
    let (atr_pre, atr_spike, atr_wash) = (pre_price.atr, spike_price.atr, washout_price.atr);
    match (atr_pre, atr_spike, atr_wash) {
        (Some(pre), Some(spike), Some(wash)) => {
            assert!(spike > pre, "spike inflates ATR: {spike} > {pre}");
            assert!(wash < spike, "ATR excursion decays: {wash} < {spike}");
        }
        other => panic!("ATR present on flat tapes, got {other:?}"),
    }
}

#[test]
fn quiet_shapes_stay_untrending() {
    // Calm wobble (Pine `calm` shape) and pure flat: neither may label
    // trending. The ranging band is a regime-level property here, never a
    // health-threshold rewrite.
    let calm = wobble(100.0, Some((38, 1_500)), Some(39));
    let calm_regime = last_regime(&calm);
    assert!(
        !is_trending(calm_regime),
        "calm wobble stays untrending: {calm_regime:?}"
    );
    let flat: Vec<Bar> = (0u32..30)
        .map(|i| {
            bar(
                "100",
                "100",
                "100",
                "100",
                1_000,
                1_700_000_000 + i64::from(i) * 300,
            )
        })
        .collect();
    let flat_regime = last_regime(&flat);
    assert!(
        !is_trending(flat_regime),
        "flat tape stays untrending: {flat_regime:?}"
    );
}

#[test]
fn flat_tape_zeroes_the_atr_guarded_ratios() {
    // A flat tape zeroes ATR (and stdev): the `atr != 0.0` guards refuse
    // both ratios instead of dividing — the exact arms the `div_or_none`
    // migration must preserve bit-for-bit.
    let flat: Vec<Bar> = (0u32..40)
        .map(|i| {
            bar(
                "100",
                "100",
                "100",
                "100",
                1_000,
                1_700_000_000 + i64::from(i) * 300,
            )
        })
        .collect();
    let ctx = native_regime(&flat, 39);
    assert_eq!(
        ctx.atr_distance, None,
        "zero ATR refuses the distance ratio"
    );
    assert_eq!(
        ctx.volatility_ratio, None,
        "zero ATR refuses the volatility ratio"
    );
}
