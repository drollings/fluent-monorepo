use super::*;
use crate::economics::spread_economics;
use crate::proposal::{Direction, Proposal, ProposalSide, StructureKind};
use chrono::{NaiveDate, TimeZone, Utc};
use fluent_wvr::validate::Validate;
use fluent_wvr::{Describable, FieldAccess};
use protopyne_types::{
    ChainQuote, LegSide, Money, OptionContract, OptionKind, Qty, SpreadLeg, SpreadLegs,
};
use rust_decimal::Decimal;
use std::str::FromStr;

fn money(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

fn proposal() -> Proposal {
    Proposal::new()
        .id("p1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(Direction::Long)
        .side(ProposalSide::Entry)
        .structure(StructureKind::Vertical)
        .quantity(Qty::new(Decimal::from(2)))
        .limit_price(money(250))
        .max_loss(money(300))
        .notional(money(1_000))
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.7)
        .state(crate::proposal::ProposalState::Proposed)
        .created_at(Utc::now())
        .build()
}

fn grant() -> Grant {
    // The shared fixture's shape with the deny suite's tiers: the thresholds
    // these assertions sit just inside or outside of.
    crate::fixtures::grant_with(|grant| {
        grant.id = "g1".to_owned();
        grant.scope.structures = vec![StructureKind::Vertical, StructureKind::NakedShort];
        grant.budgets = GrantBudgets::new()
            .max_trades(10)
            .max_contracts(100)
            .max_loss(money(5_000))
            .max_notional(money(100_000))
            .build();
        grant.signal.min_conviction = 0.5;
        grant.signal.min_activity_score = 0.5;
        grant.trade.min_dte_days = 7;
        grant.trade.max_dte_days = 60;
        grant.portfolio.max_open_risk = money(10_000);
        grant.valid_from = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    })
}

fn context() -> (GrantUsage, GrantMarket, BreakerState) {
    (
        GrantUsage::new().build(),
        GrantMarket::new()
            .quote_age_secs(5)
            .spread_width(money(10))
            .build(),
        BreakerState::new().build(),
    )
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
}

fn deny_reasons(decision: GrantDecision) -> Vec<String> {
    match decision {
        GrantDecision::Allow => panic!("expected Deny, got Allow"),
        GrantDecision::Deny { reasons } => reasons,
    }
}

#[test]
fn valid_defined_risk_vertical_is_allowed() {
    let (usage, market, breakers) = context();
    let decision = evaluate_grant(&proposal(), &grant(), &usage, &market, &breakers, now());
    assert!(decision.is_allow(), "unexpected denial: {decision:?}");
}

#[test]
fn revoked_and_expired_grants_authorize_nothing() {
    let (usage, market, breakers) = context();
    let revoked = grant().revoked_copy();
    assert!(!revoked.is_live(now()));
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &revoked,
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("revoked")));

    let after = Utc.with_ymd_and_hms(2028, 1, 1, 0, 0, 0).unwrap();
    assert!(!grant().is_live(after));
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &grant(),
        &usage,
        &market,
        &breakers,
        after,
    ));
    assert!(reasons.iter().any(|r| r.contains("window")));
}

#[test]
fn scope_mismatches_deny() {
    let (usage, market, breakers) = context();
    let mut p = proposal();
    p.strategy = "other".to_owned();
    let reasons = deny_reasons(evaluate_grant(
        &p,
        &grant(),
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("scope")));

    let mut p = proposal();
    p.side = ProposalSide::Roll;
    let reasons = deny_reasons(evaluate_grant(
        &p,
        &grant(),
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("scope")));
}

#[test]
fn exhausted_budgets_deny() {
    let (_, market, breakers) = context();
    let usage = GrantUsage::new().used_trades(10).build();
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &grant(),
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("trade budget")));

    let usage = GrantUsage::new().used_contracts(99).build();
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &grant(),
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("contract budget")));

    let usage = GrantUsage::new().used_loss(money(4_900)).build();
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &grant(),
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("max-loss budget")));

    let usage = GrantUsage::new().used_notional(money(99_500)).build();
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &grant(),
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("notional budget")));
}

#[test]
fn open_risk_reading_replaces_cumulative_loss_in_the_open_risk_gate() {
    // Ten closed books leave a lifetime-cumulative tally past the cap
    // while the flat book holds no open risk: the gate reads the book,
    // not the ledger's lifetime tally (which stays a budget concern only).
    let mut grant = grant();
    grant.budgets.max_loss = money(1_000_000);
    let usage = GrantUsage::new().used_loss(money(50_000)).build();
    let (_, market, breakers) = context();
    // No reading: the caller declared the producer gates irrelevant, so
    // the lifetime tally alone never trips the open-risk cap.
    assert!(
        evaluate_grant(&proposal(), &grant, &usage, &market, &breakers, now()).is_allow(),
        "no reading is inert, not a denial"
    );
    // An observed flat book passes under the cap despite the tally.
    let flat = GrantMarket::new()
        .quote_age_secs(5)
        .spread_width(money(10))
        .open_risk(money(0))
        .build();
    assert!(
        evaluate_grant(&proposal(), &grant, &usage, &flat, &breakers, now()).is_allow(),
        "a flat book passes the open-risk cap"
    );
    // An observed book over the cap denies even with no consumed history.
    let full = GrantMarket::new()
        .quote_age_secs(5)
        .spread_width(money(10))
        .open_risk(money(9_900))
        .build();
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &grant,
        &GrantUsage::new().build(),
        &full,
        &breakers,
        now(),
    ));
    assert!(
        reasons
            .iter()
            .any(|r| r.contains("portfolio open-risk cap exceeded")),
        "the reading, not the tally, trips the cap, got: {reasons:?}"
    );
}

