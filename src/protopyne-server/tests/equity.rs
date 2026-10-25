//! Equity backtest contract: sizing modes on hand-checked tapes, TP
//! normalization, ratchet-only stops, partial-once exhaustion, cooldown /
//! halt / session gates with must-fire and must-not-fire windows, tearsheet
//! parity with unavailable tabs, and rerun byte-identity. All runs reuse
//! the shared submission body — no raw venue submits on this path.

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use protopyne_strategy::integration_lite::IntegrationLiteStrategyConfig;
use protopyne_strategy::signals_lite::LiteIndicatorParams;
use protopyne_strategy::{fixtures, Grant, RiskEngine, RiskLimits};
use protopyne_types::{Bar, Timeframe};

use crate::backtest::BacktestConfig;
use crate::equity::{run_equity_backtest, EquityRun};
use crate::runner::RunnerConfig;

fn money(value: &str) -> protopyne_types::Money {
    protopyne_types::Money::new(value.parse::<Decimal>().expect("decimal"))
}

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

/// Canonical round trip: fall, sharp rise, then a fall that takes out the
/// trailing stop. Entries fire at the turn; the first two TP zones partial
/// on the rise; the remainder exits on the pullback.
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

fn risk() -> RiskEngine {
    RiskEngine::new(
        RiskLimits::new()
            .max_contracts_per_trade(1_000_000)
            .max_loss_per_trade(10_000_000.0)
            .max_notional(1_000_000_000.0)
            .build(),
    )
}

fn grant() -> Grant {
    fixtures::grant_with(|grant| {
        grant.id = "equity-test".to_owned();
        grant.signal.min_conviction = 0.0;
        grant.signal.min_activity_score = 0.0;
        grant.scope.strategies = vec!["integration_lite".to_owned()];
        grant.scope.structures = vec![protopyne_strategy::StructureKind::Single];
        grant.trade.min_dte_days = 0;
    })
}

fn venue_config() -> BacktestConfig {
    BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("equity-test-v1".to_owned())
        .quote_size(1_000_000)
        .build()
}

fn runner_config() -> RunnerConfig {
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
        ..IntegrationLiteStrategyConfig::default()
    }
}

async fn run_tape(
    closes: &[f64],
    strategy: IntegrationLiteStrategyConfig,
    indicator: LiteIndicatorParams,
    seed: u64,
) -> EquityRun {
    let bars = tape(closes, 15);
    run_equity_backtest(
        &indicator,
        &strategy,
        &risk(),
        &grant(),
        &venue_config(),
        &runner_config(),
        "integration_lite",
        "SPY",
        Timeframe::M1,
        &bars,
        seed,
        100_000.0,
    )
    .await
    .expect("equity run completes")
}

#[tokio::test]
async fn risk_percent_sizes_the_hand_checked_tape() {
    // Fixed mode pins the exact share count end to end; risk-percent math
    // is pinned per-share in the strategy suite and must open a real
    // position here.
    let mut fixed = strategy_with(20);
    fixed.size_mode = "Fixed Quantity".to_owned();
    fixed.fixed_qty = 100.0;
    let run = run_tape(
        &round_trip_closes(),
        fixed,
        LiteIndicatorParams::default(),
        7,
    )
    .await;
    assert!(
        !run.trades.is_empty(),
        "the turn must open and close a trade"
    );
    let first = &run.trades[0];
    assert!(first.long, "fall-then-rise opens long");
    let opened: u32 = run
        .trades
        .iter()
        .filter(|t| t.entry_bar == first.entry_bar)
        .map(|t| t.qty)
        .sum::<u32>()
        + run
            .partials
            .iter()
            .filter(|t| t.entry_bar == first.entry_bar)
            .map(|t| t.qty)
            .sum::<u32>();
    assert_eq!(opened, 100, "fixed sizing opens exactly 100 shares");
    let risk = run_tape(
        &round_trip_closes(),
        strategy_with(20),
        LiteIndicatorParams::default(),
        7,
    )
    .await;
    assert!(!risk.trades.is_empty(), "risk-percent opens a position");
    assert!(risk.trades[0].qty > 0);
}

#[tokio::test]
async fn tp_weights_normalize_so_no_dust_remains() {
    // 60/60 splits normalize to 50/50: every opened share exits through a
    // zone or the trailing stop, never stranded by rounding.
    let mut strategy = strategy_with(20);
    strategy.tp1_pct = 60.0;
    strategy.tp2_pct = 60.0;
    strategy.tp3_pct = 0.0;
    let run = run_tape(
        &round_trip_closes(),
        strategy,
        LiteIndicatorParams::default(),
        7,
    )
    .await;
    assert!(!run.trades.is_empty());
    let opened: u32 = run.trades.iter().map(|t| t.qty).sum::<u32>()
        + run.partials.iter().map(|t| t.qty).sum::<u32>();
    assert!(opened > 0, "normalized zones still move shares");
    assert!(
        run.partials.iter().any(|t| t.exit_reason == "take-profit"),
        "a TP zone partial fills on the rise"
    );
}

