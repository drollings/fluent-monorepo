//! Gate-order pins for the shared submission path: `submit_decided` must
//! evaluate the same gates in the same order as the pipeline, recording the
//! full reason split on every verdict. Each case drives one refusal and
//! asserts the axis that fired plus the recorded split — a second
//! implementation of the order would drift from these rows.

use super::*;

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use protopyne_strategy::{
    BreakerLimits, Direction, Grant, GrantBudgets, GrantLedger, GrantPermission, GrantScope,
    PortfolioThresholds, RiskEngine, RiskLimits, SignalThresholds, StructureKind, TradeThresholds,
};
use protopyne_types::{Money, Qty};
use protopyne_venue::{Journal, PaperConfig, PaperVenue};

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
}

fn proposal(quantity: &str) -> Proposal {
    Proposal::new()
        .id("gate-prop".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(Direction::Long)
        .side(ProposalSide::Entry)
        .structure(StructureKind::Vertical)
        .quantity(Qty::new(quantity.parse::<Decimal>().expect("decimal")))
        .limit_price(money("600.50"))
        .max_loss(money("300"))
        .notional(money("1000"))
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.7)
        .created_at(now())
        .build()
}

fn grant(min_conviction: f64, max_errors: u32) -> Grant {
    Grant::new()
        .id("gate-grant".to_owned())
        .scope(
            GrantScope::new()
                .strategies(vec!["ribbon".to_owned()])
                .underlyings(vec!["SPY".to_owned()])
                .structures(vec![StructureKind::Vertical])
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
                .max_loss(money("1000000"))
                .max_notional(money("100000000"))
                .build(),
        )
        .signal(
            SignalThresholds::new()
                .min_conviction(min_conviction)
                .min_activity_score(0.5)
                .build(),
        )
        .trade(
            TradeThresholds::new()
                .max_loss_per_trade(money("1000"))
                .max_spread_width(money("50"))
                .min_dte_days(1)
                .max_dte_days(90)
                .build(),
        )
        .portfolio(
            PortfolioThresholds::new()
                .max_open_risk(money("10000000"))
                .quote_max_age_secs(60)
                .build(),
        )
        .breakers(
            BreakerLimits::new()
                .max_consecutive_errors(max_errors)
                .build(),
        )
        .valid_from(Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap())
        .valid_until(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap())
        .build()
}

fn config() -> RunnerConfig {
    RunnerConfig::new(
        "SPY".to_owned(),
        "ribbon".to_owned(),
        money("400"),
        money("2000"),
        30,
        0,
    )
}

fn bar() -> Bar {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    let bars = crate::csv::load_bars_csv(&path).expect("vendored history loads");
    bars.into_iter().nth(3).expect("fixture has a fourth bar")
}

fn quote_for(bar: &Bar) -> protopyne_venue::Quote {
    let ask = Money::new(bar.c + Decimal::from(1));
    protopyne_venue::Quote::new()
        .bid(Money::new(bar.c))
        .ask(ask)
        .bid_size(50)
        .ask_size(50)
        .ts(bar.ts)
        .build()
}

/// Drive one proposal through the shared submission path and return its
/// verdict. The venue is paper, the recorder ephemeral, the chain absent:
/// only the gates under test can refuse.
async fn decide(
    proposal: Proposal,
    grant: Grant,
    kill_switch: bool,
    trip_breaker: bool,
) -> BarDecision {
    let bar = bar();
    let quote = quote_for(&bar);
    let risk = RiskEngine::new(RiskLimits::new().kill_switch(kill_switch).build());
    let ledger = GrantLedger::new(grant);
    if trip_breaker {
        for _ in 0..=1 {
            ledger.observe_error(true);
        }
    }
    let venue = PaperVenue::new(PaperConfig::new().build());
    let journal = Journal::ephemeral();
    let recorder = protopyne_venue::Recorder::ephemeral(&journal);
    submit_decided(
        proposal,
        quote,
        &risk,
        &ledger,
        &venue,
        &recorder,
        BarHealth::rehearsed(),
        None,
        3,
        &bar,
        &config(),
    )
    .await
}

#[tokio::test]
async fn clean_proposal_submits_with_every_verdict_set() {
    let decision = decide(proposal("2"), grant(0.5, 3), false, false).await;
    assert!(decision.risk_pass, "{:?}", decision.risk_reasons);
    assert!(decision.grant_merit);
    assert!(decision.grant_authorized);
    assert!(decision.submitted);
    assert!(decision.skipped.is_none());
}

#[tokio::test]
async fn risk_refusal_records_merit_and_authorization() {
    let decision = decide(proposal("2"), grant(0.5, 3), true, false).await;
    assert!(!decision.risk_pass);
    assert!(!decision.risk_reasons.is_empty());
    // Merit is evaluated without breaker input even alongside the refusal,
    // and the reservation attempt still runs — the split is recorded, not
    // defaulted.
    assert!(decision.grant_merit);
    assert!(decision.grant_authorized);
    assert!(!decision.submitted);
    assert!(decision.skipped.is_none());
}

#[tokio::test]
async fn merit_denial_stops_before_authorization() {
    let decision = decide(proposal("2"), grant(0.95, 3), false, false).await;
    assert!(decision.risk_pass);
    assert!(!decision.grant_merit);
    assert!(!decision.grant_merit_reasons.is_empty());
    assert!(!decision.grant_authorized);
    assert!(!decision.submitted);
}

#[tokio::test]
async fn breaker_trip_disarms_entries_without_rewriting_merit() {
    let decision = decide(proposal("2"), grant(0.5, 1), false, true).await;
    assert!(decision.risk_pass);
    assert!(decision.grant_merit, "a producer trip never rewrites merit");
    assert!(!decision.grant_authorized);
    assert!(
        decision
            .grant_authorization_reasons
            .iter()
            .any(|reason| reason.contains("breaker")),
        "{:?}",
        decision.grant_authorization_reasons
    );
    assert!(decision
        .skipped
        .as_deref()
        .is_some_and(|reason| reason.contains("grant denied submission")));
    assert!(!decision.submitted);
}

#[tokio::test]
async fn fractional_quantity_refuses_before_any_submission() {
    // A fractional quantity is unrepresentable twice over: the pipeline
    // records the correctness refusal and the engine refuses it as
    // unrepresentable, so the run stops at the gates with nothing submitted.
    let decision = decide(proposal("1.5"), grant(0.5, 3), false, false).await;
    assert!(!decision.risk_pass);
    assert!(
        decision
            .risk_reasons
            .iter()
            .any(|reason| reason.contains("Unrepresentable")),
        "{:?}",
        decision.risk_reasons
    );
    assert!(!decision.submitted);
    assert!(decision.skipped.is_none());
}