#[test]
fn contract_budget_counts_leg_contracts() {
    // The budget axis counts leg contracts: one package unit of a two-leg
    // vertical demands two, so a grant covering one denies it and one covering
    // two admits it.
    let two_leg = Proposal::new()
        .id("p2".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(Direction::Long)
        .side(ProposalSide::Entry)
        .structure(StructureKind::Vertical)
        .quantity(Qty::new(Decimal::from(1)))
        .limit_price(money(250))
        .max_loss(money(300))
        .notional(money(1_000))
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.7)
        .state(crate::proposal::ProposalState::Proposed)
        .created_at(Utc::now())
        .legs(spread(vec![
            leg(call("600"), LegSide::Long),
            leg(call("610"), LegSide::Short),
        ]))
        .build();
    assert_eq!(two_leg.contracts(), Some(1), "one package unit");
    assert_eq!(two_leg.leg_contracts(), Some(2), "two leg contracts");

    let (usage, market, breakers) = context();
    let mut one = grant();
    one.budgets.max_contracts = 1;
    let reasons = deny_reasons(evaluate_grant(
        &two_leg,
        &one,
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("contract budget")));

    let mut two = grant();
    two.budgets.max_contracts = 2;
    assert!(
        evaluate_grant(&two_leg, &two, &usage, &market, &breakers, now()).is_allow(),
        "a two-contract budget covers the two-leg unit"
    );
}

#[test]
fn weak_signals_and_bad_trades_deny() {
    let (usage, market, breakers) = context();
    let mut p = proposal();
    p.conviction = 0.1;
    let reasons = deny_reasons(evaluate_grant(
        &p,
        &grant(),
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("conviction")));

    let mut p = proposal();
    p.activity_score = 0.1;
    let reasons = deny_reasons(evaluate_grant(
        &p,
        &grant(),
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("activity")));

    let mut p = proposal();
    p.max_loss = money(5_000);
    let reasons = deny_reasons(evaluate_grant(
        &p,
        &grant(),
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("per-trade")));

    let wide = GrantMarket::new()
        .quote_age_secs(5)
        .spread_width(money(500))
        .build();
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &grant(),
        &usage,
        &wide,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("spread width")));

    let mut p = proposal();
    p.dte_days = 2;
    let reasons = deny_reasons(evaluate_grant(
        &p,
        &grant(),
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("DTE")));
}

#[test]
fn naked_short_is_excluded_even_when_in_scope() {
    let (usage, market, breakers) = context();
    let mut p = proposal();
    p.structure = StructureKind::NakedShort;
    let reasons = deny_reasons(evaluate_grant(
        &p,
        &grant(),
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("naked")));
}

#[test]
fn stale_quotes_and_tripped_breakers_deny() {
    let (usage, _, breakers) = context();
    let stale = GrantMarket::new()
        .quote_age_secs(600)
        .spread_width(money(10))
        .build();
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &grant(),
        &usage,
        &stale,
        &breakers,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("stale")));

    let (_, market, _) = context();
    let tripped = BreakerState::new()
        .tripped(true)
        .trip_reason("loss limit".to_owned())
        .build();
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &grant(),
        &usage,
        &market,
        &tripped,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("breaker")));

    let errors = BreakerState::new().consecutive_errors(10).build();
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &grant(),
        &usage,
        &market,
        &errors,
        now(),
    ));
    assert!(reasons.iter().any(|r| r.contains("consecutive-error")));
}

#[test]
fn evaluation_is_deterministic() {
    let (usage, market, breakers) = context();
    let first = evaluate_grant(&proposal(), &grant(), &usage, &market, &breakers, now());
    let second = evaluate_grant(&proposal(), &grant(), &usage, &market, &breakers, now());
    assert_eq!(first, second);
}

#[test]
fn tighten_accepts_restriction_and_refuses_loosening() {
    let tight_budgets = GrantBudgets::new()
        .max_trades(5)
        .max_contracts(50)
        .max_loss(money(2_000))
        .max_notional(money(50_000))
        .build();
    let tightened = grant()
        .tighten(GrantPatch::new().budgets(tight_budgets).build())
        .expect("tighter budgets");
    assert_eq!(tightened.budgets.max_trades, 5);

    let loose_budgets = GrantBudgets::new()
        .max_trades(50)
        .max_contracts(500)
        .max_loss(money(50_000))
        .max_notional(money(500_000))
        .build();
    assert!(
        grant()
            .tighten(GrantPatch::new().budgets(loose_budgets).build())
            .is_err(),
        "raising a budget must need a new grant"
    );

    let loose_signal = SignalThresholds::new()
        .min_conviction(0.1)
        .min_activity_score(0.1)
        .build();
    assert!(
        grant()
            .tighten(GrantPatch::new().signal(loose_signal).build())
            .is_err(),
        "lowering a signal threshold must need a new grant"
    );

    let later = Utc.with_ymd_and_hms(2028, 1, 1, 0, 0, 0).unwrap();
    assert!(
        grant()
            .tighten(GrantPatch::new().valid_until(later).build())
            .is_err(),
        "extending the window must need a new grant"
    );

    let earlier = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();
    let shortened = grant()
        .tighten(GrantPatch::new().valid_until(earlier).build())
        .expect("shorter window");
    assert_eq!(shortened.valid_until, earlier);
}

