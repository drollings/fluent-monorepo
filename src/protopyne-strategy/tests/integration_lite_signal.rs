//! Native IntegrationLite signal contract: warmup honesty, non-repainting
//! confirmed decisions, determinism, and the session gate, plus
//! batch/stateless agreement (the instance adds no second recurrence).

use chrono::{TimeZone, Utc};
use protopyne_types::{Bar, Timeframe};
use rust_decimal::Decimal;

use crate::elements::{Signal, SignalConfig, SignalOutput};
use crate::integration_lite::IntegrationLiteStrategyConfig;
use crate::proposal::Direction;
use crate::signals_lite::{IntegrationLiteSignal, LiteIndicatorParams};
use crate::view::{MarketData, MarketView};

fn tape(closes: &[f64], start_hour_utc: u32) -> (Vec<Bar>, Vec<f64>) {
    // 1-minute bars from 2026-01-05; 15:00 UTC is 10:00 ET (inside the
    // default 0930-1600 session), 22:00 UTC is 17:00 ET (outside it).
    let bars: Vec<Bar> = closes
        .iter()
        .enumerate()
        .map(|(i, close)| {
            Bar::new()
                .ts(Utc
                    .with_ymd_and_hms(2026, 1, 5, start_hour_utc, 0, 0)
                    .single()
                    .expect("valid time")
                    + chrono::Duration::minutes(i as i64))
                .o(Decimal::from_f64_retain(*close - 0.2).expect("finite"))
                .h(Decimal::from_f64_retain(*close + 0.5).expect("finite"))
                .l(Decimal::from_f64_retain(*close - 0.5).expect("finite"))
                .c(Decimal::from_f64_retain(*close).expect("finite"))
                .volume(10_000)
                .maybe_adj(None)
                .build()
        })
        .collect();
    let closes = closes.to_vec();
    (bars, closes)
}

fn market_data(bars: &[Bar]) -> MarketData<'_> {
    MarketData {
        bars,
        chain: None,
        dte_days: 30,
        timeframe: Timeframe::M1,
    }
}

fn market_view<'a>(data: &'a MarketData<'a>, closes: &'a [f64]) -> MarketView<'a> {
    MarketView {
        data,
        closes,
        as_of: data.bars.last().map_or_else(Utc::now, |bar| bar.ts),
    }
}

fn flat_closes(n: usize) -> Vec<f64> {
    vec![100.0; n]
}

/// Strategy config with the setup window open: a trigger stays armed 20
/// bars so the test can observe the trigger-plus-filter conjunction the
/// runner sees bar by bar (window 0 would need the cross and the filter
/// alignment on the same bar).
fn trend_strategy() -> IntegrationLiteStrategyConfig {
    IntegrationLiteStrategyConfig {
        sig_window: 20,
        ..IntegrationLiteStrategyConfig::default()
    }
}

fn signal() -> IntegrationLiteSignal {
    IntegrationLiteSignal::new(
        LiteIndicatorParams::default(),
        IntegrationLiteStrategyConfig::default(),
    )
    .expect("defaults validate")
}

#[test]
fn warmup_refuses_before_the_tape_is_seeded() {
    let sig = signal();
    let warmup = sig
        .bind(&SignalConfig::new(Timeframe::M1))
        .expect("binds")
        .warmup();
    assert!(warmup >= 100, "strategy warmup dial is folded in");
    let (bars, closes) = tape(&vec![100.0; warmup - 1], 15);
    let data = market_data(&bars);
    let view = market_view(&data, &closes);
    let mut instance = sig.bind(&SignalConfig::new(Timeframe::M1)).expect("binds");
    let err = instance.evaluate(&view).expect_err("short tape refuses");
    assert!(
        err.to_string().contains("warmup"),
        "refusal names warmup, got {err}"
    );
}

#[test]
fn trend_tapes_decide_with_the_trend() {
    // Backtest-style scan: the first committed decision on a steady trend
    // follows the trend with bounded scores and measured volatility.
    // V-reversals: the turn re-arms triggers after warmup; steady ramps
    // cross before the tape is seeded, so their triggers are stale by
    // construction (window 0 would need cross and filter alignment on one
    // bar — the setup window exists for exactly this conjunction).
    for (closes, want) in [
        (v_up_closes(), Direction::Long),
        (v_down_closes(), Direction::Short),
    ] {
        let (bars, owned) = tape(&closes, 15);
        let first = first_decision(&bars, &owned).expect("trend fires at least once");
        assert_eq!(first.1.direction, want, "trend decides with the trend");
        assert!(
            (0.0..=1.0).contains(&first.1.conviction),
            "conviction is a score"
        );
        assert!(
            (0.0..=1.0).contains(&first.1.activity_score),
            "activity is a score"
        );
        assert!(
            first
                .1
                .volatility
                .is_some_and(|v| v.is_finite() && v >= 0.0),
            "realised volatility is measured on a trend"
        );
        assert!(first.1.edge.is_none() && first.1.odds.is_none());
    }
}