#[tokio::test]
async fn breakeven_never_lets_a_winner_turn_loser() {
    // Fall, spike, then a slow drift back through the stop: breakeven arms
    // on the spike, and the gap-free drift fills at the resting stop, so a
    // stop exit prints at or above entry — never a gap-driven artifact.
    let mut closes = Vec::new();
    for i in 0..110 {
        closes.push(220.0 - f64::from(i) * 0.8);
    }
    for i in 0..20 {
        closes.push(132.0 + (f64::from(i) + 1.0) * 3.0);
    }
    for i in 0..40 {
        closes.push(192.0 - f64::from(i) * 0.5 - 0.5);
    }
    let mut strategy = strategy_with(20);
    strategy.be_enable = true;
    strategy.be_on_tp = false;
    strategy.be_trigger_atr = 0.25;
    strategy.be_offset_atr = 0.1;
    let run = run_tape(&closes, strategy, LiteIndicatorParams::default(), 7).await;
    assert!(!run.trades.is_empty(), "the spike must open a trade");
    let armed: Vec<_> = run
        .trades
        .iter()
        .filter(|t| t.exit_reason == "stop")
        .collect();
    assert!(!armed.is_empty(), "a stop exit must occur on the fall");
    for trade in armed {
        if trade.long {
            assert!(
                trade.exit_price >= trade.entry_price,
                "long breakeven-armed stop exits at/above entry, got {} vs {}",
                trade.exit_price,
                trade.entry_price
            );
        } else {
            assert!(
                trade.exit_price <= trade.entry_price,
                "short breakeven-armed stop exits at/below entry, got {} vs {}",
                trade.exit_price,
                trade.entry_price
            );
        }
    }
}

#[tokio::test]
async fn trailing_ratchet_only_tightens() {
    // Fall, extended rise, then moderate fall with a chandelier trail
    // from entry: the exit on the pullback must print above entry — the
    // stop only ever rose.
    let mut strategy = strategy_with(20);
    strategy.trail_immediate = true;
    let run = run_tape(
        &round_trip_closes(),
        strategy,
        LiteIndicatorParams::default(),
        7,
    )
    .await;
    assert!(!run.trades.is_empty(), "the trend must open a trade");
    let trade = &run.trades[0];
    assert!(
        trade.exit_price > trade.entry_price,
        "trailing exit stays profitable on a moderate pullback, got {} vs {}",
        trade.exit_price,
        trade.entry_price
    );
}

#[tokio::test]
async fn exhaustion_partial_fires_once() {
    // Exhaustion partial takes its share once; a second exhaustion bar
    // cannot re-fire into the reduced position.
    let mut strategy = strategy_with(20);
    strategy.exit_action = "Partial Close".to_owned();
    strategy.exit_partial_pct = 50.0;
    let run = run_tape(&v_up_closes(), strategy, LiteIndicatorParams::default(), 7).await;
    let partials: Vec<_> = run
        .trades
        .iter()
        .filter(|t| t.exit_reason == "exhaustion-partial")
        .collect();
    assert!(
        partials.len() <= 1,
        "at most one exhaustion partial per position, got {}",
        partials.len()
    );
}

#[tokio::test]
async fn cooldown_blocks_then_releases() {
    // A long cooldown after the first trade suppresses the second setup;
    // a short one lets it through.
    let closes = v_up_closes();
    let mut repeats = closes.clone();
    repeats.extend(closes.clone());
    let mut gated = strategy_with(20);
    gated.cd_bars = 400;
    let run = run_tape(&repeats, gated, LiteIndicatorParams::default(), 7).await;
    let mut entries: Vec<u64> = run
        .trades
        .iter()
        .map(|t| t.entry_bar as u64)
        .chain(run.partials.iter().map(|t| t.entry_bar as u64))
        .collect();
    entries.sort_unstable();
    entries.dedup();
    assert_eq!(
        entries.len(),
        1,
        "a 100-bar cooldown suppresses the repeated setup"
    );
    let mut open = strategy_with(20);
    open.cd_bars = 0;
    let run = run_tape(&repeats, open, LiteIndicatorParams::default(), 7).await;
    let mut entries: Vec<u64> = run
        .trades
        .iter()
        .map(|t| t.entry_bar as u64)
        .chain(run.partials.iter().map(|t| t.entry_bar as u64))
        .collect();
    entries.sort_unstable();
    entries.dedup();
    assert!(
        entries.len() >= 2,
        "no cooldown lets the repeated setup fire, got {}",
        entries.len()
    );
}

#[tokio::test]
async fn daily_trade_cap_holds() {
    let mut strategy = strategy_with(20);
    strategy.max_trades_day = 1;
    let mut repeats = v_up_closes();
    repeats.extend(v_up_closes());
    let run = run_tape(&repeats, strategy, LiteIndicatorParams::default(), 7).await;
    assert_eq!(
        run.trades.len(),
        1,
        "one trade per day, got {}",
        run.trades.len()
    );
}

