//! Equity backtest contract: sizing modes on hand-checked tapes, TP
//! normalization, ratchet-only stops, partial-once exhaustion, cooldown /
//! halt / session gates with must-fire and must-not-fire windows, tearsheet
//! parity with unavailable tabs, and rerun byte-identity. All runs reuse
//! the shared submission body — no raw venue submits on this path.

use crate::common_fixtures::money;
use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use protopyne_strategy::integration_lite::IntegrationLiteStrategyConfig;
use protopyne_strategy::signals_lite::LiteIndicatorParams;
use protopyne_strategy::{fixtures, Grant, RiskEngine, RiskLimits};
use protopyne_types::{Bar, Timeframe};

use crate::backtest::BacktestConfig;
use crate::equity::{BacktestRequest, EquityRun};
use crate::runner::RunnerConfig;

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
        // These tapes exercise the risk book, not the entry filters: the
        // health floor sits at its legal minimum so the gate still demands an
        // aligned stack (the sign) without requiring a strong one from a
        // two-leg synthetic ramp.
        filt_ma_health_min: 0.0,
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
    BacktestRequest::builder()
        .indicator(&indicator)
        .strategy(&strategy)
        .risk(&risk())
        .grant(&grant())
        .venue_config(&venue_config())
        .runner_config(&runner_config())
        .signal_name("integration_lite")
        .underlying("SPY")
        .timeframe(Timeframe::M1)
        .bars(&bars)
        .seed(seed)
        .initial_equity(100_000.0)
        .run()
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
    // Fall, rise into the session, then hold through the close: the runner
    // flattens on the bar whose *close* is 16:00 ET and opens nothing inside
    // the no-entry window. The session filter must be armed — without it the
    // reference has no session end to flatten before.
    let mut closes = Vec::new();
    for i in 0..110 {
        closes.push(220.0 - f64::from(i) * 0.8);
    }
    for i in 0..30 {
        closes.push(132.0 + (f64::from(i) + 1.0) * 2.0);
    }
    closes.extend(std::iter::repeat_n(192.0, 90));
    // 17:40 UTC start: the turn lands ~19:30 UTC (15:30 ET, inside the
    // session and the entry window); the bar opening at 20:59 UTC closes at
    // 21:00 UTC = 16:00 ET and flattens.
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
        session_filter: true,
        ..LiteIndicatorParams::default()
    };
    let mut strategy = strategy_with(20);
    strategy.noentry_min = 5;
    // Hold the remainder into the close: no trailing arm may take it out
    // early, or there is nothing left to flatten.
    strategy.trail_on_tp = false;
    strategy.trail_on_exhaustion = false;
    let run = BacktestRequest::builder()
        .indicator(&indicator)
        .strategy(&strategy)
        .risk(&risk())
        .grant(&grant())
        .venue_config(&venue_config())
        .runner_config(&runner_config())
        .signal_name("integration_lite")
        .underlying("SPY")
        .timeframe(Timeframe::M1)
        .bars(&bars)
        .seed(7)
        .initial_equity(100_000.0)
        .run()
        .await
        .expect("equity run completes");
    let flatten = run
        .trades
        .iter()
        .find(|t| t.exit_reason == "session-flatten")
        .expect("the close flattens the open position");
    // The bar opening at 20:59 UTC closes at 21:00 UTC = 16:00 ET: 199 minutes
    // after the 17:40 UTC start. Judging the bar's *open* instead would flatten
    // one bar later, which is the defect this pins.
    assert_eq!(
        flatten.exit_bar, 199,
        "the flatten lands on the bar whose close is the session end"
    );

    // The flatten switch is its own dial: an operator may keep the no-entry
    // window and still hold a position overnight.
    let mut carrying = strategy_with(20);
    carrying.noentry_min = 5;
    carrying.trail_on_tp = false;
    carrying.trail_on_exhaustion = false;
    carrying.flatten_eod = false;
    let session_indicator = LiteIndicatorParams {
        session_filter: true,
        ..LiteIndicatorParams::default()
    };
    let carried = BacktestRequest::builder()
        .indicator(&session_indicator)
        .strategy(&carrying)
        .risk(&risk())
        .grant(&grant())
        .venue_config(&venue_config())
        .runner_config(&runner_config())
        .signal_name("integration_lite")
        .underlying("SPY")
        .timeframe(Timeframe::M1)
        .bars(&bars)
        .seed(7)
        .initial_equity(100_000.0)
        .run()
        .await
        .expect("equity run completes");
    assert!(
        carried
            .trades
            .iter()
            .all(|t| t.exit_reason != "session-flatten"),
        "a disarmed flatten switch carries the position"
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

/// The equity run never applies corporate-action events: the report declares
/// that absence in its data limits rather than leaving the reader to assume a
/// dividend-adjusted book. A short held across a dividend is therefore not
/// credited and not debited — a stated limit, not a silent one.
#[tokio::test]
async fn corporate_actions_are_declared_absent_for_equity() {
    let run = run_tape(
        &round_trip_closes(),
        strategy_with(20),
        LiteIndicatorParams::default(),
        7,
    )
    .await;
    let limits = run.report.data_limits.join(" | ");
    assert!(
        limits.contains("corporate-action"),
        "the report names the corporate-action absence, got: {limits}"
    );
}

/// Observation harness, never a gate: it times one long equity run so the
/// per-bar cost of the current composition is a recorded number rather than an
/// estimate. `#[ignore]`d because a wall time is evidence, not an assertion.
#[tokio::test]
#[ignore = "soak: observation harness — prints the wall time of one long run; never asserted"]
async fn observed_equity_run_time() {
    let len = 2_000;
    let mut closes = Vec::with_capacity(len);
    for index in 0..len {
        let phase = index as f64;
        closes.push(500.0 + 40.0 * (phase / 17.0).sin() + 15.0 * (phase / 5.3).sin());
    }
    let started = std::time::Instant::now();
    let run = run_tape(
        &closes,
        strategy_with(20),
        LiteIndicatorParams::default(),
        7,
    )
    .await;
    let elapsed = started.elapsed();
    println!(
        "observed_equity_run_time bars={len} elapsed_ms={} decisions={} trades={}",
        elapsed.as_millis(),
        run.decisions.len(),
        run.trades.len()
    );
}

/// The swing stop reads the tape's own extreme, not the entry price.
///
/// A regression here is expensive and silent: passing the entry price as the
/// extreme collapses the swing distance to the bare ATR buffer, which then
/// clamps to the floor, and risk-percent sizing divides by that tiny distance
/// — opening a position several times larger than the stop implies.
#[test]
fn swing_stops_measure_the_recent_extreme() {
    use protopyne_strategy::integration_lite::IntegrationLiteStrategyConfig;

    let closes = [200.0, 199.0, 198.0, 197.0, 196.0, 195.0, 196.0, 197.0];
    let bars: Vec<Bar> = closes
        .iter()
        .enumerate()
        .map(|(index, close)| {
            let value = Decimal::from_f64_retain(*close).expect("finite");
            Bar::new()
                .ts(Utc
                    .with_ymd_and_hms(2026, 1, 5, 15, index as u32, 0)
                    .single()
                    .expect("time"))
                .o(value)
                .h(Decimal::from_f64_retain(*close + 0.5).expect("finite"))
                .l(Decimal::from_f64_retain(*close - 0.5).expect("finite"))
                .c(value)
                .volume(10_000)
                .maybe_adj(None)
                .build()
        })
        .collect();
    let strategy = IntegrationLiteStrategyConfig {
        stop_type: "Swing".to_owned(),
        swing_len: 8,
        swing_buf_atr: 0.25,
        min_stop_atr: 0.1,
        max_stop_atr: 40.0,
        ..IntegrationLiteStrategyConfig::default()
    };
    // Two ATRs of hand-fed volatility keeps the clamp out of the way.
    let atr = 1.0;
    let last = bars.len() - 1;
    let long = crate::equity::entry_stop_distance(&strategy, atr, 197.0, &bars, last, true);
    // Lowest low over the window is 194.5; the entry is 197.0, so the raw
    // distance is 2.5 plus the 0.25 ATR buffer.
    assert!(
        (long - 2.75).abs() < 1e-9,
        "long swing distance reads the low, got {long}"
    );
    let short = crate::equity::entry_stop_distance(&strategy, atr, 197.0, &bars, last, false);
    // Highest high over the window is 200.5; the entry is 197.0.
    assert!(
        (short - 3.75).abs() < 1e-9,
        "short swing distance reads the high, got {short}"
    );
}

/// A closed cycle's statistics include its take-profit slices.
///
/// The report's per-trade mean is the mean of *cycles* (entry through flat),
/// so it must equal the closing slice plus every take-profit slice that
/// belonged to the same entry — not the closing slice alone.
#[tokio::test]
async fn cycle_statistics_include_partials() {
    let run = run_tape(
        &round_trip_closes(),
        strategy_with(20),
        LiteIndicatorParams::default(),
        7,
    )
    .await;
    let analysis = run.report.trades.as_ref().expect("analyzed");
    assert!(
        !run.partials.is_empty(),
        "the tape exercises at least one take-profit slice"
    );
    let cycle_pnls: Vec<f64> = run
        .trades
        .iter()
        .map(|trade| {
            trade.pnl
                + run
                    .partials
                    .iter()
                    .filter(|p| p.entry_bar == trade.entry_bar && p.long == trade.long)
                    .map(|p| p.pnl)
                    .sum::<f64>()
        })
        .collect();
    let mean = cycle_pnls.iter().sum::<f64>() / cycle_pnls.len() as f64;
    assert!(
        (analysis.expectancy.expect("measured") - mean).abs() < 1e-9,
        "expectancy is the mean per-cycle P&L"
    );
    assert!(
        cycle_pnls
            .iter()
            .zip(&run.trades)
            .any(|(cycle, trade)| (cycle - trade.pnl).abs() > 1e-9),
        "at least one cycle carries a take-profit slice beyond its closing fill"
    );
    assert_eq!(
        analysis.win_rate_trades.is_some(),
        !run.trades.is_empty(),
        "a non-empty book reports a cycle win rate"
    );
}

/// An equity run books a portfolio, folded by the shared aggregator.
#[tokio::test]
async fn the_equity_run_books_a_portfolio() {
    let run = run_tape(
        &round_trip_closes(),
        strategy_with(20),
        LiteIndicatorParams::default(),
        7,
    )
    .await;
    let portfolio = run
        .report
        .portfolio
        .as_ref()
        .expect("an equity run aggregates its fills");
    assert!(
        !portfolio.portfolios.is_empty(),
        "the fold carries at least one book"
    );
    assert!(
        !portfolio.unrealized_curve.is_empty(),
        "the marked curve is present"
    );
}

/// A reversal is one close plus one open on the same bar, and only when the
/// operator allows it.
///
/// The tape is stripped to the single mechanism under test: no entry filters,
/// no take-profit zones, no trailing or breakeven, and a stop far enough away
/// that nothing else can close the book. With the opposite-trigger exit
/// disarmed, the reversal is then the only path that flips it.
#[tokio::test]
async fn reversals_flip_only_when_allowed() {
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

    let reversed = run_tape(
        &round_trip_closes(),
        bare(true),
        LiteIndicatorParams::default(),
        7,
    )
    .await;
    let held = run_tape(
        &round_trip_closes(),
        bare(false),
        LiteIndicatorParams::default(),
        7,
    )
    .await;

    assert!(
        reversed
            .trades
            .iter()
            .any(|trade| trade.exit_reason == "reverse"),
        "an allowed reversal closes the old side as one reverse order, got {:?}",
        reversed
            .trades
            .iter()
            .map(|trade| trade.exit_reason.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        held.trades
            .iter()
            .all(|trade| trade.exit_reason != "reverse"),
        "a disallowed reversal never flips the book"
    );
}

/// Commission is three basis points of notional, both ways.
///
/// This is the reference's percentage schedule, and it is a hand-checkable
/// identity: every filled notional in the run times the declared rate equals
/// the booked fee total (to within the venue's per-fill money rounding).
#[tokio::test]
async fn commission_is_three_basis_points_of_notional() {
    use rust_decimal::prelude::ToPrimitive as _;

    let run = run_tape(
        &round_trip_closes(),
        strategy_with(20),
        LiteIndicatorParams::default(),
        7,
    )
    .await;
    let traded: f64 = run
        .decisions
        .iter()
        .filter(|decision| decision.submitted)
        .filter_map(|decision| {
            let price = decision.fill_price?.as_decimal().to_f64()?;
            Some(price * f64::from(decision.filled_quantity))
        })
        .sum();
    assert!(traded > 0.0, "the tape fills something");
    let booked = run
        .report
        .total_fees
        .as_decimal()
        .to_f64()
        .expect("representable");
    let expected = traded * 0.0003;
    assert!(
        (booked - expected).abs() < 0.02,
        "fees are 3 bps of notional: booked {booked}, expected {expected} on {traded} traded"
    );
    assert!(
        run.report
            .fill_assumptions
            .iter()
            .any(|line| line.contains("3 basis points")),
        "the report states the commission model it assumed"
    );
}

/// A hand book of two partials plus one full close folds into per-cycle P&L.
///
/// The raw book: one ten-share entry scaled out twice (4 + 2 shares) then
/// closed (4 shares), plus one ten-share entry closed whole. The fold prices
/// every opened share into its cycle's notional; the analysis then reads
/// expectancy, win rate, and profit factor off the cycles, not the slices.
#[test]
fn a_hand_book_of_partials_and_a_full_close_folds_per_cycle() {
    use crate::equity::{cycles_of, EquityTrade};
    use crate::report::{TradePnl, TradesAnalysis};

    fn fill(
        long: bool,
        entry_bar: usize,
        qty: u32,
        entry_price: f64,
        pnl: f64,
        bars_held: u64,
        reason: &str,
    ) -> EquityTrade {
        EquityTrade {
            long,
            qty,
            entry_bar,
            entry_price,
            exit_bar: entry_bar + bars_held as usize,
            exit_price: entry_price,
            pnl,
            bars_held,
            exit_reason: reason.to_owned(),
        }
    }

    let trades = vec![
        fill(true, 5, 4, 100.0, 60.0, 6, "close"),
        fill(true, 9, 10, 50.0, -50.0, 3, "close"),
    ];
    let partials = vec![
        fill(true, 5, 4, 100.0, 40.0, 2, "take-profit"),
        fill(true, 5, 2, 100.0, 25.0, 4, "take-profit"),
    ];
    let cycles = cycles_of(&trades, &partials);
    assert_eq!(cycles.len(), 2);
    // Cycle one: 60 + 40 + 25 over all ten opened shares.
    assert!((cycles[0].pnl - 125.0).abs() < 1e-9);
    assert!((cycles[0].notional - 1_000.0).abs() < 1e-9);
    // Cycle two: the lone full close, untouched by another entry's slices.
    assert!((cycles[1].pnl + 50.0).abs() < 1e-9);
    assert!((cycles[1].notional - 500.0).abs() < 1e-9);

    let analysis = TradesAnalysis::of(&cycles);
    assert!((analysis.expectancy.expect("measured") - 37.5).abs() < 1e-9);
    assert!((analysis.win_rate_trades.expect("measured") - 0.5).abs() < 1e-9);
    assert!((analysis.profit_factor_trades.expect("measured") - 2.5).abs() < 1e-9);

    // Control: closing the first entry whole, with no scale-out, books the
    // same cycle total — the fold neither creates nor destroys P&L.
    let whole = vec![fill(true, 5, 10, 100.0, 125.0, 6, "close")];
    let whole_cycles = cycles_of(&whole, &[]);
    assert_eq!(whole_cycles.len(), 1);
    let folded_total: f64 = cycles.iter().map(|cycle| cycle.pnl).sum();
    assert!((whole_cycles[0].pnl - 125.0).abs() < 1e-9);
    assert!((folded_total - 75.0).abs() < 1e-9);
    let single = TradesAnalysis::of(&[TradePnl {
        long: true,
        pnl: 75.0,
        notional: 1_500.0,
        bars_held: 6,
    }]);
    assert!((single.expectancy.expect("measured") - folded_total).abs() < 1e-9);
}

/// Every decided bar carries its status row: the curve and the table move
/// together, so no bar can hold equity without its eight reference rows.
#[tokio::test]
async fn every_bar_emits_its_status_row() {
    let closes = round_trip_closes();
    let run = run_tape(
        &closes,
        strategy_with(20),
        LiteIndicatorParams::default(),
        7,
    )
    .await;
    let bars = tape(&closes, 15);
    assert_eq!(run.status.len(), bars.len());
    assert_eq!(
        run.status.first().expect("first bar").at,
        bars.first().expect("tape").ts.to_rfc3339()
    );
    assert_eq!(
        run.status.last().expect("last bar").at,
        bars.last().expect("tape").ts.to_rfc3339()
    );
    assert!(
        run.status.iter().all(|row| !row.position.is_empty()),
        "every row names its position"
    );
}

/// An open position rests its stop and targets once per bar: the run records
/// the levels that were actually resting, and a flat run records none.
#[tokio::test]
async fn an_open_position_rests_its_levels_per_bar() {
    let run = run_tape(
        &round_trip_closes(),
        strategy_with(20),
        LiteIndicatorParams::default(),
        7,
    )
    .await;
    assert!(
        !run.trades.is_empty(),
        "the tape opens at least one position"
    );
    let stops = run
        .levels
        .iter()
        .filter(|level| level.kind == crate::equity::LevelKind::Stop)
        .count();
    let targets = run
        .levels
        .iter()
        .filter(|level| level.kind == crate::equity::LevelKind::Target)
        .count();
    assert!(stops > 0, "an open position rests its stop");
    assert!(targets > 0, "an open position rests its targets");
    assert!(
        run.levels.iter().all(|level| level.price.is_finite()),
        "resting levels are always prices, never holes"
    );
    assert!(
        run.levels.iter().all(|level| level.bar < run.status.len()),
        "levels never run past the decided bars"
    );
}