/// First `(index, output)` the signal commits to over the closed tape, or
/// `None` when nothing arms. Bar-by-bar like the runner feeds it.
fn first_decision(bars: &[Bar], closes: &[f64]) -> Option<(usize, SignalOutput)> {
    let sig = IntegrationLiteSignal::new(LiteIndicatorParams::default(), trend_strategy())
        .expect("trend signal validates");
    for end in 1..=bars.len() {
        let data = market_data(&bars[..end]);
        let view = market_view(&data, &closes[..end]);
        let mut instance = sig.bind(&SignalConfig::new(Timeframe::M1)).expect("binds");
        // Fresh instance per prefix: prefix-equality of causal batches is
        // what the non-repaint test pins separately; here only the firing
        // bar matters.
        if let Ok(out) = instance.evaluate(&view) {
            return Some((end - 1, out));
        }
    }
    None
}

/// V-reversal tapes: the turn re-arms triggers after warmup (steady
/// ramps cross before the tape is seeded, so their triggers are stale by
/// construction).
fn v_up_closes() -> Vec<f64> {
    let mut closes = Vec::new();
    for i in 0..120 {
        closes.push(220.0 - f64::from(i) * 0.8);
    }
    for i in 0..60 {
        closes.push(124.0 + (f64::from(i) + 1.0) * 1.5);
    }
    closes
}

fn v_down_closes() -> Vec<f64> {
    let mut closes = Vec::new();
    for i in 0..120 {
        closes.push(100.0 + f64::from(i) * 0.8);
    }
    for i in 0..60 {
        closes.push(196.0 - (f64::from(i) + 1.0) * 1.5);
    }
    closes
}

#[test]
fn flat_tape_proposes_nothing() {
    let (bars, owned) = tape(&flat_closes(160), 15);
    let data = market_data(&bars);
    let view = market_view(&data, &owned);
    let mut instance = signal()
        .bind(&SignalConfig::new(Timeframe::M1))
        .expect("binds");
    assert!(
        instance.evaluate(&view).is_err(),
        "no trigger arms on a flat tape"
    );
}

#[test]
fn confirmed_decisions_survive_forming_bar_reevaluation() {
    // Path independence: sequential commits and one-shot evaluation agree
    // at the firing bar, and re-evaluating the same tape returns the cached
    // confirmed decision.
    let closes = v_up_closes();
    let (bars, owned) = tape(&closes, 15);
    let (fire_at, _) = first_decision(&bars, &owned).expect("V fires");
    let end = fire_at + 1;
    let data = market_data(&bars[..end]);
    let view = market_view(&data, &owned[..end]);
    let sig = IntegrationLiteSignal::new(LiteIndicatorParams::default(), trend_strategy())
        .expect("trend signal validates");
    // Sequential: commit an earlier prefix first, then the firing prefix.
    let mut sequential = sig.bind(&SignalConfig::new(Timeframe::M1)).expect("binds");
    if end > 101 {
        let pdata = market_data(&bars[..end - 1]);
        let pview = market_view(&pdata, &owned[..end - 1]);
        let _ = sequential.evaluate(&pview);
    }
    let second = sequential.evaluate(&view).expect("firing prefix decides");
    let mut oneshot = sig.bind(&SignalConfig::new(Timeframe::M1)).expect("binds");
    let direct = oneshot.evaluate(&view).expect("one-shot decides");
    assert_eq!(second.direction, direct.direction);
    // Byte-identity is the point: exact bit equality, not an eps.
    assert_eq!(second.conviction.to_bits(), direct.conviction.to_bits());
    assert_eq!(
        second.activity_score.to_bits(),
        direct.activity_score.to_bits()
    );
    assert_eq!(
        second.volatility.map(f64::to_bits),
        direct.volatility.map(f64::to_bits)
    );
    // Same tape twice: forming-bar re-evaluation cannot move the confirmed
    // decision (the instance returns its cache).
    let repeat = sequential.evaluate(&view).expect("repeat decides");
    assert_eq!(second.direction, repeat.direction);
    assert_eq!(second.conviction.to_bits(), repeat.conviction.to_bits());
}

#[test]
fn same_bars_same_config_is_byte_identical() {
    let closes = v_up_closes();
    let (bars, owned) = tape(&closes, 15);
    let (fire_at, _) = first_decision(&bars, &owned).expect("V fires");
    let end = fire_at + 1;
    let data = market_data(&bars[..end]);
    let view = market_view(&data, &owned[..end]);
    let sig = IntegrationLiteSignal::new(LiteIndicatorParams::default(), trend_strategy())
        .expect("trend signal validates");
    let mut first = sig.bind(&SignalConfig::new(Timeframe::M1)).expect("binds");
    let mut second = sig.bind(&SignalConfig::new(Timeframe::M1)).expect("binds");
    let a = first.evaluate(&view).expect("decides");
    let b = second.evaluate(&view).expect("decides");
    assert_eq!(a.direction, b.direction);
    assert_eq!(a.conviction.to_bits(), b.conviction.to_bits());
    assert_eq!(a.activity_score.to_bits(), b.activity_score.to_bits());
    assert_eq!(
        a.volatility.map(f64::to_bits),
        b.volatility.map(f64::to_bits)
    );
}

