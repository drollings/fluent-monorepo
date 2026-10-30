//! Four-driver parity: one tape through the batch runner, the stepped
//! session, the interactive driver, and the paper loop must agree
//! decision-for-decision, fill-for-fill, fee-for-fee, with one equity curve.
//!
//! Every composition here is strategy-runnable (fixed signal, explicit
//! single-leg structure, composable loss rules), so each driver below runs
//! the same shared per-bar body with the same venue tuning. The equity batch
//! runner is the simple decision stack (per-bar priced units, manager-owned
//! exits, percent fees, no proposal/grant/risk/journal layers) and is not
//! strategy-runnable; its parity story stays rerun-identity plus the
//! calibration tallies.

use crate::common_fixtures::money;
use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;
use tokio::sync::mpsc;

use protopyne_strategy::{
    elements::{ExplicitStructure, LossSet, ProfitTarget, StopLoss},
    fixtures::grant_with,
    integration_lite::IntegrationLiteStrategyConfig,
    signals_lite::LiteIndicatorParams,
    Direction, FixedSignal, Grant, ProposalSide, RiskEngine, RiskLimits, Strategy, StructureKind,
};
use protopyne_types::{Bar, Money, Timeframe};
use protopyne_venue::{PaperConfig, Venue, VenueRegistry, VenueStorage};

use crate::backtest::{BacktestConfig, BacktestVenue};
use crate::equity::EquityRun;
use crate::replay_session::{ChainPolicy, ReplayDriver, ReplaySession};
use crate::runner::{equity_curve_for, run_backtest, BacktestRun, BarDecision, RunnerConfig};
use crate::simple_backtest::SimpleBacktestRequest;

