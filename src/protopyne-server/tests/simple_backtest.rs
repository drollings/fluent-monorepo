//! Simple-backtest contract: entries fill at the touch, resting exits
//! fill at their own level, the daily-loss halt holds for the rest of the
//! day, and reruns are byte-identical. Runs alongside the old path — no
//! caller migrates in this milestone.

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use protopyne_strategy::integration_lite::IntegrationLiteStrategyConfig;
use protopyne_strategy::position::entry_stop_distance;
use protopyne_strategy::signals_lite::LiteIndicatorParams;
use protopyne_types::{Bar, Timeframe};

use crate::backtest::{quote_for_bar, BacktestConfig};
use crate::equity::EquityRun;
use crate::runner::RunnerConfig;
use crate::simple_backtest::SimpleBacktestRequest;

fn tape(closes: &[f64], start_hour_utc: u32) -> Vec<Bar> {
    closes
        .iter()
        .enumerate()
        .map(|(i, close)| {
            let value = Decimal::from_f64_retain(*close).expect("finite close");
            let high = Decimal::from_f64_retain(*close + 0.5).expect("finite high");
            let low = Decimal::from_f64_retain(*close - 0.5).expect("finite low");
            Bar::new()
                .ts(Utc
                    .with_ymd_and_hms(2026, 1, 5, start_hour_utc, 0, 0)
                    .single()
                    .expect("valid time")
                    + chrono::Duration::minutes(i as i64))
                .o(value)
                .h(high)
                .l(low)
                .c(value)
                .volume(10_000)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

fn round_trip_closes() -> Vec<f64> {
    let mut closes = Vec::new();
    for i in 0..110 {
        closes.push(220.0 - f64::from(i) * 0.8);
    }
    for i in 0..30 {
        closes.push(132.0 + (f64::from(i) + 1.0) * 2.0);
    }
    for i in 0..40 {
        closes.push(192.0 - (f64::from(i) + 1.0) * 1.5);
    }
    closes
}

fn venue_config() -> BacktestConfig {
    BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("simple-v1".to_owned())
        .quote_size(1_000_000)
        .build()
}

fn runner_config() -> RunnerConfig {
    use crate::common_fixtures::money;
    RunnerConfig::new(
        "SPY".to_owned(),
        "integration_lite".to_owned(),
        money("400"),
        money("2_000"),
        30,
        0,
    )
}

fn strategy_with(window: i64) -> IntegrationLiteStrategyConfig {
    IntegrationLiteStrategyConfig {
        sig_window: window,
        filt_ma_health_min: 0.0,
        ..IntegrationLiteStrategyConfig::default()
    }
}

/// Every exit isolated except the one under test: no filters, no breakeven
/// or trailing, no opposing exits; the stop scale is the caller's choice.
fn bare(allow_reverse: bool) -> IntegrationLiteStrategyConfig {
    let mut strategy = strategy_with(20);
    strategy.allow_reverse = allow_reverse;
    strategy.exit_opposite = false;
    strategy.exit_ribbon = false;
    strategy.exit_zero_cross = false;
    strategy.filt_ribbon = false;
    strategy.filt_ma_health = false;
    strategy.tp1_pct = 0.0;
    strategy.tp2_pct = 0.0;
    strategy.tp3_pct = 0.0;
    strategy.be_enable = false;
    strategy.trail_immediate = false;
    strategy.trail_on_tp = false;
    strategy.trail_on_exhaustion = false;
    strategy.trail_on_profit = false;
    strategy
}

fn run_simple(
    closes: &[f64],
    strategy: &IntegrationLiteStrategyConfig,
    seed: u64,
) -> (EquityRun, Vec<Bar>) {
    let bars = tape(closes, 15);
    let run = SimpleBacktestRequest::builder()
        .indicator(&LiteIndicatorParams::default())
        .strategy(strategy)
        .venue_config(&venue_config())
        .runner_config(&runner_config())
        .signal_name("integration_lite")
        .underlying("SPY")
        .timeframe(Timeframe::M1)
        .bars(&bars)
        .seed(seed)
        .initial_equity(100_000.0)
        .run()
        .expect("simple run completes");
    (run, bars)
}

/// Entries fill at the quoted touch: the ask for longs, the bid for shorts.
/// No venue stands between the signal and the fill, so the price is the
/// touch exactly — not a slipped or sided approximation of it.
#[test]
fn entries_fill_at_the_touch() {
    let (run, bars) = run_simple(&round_trip_closes(), &strategy_with(20), 7);
    assert!(!run.trades.is_empty(), "the turn must open a trade");
    let config = venue_config();
    for trade in run.trades.iter().chain(run.partials.iter()) {
        let quote = quote_for_bar(&bars[trade.entry_bar], &config).expect("entry bar quotable");
        let touch = if trade.long { quote.ask } else { quote.bid };
        assert_eq!(
            trade.entry_price.to_bits(),
            touch
                .as_decimal()
                .to_string()
                .parse::<f64>()
                .expect("representable")
                .to_bits(),
            "entry fills at the touch, bar {}",
            trade.entry_bar
        );
    }
}

/// A touched take-profit zone fills at its own level: entry plus one stop
/// distance for TP1 under the default R-multiple basis.
#[test]
fn take_profit_fills_at_its_level() {
    let mut strategy = bare(false);
    strategy.exit_action = "Off".to_owned();
    strategy.tp1_pct = 100.0;
    // A default-scale stop keeps TP1 inside the 60-point rise.
    strategy.stop_atr_mult = 1.5;
    strategy.min_stop_atr = 0.5;
    strategy.max_stop_atr = 4.0;
    let (run, bars) = run_simple(&round_trip_closes(), &strategy, 7);
    let trade = run
        .trades
        .iter()
        .find(|t| t.exit_reason == "take-profit")
        .expect("TP1 must fill on the rise");
    assert!(run.partials.is_empty(), "100% at TP1 leaves no slices");
    let legs = protopyne_kernels::lite::lite_legs(
        &bars,
        &LiteIndicatorParams::default(),
        strategy.risk_atr_len,
    )
    .expect("legs batch");
    let atr = legs.atr_risk[trade.entry_bar].expect("atr seeded");
    let dist = entry_stop_distance(
        &strategy,
        atr,
        trade.entry_price,
        &bars,
        trade.entry_bar,
        trade.long,
    );
    let expected = trade.entry_price
        + if trade.long {
            dist * strategy.tp1_mult
        } else {
            -dist * strategy.tp1_mult
        };
    assert!(
        (trade.exit_price - expected).abs() < 1e-9,
        "TP1 fills at its level: got {} want {expected}",
        trade.exit_price
    );
}

/// A touched stop fills at its level: entry minus one stop distance for a
/// long under the default ATR construction, with no ratchet armed to move
/// it.
#[test]
fn stop_fills_at_its_level() {
    let mut strategy = bare(false);
    strategy.exit_action = "Off".to_owned();
    strategy.stop_atr_mult = 1.5;
    strategy.min_stop_atr = 0.5;
    strategy.max_stop_atr = 4.0;
    let (run, bars) = run_simple(&round_trip_closes(), &strategy, 7);
    let trade = run
        .trades
        .iter()
        .find(|t| t.exit_reason == "stop")
        .expect("the fall must take out the stop");
    let legs = protopyne_kernels::lite::lite_legs(
        &bars,
        &LiteIndicatorParams::default(),
        strategy.risk_atr_len,
    )
    .expect("legs batch");
    let atr = legs.atr_risk[trade.entry_bar].expect("atr seeded");
    let dist = entry_stop_distance(
        &strategy,
        atr,
        trade.entry_price,
        &bars,
        trade.entry_bar,
        trade.long,
    );
    let expected = trade.entry_price + if trade.long { -dist } else { dist };
    assert!(
        (trade.exit_price - expected).abs() < 1e-9,
        "stop fills at its level: got {} want {expected}",
        trade.exit_price
    );
}

/// Commission is three basis points of notional on both legs, paid out of
/// cash: the fee total equals 3 bps of every filled notional, and the cash
/// behind the curve carries both legs.
#[test]
fn commission_is_three_basis_points_on_both_legs() {
    use rust_decimal::prelude::ToPrimitive as _;

    let (run, _) = run_simple(&round_trip_closes(), &strategy_with(20), 7);
    assert!(!run.trades.is_empty(), "the tape must trade");
    let traded: f64 = run
        .decisions
        .iter()
        .filter(|d| d.submitted)
        .filter_map(|d| {
            let price = d.fill_price?.as_decimal().to_f64()?;
            Some(price * f64::from(d.filled_quantity))
        })
        .sum();
    assert!(traded > 0.0, "the tape fills something");
    let booked = run
        .report
        .total_fees
        .as_decimal()
        .to_f64()
        .expect("representable");
    // Every fill prices through the same schedule the loop applies, so the
    // booked total is exactly 3 bps of filled notional (to Money rounding).
    assert!(
        (booked - traded * 0.0003).abs() < 0.02,
        "fees are 3 bps of notional on both legs: booked {booked} on {traded} traded"
    );
}

/// Notional is counted once per fill: the booked filled-notional must
/// equal the exact sum of every submitted fill. A second add of any leg
/// (e.g. once exact-`Decimal`, once via `f64`) would inflate the total
/// without touching per-trade P&L or fees.
#[test]
fn filled_notional_counts_each_leg_once() {
    use rust_decimal::Decimal;

    let (run, _) = run_simple(&round_trip_closes(), &strategy_with(20), 7);
    assert!(!run.trades.is_empty(), "the tape must trade");
    // Reconcile fills, not trades: open positions at the tape end have
    // entries without closes, so the trade list undercounts by design.
    // Every submitted fill lands in the ledger exactly once.
    let mut expected = Decimal::ZERO;
    let mut fills = 0u64;
    for decision in run.decisions.iter().filter(|d| d.submitted) {
        if let (Some(price), qty) = (decision.fill_price, decision.filled_quantity) {
            assert!(qty > 0, "submitted fills move size");
            expected += price.as_decimal() * Decimal::from(qty);
            fills += 1;
        }
    }
    assert!(fills > 0, "the tape fills");
    assert_eq!(
        run.report.filled_notional.as_decimal(),
        expected,
        "filled notional counts each fill exactly once"
    );
}

/// The reference halts entries for the rest of the day after a daily-loss
/// flatten. The legacy path re-enters after cooldown; the simple loop
/// latches the halt.
#[test]
fn daily_loss_halts_entries_for_the_rest_of_the_day() {
    let mut strategy = bare(false);
    strategy.size_mode = "Fixed Quantity".to_owned();
    strategy.fixed_qty = 100.0;
    strategy.max_daily_loss_pct = 0.1;
    strategy.stop_atr_mult = 1.5;
    strategy.min_stop_atr = 0.5;
    strategy.max_stop_atr = 4.0;
    strategy.exit_action = "Off".to_owned();
    strategy.sig_entry_level = 30.0;
    // A second V after the fall: the rebound arms a fresh trigger while the
    // book is still breached, so a missing day-halt would show as a new
    // entry.
    let mut closes = round_trip_closes();
    for i in 0..40 {
        closes.push(132.0 + (f64::from(i) + 1.0) * 2.0);
    }
    let (run, _) = run_simple(&closes, &strategy, 7);
    let flatten = run
        .trades
        .iter()
        .find(|t| t.exit_reason == "daily-loss")
        .expect("the breach must flatten the open position");
    // The halt covers fills, not just closes: an entry that never exits
    // still violates it, so the assertion reads submitted decisions.
    assert!(
        run.decisions
            .iter()
            .all(|d| !d.submitted || d.bar_index <= flatten.exit_bar),
        "no entry may open after the daily-loss halt, flatten at bar {}",
        flatten.exit_bar
    );
    assert!(
        run.status
            .iter()
            .any(|row| row.status.starts_with("HALTED")),
        "the status table names the halt"
    );
    assert_eq!(
        run.status.len(),
        closes.len(),
        "every bar carries its status row, reversals or not"
    );
}

/// Identical inputs rerun byte-identically through the simple path.
#[test]
fn simple_rerun_is_byte_identical() {
    let closes = round_trip_closes();
    let (first, _) = run_simple(&closes, &strategy_with(20), 7);
    let (second, _) = run_simple(&closes, &strategy_with(20), 7);
    assert_eq!(
        first.to_canonical_json(),
        second.to_canonical_json(),
        "identical inputs rerun byte-identically"
    );
    assert!(!first.trades.is_empty(), "the tape must trade");
}