#[test]
fn session_gate_blocks_outside_bars_when_armed() {
    // The firing bar inside the session decides; the same shape shifted to
    // 22:00 UTC (17:00 ET, past the 16:00 close) refuses naming the
    // session; disarmed, the same tape decides.
    let closes = v_up_closes();
    let (bars, owned) = tape(&closes, 15);
    let (fire_at, _) = first_decision(&bars, &owned).expect("V fires");
    let end = fire_at + 1;
    let (obars, oowned) = tape(&closes, 22);
    let odata = market_data(&obars[..end]);
    let oview = market_view(&odata, &oowned[..end]);
    let sig = IntegrationLiteSignal::new(LiteIndicatorParams::default(), trend_strategy())
        .expect("trend signal validates");
    let mut instance = sig.bind(&SignalConfig::new(Timeframe::M1)).expect("binds");
    let err = instance
        .evaluate(&oview)
        .expect_err("outside-session bars never propose");
    assert!(
        err.to_string().contains("session"),
        "refusal names the session, got {err}"
    );
    let indicator = LiteIndicatorParams {
        session_filter: false,
        ..LiteIndicatorParams::default()
    };
    let open = IntegrationLiteSignal::new(indicator, trend_strategy()).expect("disarmed validates");
    let mut instance = open.bind(&SignalConfig::new(Timeframe::M1)).expect("binds");
    assert!(
        instance.evaluate(&oview).is_ok(),
        "disarmed session lets the tape decide"
    );
}

#[test]
fn instance_agrees_with_stateless_evaluation() {
    // The instance adds caching only; the math runs once, statelessly.
    let closes = v_up_closes();
    let (bars, owned) = tape(&closes, 15);
    let (fire_at, _) = first_decision(&bars, &owned).expect("V fires");
    let end = fire_at + 1;
    let data = market_data(&bars[..end]);
    let view = market_view(&data, &owned[..end]);
    let sig = IntegrationLiteSignal::new(LiteIndicatorParams::default(), trend_strategy())
        .expect("trend signal validates");
    let mut instance = sig.bind(&SignalConfig::new(Timeframe::M1)).expect("binds");
    let via_instance = instance.evaluate(&view).expect("decides");
    let via_stateless = sig.evaluate(&view).expect("decides");
    assert_eq!(via_instance.direction, via_stateless.direction);
    assert_eq!(
        via_instance.conviction.to_bits(),
        via_stateless.conviction.to_bits()
    );
    assert_eq!(
        via_instance.activity_score.to_bits(),
        via_stateless.activity_score.to_bits()
    );
    assert_eq!(
        via_instance.volatility.map(f64::to_bits),
        via_stateless.volatility.map(f64::to_bits)
    );
}

#[test]
fn adaptive_relay_applies_once_over_raw_bars() {
    // The adaptive EIT relay must read raw bars exactly once: the script's
    // chart-timeframe context fetches ATR and MAs from raw bars while only
    // the oscillator source rides the relay. A relay-of-relay (or ATR over
    // the relay frame) drifts every downstream filter and stop.
    use protopyne_kernels::indicator::{builtin_registry, OwnedOhlcv, ParamSet};

    use crate::signals_lite::market_legs;
    let mut closes = Vec::new();
    for i in 0..60 {
        closes.push(200.0 + f64::from(i) * 1.5 + f64::from((i * 7) % 5) * 0.4);
    }
    let (bars, _) = tape(&closes, 15);
    let params = LiteIndicatorParams {
        adaptive: true,
        ..LiteIndicatorParams::default()
    };
    let legs = market_legs(&params, &bars).expect("legs evaluate");
    let frame = OwnedOhlcv::from_bars(&bars);
    let registry = builtin_registry().expect("registry builds");
    let raw_eit = registry
        .get("eit")
        .expect("eit kernel")
        .batch(
            &frame.views(),
            &ParamSet::new().with_float("alpha", params.eit_alpha),
        )
        .expect("eit batches")
        .values;
    let raw_atr = registry
        .get("atr")
        .expect("atr kernel")
        .batch(&frame.views(), &ParamSet::new().with_int("length", 14))
        .expect("atr batches")
        .values;
    let mut compared = 0;
    for i in 0..bars.len() {
        if let (Some(got), Some(want)) = (legs.adaptive[i], raw_eit[i]) {
            assert!(
                (got - want).abs() <= 1e-9,
                "adaptive leg drifts at bar {i}: {got} vs {want}"
            );
            compared += 1;
        }
        if let (Some(got), Some(want)) = (legs.atr[i], raw_atr[i]) {
            assert!(
                (got - want).abs() <= 1e-9,
                "atr leg drifts at bar {i}: {got} vs {want}"
            );
            compared += 1;
        }
    }
    assert!(compared >= 60, "too few co-read bars: {compared}");
}