/// One-minute bars from closes; high/low equal the close unless the caller
/// edits them afterwards.
fn bars_from_closes(closes: &[i64]) -> Vec<Bar> {
    closes
        .iter()
        .enumerate()
        .map(|(index, close)| {
            let price = Decimal::from(*close);
            Bar::new()
                .ts(Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
                    + chrono::Duration::minutes(index as i64))
                .o(price)
                .h(price)
                .l(price)
                .c(price)
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

fn rehearsal_strategy() -> Strategy {
    Strategy::with_signal(Arc::new(
        FixedSignal::new(Direction::Long, 0.8, 0.8).expect("valid signal"),
    ))
    .with_structure(Arc::new(ExplicitStructure {
        structure: StructureKind::Single,
    }))
}

fn exit_strategy() -> Strategy {
    rehearsal_strategy().with_loss(Arc::new(LossSet::new(vec![
        Arc::new(StopLoss::new(money("3")).expect("positive stop")),
        Arc::new(ProfitTarget::new(money("4")).expect("positive target")),
    ])))
}

fn risk() -> RiskEngine {
    RiskEngine::new(RiskLimits::new().build())
}

fn grant() -> Grant {
    grant_with(|grant| {
        grant.id = "parity-soak".to_owned();
        grant.scope.structures = vec![StructureKind::Single];
        grant.signal.min_activity_score = 0.5;
    })
}

fn venue_config() -> BacktestConfig {
    BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("parity-soak-v1".to_owned())
        .build()
}

fn runner_config() -> RunnerConfig {
    RunnerConfig::new(
        "SPY".to_owned(),
        "ribbon".to_owned(),
        money("400"),
        money("2_000"),
        30,
        0,
    )
}

fn venue_for(bars: &[Bar]) -> BacktestVenue {
    BacktestVenue::new(venue_config(), bars.to_vec(), PaperConfig::new().build())
        .expect("venue pins")
}

async fn batch_run(strategy: &Strategy, bars: &[Bar], seed: u64) -> BacktestRun {
    run_backtest(
        strategy,
        &risk(),
        &grant(),
        &venue_config(),
        &runner_config(),
        bars,
        seed,
    )
    .await
    .expect("batch runs")
}

async fn session_run(strategy: &Strategy, bars: &[Bar], seed: u64) -> Vec<BarDecision> {
    let venue = venue_for(bars);
    let risk = risk();
    let grant = grant();
    let mut session = ReplaySession::start(
        strategy,
        &risk,
        &grant,
        &venue,
        venue_config(),
        runner_config(),
        ChainPolicy::RequireChains,
        None,
        bars,
        seed,
    )
    .expect("session starts");
    let mut stepped = Vec::new();
    while let Some(decision) = session.step().await {
        stepped.push(decision);
    }
    stepped
}

async fn driver_run(strategy: &Strategy, bars: &[Bar], seed: u64) -> Vec<BarDecision> {
    let mut driver = ReplayDriver::start(
        strategy.clone(),
        risk(),
        grant(),
        venue_config(),
        runner_config(),
        bars.to_vec(),
        seed,
        0,
    )
    .expect("driver starts");
    // Step interactively in chunks, then drain: an operator stepping
    // frame-by-frame must see the same run as the batch.
    while driver.cursor() < driver.total_bars() {
        driver.step_n(3).await;
    }
    driver.decisions().to_vec()
}

fn feed(bars: &[Bar]) -> mpsc::Receiver<Bar> {
    let (tx, rx) = mpsc::channel(bars.len().max(1));
    for bar in bars {
        tx.try_send(bar.clone()).expect("capacity fits every bar");
    }
    drop(tx);
    rx
}

async fn loop_run(strategy: &Strategy, bars: &[Bar], seed: u64, storage: &str) -> Vec<BarDecision> {
    use protopyne_venue::session::{SessionConfig, TradingSession};

    use crate::trading_loop::TradingLoopExt;

    let venue: Arc<dyn Venue> = Arc::new(venue_for(bars));
    let session = TradingSession::boot(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
        VenueStorage::new(
            std::env::temp_dir().join(format!("parity-soak-{storage}-{}.db", std::process::id())),
        ),
        None,
    )
    .expect("boot");
    let mut receiver = feed(bars);
    let decisions = session
        .run_trading_loop(
            &mut receiver,
            Vec::new(),
            strategy,
            &risk(),
            &grant(),
            &venue_config(),
            &runner_config(),
            Timeframe::infer(bars).unwrap_or(Timeframe::D1),
            ChainPolicy::RequireChains,
            Duration::ZERO,
            seed,
            None,
        )
        .await
        .expect("loop runs");
    session.shutdown().await;
    decisions
}

fn fee_sum(decisions: &[BarDecision]) -> Decimal {
    decisions
        .iter()
        .filter_map(|decision| decision.fee)
        .map(Money::as_decimal)
        .sum()
}

fn exit_count(decisions: &[BarDecision]) -> usize {
    decisions
        .iter()
        .filter(|decision| decision.side == Some(ProposalSide::Exit))
        .count()
}

/// The four drivers agree on one tape: same verdicts, same fills and fees
/// (carried on the verdicts), one equity curve. The tape must also be
/// non-vacuous: at least one fill must occur, or the agreement proves
/// nothing.
async fn assert_four_way_agree(
    strategy: &Strategy,
    bars: &[Bar],
    seed: u64,
    storage: &str,
    min_exits: usize,
) {
    let batch = batch_run(strategy, bars, seed).await;
    let stepped = session_run(strategy, bars, seed).await;
    let driven = driver_run(strategy, bars, seed).await;
    let looped = loop_run(strategy, bars, seed, storage).await;
    assert_eq!(stepped, batch.decisions, "session steps what batch ran");
    assert_eq!(driven, batch.decisions, "interactive steps match the batch");
    assert_eq!(looped, batch.decisions, "the paper loop decides the batch");
    assert!(
        batch
            .decisions
            .iter()
            .any(|decision| decision.filled_quantity > 0),
        "the tape must fill at least once"
    );
    assert!(
        exit_count(&batch.decisions) >= min_exits,
        "the tape must exit at least {min_exits} time(s)"
    );
    let curve = equity_curve_for(bars, &batch.decisions);
    for (name, decisions) in [("session", stepped), ("driver", driven), ("loop", looped)] {
        assert_eq!(
            equity_curve_for(bars, &decisions),
            curve,
            "{name} marks the same equity curve"
        );
        assert_eq!(
            fee_sum(&decisions),
            fee_sum(&batch.decisions),
            "{name} pays the same fees"
        );
    }
}

fn fill_tape() -> Vec<Bar> {
    bars_from_closes(&[
        100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115,
    ])
}

fn exit_tape() -> Vec<Bar> {
    bars_from_closes(&[
        100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 106, 101, 96, 94, 95, 96, 97, 98,
        99, 100, 101, 102, 102,
    ])
}

fn gap_tape() -> Vec<Bar> {
    bars_from_closes(&[100, 101, 102, 90, 90, 90, 91, 92])
}

fn wick_tape() -> Vec<Bar> {
    let mut bars = bars_from_closes(&[100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110]);
    bars[3].h = Decimal::from(110);
    bars
}

fn dup_ts_tape() -> Vec<Bar> {
    let mut bars = bars_from_closes(&[100, 101, 102, 103, 104, 105, 106, 107]);
    bars[6].ts = bars[5].ts;
    bars
}

fn session_close_tape() -> Vec<Bar> {
    // 19:30–20:05 UTC: bar 6 is the 20:00 UTC bar, the 16:00 ET close.
    let closes = [100, 101, 102, 103, 104, 105, 106, 107];
    let mut bars = Vec::new();
    for (index, close) in closes.iter().enumerate() {
        let price = Decimal::from(*close);
        let ts = Utc.with_ymd_and_hms(2026, 6, 1, 19, 30, 0).unwrap()
            + chrono::Duration::minutes(index as i64 * 5);
        bars.push(
            Bar::new()
                .ts(ts)
                .o(price)
                .h(price)
                .l(price)
                .c(price)
                .volume(1_000)
                .maybe_adj(None)
                .build(),
        );
    }
    bars
}

#[tokio::test]
async fn four_drivers_agree_on_a_fill_tape() {
    assert_four_way_agree(&rehearsal_strategy(), &fill_tape(), 7, "fill", 0).await;
}

#[tokio::test]
async fn four_drivers_agree_when_stops_and_targets_fire() {
    // Entries at 100/105/110/101/94; the $4 target fires on the climbs and
    // the $3 stop fires on the drops; the tail holds an open position.
    assert_four_way_agree(&exit_strategy(), &exit_tape(), 7, "exits", 3).await;
}

#[tokio::test]
async fn four_drivers_agree_across_a_gap_down_through_the_stop() {
    // The open gaps straight past the $3 stop: every driver must fire the
    // stop on the gap bar, never invent an intrabar fill.
    let bars = gap_tape();
    assert_four_way_agree(&exit_strategy(), &bars, 7, "gap", 1).await;
}

#[tokio::test]
async fn four_drivers_agree_when_a_wick_touches_target_but_close_holds() {
    // The mark is the close: a high that tags the $4 target with a close
    // below it fires nothing, on every driver.
    let bars = wick_tape();
    assert_four_way_agree(&exit_strategy(), &bars, 7, "wick", 1).await;
}

#[tokio::test]
async fn four_drivers_agree_with_duplicate_timestamps() {
    let bars = dup_ts_tape();
    assert_four_way_agree(&rehearsal_strategy(), &bars, 7, "dup-ts", 0).await;
}

#[tokio::test]
async fn four_drivers_agree_across_the_session_close() {
    // Bars straddling the 16:00 ET close: the generic path carries no
    // session gate, so every driver must treat the boundary bar like any
    // other bar — identically.
    assert_four_way_agree(
        &rehearsal_strategy(),
        &session_close_tape(),
        7,
        "session",
        0,
    )
    .await;
}

fn equity_strategy() -> IntegrationLiteStrategyConfig {
    IntegrationLiteStrategyConfig {
        sig_window: 20,
        // These tapes exercise determinism, not the entry filters: the
        // health floor sits at its legal minimum and sizing names a fixed
        // quantity, so the run is fully stated by the tape and the seed.
        filt_ma_health_min: 0.0,
        size_mode: "Fixed Quantity".to_owned(),
        fixed_qty: 10.0,
        ..IntegrationLiteStrategyConfig::default()
    }
}

fn equity_venue_config() -> BacktestConfig {
    BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("parity-soak-equity-v1".to_owned())
        .quote_size(1_000_000)
        .build()
}

fn equity_runner_config() -> RunnerConfig {
    RunnerConfig::new(
        "SPY".to_owned(),
        "integration_lite".to_owned(),
        money("400"),
        money("2_000"),
        30,
        0,
    )
}

fn equity_run(bars: &[Bar], seed: u64) -> EquityRun {
    let indicator = LiteIndicatorParams::default();
    let strategy = equity_strategy();
    SimpleBacktestRequest::builder()
        .indicator(&indicator)
        .strategy(&strategy)
        .venue_config(&equity_venue_config())
        .runner_config(&equity_runner_config())
        .signal_name("integration_lite")
        .underlying("SPY")
        .timeframe(Timeframe::M1)
        .bars(bars)
        .seed(seed)
        .initial_equity(100_000.0)
        .run()
        .expect("equity run completes")
}

/// The equity batch runner is a different decision stack with no other
/// driver, so its parity story is determinism: every harness tape reruns
/// byte-identical, including the adversarial ones — and a tape shaped like
/// the suite's proven round trip must actually trade while doing so.
#[test]
fn equity_batch_is_deterministic_on_every_harness_tape() {
    for (name, bars) in [
        ("fill", fill_tape()),
        ("exits", exit_tape()),
        ("gap", gap_tape()),
        ("wick", wick_tape()),
        ("dup-ts", dup_ts_tape()),
        ("session", session_close_tape()),
    ] {
        let first = equity_run(&bars, 7);
        let rerun = equity_run(&bars, 7);
        assert_eq!(
            rerun.to_canonical_json(),
            first.to_canonical_json(),
            "{name} reruns byte-identical"
        );
    }
    // The round trip falls, rises, then falls again: entries, partials, and
    // exits must all fire — determinism over an empty run proves nothing.
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
    let bars: Vec<Bar> = closes
        .iter()
        .enumerate()
        .map(|(i, close)| {
            let value = Decimal::from_f64_retain(*close).expect("finite close");
            Bar::new()
                .ts(Utc.with_ymd_and_hms(2026, 1, 5, 15, 0, 0).unwrap()
                    + chrono::Duration::minutes(i as i64))
                .o(value)
                .h(value + Decimal::ONE / Decimal::from(2))
                .l(value - Decimal::ONE / Decimal::from(2))
                .c(value)
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect();
    let first = equity_run(&bars, 7);
    assert!(
        !first.trades.is_empty() || !first.partials.is_empty(),
        "the round trip must trade on the equity path"
    );
    let rerun = equity_run(&bars, 7);
    assert_eq!(
        rerun.to_canonical_json(),
        first.to_canonical_json(),
        "the trading round trip reruns byte-identical"
    );
}

/// Fifty thousand deterministic bars through the batch and the stepped
/// session: the run completes, the drivers agree, and a rerun is
/// byte-identical. Timing is observed (printed), never asserted.
#[tokio::test]
#[ignore = "soak: 50k-bar hermetic four-driver parity soak"]
async fn fifty_k_bar_soak_reproduces_across_drivers() {
    let bars: Vec<Bar> = (0..50_000)
        .map(|index| {
            let price = Decimal::from(100)
                + Decimal::from(index / 500)
                + Decimal::from(index % 7) * Decimal::new(3, 1);
            Bar::new()
                .ts(Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
                    + chrono::Duration::minutes(i64::from(index)))
                .o(price)
                .h(price + Decimal::ONE)
                .l(price - Decimal::ONE)
                .c(price)
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect();
    let strategy = exit_strategy();
    let started = std::time::Instant::now();
    let first = batch_run(&strategy, &bars, 7).await;
    let batch_ms = started.elapsed().as_millis();
    let rerun = batch_run(&strategy, &bars, 7).await;
    assert_eq!(
        rerun.to_canonical_json(),
        first.to_canonical_json(),
        "a 50k-bar rerun is byte-identical"
    );
    let stepped = session_run(&strategy, &bars, 7).await;
    assert_eq!(
        stepped, first.decisions,
        "the session steps the 50k-bar batch"
    );
    let driven = driver_run(&strategy, &bars, 7).await;
    assert_eq!(
        driven, first.decisions,
        "the driver steps the 50k-bar batch"
    );
    println!(
        "50k-bar soak: batch {batch_ms} ms, {} decisions, {} exits, {} fills",
        first.decisions.len(),
        exit_count(&first.decisions),
        first
            .decisions
            .iter()
            .filter(|decision| decision.filled_quantity > 0)
            .count(),
    );
}
