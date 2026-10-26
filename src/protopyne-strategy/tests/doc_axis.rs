//! Every gate, trigger, and threshold states which axis it measures.
//!
//! Three axes exist and they are not interchangeable: **correctness** (a hard
//! bound that may only be tightened), **task-value** (merit of the outcome),
//! and **producer-confidence** (the producer's doubt about its own reading).
//! A schema-bearing field that gates behaviour without naming its axis is an
//! ambiguity, so this audit fails until it is resolved — either by tagging the
//! field or by listing it as presentation-only, which is a decision recorded
//! here and visible in review.
//!
//! Presentation fields carry no axis on purpose: they change what the operator
//! sees, never what the platform does.

use std::collections::BTreeMap;

use fluent_wvr::Describable;

use crate::axes::GateAxis;
use crate::grant::{
    BreakerLimits, GrantBudgets, PortfolioThresholds, SignalThresholds, TradeThresholds,
};
use crate::integration_lite::IntegrationLiteStrategyConfig;
use crate::risk::RiskLimits;
use protopyne_types::Money;

/// The grant tiers have no constructor defaults (every tier member is
/// required), so each is built explicitly here; only the axes are under test.
fn signal_thresholds() -> SignalThresholds {
    SignalThresholds::new()
        .min_conviction(0.0)
        .min_activity_score(0.0)
        .build()
}

fn trade_thresholds() -> TradeThresholds {
    TradeThresholds::new()
        .max_loss_per_trade(Money::from(1))
        .max_spread_width(Money::from(50))
        .min_dte_days(1)
        .max_dte_days(90)
        .build()
}

fn portfolio_thresholds() -> PortfolioThresholds {
    PortfolioThresholds::new()
        .max_open_risk(Money::from(1))
        .quote_max_age_secs(60)
        .build()
}

fn grant_budgets() -> GrantBudgets {
    GrantBudgets::new()
        .max_trades(1)
        .max_contracts(1)
        .max_loss(Money::from(1))
        .max_notional(Money::from(1))
        .build()
}

fn breaker_limits() -> BreakerLimits {
    BreakerLimits::new().max_consecutive_errors(3).build()
}

/// Fields that change the display and nothing else. Anything added here is a
/// claim that the field cannot gate a decision; it is reviewed like code.
const PRESENTATION: &[&str] = &[
    "show_markers",
    "marker_long_color",
    "marker_short_color",
    "show_table",
    "table_position",
    "send_alerts",
];

/// The axis of every schema property, in schema order. `None` means the field
/// declares no axis.
fn axes_of(schema: &serde_json::Value) -> BTreeMap<String, Option<String>> {
    let properties = schema["properties"]
        .as_object()
        .expect("a component schema has properties");
    properties
        .iter()
        .map(|(name, hint)| {
            let axis = hint
                .get("x-axis")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            (name.clone(), axis)
        })
        .collect()
}

fn assert_every_gate_names_its_axis(schema: &serde_json::Value, presentation: &[&str]) -> usize {
    let axes = axes_of(schema);
    let mut tagged = 0;
    for (name, axis) in &axes {
        match axis {
            Some(word) => {
                assert!(
                    GateAxis::parse(word).is_some(),
                    "{name}: `{word}` is not a listed axis"
                );
                tagged += 1;
            }
            None => assert!(
                presentation.contains(&name.as_str()),
                "{name} carries no axis and is not declared presentation-only"
            ),
        }
    }
    tagged
}

#[test]
fn strategy_dials_state_their_axis() {
    let schema = IntegrationLiteStrategyConfig::default().describe();
    let tagged = assert_every_gate_names_its_axis(&schema, PRESENTATION);
    assert_eq!(tagged, 81, "every non-presentation strategy dial is tagged");
}

#[test]
fn risk_limits_state_their_axis() {
    let schema = RiskLimits::new().build().describe();
    let tagged = assert_every_gate_names_its_axis(&schema, &[]);
    assert_eq!(tagged, 10, "all ten risk bounds are tagged");
}

#[test]
fn grant_tiers_state_their_axis() {
    for (schema, expected) in [
        (signal_thresholds().describe(), 2),
        (trade_thresholds().describe(), 4),
        (portfolio_thresholds().describe(), 2),
        (grant_budgets().describe(), 4),
        (breaker_limits().describe(), 2),
    ] {
        let tagged = assert_every_gate_names_its_axis(&schema, &[]);
        assert_eq!(tagged, expected, "each tier member is tagged");
    }
}

/// The correctness axis is the one configuration may never widen, so it must
/// be identifiable from the schema alone: a form that knows a bound is
/// correctness can refuse to store a looser value even before the core does.
#[test]
fn correctness_bounds_are_identifiable_from_the_schema() {
    let risk = axes_of(&RiskLimits::new().build().describe());
    for field in [
        "max_contracts_per_trade",
        "max_loss_per_trade",
        "max_notional",
        "quote_max_age_secs",
        "clock_skew_secs",
        "limit_only_spreads",
        "kill_switch",
    ] {
        assert_eq!(
            risk.get(field).and_then(Option::as_deref),
            Some("correctness"),
            "{field} is a hard bound"
        );
    }
    assert_eq!(
        risk.get("max_open_risk").and_then(Option::as_deref),
        Some("task-value"),
        "an aggregate exposure cap is a judgement about the outcome"
    );

    let portfolio = axes_of(&portfolio_thresholds().describe());
    assert_eq!(
        portfolio
            .get("quote_max_age_secs")
            .and_then(Option::as_deref),
        Some("correctness"),
        "stale data is a validity bound, never a merit judgement"
    );

    let strategy = axes_of(&IntegrationLiteStrategyConfig::default().describe());
    assert_eq!(
        strategy
            .get("max_daily_loss_pct")
            .and_then(Option::as_deref),
        Some("correctness"),
        "the daily loss cap is a risk bound"
    );
    assert_eq!(
        strategy.get("cd_bars").and_then(Option::as_deref),
        Some("task-value"),
        "a cooldown is an operator cadence policy, not a safety bound"
    );
}

/// A producer-confidence reading may silence a producer and must never judge a
/// trade. The breaker limits are the only producer-confidence tier in the
/// grant, so they are the one place a form must not offer a "loosen to trade"
/// affordance.
#[test]
fn producer_confidence_gates_are_isolated_to_the_breaker_tier() {
    let breaker = axes_of(&breaker_limits().describe());
    assert!(
        breaker
            .values()
            .all(|axis| axis.as_deref() == Some("producer-confidence")),
        "the breaker tier measures the producer, nothing else"
    );
    for schema in [
        signal_thresholds().describe(),
        trade_thresholds().describe(),
        portfolio_thresholds().describe(),
        grant_budgets().describe(),
    ] {
        let axes = axes_of(&schema);
        assert!(
            axes.values()
                .all(|axis| axis.as_deref() != Some("producer-confidence")),
            "only the breaker tier carries producer confidence"
        );
    }
}
