//! The shared grant fixture: its baseline is arming-valid and its deny order is
//! the documented one.
//!
//! The fixture is the one place a deny stage is named in order, so a new stage
//! (or a stage that moves) has to change [`deny_order`] and this pin together —
//! every test target that arms a grant then reads the same contract.

use super::*;

use chrono::{TimeZone, Utc};
use fluent_wvr::validate::Validate;
use protopyne_types::{Money, Qty};
use rust_decimal::Decimal;

use crate::grant::{evaluate_grant, BreakerState, GrantDecision, GrantMarket, GrantUsage};
use crate::proposal::{Proposal, ProposalSide};
use crate::ReAttestationWindow;

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0)
        .single()
        .expect("valid instant")
}

fn money(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

/// A proposal that fails every tier at once: out of scope, over budget, under
/// the signal floor, outside the trade window, and structurally disallowed.
fn everything_wrong() -> Proposal {
    Proposal::new()
        .id("fixture-deny".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("QQQ".to_owned())
        .direction(Direction::Long)
        .side(ProposalSide::Entry)
        .structure(StructureKind::NakedShort)
        .quantity(Qty::new(Decimal::from(2)))
        .limit_price(money(250))
        .max_loss(money(500))
        .notional(money(1_000))
        .dte_days(5)
        .conviction(0.1)
        .activity_score(0.1)
        .state(crate::proposal::ProposalState::Proposed)
        .created_at(now())
        .build()
}

#[test]
fn the_baseline_fixture_is_a_live_permissive_grant() {
    let grant = grant();
    assert!(
        grant.is_live(now()),
        "the baseline is live at the fixture instant"
    );
    assert!(
        grant.validate().is_ok(),
        "the baseline is structurally sound: {:?}",
        grant.validate()
    );
    assert!(
        grant.scope.permissions.entries && grant.scope.permissions.exits,
        "both lifecycle permissions start on"
    );
    let exit_only = grant_exit_only();
    assert!(
        !exit_only.scope.permissions.entries && exit_only.scope.permissions.exits,
        "the exit-only shape keeps exits and drops entries"
    );
}

#[test]
fn every_tier_denies_in_the_documented_order() {
    let order = deny_order();
    let grant = grant_with(|grant| {
        grant.revoked = true;
        grant.valid_from = Utc
            .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
            .single()
            .expect("valid window");
        grant.valid_until = Utc
            .with_ymd_and_hms(2025, 6, 1, 0, 0, 0)
            .single()
            .expect("valid window");
        grant.scope.underlyings = vec!["SPY".to_owned()];
        grant.signal.min_conviction = 0.9;
        grant.signal.min_activity_score = 0.9;
        grant.trade.max_loss_per_trade = money(10);
        grant.trade.max_spread_width = money(1);
        grant.trade.min_dte_days = 30;
        grant.trade.max_dte_days = 40;
        grant.portfolio.max_open_risk = money(100);
        grant.portfolio.quote_max_age_secs = 1;
        grant.re_attestation = Some(ReAttestationWindow {
            cadence: std::time::Duration::from_secs(3600),
            grace: std::time::Duration::ZERO,
        });
    });
    let mut usage = GrantUsage::new()
        .used_trades(1_000)
        .used_contracts(10_000)
        .used_loss(money(1_000_000))
        .used_notional(money(100_000_000))
        .build();
    usage.last_attested = Some(
        Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0)
            .single()
            .expect("valid instant"),
    );
    let market = GrantMarket::new()
        .quote_age_secs(100)
        .spread_width(money(50))
        .open_risk(money(1_000_000))
        .build();
    let breakers = BreakerState::new().tripped(true).build();
    let reasons = match evaluate_grant(
        &everything_wrong(),
        &grant,
        &usage,
        &market,
        &breakers,
        now(),
    ) {
        GrantDecision::Allow => panic!("an all-stage violation cannot be allowed"),
        GrantDecision::Deny { reasons } => reasons,
    };
    let mut previous = None;
    for stage in order {
        let position = reasons
            .iter()
            .position(|reason| reason == stage)
            .unwrap_or_else(|| panic!("{stage:?} missing from {reasons:?}"));
        if let Some(previous) = previous {
            assert!(
                previous < position,
                "{stage:?} reported at {position}, before the previous stage at {previous}: {reasons:?}"
            );
        }
        previous = Some(position);
    }
    assert_eq!(
        order.len(),
        9,
        "the documented order names every tier exactly once"
    );
}
