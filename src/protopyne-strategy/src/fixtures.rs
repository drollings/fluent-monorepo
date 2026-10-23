//! One grant fixture for every test target that arms one.
//!
//! Grant-builder boilerplate was duplicated between the strategy's own suite and
//! the server's test targets, which is a drift risk around a *security-relevant*
//! object: a tightening rule exercised on one copy and not the other reads as
//! coverage while pinning nothing. This module is the one baseline — a live,
//! permissive, defined-risk grant over `ribbon`/`SPY` — and a tweak closure for
//! the fields a test genuinely changes.
//!
//! Available to the crate's own tests (`cfg(test)`) and to downstream test
//! targets through the `test-support` feature. It is not a runtime surface: no
//! shipping code arms a grant from a fixture.
//!
//! [`deny_order`] is the other half: the fixed sequence in which
//! [`evaluate_grant`](crate::grant::evaluate_grant) walks its tiers. A new deny
//! stage must be added there (and in the pin), so the order is one documented
//! fact rather than an accident of where a `push` was typed.

use chrono::{TimeZone, Utc};
use protopyne_types::Money;

use crate::grant::{
    BreakerLimits, Grant, GrantBudgets, GrantPermission, GrantScope, PortfolioThresholds,
    SignalThresholds, TradeThresholds,
};
use crate::proposal::{Direction, StructureKind};

/// The baseline grant, with `tweak` applied after construction.
///
/// The baseline is the permissive *rehearsal* shape the server's targets share:
/// wide budgets, both directions, every entry/exit permission, thresholds at
/// zero, a 2025–2027 window. A test that needs a narrower object names only what
/// differs.
#[must_use]
pub fn grant_with(tweak: impl FnOnce(&mut Grant)) -> Grant {
    let mut grant = Grant::new()
        .id("fixture-grant".to_owned())
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
                .max_loss(Money::from(1_000_000))
                .max_notional(Money::from(100_000_000))
                .build(),
        )
        .signal(
            SignalThresholds::new()
                .min_conviction(0.0)
                .min_activity_score(0.0)
                .build(),
        )
        .trade(
            TradeThresholds::new()
                .max_loss_per_trade(Money::from(1_000))
                .max_spread_width(Money::from(50))
                .min_dte_days(1)
                .max_dte_days(90)
                .build(),
        )
        .portfolio(
            PortfolioThresholds::new()
                .max_open_risk(Money::from(10_000_000))
                .quote_max_age_secs(60)
                .build(),
        )
        .breakers(BreakerLimits::new().max_consecutive_errors(3).build())
        .valid_from(
            Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
                .single()
                .expect("valid window"),
        )
        .valid_until(
            Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0)
                .single()
                .expect("valid window"),
        )
        .build();
    tweak(&mut grant);
    grant
}

/// The baseline grant unchanged.
#[must_use]
pub fn grant() -> Grant {
    grant_with(|_| {})
}

/// The baseline with exits permitted and entries not: the shape a
/// liquidation-only window arms.
#[must_use]
pub fn grant_exit_only() -> Grant {
    grant_with(|grant| {
        grant.scope.permissions = GrantPermission {
            entries: false,
            exits: true,
            rolls: false,
        };
    })
}

/// The fixed tier order [`evaluate_grant`](crate::grant::evaluate_grant) uses,
/// one representative reason per stage.
///
/// Revoked → window → attestation → scope → budgets → signal → trade →
/// portfolio → breaker: authority before liveness before operator authority
/// before fit before capacity before merit, with the producer-confidence
/// breaker last because it measures the producer, not the trade. The pin
/// (`tests/fixtures.rs`) asserts an all-stages violation reports them in
/// exactly this order.
#[must_use]
pub fn deny_order() -> [&'static str; 9] {
    [
        "grant revoked",
        "grant window not live",
        "grant attestation lapsed",
        "proposal outside grant scope",
        "trade budget exhausted",
        "conviction below signal threshold",
        "per-trade max loss exceeded",
        "portfolio open-risk cap exceeded",
        "breaker tripped: entries disarmed",
    ]
}

#[cfg(test)]
#[path = "../tests/fixtures.rs"]
mod tests;
