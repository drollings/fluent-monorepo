//! Composition pins for sizing and protective exits.
//!
//! These tests freeze the input/output contract of the sizing policies and
//! loss rules that later composition work (unified resolution, advisor
//! exits) must preserve: same signal and facts in, same quantity out; same
//! book and mark in, same actions out.

use super::*;
use crate::risk::PortfolioFacts;

fn money(value: i64) -> protopyne_types::Money {
    protopyne_types::Money::new(rust_decimal::Decimal::from(value))
}

fn signal_with_volatility(volatility: Option<f64>) -> SignalOutput {
    SignalOutput {
        direction: Direction::Long,
        conviction: 0.8,
        activity_score: 0.5,
        volatility,
        edge: None,
        odds: None,
    }
}

fn unobserved_facts() -> PortfolioFacts {
    PortfolioFacts::unobserved()
}

fn book() -> PositionView<'static> {
    PositionView {
        underlying: "SPY".to_owned(),
        side: Direction::Long,
        quantity: 2,
        entry_avg: money(250),
        max_loss: money(600),
        dte_days: None,
        legs: None,
        events: None,
    }
}

fn marked(price: i64) -> MarkContext {
    MarkContext {
        basis: protopyne_types::MarkBasis::ChainMid,
        mark: Some(money(price)),
    }
}

fn absent_mark() -> MarkContext {
    MarkContext {
        basis: protopyne_types::MarkBasis::Absent,
        mark: None,
    }
}

#[test]
fn volatility_scaled_sizes_base_at_target_vol_and_zero_without_a_reading() {
    let policy = VolatilityScaled::new(10, 0.000_411, 0.000_205).expect("valid policy");
    let facts = unobserved_facts();
    let at_target = policy.size(&signal_with_volatility(Some(0.000_411)), &facts);
    assert_eq!(at_target.as_decimal(), rust_decimal::Decimal::from(10));
    let calm = policy.size(&signal_with_volatility(Some(0.000_001)), &facts);
    assert_eq!(
        calm.as_decimal(),
        rust_decimal::Decimal::from(10),
        "reduce-only: the floor stops growth at base"
    );
    let stormy = policy.size(&signal_with_volatility(Some(0.004_11)), &facts);
    assert_eq!(stormy.as_decimal(), rust_decimal::Decimal::from(1));
    let unmeasured = policy.size(&signal_with_volatility(None), &facts);
    assert_eq!(unmeasured, Qty::zero(), "absence is not a zero reading");
}

#[test]
fn stop_loss_fires_past_the_threshold_and_holds_on_it() {
    let rule = StopLoss::new(money(200)).expect("valid stop");
    assert_eq!(rule.name(), "stop");
    let position = book();
    let past = rule.review(&position, &marked(100));
    assert_eq!(past.len(), 1, "unrealised -300 past a 200 stop must fire");
    assert_eq!(past[0].kind, "stop");
    let on = rule.review(&position, &marked(150));
    assert!(
        on.is_empty(),
        "exactly on the threshold holds: the boundary is not a trigger"
    );
    let above = rule.review(&position, &marked(300));
    assert!(above.is_empty(), "a winner never trips a stop");
    let absent = rule.review(&position, &absent_mark());
    assert!(
        absent.is_empty(),
        "an absent mark acts on nothing: absence is not a trigger"
    );
}
