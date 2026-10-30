//! Pine-parity characterization: the reference script's semantics pinned at
//! the equity-run level, exercised through the simple decision stack.
//!
//! Source: `data/protopyne-rs/strategies/_IntegrationLiteStrategy.pine`
//! (single position, entries at close, `calcQty` sizing, stop + 3 TP zones +
//! runner exits, commission 0.03 %, slippage 1, win-rate/net-profit math).
//! Each test names the Pine lines it pins.

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use protopyne_strategy::integration_lite::IntegrationLiteStrategyConfig;
use protopyne_strategy::signals_lite::LiteIndicatorParams;
use protopyne_types::{Bar, Timeframe};

use crate::backtest::BacktestConfig;
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

/// Fall, sharp rise, then a fall: entries fire at the turn (Pine trigger +
/// filter stack on confirmed bars).
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
        .data_version("pine-parity-v1".to_owned())
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

/// Every exit isolated except the one under test: no filters, no TP zones,
/// no breakeven/trailing, and a stop far enough away that nothing else can
/// close the book.
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
    strategy.stop_atr_mult = 50.0;
    strategy.min_stop_atr = 0.1;
    strategy.max_stop_atr = 1_000.0;
    strategy
}

fn run_tape(
    closes: &[f64],
    strategy: &IntegrationLiteStrategyConfig,
    indicator: &LiteIndicatorParams,
    seed: u64,
) -> (EquityRun, Vec<Bar>) {
    let bars = tape(closes, 15);
    let run = SimpleBacktestRequest::builder()
        .indicator(indicator)
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
        .expect("equity run completes");
    (run, bars)
}

/// Pine header lines 5-8: `process_orders_on_close=true` — entries print at
/// the signal bar's close. The venue synthesizes the touch from the close
/// with a symmetric spread, so a long must fill at the ask: above the close
/// by half the spread, never better than the close itself.
#[test]
fn entries_fill_at_the_close() {
    let (run, bars) = run_tape(
        &round_trip_closes(),
        &strategy_with(20),
        &LiteIndicatorParams::default(),
        7,
    );
    assert!(!run.trades.is_empty(), "the turn must open a trade");
    for trade in &run.trades {
        let close = bars[trade.entry_bar]
            .c
            .to_string()
            .parse::<f64>()
            .expect("representable");
        let drift = (trade.entry_price - close) / close;
        if trade.long {
            // Ask touch: half of the 5 bps synthesis spread above the close.
            assert!(
                (0.0..0.001).contains(&drift),
                "long entry fills at the close ask, got drift {drift} on close {close}",
            );
        } else {
            assert!(
                (-0.001..0.0).contains(&drift),
                "short entry fills at the close bid, got drift {drift} on close {close}",
            );
        }
    }
}

/// Pine lines 800-838: every exit reason the loop can name must belong to
/// the reference vocabulary (flatten reasons, opposite/ribbon/zero/time
/// exits, stop + TP zones + runner, exhaustion, reversal).
#[test]
fn exits_speak_only_the_pine_vocabulary() {
    const VOCABULARY: &[&str] = &[
        "stop",
        "take-profit",
        "exhaustion-close",
        "exhaustion-partial",
        "opposite-signal",
        "ribbon-flip",
        "zero-cross",
        "time-stop",
        "session-flatten",
        "daily-loss",
        "reverse",
    ];
    let (run, _) = run_tape(
        &round_trip_closes(),
        &strategy_with(20),
        &LiteIndicatorParams::default(),
        7,
    );
    assert!(!run.trades.is_empty(), "the tape must trade");
    for trade in run.trades.iter().chain(run.partials.iter()) {
        assert!(
            VOCABULARY.contains(&trade.exit_reason.as_str()),
            "exit reason {:?} outside the Pine vocabulary",
            trade.exit_reason
        );
    }
}