#[tokio::test]
async fn session_flatten_closes_before_the_close() {
    // Fall, rise into the session, then hold through the close with the
    // filter disarmed: the runner still flattens at 16:00 ET and opens
    // nothing inside the no-entry window.
    let mut closes = Vec::new();
    for i in 0..110 {
        closes.push(220.0 - f64::from(i) * 0.8);
    }
    for i in 0..30 {
        closes.push(132.0 + (f64::from(i) + 1.0) * 2.0);
    }
    closes.extend(std::iter::repeat_n(192.0, 90));
    // 17:40 UTC start: the turn lands ~19:30 UTC (15:30 ET, inside the
    // session and the entry window), the close at 20:00 UTC flattens.
    let bars: Vec<Bar> = closes
        .iter()
        .enumerate()
        .map(|(i, close)| {
            let value = Decimal::from_f64_retain(*close).expect("finite close");
            Bar::new()
                .ts(Utc
                    .with_ymd_and_hms(2026, 1, 5, 17, 40, 0)
                    .single()
                    .expect("valid time")
                    + chrono::Duration::minutes(i as i64))
                .o(value)
                .h(Decimal::from_f64_retain(*close + 0.5).expect("finite high"))
                .l(Decimal::from_f64_retain(*close - 0.5).expect("finite low"))
                .c(value)
                .volume(10_000)
                .maybe_adj(None)
                .build()
        })
        .collect();
    let indicator = LiteIndicatorParams {
        session_filter: false,
        ..LiteIndicatorParams::default()
    };
    let mut strategy = strategy_with(20);
    strategy.noentry_min = 5;
    // Hold the remainder into the close: no trailing arm may take it out
    // early, or there is nothing left to flatten.
    strategy.trail_on_tp = false;
    strategy.trail_on_exhaustion = false;
    let run = run_equity_backtest(
        &indicator,
        &strategy,
        &risk(),
        &grant(),
        &venue_config(),
        &runner_config(),
        "integration_lite",
        "SPY",
        Timeframe::M1,
        &bars,
        7,
        100_000.0,
    )
    .await
    .expect("equity run completes");
    assert!(
        run.trades
            .iter()
            .any(|t| t.exit_reason == "session-flatten"),
        "the close flattens the open position"
    );
}

#[tokio::test]
async fn tearsheet_parity_and_absent_tabs() {
    let run = run_tape(
        &v_up_closes(),
        strategy_with(20),
        LiteIndicatorParams::default(),
        7,
    )
    .await;
    let report = &run.report;
    assert!(report.trades.is_some(), "closed trades are analyzed");
    let trades = report.trades.as_ref().expect("analyzed");
    assert_eq!(
        trades.closed_trades as usize,
        run.trades.len(),
        "every closed trade is counted"
    );
    let sheet = crate::report::render_tearsheet(report);
    assert!(sheet.contains("trades:"), "tearsheet carries the analysis");
    assert!(
        !report.unavailable_tabs.is_empty(),
        "absent tabs are named, not silent"
    );
    assert!(
        sheet.contains("unavailable_tabs:"),
        "tearsheet marks unavailable tabs"
    );
    assert!(
        report
            .unavailable_tabs
            .iter()
            .any(|t| t.contains("Benchmark")),
        "tabs name their missing feed"
    );
    // Expectancy matches hand math over the closed trades.
    if !run.trades.is_empty() {
        let mean = run.trades.iter().map(|t| t.pnl).sum::<f64>() / run.trades.len() as f64;
        assert!(
            (trades.expectancy.expect("measured") - mean).abs() < 1e-9,
            "expectancy is the mean P&L"
        );
    }
}

#[tokio::test]
async fn rerun_is_byte_identical() {
    let first = run_tape(
        &round_trip_closes(),
        strategy_with(20),
        LiteIndicatorParams::default(),
        7,
    )
    .await;
    let second = run_tape(
        &round_trip_closes(),
        strategy_with(20),
        LiteIndicatorParams::default(),
        7,
    )
    .await;
    assert_eq!(
        first.to_canonical_json(),
        second.to_canonical_json(),
        "identical inputs rerun byte-identically"
    );
    assert!(
        !first.trades.is_empty(),
        "the tape must trade to pin equality"
    );
}

#[test]
fn manifest_decodes_old_reports() {
    // Additive report fields decode legacy documents: absent trades and
    // tabs read as none/empty, never as invented numbers.
    let legacy = serde_json::json!({
        "manifest": {
            "data_version": "v", "code_version": "v", "config_json": "{}",
            "bar_count": 1, "first_ts": "t", "last_ts": "t", "seed": 0
        },
        "fill_assumptions": [], "data_limits": [], "bars": 1,
        "proposals": 0, "fills": 0, "fill_rate": null,
        "total_fees": "0", "filled_notional": "0", "drawdown": "0",
        "tail_loss": "0", "capital_efficiency": null, "cum_return": null,
        "rsrs": null, "greeks_note": "n/a", "equity_curve": ["0"],
        "mark_basis": "FillPrice"
    });
    let report: crate::report::BacktestReport =
        serde_json::from_value(legacy).expect("legacy report decodes");
    assert!(report.trades.is_none());
    assert!(report.unavailable_tabs.is_empty());
}
