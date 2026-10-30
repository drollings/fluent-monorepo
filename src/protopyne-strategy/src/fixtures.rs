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

use crate::grant::{refusal, Grant, GrantPermission, GrantPreset};
use crate::proposal::StructureKind;

/// The baseline grant, with `tweak` applied after construction.
///
/// The baseline is the permissive *rehearsal* shape the server's targets share:
/// wide budgets, both directions, every entry/exit permission, thresholds at
/// zero, a 2025–2027 window. A test that needs a narrower object names only what
/// differs.
#[must_use]
pub fn grant_with(tweak: impl FnOnce(&mut Grant)) -> Grant {
    // (M6.1c) The baseline IS the rehearsal preset: same scope, budgets,
    // thresholds, and breakers — only the window differs (fixture
    // 2025–2027 vs preset epoch stamps), so drift in any tier fails the
    // shared preset suites instead of one copy silently.
    let mut grant = Grant::preset(GrantPreset::Rehearsal {
        id: "fixture-grant".to_owned(),
        structures: vec![StructureKind::Vertical],
    });
    grant.valid_from = Utc
        .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
        .single()
        .expect("valid window");
    grant.valid_until = Utc
        .with_ymd_and_hms(2027, 1, 1, 0, 0, 0)
        .single()
        .expect("valid window");
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
        refusal::REVOKED,
        refusal::WINDOW_NOT_LIVE,
        refusal::ATTESTATION_LAPSED,
        refusal::OUTSIDE_SCOPE,
        refusal::TRADE_BUDGET_EXHAUSTED,
        refusal::CONVICTION_BELOW_THRESHOLD,
        refusal::MAX_LOSS_EXCEEDED,
        refusal::OPEN_RISK_EXCEEDED,
        refusal::BREAKER_TRIPPED,
    ]
}

#[cfg(test)]
#[path = "../tests/fixtures.rs"]
mod tests;
