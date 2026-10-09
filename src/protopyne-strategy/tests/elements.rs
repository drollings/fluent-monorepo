use super::*;
use crate::proposal::{ProposalSide, ProposalState, StructureKind};
use chrono::Utc;
use protopyne_types::Money;
use rust_decimal::Decimal;
use std::sync::Arc;

fn money(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

fn signal() -> Arc<dyn Signal> {
    Arc::new(
        FixedSignal::new(Direction::Long, 0.8, 0.7).expect("valid scores"),
    )
}

fn input(closes: &[f64]) -> ProposeInput<'_> {
    ProposeInput {
        strategy: "ribbon",
        underlying: "SPY",
        side: ProposalSide::Entry,
        closes,
        limit_price: money(250),
        max_loss: money(300),
        notional: money(1_000),
        dte_days: 30,
        created_at: Utc::now(),
    }
}

#[test]
fn fixed_signal_rejects_bad_scores_and_empty_input() {
    assert!(FixedSignal::new(Direction::Long, 1.5, 0.5).is_err());
    assert!(FixedSignal::new(Direction::Long, 0.5, f64::NAN).is_err());
    let s = signal();
    assert!(s.evaluate(&[]).is_err());
    let out = s.evaluate(&[100.0, 101.0]).expect("scores");
    assert_eq!(out.direction, Direction::Long);
}

#[test]
fn strategy_proposes_with_defaults() {
    let strategy = Strategy::with_signal(signal());
    assert_eq!(strategy.signal_name(), "fixed");
    let closes = [100.0, 101.0, 102.0];
    let p = strategy.propose("p1", input(&closes)).expect("proposal");
    assert_eq!(p.state, ProposalState::Proposed);
    assert_eq!(p.structure, StructureKind::Vertical);
    assert_eq!(p.contracts(), Some(1));
    assert_eq!(p.underlying, "SPY");
}

#[test]
fn optional_elements_swap_independently() {
    let strategy = Strategy::with_signal(signal())
        .with_structure(Arc::new(ExplicitStructure {
            structure: StructureKind::Condor,
        }))
        .with_sizing(Arc::new(FixedContracts { contracts: 4 }));
    let closes = [100.0, 101.0];
    let p = strategy.propose("p2", input(&closes)).expect("proposal");
    assert_eq!(p.structure, StructureKind::Condor);
    assert_eq!(p.contracts(), Some(4));
}

#[test]
fn loss_and_monitor_default_to_quiet() {
    let strategy = Strategy::with_signal(signal());
    assert!(strategy.review_position(money(-1_000), money(-500)).is_none());
    assert!(
        strategy
            .observe(money(100), money(10_000))
            .is_empty()
    );
}

#[test]
fn limit_monitor_alerts_only_above_cap() {
    let monitor = LimitMonitor {
        portfolio: "tasty".to_owned(),
    };
    assert!(monitor.observe(money(100), money(10_000)).is_empty());
    let alerts = monitor.observe(money(20_000), money(10_000));
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].portfolio, "tasty");
}

fn negative_money() -> Money {
    Money::new(Decimal::from(-1))
}

#[test]
fn propose_refuses_negative_economics() {
    // Negative economics would sail through caps and poison budget
    // accounting downward: each field refuses with a reason, never clamps.
    let strategy = Strategy::with_signal(signal());
    let closes = [100.0, 101.0];
    let mut bad_limit = input(&closes);
    bad_limit.limit_price = negative_money();
    let err = strategy.propose("p", bad_limit).expect_err("negative limit");
    assert!(matches!(err, StrategyError::InvalidElement(_)));
    assert!(err.to_string().contains("limit"), "refusal names the field, got: {err}");

    let mut bad_loss = input(&closes);
    bad_loss.max_loss = negative_money();
    let err = strategy.propose("p", bad_loss).expect_err("negative max loss");
    assert!(matches!(err, StrategyError::InvalidElement(_)));
    assert!(err.to_string().contains("max loss"), "refusal names the field, got: {err}");

    let mut bad_notional = input(&closes);
    bad_notional.notional = negative_money();
    let err = strategy.propose("p", bad_notional).expect_err("negative notional");
    assert!(matches!(err, StrategyError::InvalidElement(_)));
    assert!(err.to_string().contains("notional"), "refusal names the field, got: {err}");

    assert!(strategy.propose("p", input(&closes)).is_ok(), "non-negative economics still propose");
}