/// Pine line 819: an opposite raw trigger closes the trade when the operator
/// disallows reversal (with reversal allowed the same trigger flips instead).
#[test]
fn opposite_signal_closes_when_reversal_disallowed() {
    let mut strategy = bare(false);
    strategy.exit_opposite = true;
    let (run, _) = run_tape(
        &round_trip_closes(),
        &strategy,
        &LiteIndicatorParams::default(),
        7,
    );
    assert!(
        run.trades
            .iter()
            .any(|t| t.exit_reason == "opposite-signal"),
        "an opposite trigger must close the book, got {:?}",
        run.trades
            .iter()
            .map(|t| t.exit_reason.as_str())
            .collect::<Vec<_>>()
    );
}

/// Pine line 828: the time stop counts bars held and flattens past the cap.
#[test]
fn time_stop_closes_an_open_position() {
    let mut strategy = bare(false);
    strategy.exit_time_bars = 3;
    let (run, _) = run_tape(
        &round_trip_closes(),
        &strategy,
        &LiteIndicatorParams::default(),
        7,
    );
    let stopped = run
        .trades
        .iter()
        .find(|t| t.exit_reason == "time-stop")
        .expect("the time stop must close the position");
    assert!(
        stopped.bars_held >= 3,
        "the stop fires at or past the cap, got {}",
        stopped.bars_held
    );
}

/// Pine line 822: the ribbon flipping against the open side flattens.
#[test]
fn ribbon_flip_closes_against_the_ribbon() {
    let mut strategy = bare(false);
    strategy.exit_ribbon = true;
    let (run, _) = run_tape(
        &round_trip_closes(),
        &strategy,
        &LiteIndicatorParams::default(),
        7,
    );
    assert!(
        run.trades.iter().any(|t| t.exit_reason == "ribbon-flip"),
        "a ribbon flip must close the book, got {:?}",
        run.trades
            .iter()
            .map(|t| t.exit_reason.as_str())
            .collect::<Vec<_>>()
    );
}

/// Pine line 825: the fast oscillator crossing zero against the side exits.
#[test]
fn zero_cross_closes_against_the_fast_osc() {
    let mut strategy = bare(false);
    strategy.exit_zero_cross = true;
    let (run, _) = run_tape(
        &round_trip_closes(),
        &strategy,
        &LiteIndicatorParams::default(),
        7,
    );
    assert!(
        run.trades.iter().any(|t| t.exit_reason == "zero-cross"),
        "a zero-cross must close the book, got {:?}",
        run.trades
            .iter()
            .map(|t| t.exit_reason.as_str())
            .collect::<Vec<_>>()
    );
}

/// Pine lines 758-762 + 833-835: breaching the daily-loss cap flattens the
/// open position. (The rest-of-day entry halt is a separate pin owned by the
/// simple loop, which latches it; the current path only flattens.)
#[test]
fn daily_loss_flattens_the_open_position() {
    let mut strategy = bare(false);
    strategy.size_mode = "Fixed Quantity".to_owned();
    strategy.fixed_qty = 100.0;
    strategy.max_daily_loss_pct = 0.1;
    // A default-scale stop keeps the proposal's max loss inside the test
    // grant's per-trade budget (wide-stop × 100 shares would price $10k of
    // max loss and the grant would deny the entry before any bar bleeds).
    strategy.stop_atr_mult = 1.5;
    strategy.min_stop_atr = 0.5;
    strategy.max_stop_atr = 4.0;
    // No exhaustion response: the full 100 shares must ride the fall so the
    // marked equity crosses the cap before any stop or partial can exit.
    strategy.exit_action = "Off".to_owned();
    // A low trigger level so the rebound arms promptly after cooldown.
    strategy.sig_entry_level = 30.0;
    // A second V after the fall: the rebound arms a fresh trigger while the
    // book is still breached, so a missing day-halt shows as a new entry.
    let mut closes = round_trip_closes();
    for i in 0..40 {
        closes.push(132.0 + (f64::from(i) + 1.0) * 2.0);
    }
    let (run, _) = run_tape(&closes, &strategy, &LiteIndicatorParams::default(), 7);
    let flatten = run
        .trades
        .iter()
        .find(|t| t.exit_reason == "daily-loss")
        .expect("the breach must flatten the open position");
    // The halt half of the Pine contract: no entries for the rest of the
    // day, fills included. The simple loop latches the halt at the breach.
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
}