fn tightened_every_tier() -> Grant {
    let mut next = grant();
    next.budgets.max_trades -= 1;
    next.budgets.max_contracts -= 1;
    next.budgets.max_loss = money(4_000);
    next.budgets.max_notional = money(90_000);
    next.signal.min_conviction += 0.1;
    next.signal.min_activity_score += 0.1;
    next.trade.max_loss_per_trade = money(900);
    next.trade.max_spread_width = money(40);
    next.trade.min_dte_days += 1;
    next.trade.max_dte_days -= 1;
    next.portfolio.max_open_risk = money(9_000);
    next.portfolio.quote_max_age_secs -= 1;
    next.breakers.max_consecutive_errors -= 1;
    next.scope.directions.retain(|d| *d == Direction::Long);
    next.valid_from += chrono::Duration::days(1);
    next.valid_until -= chrono::Duration::days(1);
    next
}

#[test]
fn tightening_predicate_accepts_self_and_stricter_grants() {
    let current = grant();
    assert!(current.tightens(&current), "a grant tightens itself");
    let tighter = tightened_every_tier();
    assert!(
        tighter.tightens(&current),
        "every tier stricter must count as tightening"
    );
    assert!(
        !current.tightens(&tighter),
        "the wider grant is not a tightening of the narrower one"
    );
}

#[test]
fn tightening_predicate_refuses_every_loosened_tier() {
    let current = grant();
    let mut widened = current.clone();
    widened.budgets.max_trades += 1;
    assert!(!widened.tightens(&current), "raised trade budget");
    let mut widened = current.clone();
    widened.budgets.max_contracts += 1;
    assert!(!widened.tightens(&current), "raised contract budget");
    let mut widened = current.clone();
    widened.budgets.max_loss = money(6_000);
    assert!(!widened.tightens(&current), "raised loss budget");
    let mut widened = current.clone();
    widened.budgets.max_notional = money(200_000);
    assert!(!widened.tightens(&current), "raised notional budget");
    let mut widened = current.clone();
    widened.signal.min_conviction -= 0.1;
    assert!(!widened.tightens(&current), "lowered conviction threshold");
    let mut widened = current.clone();
    widened.signal.min_activity_score -= 0.1;
    assert!(!widened.tightens(&current), "lowered activity threshold");
    let mut widened = current.clone();
    widened.trade.max_loss_per_trade = money(1_100);
    assert!(!widened.tightens(&current), "widened per-trade loss");
    let mut widened = current.clone();
    widened.trade.max_spread_width = money(60);
    assert!(!widened.tightens(&current), "widened spread width");
    let mut widened = current.clone();
    widened.trade.min_dte_days -= 1;
    assert!(!widened.tightens(&current), "lowered minimum DTE");
    let mut widened = current.clone();
    widened.trade.max_dte_days += 1;
    assert!(!widened.tightens(&current), "raised maximum DTE");
    let mut widened = current.clone();
    widened.portfolio.max_open_risk = money(11_000);
    assert!(!widened.tightens(&current), "raised open-risk cap");
    let mut widened = current.clone();
    widened.portfolio.quote_max_age_secs += 1;
    assert!(!widened.tightens(&current), "raised quote-age cap");
    let mut widened = current.clone();
    widened.breakers.max_consecutive_errors += 1;
    assert!(!widened.tightens(&current), "raised breaker limit");
    let mut widened = current.clone();
    widened.scope.permissions.rolls = true;
    assert!(!widened.tightens(&current), "widened scope permissions");
    let mut widened = current.clone();
    widened.valid_until += chrono::Duration::days(1);
    assert!(!widened.tightens(&current), "extended window end");
    let mut widened = current.clone();
    widened.valid_from -= chrono::Duration::days(1);
    assert!(!widened.tightens(&current), "extended window start");
}

#[test]
fn tightening_predicate_agrees_with_tighten_on_every_tier() {
    // The predicate and the patch path must stay one rule: every tier the
    // predicate accepts must also pass through `tighten`, and vice versa.
    let current = grant();
    let tighter = tightened_every_tier();
    assert!(tighter.tightens(&current));
    let via_patch = current
        .tighten(
            GrantPatch::new()
                .budgets(tighter.budgets)
                .signal(tighter.signal)
                .trade(tighter.trade)
                .portfolio(tighter.portfolio)
                .breakers(tighter.breakers)
                .scope(tighter.scope.clone())
                .valid_until(tighter.valid_until)
                .build(),
        )
        .expect("predicate-accepted tiers must pass the patch path");
    assert_eq!(via_patch.budgets, tighter.budgets);
    assert!(via_patch.tightens(&current));
}

