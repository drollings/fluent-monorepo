use super::*;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use protopyne_strategy::{
    elements::ExplicitStructure, Direction, FixedSignal, Grant, GrantBudgets, GrantPermission,
    GrantScope, PortfolioThresholds, RiskEngine, RiskLimits, SignalThresholds, Strategy,
    StructureKind, TradeThresholds,
};
use protopyne_types::{Bar, Money};
use protopyne_venue::{
    CorporateEvent, CorporateEventArgs, OrderIntent, OrderSide, PaperConfig, PaperVenue, Venue,
};

use crate::backtest::{BacktestConfig, BacktestVenue};
use crate::replay_session::ChainPolicy;
use crate::runner::{run_backtest, RunnerConfig};

fn money(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

fn synthetic_bars(count: usize) -> Vec<Bar> {
    (0..count)
        .map(|index| {
            let price = Decimal::from(100 + index as i64);
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

fn strategy_with(structure: StructureKind) -> Strategy {
    Strategy::with_signal(Arc::new(
        FixedSignal::new(Direction::Long, 0.8, 0.8).expect("valid signal"),
    ))
    .with_structure(Arc::new(ExplicitStructure { structure }))
}

fn risk() -> RiskEngine {
    RiskEngine::new(RiskLimits::new().build())
}

fn grant_for(structures: Vec<StructureKind>) -> Grant {
    Grant::new()
        .id("replay-test".to_owned())
        .scope(
            GrantScope::new()
                .strategies(vec!["ribbon".to_owned()])
                .underlyings(vec!["SPY".to_owned()])
                .structures(structures)
                .directions(vec![Direction::Long, Direction::Short])
                .permissions(GrantPermission {
                    entries: true,
                    exits: true,
                    rolls: false,
                })
                .build(),
        )
        .budgets(
            GrantBudgets::new()
                .max_trades(1_000)
                .max_contracts(10_000)
                .max_loss(money(1_000_000))
                .max_notional(money(100_000_000))
                .build(),
        )
        .signal(
            SignalThresholds::new()
                .min_conviction(0.0)
                .min_activity_score(0.5)
                .build(),
        )
        .trade(
            TradeThresholds::new()
                .max_loss_per_trade(money(1_000))
                .max_spread_width(money(50))
                .min_dte_days(1)
                .max_dte_days(90)
                .build(),
        )
        .portfolio(
            PortfolioThresholds::new()
                .max_open_risk(money(10_000_000))
                .quote_max_age_secs(60)
                .build(),
        )
        .breakers(
            protopyne_strategy::BreakerLimits::new()
                .max_consecutive_errors(3)
                .build(),
        )
        .valid_from(Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap())
        .valid_until(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap())
        .build()
}

fn venue_config() -> BacktestConfig {
    BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("replay-test-v1".to_owned())
        .build()
}

fn runner_config() -> RunnerConfig {
    RunnerConfig::new(
        "SPY".to_owned(),
        "ribbon".to_owned(),
        money(400),
        money(2_000),
        30,
        0,
    )
}

/// Stepping a session to completion reproduces the batch run
/// decision-for-decision: one loop body, owned by the session.
#[tokio::test]
async fn stepping_to_completion_reproduces_the_batch_run() {
    let bars = synthetic_bars(12);
    let strategy = strategy_with(StructureKind::Single);
    let risk = risk();
    let grant = grant_for(vec![StructureKind::Single]);
    let batch = run_backtest(
        &strategy,
        &risk,
        &grant,
        &venue_config(),
        &runner_config(),
        &bars,
        7,
    )
    .await
    .expect("batch runs");
    let venue = BacktestVenue::new(venue_config(), bars.clone(), PaperConfig::new().build())
        .expect("venue pins");
    let mut session = ReplaySession::start(
        &strategy,
        &risk,
        &grant,
        &venue,
        venue_config(),
        runner_config(),
        ChainPolicy::RequireChains,
        None,
        &bars,
        7,
    )
    .expect("session starts");
    let mut stepped = Vec::new();
    while let Some(decision) = session.step().await {
        stepped.push(decision);
    }
    assert_eq!(stepped, batch.decisions, "stepped and batch verdicts agree");
    assert_eq!(
        session.cursor(),
        bars.len(),
        "the cursor stands past the last bar"
    );
    assert_eq!(session.total_bars(), bars.len());
    assert!(
        session.step().await.is_none(),
        "stepping past the end holds"
    );
}

/// A corporate-action tape lands on the bar whose timestamp first reaches the
/// event, never earlier: the book a proposal sees is the post-action book.
#[tokio::test]
async fn a_corporate_action_tape_applies_at_its_effective_bar() {
    let bars = synthetic_bars(6);
    let tape = vec![CorporateEventArgs::new()
        .underlying("SPY".to_owned())
        .at(bars[2].ts)
        .event(CorporateEvent::Dividend)
        .amount(money(10))
        .build()];
    let config = BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("replay-test-v1".to_owned())
        .corporate_actions(tape)
        .build();
    let venue = BacktestVenue::new(config.clone(), bars.clone(), PaperConfig::new().build())
        .expect("venue pins");
    // Seed a stock position so the dividend has shares to credit.
    let seed_quote = crate::backtest::quote_for_bar(&bars[0], &config).expect("quotable");
    let seed = OrderIntent::new()
        .client_id("seed#0".to_owned())
        .proposal_id("seed".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(2)
        .limit_price(seed_quote.ask)
        .build();
    venue
        .submit(seed, &seed_quote, bars[0].ts)
        .await
        .expect("seed fills");
    let strategy = strategy_with(StructureKind::Single);
    let risk = risk();
    let grant = grant_for(vec![StructureKind::Single]);
    let mut session = ReplaySession::start(
        &strategy,
        &risk,
        &grant,
        &venue,
        config,
        runner_config(),
        ChainPolicy::RequireChains,
        None,
        &bars,
        7,
    )
    .expect("session starts");
    session.step().await.expect("bar 0");
    session.step().await.expect("bar 1");
    assert!(
        venue.booked_dividends().is_empty(),
        "the tape event is not due before its effective bar"
    );
    let shares = venue
        .positions()
        .iter()
        .find(|position| position.underlying == "SPY")
        .map_or(0, |position| position.quantity);
    assert!(shares > 0, "the seeded book holds shares");
    session.step().await.expect("bar 2");
    assert_eq!(
        venue.booked_dividends().get("SPY"),
        Some(&money(10 * shares)),
        "10 per share on the held shares"
    );
}

/// Two sessions over the same bars agree exactly.
#[tokio::test]
async fn two_sessions_over_the_same_bars_agree() {
    let bars = synthetic_bars(10);
    let venue = BacktestVenue::new(venue_config(), bars.clone(), PaperConfig::new().build())
        .expect("venue pins");
    let strategy_first = strategy_with(StructureKind::Single);
    let risk_first = risk();
    let grant_first = grant_for(vec![StructureKind::Single]);
    let strategy_second = strategy_with(StructureKind::Single);
    let risk_second = risk();
    let grant_second = grant_for(vec![StructureKind::Single]);
    let mut first = ReplaySession::start(
        &strategy_first,
        &risk_first,
        &grant_first,
        &venue,
        venue_config(),
        runner_config(),
        ChainPolicy::RequireChains,
        None,
        &bars,
        3,
    )
    .expect("first session starts");
    let mut second = ReplaySession::start(
        &strategy_second,
        &risk_second,
        &grant_second,
        &venue,
        venue_config(),
        runner_config(),
        ChainPolicy::RequireChains,
        None,
        &bars,
        3,
    )
    .expect("second session starts");
    let mut first_run = Vec::new();
    let mut second_run = Vec::new();
    while let Some(decision) = first.step().await {
        first_run.push(decision);
    }
    while let Some(decision) = second.step().await {
        second_run.push(decision);
    }
    assert_eq!(first_run, second_run, "replay is deterministic");
}

/// Spreads refuse without a chain feed: every proposal-bearing decision goes
/// unsubmitted and names the missing feed on its skip reason.
#[tokio::test]
async fn spreads_refuse_without_a_chain_feed() {
    let bars = synthetic_bars(8);
    let venue = BacktestVenue::new(venue_config(), bars.clone(), PaperConfig::new().build())
        .expect("venue pins");
    let strategy = strategy_with(StructureKind::Vertical);
    let risk = risk();
    let grant = grant_for(vec![StructureKind::Vertical]);
    let mut session = ReplaySession::start(
        &strategy,
        &risk,
        &grant,
        &venue,
        venue_config(),
        runner_config(),
        ChainPolicy::RequireChains,
        None,
        &bars,
        11,
    )
    .expect("session starts");
    let mut proposed = 0;
    while let Some(decision) = session.step().await {
        if decision.direction.is_none() && decision.skipped.as_deref() != Some(CHAIN_GATE_NOTE) {
            continue;
        }
        proposed += 1;
        // Screened before the shared path: no attribution, no lifecycle, no
        // submission — just the named refusal.
        assert_eq!(decision.direction, None);
        assert_eq!(decision.structure, None);
        assert!(
            !decision.submitted,
            "no spread reaches the venue without leg quotes"
        );
        assert_eq!(
            decision.skipped.as_deref(),
            Some(CHAIN_GATE_NOTE),
            "the skip names the missing feed"
        );
    }
    assert!(proposed > 0, "the run proposed spreads to refuse");
}

/// Single-stock legs fill honestly from bar-derived quotes.
#[tokio::test]
async fn single_stock_legs_fill_honestly() {
    let bars = synthetic_bars(8);
    let venue = BacktestVenue::new(venue_config(), bars.clone(), PaperConfig::new().build())
        .expect("venue pins");
    let strategy = strategy_with(StructureKind::Single);
    let risk = risk();
    let grant = grant_for(vec![StructureKind::Single]);
    let mut session = ReplaySession::start(
        &strategy,
        &risk,
        &grant,
        &venue,
        venue_config(),
        runner_config(),
        ChainPolicy::RequireChains,
        None,
        &bars,
        11,
    )
    .expect("session starts");
    let mut filled = 0;
    while let Some(decision) = session.step().await {
        if decision.filled_quantity > 0 {
            filled += 1;
            assert!(decision.fill_price.is_some(), "fills carry a price");
            let skipped = decision.skipped.as_deref().unwrap_or_default();
            assert!(
                !skipped.contains("option-chain feed"),
                "honest fills carry no chain note: {skipped:?}"
            );
        }
    }
    assert!(filled > 0, "stock singles fill from bar-derived quotes");
}

/// Risk and grant denials are merit verdicts, never chain notes.
#[tokio::test]
async fn gate_denials_carry_no_chain_note() {
    let bars = synthetic_bars(6);
    let venue = BacktestVenue::new(venue_config(), bars.clone(), PaperConfig::new().build())
        .expect("venue pins");
    // A grant that covers nothing the strategy emits: every proposal denies
    // on merit, and the denial must not blame missing data.
    let strategy = strategy_with(StructureKind::Single);
    let risk = risk();
    let grant = grant_for(vec![StructureKind::Vertical]);
    let mut session = ReplaySession::start(
        &strategy,
        &risk,
        &grant,
        &venue,
        venue_config(),
        runner_config(),
        ChainPolicy::RequireChains,
        None,
        &bars,
        11,
    )
    .expect("session starts");
    let mut denied = 0;
    while let Some(decision) = session.step().await {
        if decision.direction.is_none() || decision.grant_merit {
            continue;
        }
        denied += 1;
        assert!(!decision.submitted);
        assert!(
            decision.skipped.is_none(),
            "merit denials carry no skip reason"
        );
    }
    assert!(denied > 0, "the narrow grant denied proposals on merit");
}

/// The legacy policy preserves batch behavior exactly: an unscreened session
/// books multi-leg-labeled proposals as single legs, decision-for-decision
/// with the batch run. This pins the divergence contract — not an
/// endorsement — until a chain feed lets both paths price legs.
#[tokio::test]
async fn unscreened_session_matches_batch_on_spreads() {
    let bars = synthetic_bars(8);
    let strategy = strategy_with(StructureKind::Vertical);
    let risk = risk();
    let grant = grant_for(vec![StructureKind::Vertical]);
    let batch = run_backtest(
        &strategy,
        &risk,
        &grant,
        &venue_config(),
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("batch runs");
    let venue = BacktestVenue::new(venue_config(), bars.clone(), PaperConfig::new().build())
        .expect("venue pins");
    let mut session = ReplaySession::start(
        &strategy,
        &risk,
        &grant,
        &venue,
        venue_config(),
        runner_config(),
        ChainPolicy::Unscreened,
        None,
        &bars,
        11,
    )
    .expect("session starts");
    let mut stepped = Vec::new();
    while let Some(decision) = session.step().await {
        stepped.push(decision);
    }
    assert_eq!(
        stepped, batch.decisions,
        "unscreened stepping matches batch"
    );
    assert!(
        stepped.iter().any(|decision| decision.submitted),
        "legacy books spread-labeled proposals as single legs"
    );
}

/// Construction fails closed: empty history and unnamed configs refuse.
#[test]
fn start_refuses_empty_history_and_bad_configs() {
    let bars = synthetic_bars(4);
    let venue = BacktestVenue::new(venue_config(), bars.clone(), PaperConfig::new().build())
        .expect("venue pins");
    let empty: Vec<Bar> = Vec::new();
    let strategy = strategy_with(StructureKind::Single);
    let risk = risk();
    let grant = grant_for(vec![StructureKind::Single]);
    assert!(
        ReplaySession::start(
            &strategy,
            &risk,
            &grant,
            &venue,
            venue_config(),
            runner_config(),
            ChainPolicy::RequireChains,
            None,
            &empty,
            1,
        )
        .is_err(),
        "empty history refuses"
    );
    let paper = PaperVenue::new(PaperConfig::new().build());
    assert!(
        ReplaySession::start(
            &strategy,
            &risk,
            &grant,
            &paper,
            venue_config(),
            RunnerConfig::new(
                String::new(),
                "ribbon".to_owned(),
                money(400),
                money(2_000),
                30,
                0,
            ),
            ChainPolicy::RequireChains,
            None,
            &bars,
            1,
        )
        .is_err(),
        "an unnamed underlying refuses"
    );
}
