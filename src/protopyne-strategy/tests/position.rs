//! The position manager: stop precedence, targets, partial-once, ratchets,
//! time stop, and the forced flatten paths.

use chrono::{TimeZone as _, Utc};
use protopyne_types::session::TradingSession;
use protopyne_types::Bar;
use rust_decimal::Decimal;

use crate::integration_lite::IntegrationLiteStrategyConfig;
use crate::position::{OpenPosition, PositionManager, PositionSpec};

fn bars(closes: &[(f64, f64, f64)]) -> Vec<Bar> {
    closes
        .iter()
        .enumerate()
        .map(|(index, (high, low, close))| {
            Bar::new()
                .ts(Utc
                    .with_ymd_and_hms(2026, 1, 5, 15, 0, 0)
                    .single()
                    .expect("time")
                    + chrono::Duration::minutes(index as i64))
                .o(Decimal::from_f64_retain(*close).expect("finite"))
                .h(Decimal::from_f64_retain(*high).expect("finite"))
                .l(Decimal::from_f64_retain(*low).expect("finite"))
                .c(Decimal::from_f64_retain(*close).expect("finite"))
                .volume(10_000)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

fn manager(strategy: IntegrationLiteStrategyConfig) -> PositionManager {
    let session = TradingSession::parse("0930-1600").expect("session");
    PositionManager::new(PositionSpec::from(strategy), session, 60_000, true)
}

fn position(
    mut manager: PositionManager,
    bars: &[Bar],
    _index: usize,
) -> (PositionManager, OpenPosition) {
    let position = manager.open(true, 100, 100.0, 0, 1.0, bars);
    (manager, position)
}

fn position_at(
    mut manager: PositionManager,
    bars: &[Bar],
    index: usize,
) -> (PositionManager, OpenPosition) {
    let position = manager.open(true, 100, 100.0, index, 1.0, bars);
    (manager, position)
}

#[test]
fn a_touched_stop_wins_before_a_touched_target() {
    let strategy = IntegrationLiteStrategyConfig {
        stop_atr_mult: 1.0,
        min_stop_atr: 0.1,
        max_stop_atr: 10.0,
        tp1_mult: 1.0,
        tp1_pct: 100.0,
        tp_basis: "ATR".to_owned(),
        ..IntegrationLiteStrategyConfig::default()
    };
    // Entry 100, stop 99, target 101; the bar touches both.
    let bars = bars(&[(101.5, 98.5, 100.0)]);
    let (mut manager, _) = position(manager(strategy), &bars, 0);
    let legs = protopyne_kernels::lite::lite_legs(
        &bars,
        &crate::signals_lite::LiteIndicatorParams::default(),
        14,
    )
    .expect("legs");
    let tape = crate::tape::Tape::new(&bars, &legs, &legs.close, 0).expect("tape");
    let intents = manager.review_exits(&tape, &bars, 0);
    assert_eq!(intents.len(), 1);
    assert_eq!(intents[0].kind, "stop", "the stop resolves first");
}

#[test]
fn a_target_takes_its_fraction_and_arms_breakeven() {
    let strategy = IntegrationLiteStrategyConfig {
        stop_atr_mult: 2.0,
        min_stop_atr: 0.1,
        max_stop_atr: 10.0,
        tp1_mult: 1.0,
        tp1_pct: 50.0,
        tp2_pct: 0.0,
        tp3_pct: 0.0,
        tp_basis: "ATR".to_owned(),
        be_on_tp: true,
        be_offset_atr: 0.0,
        ..IntegrationLiteStrategyConfig::default()
    };
    let mut tape_rows: Vec<(f64, f64, f64)> = (0..20).map(|_| (100.5, 99.5, 100.0)).collect();
    tape_rows.push((102.0, 99.0, 101.5));
    let bars = bars(&tape_rows);
    let (mut manager, _) = position_at(manager(strategy), &bars, 20);
    let legs = protopyne_kernels::lite::lite_legs(
        &bars,
        &crate::signals_lite::LiteIndicatorParams::default(),
        14,
    )
    .expect("legs");
    let tape = crate::tape::Tape::new(&bars, &legs, &legs.close, 20).expect("tape");
    let intents = manager.review_exits(&tape, &bars, 20);
    assert_eq!(intents[0].kind, "take-profit");
    assert!(manager.position().expect("open").target_fills[0]);
    assert!(manager.position().expect("open").be_armed);
}

#[test]
fn a_trailing_stop_only_tightens() {
    let strategy = IntegrationLiteStrategyConfig {
        stop_atr_mult: 3.0,
        min_stop_atr: 0.1,
        max_stop_atr: 10.0,
        tp1_pct: 0.0,
        tp2_pct: 0.0,
        tp3_pct: 0.0,
        trail_immediate: true,
        trail_type: "Chandelier".to_owned(),
        trail_mult: 1.0,
        ..IntegrationLiteStrategyConfig::default()
    };
    let mut rows: Vec<(f64, f64, f64)> = (0..20).map(|_| (100.5, 99.5, 100.0)).collect();
    rows.push((110.0, 109.0, 109.5));
    rows.push((104.0, 103.0, 103.5));
    let tape_bars = bars(&rows);
    let (mut manager, _) = position_at(manager(strategy), &tape_bars, 19);
    let legs = protopyne_kernels::lite::lite_legs(
        &tape_bars,
        &crate::signals_lite::LiteIndicatorParams::default(),
        14,
    )
    .expect("legs");
    manager.begin_bar(&tape_bars[20], 109.5, 100_000.0);
    let tape =
        crate::tape::Tape::new(&tape_bars[..=20], &legs, &legs.close[..=20], 20).expect("tape");
    let _ = manager.review_exits(&tape, &tape_bars, 20);
    let tightened = manager.position().expect("open").stop;

    let mut rows2 = rows.clone();
    rows2.push((106.0, 98.0, 99.0));
    let bars2 = bars(&rows2);
    let legs2 = protopyne_kernels::lite::lite_legs(
        &bars2,
        &crate::signals_lite::LiteIndicatorParams::default(),
        14,
    )
    .expect("legs");
    manager.begin_bar(&bars2[21], 99.0, 100_000.0);
    let tape2 =
        crate::tape::Tape::new(&bars2[..=21], &legs2, &legs2.close[..=21], 21).expect("tape");
    let _ = manager.review_exits(&tape2, &bars2, 21);
    assert!(
        manager.position().expect("open").stop >= tightened,
        "the trailing stop only ever tightens"
    );
}

#[test]
fn the_time_stop_closes_after_the_configured_bars() {
    let strategy = IntegrationLiteStrategyConfig {
        stop_atr_mult: 5.0,
        min_stop_atr: 0.1,
        max_stop_atr: 10.0,
        tp1_pct: 0.0,
        tp2_pct: 0.0,
        tp3_pct: 0.0,
        exit_time_bars: 1,
        ..IntegrationLiteStrategyConfig::default()
    };
    let rows: Vec<(f64, f64, f64)> = (0..22).map(|_| (100.5, 99.5, 100.0)).collect();
    let tape_bars = bars(&rows);
    let (mut manager, _) = position_at(manager(strategy), &tape_bars, 19);
    let legs = protopyne_kernels::lite::lite_legs(
        &tape_bars,
        &crate::signals_lite::LiteIndicatorParams::default(),
        14,
    )
    .expect("legs");
    let tape =
        crate::tape::Tape::new(&tape_bars[..=20], &legs, &legs.close[..=20], 20).expect("tape");
    let intents = manager.review_exits(&tape, &tape_bars, 20);
    assert_eq!(
        intents.first().map(|intent| intent.kind.as_str()),
        Some("time-stop")
    );
}

#[test]
fn the_session_gate_forces_a_flatten_near_the_close() {
    let strategy = IntegrationLiteStrategyConfig {
        flatten_eod: true,
        flatten_min_before: 5,
        ..IntegrationLiteStrategyConfig::default()
    };
    let bars = bars(&[(100.5, 99.5, 100.0)]);
    let mut manager = PositionManager::new(
        PositionSpec::from(strategy),
        TradingSession::parse("0930-1600").expect("session"),
        60_000,
        true,
    );
    manager.open(true, 10, 100.0, 0, 1.0, &bars);
    let mut late = bars[0].clone();
    late.ts = Utc
        .with_ymd_and_hms(2026, 1, 5, 20, 59, 0)
        .single()
        .expect("time");
    manager.begin_bar(&late, 100.0, 100_000.0);
    assert_eq!(manager.forced_flatten(), Some("session-flatten"));
}

#[test]
fn the_daily_loss_gate_forces_a_flatten() {
    let strategy = IntegrationLiteStrategyConfig {
        max_daily_loss_pct: 5.0,
        ..IntegrationLiteStrategyConfig::default()
    };
    let bars = bars(&[(100.5, 99.5, 100.0), (100.5, 99.5, 100.0)]);
    let mut manager = manager(strategy);
    manager.begin_bar(&bars[0], 100.0, 100_000.0);
    assert_eq!(manager.forced_flatten(), None);
    manager.begin_bar(&bars[1], 100.0, 94_000.0);
    assert_eq!(manager.forced_flatten(), Some("daily-loss"));
}

#[test]
fn a_gap_through_the_stop_still_rests_at_the_stop() {
    let strategy = IntegrationLiteStrategyConfig {
        stop_atr_mult: 1.0,
        min_stop_atr: 0.1,
        max_stop_atr: 10.0,
        tp1_pct: 0.0,
        tp2_pct: 0.0,
        tp3_pct: 0.0,
        ..IntegrationLiteStrategyConfig::default()
    };
    let bars = bars(&[(96.0, 94.0, 95.0)]);
    let (mut manager, position) = position(manager(strategy), &bars, 0);
    let legs = protopyne_kernels::lite::lite_legs(
        &bars,
        &crate::signals_lite::LiteIndicatorParams::default(),
        14,
    )
    .expect("legs");
    let tape = crate::tape::Tape::new(&bars, &legs, &legs.close, 0).expect("tape");
    let intents = manager.review_exits(&tape, &bars, 0);
    assert_eq!(intents[0].kind, "stop");
    assert_eq!(
        intents[0].price,
        crate::slots::exit::RestingPrice::At(crate::position::money_for_test(position.stop)),
        "the stop rests at its own price, not the gap print"
    );
}

#[test]
fn a_touched_target_outranks_an_elapsed_time_stop() {
    let strategy = IntegrationLiteStrategyConfig {
        stop_atr_mult: 5.0,
        min_stop_atr: 0.1,
        max_stop_atr: 10.0,
        tp_basis: "ATR".to_owned(),
        tp1_mult: 1.0,
        tp1_pct: 100.0,
        tp2_pct: 0.0,
        tp3_pct: 0.0,
        exit_time_bars: 1,
        ..IntegrationLiteStrategyConfig::default()
    };
    let mut tape_rows: Vec<(f64, f64, f64)> = (0..20).map(|_| (100.5, 99.5, 100.0)).collect();
    tape_rows.push((102.0, 99.0, 101.5));
    let bars = bars(&tape_rows);
    let (mut manager, _) = position_at(manager(strategy), &bars, 19);
    let legs = protopyne_kernels::lite::lite_legs(
        &bars,
        &crate::signals_lite::LiteIndicatorParams::default(),
        14,
    )
    .expect("legs");
    let tape = crate::tape::Tape::new(&bars, &legs, &legs.close, 20).expect("tape");
    let intents = manager.review_exits(&tape, &bars, 20);
    assert_eq!(
        intents.first().map(|intent| intent.kind.as_str()),
        Some("take-profit"),
        "the target zone precedes the time stop in the chain"
    );
}

#[test]
fn the_exhaustion_partial_fires_at_most_once() {
    use crate::equity::PartialOnce;
    let mut once = PartialOnce::default();
    assert!(once.may_fire(true, false, false));
    once.latch();
    assert!(
        !once.may_fire(true, true, false),
        "the latch holds after the fill regardless of later bars"
    );
}

#[test]
fn the_breakeven_ratchet_never_retreats() {
    use crate::equity::ratchet_stop;
    assert_eq!(
        ratchet_stop(true, 99.0, 100.0).to_bits(),
        100.0_f64.to_bits()
    );
    assert_eq!(
        ratchet_stop(true, 100.0, 99.5).to_bits(),
        100.0_f64.to_bits(),
        "a long stop never moves back down"
    );
    assert_eq!(
        ratchet_stop(false, 101.0, 100.0).to_bits(),
        100.0_f64.to_bits()
    );
    assert_eq!(
        ratchet_stop(false, 100.0, 100.5).to_bits(),
        100.0_f64.to_bits(),
        "a short stop never moves back up"
    );
}

#[test]
fn pair_at_guards_the_window_edge_and_flattens_holes() {
    use super::pair_at;
    assert_eq!(pair_at(&[Some(1.0), Some(2.0)], 1), (Some(1.0), Some(2.0)));
    assert_eq!(pair_at(&[Some(1.0), Some(2.0)], 0), (None, Some(1.0)));
    assert_eq!(pair_at(&[None, Some(2.0)], 1), (None, Some(2.0)));
    assert_eq!(pair_at(&[], 0), (None, None));
    assert_eq!(pair_at(&[Some(1.0)], 5), (None, None));
}