#[test]
fn budget_arithmetic_overflow_denies_instead_of_wrapping() {
    // Sums that overflow Decimal deny with an overflow reason: wrapping or
    // saturating into an authorization would let exposure exceed every cap.
    let (mut usage, market, breakers) = context();
    usage.used_loss = Money::new(Decimal::MAX);
    usage.used_notional = Money::new(Decimal::MAX);
    let reasons = deny_reasons(evaluate_grant(
        &proposal(),
        &grant(),
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(
        reasons.iter().any(|reason| reason.contains("overflow")),
        "overflow must deny loudly, got: {reasons:?}"
    );
}

#[test]
fn scope_and_structure_denials_keep_distinct_meanings() {
    // Review decision: both checks stay, documented. The whole-scope gate
    // speaks for the envelope; the trade-tier checks name the structure —
    // one for the defined-risk ban (scope-independent), one for threshold
    // attribution.
    let mut naked = proposal();
    naked.structure = StructureKind::NakedShort;
    let mut narrow = grant();
    narrow
        .scope
        .structures
        .retain(|structure| *structure != StructureKind::NakedShort);
    let (usage, market, breakers) = context();
    let reasons = deny_reasons(evaluate_grant(
        &naked,
        &narrow,
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(
        reasons
            .iter()
            .any(|reason| reason == "proposal outside grant scope"),
        "whole-scope gate fires, got: {reasons:?}"
    );
    assert!(
        reasons
            .iter()
            .any(|reason| reason == "naked short structures excluded"),
        "defined-risk ban fires independently of scope, got: {reasons:?}"
    );
    assert!(
        reasons
            .iter()
            .any(|reason| reason == "structure outside grant scope"),
        "structure attribution fires for audit, got: {reasons:?}"
    );
}

/// Descriptor surface behind the schema-driven arming form. The
/// form renders from `field_names` plus `describe` with no hand-written
/// field list, so these pins are the contract the form tests build on:
/// tier names, leaf names, the JSON boundary encoding, and the refusals.
fn budgets() -> GrantBudgets {
    GrantBudgets::new()
        .max_trades(10)
        .max_contracts(100)
        .max_loss(money(5_000))
        .max_notional(money(100_000))
        .build()
}

fn permissions() -> GrantPermission {
    GrantPermission {
        entries: true,
        exits: true,
        rolls: false,
    }
}

fn assert_copy<T: Copy>() {}

#[test]
fn descriptor_tiers_are_copy_so_get_field_compiles() {
    // The generated `get_field` moves an `Option` tier out of the borrow;
    // that codegen only compiles when every settable tier is `Copy`. This
    // test pins the property so a future non-`Copy` leaf fails here with
    // its reason, not deep in macro output. `GrantScope` is deliberately
    // absent: its lists can never be `Copy`, so the patch skips it.
    assert_copy::<GrantPermission>();
    assert_copy::<SignalThresholds>();
    assert_copy::<TradeThresholds>();
    assert_copy::<PortfolioThresholds>();
    assert_copy::<GrantBudgets>();
    assert_copy::<BreakerLimits>();
}

#[test]
fn patch_descriptor_names_the_settable_tiers() {
    assert_eq!(
        GrantPatch::default().field_names(),
        &[
            "budgets",
            "signal",
            "trade",
            "portfolio",
            "breakers",
            "valid_until"
        ],
        "scope stays out of the descriptor (see GrantPatch docs)"
    );
    let schema = GrantPatch::default().describe();
    let required = schema["required"]
        .as_array()
        .expect("schema carries required");
    assert!(
        required.is_empty(),
        "every patch tier is optional, got: {required:?}"
    );
    let properties = schema["properties"]
        .as_object()
        .expect("schema carries properties");
    for tier in GrantPatch::default().field_names() {
        let hint = &properties[*tier];
        assert!(
            hint["description"].as_str().is_some_and(|d| !d.is_empty()),
            "tier {tier} carries a form hint"
        );
    }
}

#[test]
fn leaf_descriptors_name_their_fields() {
    let live = grant();
    assert_eq!(
        live.budgets.field_names(),
        &["max_trades", "max_contracts", "max_loss", "max_notional"]
    );
    assert_eq!(
        live.signal.field_names(),
        &["min_conviction", "min_activity_score"]
    );
    assert_eq!(
        live.trade.field_names(),
        &[
            "max_loss_per_trade",
            "max_spread_width",
            "min_dte_days",
            "max_dte_days"
        ]
    );
    assert_eq!(
        live.portfolio.field_names(),
        &["max_open_risk", "quote_max_age_secs"]
    );
    assert_eq!(
        live.breakers.field_names(),
        &["max_consecutive_errors", "max_portfolio_drawdown_fraction"]
    );
    assert_eq!(permissions().field_names(), &["entries", "exits", "rolls"]);
    assert_eq!(
        live.scope.field_names(),
        &["permissions"],
        "scope lists stay out (no Vec boundary encoding)"
    );
}

#[test]
fn tier_json_boundary_round_trips() {
    // Display renders the same JSON document FromStr accepts: the single
    // encoding behind every composite patch leaf.
    for (name, rendered) in [
        ("budgets", budgets().to_string()),
        ("signal", grant().signal.to_string()),
        ("trade", grant().trade.to_string()),
        ("portfolio", grant().portfolio.to_string()),
        ("breakers", grant().breakers.to_string()),
        ("permissions", permissions().to_string()),
    ] {
        let value: serde_json::Value =
            serde_json::from_str(&rendered).expect("tier displays as JSON");
        assert!(value.is_object(), "tier {name} displays as a JSON object");
    }
    assert_eq!(
        GrantBudgets::from_str(&budgets().to_string()).expect("budgets parse"),
        budgets()
    );
    assert_eq!(
        GrantPermission::from_str(&permissions().to_string()).expect("permissions parse"),
        permissions()
    );
    assert!(
        GrantBudgets::from_str("{not json").is_err(),
        "malformed tier documents refuse"
    );
}

#[test]
fn patch_set_field_round_trips_tier_json() {
    let mut patch = GrantPatch::default();
    patch
        .set_field("budgets", &budgets().to_string())
        .expect("tier JSON sets");
    assert_eq!(patch.budgets, Some(budgets()));
    let rendered = patch.get_field("budgets").expect("tier reads back");
    assert_eq!(
        GrantBudgets::from_str(&rendered).expect("rendered tier re-parses"),
        budgets()
    );
    let bad = patch.set_field("budgets", "{not json");
    assert!(
        format!("{bad:?}").contains("budgets"),
        "tier parse failures name the tier, got: {bad:?}"
    );
    assert!(
        patch.set_field("scope", "{}").is_err(),
        "the skipped scope tier refuses by name"
    );
    assert!(
        patch.set_field("nope", "1").is_err(),
        "unknown fields refuse"
    );
}

#[test]
fn patch_valid_until_parses_rfc3339() {
    let mut patch = GrantPatch::default();
    patch
        .set_field("valid_until", "2027-01-01T00:00:00Z")
        .expect("RFC 3339 sets");
    assert_eq!(
        patch.valid_until,
        Some(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap())
    );
    assert!(
        patch.set_field("valid_until", "tomorrow").is_err(),
        "vague dates refuse"
    );
}

#[test]
fn patch_descriptor_nests_each_tier_leaf_schema() {
    // The hand-written nesting agrees with the derived field lists: a new
    // tier or leaf fails here instead of vanishing from the form.
    use fluent_wvr::{Describable as _, FieldAccess as _};
    let schema = GrantPatch::default().describe();
    let properties = schema["properties"].as_object().expect("properties");
    let mut top: Vec<&str> = properties.keys().map(String::as_str).collect();
    top.sort_unstable();
    let mut fields: Vec<&str> = GrantPatch::default().field_names().to_vec();
    fields.sort_unstable();
    assert_eq!(
        top, fields,
        "descriptor tiers are exactly the settable fields"
    );
    for (tier, leaves) in [
        (
            "budgets",
            GrantBudgets::new()
                .max_trades(0)
                .max_contracts(0)
                .max_loss(Money::zero())
                .max_notional(Money::zero())
                .build()
                .field_names(),
        ),
        (
            "signal",
            SignalThresholds::new()
                .min_conviction(0.0)
                .min_activity_score(0.0)
                .build()
                .field_names(),
        ),
        (
            "trade",
            TradeThresholds::new()
                .max_loss_per_trade(Money::zero())
                .max_spread_width(Money::zero())
                .min_dte_days(0)
                .max_dte_days(0)
                .build()
                .field_names(),
        ),
        (
            "portfolio",
            PortfolioThresholds::new()
                .max_open_risk(Money::zero())
                .quote_max_age_secs(0)
                .build()
                .field_names(),
        ),
        (
            "breakers",
            BreakerLimits::new()
                .max_consecutive_errors(0)
                .build()
                .field_names(),
        ),
    ] {
        let nested = properties[tier]["properties"]
            .as_object()
            .expect("tier nests leaves");
        let mut nested_names: Vec<&str> = nested.keys().map(String::as_str).collect();
        nested_names.sort_unstable();
        let mut leaf_names: Vec<&str> = leaves.to_vec();
        leaf_names.sort_unstable();
        assert_eq!(
            nested_names, leaf_names,
            "tier {tier} nests exactly its leaves"
        );
    }
    let until = &properties["valid_until"];
    assert_eq!(
        until["type"],
        serde_json::json!("string"),
        "window end stays a string leaf"
    );
    assert_eq!(
        until["format"],
        serde_json::json!("date-time"),
        "window end keeps its date-time format"
    );
}

#[test]
fn every_tier_marks_its_money_leaves_decimal() {
    // Money leaves ride decimal text, never JSON numbers: the leaf format
    // is what the form's decimal editor reads.
    use fluent_wvr::Describable as _;
    let schema = GrantPatch::default().describe();
    let properties = schema["properties"].as_object().expect("properties");
    for (tier, leaf) in [
        ("budgets", "max_loss"),
        ("budgets", "max_notional"),
        ("trade", "max_loss_per_trade"),
        ("trade", "max_spread_width"),
        ("portfolio", "max_open_risk"),
    ] {
        assert_eq!(
            properties[tier]["properties"][leaf]["format"],
            serde_json::json!("decimal"),
            "tier {tier} leaf {leaf} is decimal text"
        );
    }
}

#[test]
fn every_tier_display_fromstr_round_trips() {
    // Both codecs carry every tier: Display renders the document the form
    // previews, FromStr parses it back. One parser per tier, no second one.
    let signal = SignalThresholds::new()
        .min_conviction(0.5)
        .min_activity_score(0.25)
        .build();
    let trade = TradeThresholds::new()
        .max_loss_per_trade(Money::from(10))
        .max_spread_width(Money::from(5))
        .min_dte_days(1)
        .max_dte_days(30)
        .build();
    let portfolio = PortfolioThresholds::new()
        .max_open_risk(Money::from(10_000))
        .quote_max_age_secs(60)
        .build();
    let breakers = BreakerLimits::new().max_consecutive_errors(3).build();
    assert_eq!(
        SignalThresholds::from_str(&signal.to_string()).expect("signal parses"),
        signal
    );
    assert_eq!(
        TradeThresholds::from_str(&trade.to_string()).expect("trade parses"),
        trade
    );
    assert_eq!(
        PortfolioThresholds::from_str(&portfolio.to_string()).expect("portfolio parses"),
        portfolio
    );
    assert_eq!(
        BreakerLimits::from_str(&breakers.to_string()).expect("breakers parse"),
        breakers
    );
}

#[test]
fn excess_decimal_scale_refuses_on_both_money_paths() {
    // Scale parity with the operator-text fence
    // (`protopyne-chart-model/tests/display_only.rs :: operator_decimal_controls_all_refuse`,
    // which refuses the same 29-place text): the tier codec refuses it here
    // too, so neither path rounds excess precision into a decision.
    let deep = "0.00000000000000000000000000001";
    let mut patch = GrantPatch::default();
    let tier = format!(
        r#"{{"max_trades":1,"max_contracts":1,"max_loss":"{deep}","max_notional":"1000"}}"#
    );
    assert!(
        patch.set_field("budgets", &tier).is_err(),
        "the tier codec refuses 29-place decimal text"
    );
}

fn unit(max_loss: i64, notional: i64) -> WorstCaseUnit {
    WorstCaseUnit {
        max_loss: money(max_loss),
        notional: money(notional),
    }
}

fn worst_of(grant: &Grant, structures: &[StructureKind]) -> ComputedWorstCase {
    worst_case_for(
        grant,
        structures,
        |structure| match structure {
            StructureKind::Vertical => Some(unit(300, 900)),
            StructureKind::Condor => Some(unit(660, 2_000)),
            _ => None,
        },
        |structure| match structure {
            StructureKind::Condor => 4,
            StructureKind::Single => 1,
            _ => 2,
        },
    )
}

#[test]
fn a_two_leg_structure_halves_the_contract_implied_trade_cap() {
    let mut grant = grant();
    grant.budgets.max_trades = 1_000;
    grant.budgets.max_contracts = 100;
    grant.budgets.max_loss = money(1_000_000);
    grant.budgets.max_notional = money(1_000_000);

    let vertical = worst_case_for(
        &grant,
        &[StructureKind::Vertical],
        |_| Some(unit(300, 900)),
        |_| 2,
    );
    assert_eq!(vertical.max_trades, 50, "100 contracts / 2 legs = 50 units");
    assert_eq!(vertical.max_contracts, 100);
    assert_eq!(vertical.max_loss, money(15_000));
    assert_eq!(vertical.max_notional, money(45_000));
    assert_eq!(vertical.basis, WorstCaseBasis::ChainDerived);

    // The same unit at one leg per structure keeps the full contract cap.
    let single = worst_case_for(
        &grant,
        &[StructureKind::Vertical],
        |_| Some(unit(300, 900)),
        |_| 1,
    );
    assert_eq!(single.max_trades, 100);
}

#[test]
fn the_worst_structure_bounds_the_exposure() {
    let mut grant = grant();
    grant.budgets.max_trades = 10;
    grant.budgets.max_contracts = 40;
    grant.budgets.max_loss = money(1_000_000);
    grant.budgets.max_notional = money(1_000_000);

    // Both structures cap at 10 units; the condor's larger per-unit loss wins.
    let computed = worst_of(&grant, &[StructureKind::Vertical, StructureKind::Condor]);
    assert_eq!(computed.max_trades, 10);
    assert_eq!(computed.max_contracts, 40, "the condor's four legs bind");
    assert_eq!(computed.max_loss, money(6_600));
    assert_eq!(computed.basis, WorstCaseBasis::ChainDerived);
}

#[test]
fn a_condor_worst_case_uses_the_priced_wider_wing() {
    let (p90, p95) = (put("90"), put("95"));
    let (c105, c115) = (call("105"), call("115"));
    let legs = spread(vec![
        leg(p90.clone(), LegSide::Long),
        leg(p95.clone(), LegSide::Short),
        leg(c105.clone(), LegSide::Short),
        leg(c115.clone(), LegSide::Long),
    ]);
    // 5-wide put wing, 10-wide call wing: the wider wing bounds the package.
    let quotes = vec![
        chain_quote(p90, "0.50"),
        chain_quote(p95, "1.00"),
        chain_quote(c105, "1.00"),
        chain_quote(c115, "0.50"),
    ];
    let priced = spread_economics(&legs, &quotes, Qty::new(Decimal::ONE), Money::zero())
        .expect("condor prices");
    assert_eq!(priced.package_width, Some(money(10)));
    assert_eq!(priced.max_loss_per_unit, money(9));

    let mut grant = grant();
    grant.budgets.max_trades = 10;
    grant.budgets.max_contracts = 40;
    grant.budgets.max_loss = money(1_000_000);
    grant.budgets.max_notional = money(1_000_000);
    let unit = WorstCaseUnit {
        max_loss: priced.max_loss_per_unit,
        notional: priced.capital_requirement,
    };
    let computed = worst_case_for(&grant, &[StructureKind::Condor], |_| Some(unit), |_| 4);
    assert_eq!(computed.max_trades, 10);
    assert_eq!(computed.max_loss, money(90), "10 units of the 9 loss bound");
}

#[test]
fn no_chain_echoes_the_budget_fields() {
    let grant = grant();
    let computed = worst_case_for(&grant, &[StructureKind::Vertical], |_| None, |_| 2);
    assert_eq!(computed.basis, WorstCaseBasis::BudgetEcho);
    assert_eq!(computed.max_trades, grant.budgets.max_trades);
    assert_eq!(computed.max_contracts, grant.budgets.max_contracts);
    assert_eq!(computed.max_loss, grant.budgets.max_loss);
    assert_eq!(computed.max_notional, grant.budgets.max_notional);
}

#[test]
fn one_unpriced_structure_forces_the_echo() {
    let grant = grant();
    // Vertical prices, Condor does not: the whole bound falls back.
    let computed = worst_case_for(
        &grant,
        &[StructureKind::Vertical, StructureKind::Condor],
        |structure| (structure == StructureKind::Vertical).then(|| unit(300, 900)),
        |_| 2,
    );
    assert_eq!(computed.basis, WorstCaseBasis::BudgetEcho);
    assert_eq!(computed.max_loss, grant.budgets.max_loss);
}

#[test]
fn the_loss_budget_caps_a_computed_bound() {
    let mut grant = grant();
    grant.budgets.max_trades = 1_000;
    grant.budgets.max_contracts = 100;
    grant.budgets.max_loss = money(1_000);
    grant.budgets.max_notional = money(100_000);
    let computed = worst_case_for(
        &grant,
        &[StructureKind::Vertical],
        |_| Some(unit(300, 900)),
        |_| 2,
    );
    // 50 units × 300 = 15 000, but the loss budget is the hard ceiling.
    assert_eq!(computed.max_trades, 50);
    assert_eq!(computed.max_loss, money(1_000));
}

#[test]
fn lowering_a_budget_never_raises_the_worst_case() {
    let base = worst_of(&grant(), &[StructureKind::Vertical]);
    let tightenings: [fn(&mut Grant); 3] = [
        |grant| grant.budgets.max_trades -= 1,
        |grant| grant.budgets.max_contracts /= 2,
        |grant| grant.budgets.max_loss = money(1),
    ];
    for tighten in tightenings {
        let mut tighter = grant();
        tighten(&mut tighter);
        let computed = worst_of(&tighter, &[StructureKind::Vertical]);
        assert!(computed.max_trades <= base.max_trades);
        assert!(computed.max_contracts <= base.max_contracts);
        assert!(computed.max_loss.as_decimal() <= base.max_loss.as_decimal());
        assert!(computed.max_notional.as_decimal() <= base.max_notional.as_decimal());
    }
}

fn put(strike: &str) -> OptionContract {
    option(OptionKind::Put, strike)
}

fn call(strike: &str) -> OptionContract {
    option(OptionKind::Call, strike)
}

fn option(kind: OptionKind, strike: &str) -> OptionContract {
    OptionContract::new()
        .underlying("SPY".to_owned())
        .expiry(NaiveDate::from_ymd_opt(2026, 7, 17).expect("valid expiry"))
        .strike(strike.parse().expect("strike"))
        .kind(kind)
        .build()
}

fn leg(contract: OptionContract, side: LegSide) -> SpreadLeg {
    SpreadLeg::new().contract(contract).side(side).build()
}

fn spread(legs: Vec<SpreadLeg>) -> SpreadLegs {
    SpreadLegs::try_new(legs).expect("a defined-risk leg set")
}

fn chain_quote(contract: OptionContract, price: &str) -> ChainQuote {
    ChainQuote::new()
        .contract(contract)
        .bid(price.parse().expect("bid"))
        .ask(price.parse().expect("ask"))
        .timestamp(Utc.with_ymd_and_hms(2026, 7, 1, 14, 30, 0).unwrap())
        .build()
}

// ── Drawdown-cap validation and tightening ─────────────────────────────────

#[test]
fn arming_refuses_a_drawdown_cap_outside_zero_to_one() {
    for cap in [0.0, -0.1, 1.5, f64::NAN, f64::INFINITY] {
        let mut armed = grant();
        armed.breakers.max_portfolio_drawdown_fraction = Some(cap);
        assert!(
            armed.validate().is_err(),
            "cap {cap} must not arm: it can never be a threshold"
        );
    }
    for cap in [None, Some(0.05), Some(1.0)] {
        let mut armed = grant();
        armed.breakers.max_portfolio_drawdown_fraction = cap;
        assert!(
            armed.validate().is_ok(),
            "cap {cap:?} must arm: unset or within (0, 1]"
        );
    }
}

#[test]
fn tightening_a_drawdown_cap_only_moves_down() {
    let mut capped = grant();
    capped.breakers.max_portfolio_drawdown_fraction = Some(0.10);
    // Lowering the cap (tripping sooner) is tighter — allowed.
    let mut lower = capped.clone();
    lower.breakers.max_portfolio_drawdown_fraction = Some(0.05);
    assert!(lower.tightens(&capped));
    lower
        .tighten(
            GrantPatch::new()
                .breakers(BreakerLimits {
                    max_portfolio_drawdown_fraction: Some(0.05),
                    ..capped.breakers
                })
                .build(),
        )
        .expect("lowering the drawdown cap is a tightening");
    // Raising the cap, or dropping it, is looser — refused.
    let mut higher = capped.clone();
    higher.breakers.max_portfolio_drawdown_fraction = Some(0.20);
    assert!(!higher.tightens(&capped));
    assert!(
        capped
            .tighten(
                GrantPatch::new()
                    .breakers(BreakerLimits {
                        max_portfolio_drawdown_fraction: Some(0.20),
                        ..capped.breakers
                    })
                    .build(),
            )
            .is_err(),
        "raising the drawdown cap must need a new grant"
    );
    let mut dropped = capped.clone();
    dropped.breakers.max_portfolio_drawdown_fraction = None;
    assert!(!dropped.tightens(&capped));
    // Setting the first cap on an uncapped grant is tighter — allowed.
    let mut fresh = grant();
    assert!(capped.tightens(&fresh));
    fresh.breakers.max_portfolio_drawdown_fraction = Some(0.10);
    assert!(fresh.tightens(&grant()));
}

// ── Re-attestation: the operator's authority lapses, not the trade ─────────

use std::time::Duration;

fn attested_grant(cadence_secs: u64, grace_secs: u64) -> Grant {
    crate::fixtures::grant_with(|grant| {
        grant.id = "attested-grant".to_owned();
        grant.re_attestation = Some(ReAttestationWindow {
            cadence: Duration::from_secs(cadence_secs),
            grace: Duration::from_secs(grace_secs),
        });
    })
}

fn usage_attested_at(at: chrono::DateTime<Utc>) -> GrantUsage {
    let mut usage = GrantUsage::new().build();
    usage.last_attested = Some(at);
    usage
}

fn entry() -> Proposal {
    proposal()
}

fn exit_of(entry: &Proposal) -> Proposal {
    let mut exit = entry.clone();
    exit.side = ProposalSide::Exit;
    exit
}

// Control: no window, no check — even an ancient attestation passes.
#[test]
fn no_window_means_no_attestation_check_ever() {
    let (usage, market, breakers) = context();
    let mut stale = usage;
    stale.last_attested = Some(Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap());
    assert!(
        evaluate_grant(&entry(), &grant(), &stale, &market, &breakers, now()).is_allow(),
        "without a window there is no attestation to lapse"
    );
}

// Control: attested within cadence + grace ⇒ entries allowed.
#[test]
fn attested_within_cadence_allows_entries() {
    let (_, market, breakers) = context();
    let at = now() - chrono::Duration::hours(1);
    let usage = usage_attested_at(at);
    assert!(
        evaluate_grant(
            &entry(),
            &attested_grant(24 * 3600, 3600),
            &usage,
            &market,
            &breakers,
            now()
        )
        .is_allow(),
        "an hour-old attestation is inside a daily cadence"
    );
}

// Trigger: one instant past cadence + grace ⇒ entries denied; exits allowed.
#[test]
fn one_instant_past_cadence_plus_grace_denies_entries_not_exits() {
    let (_, market, breakers) = context();
    // Cadence 24 h + grace 1 h = 25 h; attested 25 h and one second ago.
    let at = now() - chrono::Duration::seconds(25 * 3600 + 1);
    let usage = usage_attested_at(at);
    let grant = attested_grant(24 * 3600, 3600);
    let reasons = deny_reasons(evaluate_grant(
        &entry(),
        &grant,
        &usage,
        &market,
        &breakers,
        now(),
    ));
    assert!(
        reasons.iter().any(|reason| reason.contains("attestation")),
        "a lapsed attestation denies with its own reason: {reasons:?}"
    );
    let exit = exit_of(&entry());
    assert!(
        evaluate_grant(&exit, &grant, &usage, &market, &breakers, now()).is_allow(),
        "exits route even when entries disarm — positions stay manageable"
    );
}

// Calibration: longer windows get tighter (or equal) re-attestation.
#[test]
fn window_lengths_map_to_exactly_one_monotone_cadence() {
    use std::time::Duration;
    // Boundaries and midpoints: nothing unmapped, nothing mapped twice.
    let windows = [
        Duration::ZERO,
        Duration::from_hours(1),
        Duration::from_secs(3601),
        Duration::from_hours(24),
        Duration::from_hours(24) + Duration::from_secs(1),
        Duration::from_hours(72),
        Duration::from_hours(72) + Duration::from_secs(1),
        Duration::from_hours(24) * 30,
    ];
    let mut last_tightness: Option<Duration> = None;
    for window in windows {
        let mapped = re_attestation_for_window(window);
        // Exactly one mapping per window length (None counts as mapped).
        let tightness = mapped.map(|w| w.cadence.saturating_add(w.grace));
        if let (Some(previous), Some(current)) = (last_tightness, tightness) {
            assert!(
                current <= previous,
                "a longer window must not loosen re-attestation: {window:?} maps looser than before"
            );
        }
        if tightness.is_some() {
            last_tightness = tightness;
        } else {
            // None is the loosest possible mapping: it may only lead.
            assert!(
                last_tightness.is_none(),
                "an unattested window after an attested one breaks monotonicity: {window:?}"
            );
        }
    }
    // The documented table corners.
    assert_eq!(re_attestation_for_window(Duration::from_hours(1)), None);
    assert_eq!(re_attestation_for_window(Duration::from_hours(24)), None);
    assert_eq!(
        re_attestation_for_window(Duration::from_hours(72)),
        Some(ReAttestationWindow {
            cadence: Duration::from_hours(24),
            grace: Duration::from_hours(1),
        })
    );
    assert_eq!(
        re_attestation_for_window(Duration::from_hours(72) + Duration::from_secs(1)),
        Some(ReAttestationWindow {
            cadence: Duration::from_hours(24),
            grace: Duration::ZERO,
        })
    );
}

#[test]
fn tightening_an_attestation_window_only_moves_down() {
    use std::time::Duration;
    let mut capped = grant();
    capped.re_attestation = Some(ReAttestationWindow {
        cadence: Duration::from_hours(24),
        grace: Duration::from_hours(1),
    });
    // Shortening the cadence (tripping sooner) is tighter — allowed.
    let mut lower = capped.clone();
    lower.re_attestation = Some(ReAttestationWindow {
        cadence: Duration::from_hours(12),
        grace: Duration::from_hours(1),
    });
    assert!(lower.tightens(&capped));
    // Raising the cadence, or dropping the window, is looser — refused.
    let mut higher = capped.clone();
    higher.re_attestation = Some(ReAttestationWindow {
        cadence: Duration::from_hours(48),
        grace: Duration::from_hours(1),
    });
    assert!(!higher.tightens(&capped));
    let mut dropped = capped.clone();
    dropped.re_attestation = None;
    assert!(!dropped.tightens(&capped));
    // Setting the first window on an unattested grant is tighter — allowed.
    assert!(capped.tightens(&grant()));
}
