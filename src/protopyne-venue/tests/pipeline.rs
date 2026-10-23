use super::*;
use protopyne_strategy::fixtures::grant_with;

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use protopyne_store::StoreHealth;
use protopyne_strategy::{
    Direction, Grant, PortfolioFacts, ProposalSide, ReAttestationWindow, RiskLimits, StructureKind,
};
use protopyne_types::{Money, Qty};

use crate::command::TradingState;
use crate::reconcile::ReconcileReport;

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
}

fn proposal(quantity: &str, side: ProposalSide) -> Proposal {
    Proposal::new()
        .id("prop-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(Direction::Long)
        .side(side)
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
    // The shared fixture's shape with the two knobs this suite sweeps.
    grant_with(|grant| {
        grant.id = "pipeline-grant".to_owned();
        grant.signal.min_conviction = min_conviction;
        grant.signal.min_activity_score = 0.5;
        grant.breakers.max_consecutive_errors = max_errors;
    })
}

fn risk_state() -> RiskState {
    RiskState {
        quote_age_secs: 0,
        clock_skew_secs: 0,
        reconciliation_healthy: true,
        facts: PortfolioFacts::unobserved(),
    }
}

fn health(stale: bool) -> HealthState {
    HealthState {
        quote_age_secs: if stale { 5_000 } else { 5 },
        clock_skew_secs: 1,
        stream_connected: true,
        store: StoreHealth::default(),
        reconciled: true,
        backfilled: true,
    }
}

/// One evaluation scenario. Every field has a passing default; a case flips
/// exactly the gates it wants to fail.
#[derive(Clone, Copy)]
#[allow(clippy::struct_excessive_bools)]
struct Case {
    quantity: &'static str,
    is_exit: bool,
    stale_health: bool,
    trading: TradingState,
    reconcile_halt: bool,
    backfill_suspend: bool,
    kill_switch: bool,
    min_conviction: f64,
    max_errors: u32,
    trip_breaker: bool,
    shape: OrderShape,
}

impl Default for Case {
    fn default() -> Self {
        Self {
            quantity: "2",
            is_exit: false,
            stale_health: false,
            trading: TradingState::Enabled,
            reconcile_halt: false,
            backfill_suspend: false,
            kill_switch: false,
            min_conviction: 0.5,
            max_errors: 3,
            trip_breaker: false,
            shape: OrderShape::new(true, true),
        }
    }
}

/// The owned reason split of one evaluation (the reservation borrows its
/// ledger, so only its presence escapes).
#[allow(clippy::struct_excessive_bools)]
struct Split {
    allowed: bool,
    axis: Option<GateAxis>,
    reasons: Vec<String>,
    risk_pass: bool,
    risk_reasons: Vec<String>,
    approval: bool,
    merit: Option<GrantDecision>,
    merit_reasons: Vec<String>,
    authorized: bool,
    authorization_reasons: Vec<String>,
    reserved: bool,
}

fn evaluate_split(case: &Case) -> Split {
    let side = if case.is_exit {
        ProposalSide::Exit
    } else {
        ProposalSide::Entry
    };
    let proposal = proposal(case.quantity, side);
    let ledger = GrantLedger::new(grant(case.min_conviction, case.max_errors));
    if case.trip_breaker {
        for _ in 0..=case.max_errors {
            ledger.observe_error(true);
        }
    }
    let mut pipeline = DecisionPipeline::new(StateHandle::new(case.trading));
    if case.reconcile_halt {
        let handle = ReconcileHandle::new();
        handle.observe(&ReconcileReport {
            matched: false,
            mismatches: Vec::new(),
            alerts: vec!["book disagrees with venue".to_owned()],
        });
        pipeline = pipeline.with_reconcile(handle);
    }
    if case.backfill_suspend {
        let handle = BackfillHandle::new();
        handle.suspend("unspliced hole".to_owned());
        pipeline = pipeline.with_backfill(handle);
    }
    let state = health(case.stale_health);
    let limits = HealthLimits::new().build();
    let risk = RiskEngine::new(RiskLimits::new().kill_switch(case.kill_switch).build());
    let risk_state = risk_state();
    let market = GrantMarket::new()
        .quote_age_secs(0)
        .spread_width(money("1"))
        .build();
    let verdict = pipeline.evaluate(&PipelineRequest {
        proposal: &proposal,
        shape: case.shape,
        is_exit: case.is_exit,
        health: Some((&state, &limits)),
        risk_state: &risk_state,
        market: &market,
        now: now(),
        risk: &risk,
        ledger: &ledger,
    });
    Split {
        allowed: verdict.allowed,
        axis: verdict.axis,
        reasons: verdict.reasons,
        risk_pass: verdict.risk_pass,
        risk_reasons: verdict.risk_reasons,
        approval: verdict.approval.is_some(),
        merit: verdict.grant_merit,
        merit_reasons: verdict.grant_merit_reasons,
        authorized: verdict.grant_authorized,
        authorization_reasons: verdict.grant_authorization_reasons,
        reserved: verdict.reservation.is_some(),
    }
}

/// Evaluate one case and return its verdict facts (the reservation borrows
/// its ledger, so only its presence escapes).
fn evaluate(case: &Case) -> (bool, Option<GateAxis>, Vec<String>, Option<GrantDecision>) {
    let split = evaluate_split(case);
    (split.allowed, split.axis, split.reasons, split.merit)
}

/// The full gate order, proven by fixing one axis at a time: correctness
/// refuses before producer-confidence, which refuses before task-value. Each
/// row drops the previous row's first failing gate, so the axis that fires
/// next names the order.
#[test]
fn refusal_order_follows_the_documented_axes() {
    let base = Case::default();
    let table: Vec<(&str, Case, Option<GateAxis>, &str)> = vec![
        (
            "correctness before confidence before value",
            Case {
                quantity: "1.5",
                stale_health: true,
                trading: TradingState::Disabled,
                reconcile_halt: true,
                backfill_suspend: true,
                kill_switch: true,
                ..base
            },
            Some(GateAxis::Correctness),
            "whole number",
        ),
        (
            "health before trading",
            Case {
                stale_health: true,
                trading: TradingState::Disabled,
                reconcile_halt: true,
                backfill_suspend: true,
                kill_switch: true,
                ..base
            },
            Some(GateAxis::ProducerConfidence),
            "quote age",
        ),
        (
            "trading before reconcile",
            Case {
                trading: TradingState::Disabled,
                reconcile_halt: true,
                backfill_suspend: true,
                kill_switch: true,
                ..base
            },
            Some(GateAxis::ProducerConfidence),
            "halted",
        ),
        (
            "reconcile before backfill",
            Case {
                reconcile_halt: true,
                backfill_suspend: true,
                kill_switch: true,
                ..base
            },
            Some(GateAxis::ProducerConfidence),
            "disagree",
        ),
        (
            "backfill before value",
            Case {
                backfill_suspend: true,
                kill_switch: true,
                ..base
            },
            Some(GateAxis::ProducerConfidence),
            "suspend",
        ),
        (
            "value last",
            Case {
                kill_switch: true,
                ..base
            },
            Some(GateAxis::TaskValue),
            "KillSwitch",
        ),
        ("all gates pass", base, None, ""),
    ];
    for (name, case, axis, needle) in table {
        let (allowed, got_axis, reasons, merit) = evaluate(&case);
        assert_eq!(got_axis, axis, "case '{name}': {reasons:?}");
        assert_eq!(allowed, axis.is_none(), "case '{name}': {reasons:?}");
        if needle.is_empty() {
            assert!(reasons.is_empty(), "case '{name}': {reasons:?}");
            assert_eq!(merit, Some(GrantDecision::Allow), "case '{name}'");
        } else {
            assert!(
                reasons.iter().any(|reason| reason.contains(needle)),
                "case '{name}': {reasons:?}"
            );
        }
    }
}

/// A stale quote halts entries, but an exit still routes past the whole
/// producer-confidence axis.
#[test]
fn health_refusal_still_routes_an_exit() {
    let entry = Case {
        stale_health: true,
        ..Case::default()
    };
    let (allowed, axis, _, _) = evaluate(&entry);
    assert!(!allowed);
    assert_eq!(axis, Some(GateAxis::ProducerConfidence));

    let exit = Case {
        is_exit: true,
        ..entry
    };
    let (allowed, axis, reasons, _) = evaluate(&exit);
    assert!(allowed, "exits route: {axis:?} {reasons:?}");
}

/// A reconciliation mismatch refuses entries and permits exits.
#[test]
fn reconcile_mismatch_refuses_entries_and_permits_exits() {
    let entry = Case {
        reconcile_halt: true,
        ..Case::default()
    };
    let (allowed, axis, reasons, _) = evaluate(&entry);
    assert!(!allowed);
    assert_eq!(axis, Some(GateAxis::ProducerConfidence));
    assert!(reasons[0].contains("disagree"), "{reasons:?}");

    let exit = Case {
        is_exit: true,
        ..entry
    };
    let (allowed, ..) = evaluate(&exit);
    assert!(allowed);
}

/// A tripped breaker disarms entries (producer-confidence) without rewriting
/// the grant's merit (task-value), and never blocks an exit.
#[test]
fn a_tripped_breaker_disarms_entries_without_rewriting_merit() {
    let clean = Case {
        max_errors: 1,
        ..Case::default()
    };
    let (allowed, axis, _, merit) = evaluate(&clean);
    assert!(allowed, "{axis:?}");
    assert_eq!(merit, Some(GrantDecision::Allow));

    let tripped = Case {
        trip_breaker: true,
        ..clean
    };
    let (allowed, axis, reasons, merit) = evaluate(&tripped);
    assert!(!allowed);
    assert_eq!(axis, Some(GateAxis::ProducerConfidence));
    assert!(reasons[0].contains("breaker"), "{reasons:?}");
    assert_eq!(
        merit,
        Some(GrantDecision::Allow),
        "merit is evaluated without breaker input"
    );

    let exit = Case {
        is_exit: true,
        ..tripped
    };
    let (allowed, axis, reasons, _) = evaluate(&exit);
    assert!(
        allowed,
        "exits route past a tripped breaker: {axis:?} {reasons:?}"
    );
}

/// A grant that does not cover the trade refuses on the task-value axis.
#[test]
fn a_strict_grant_refuses_on_the_task_value_axis() {
    let strict = Case {
        min_conviction: 0.95,
        ..Case::default()
    };
    let (allowed, axis, reasons, merit) = evaluate(&strict);
    assert!(!allowed);
    assert_eq!(axis, Some(GateAxis::TaskValue));
    assert!(merit.is_some_and(|decision| !decision.is_allow()));
    assert!(!reasons.is_empty());
}

/// The session's submission route: the confidence gates in order, each
/// preserving its own error variant, with exits routing past all of them.
#[test]
fn route_enforces_confidence_order_and_preserves_refusals() {
    let clean = DecisionPipeline::new(StateHandle::new(TradingState::Enabled));
    assert!(clean.route(false).is_ok());
    assert!(clean.route(true).is_ok());

    let halted = DecisionPipeline::new(StateHandle::new(TradingState::Disabled));
    let error = halted.route(false).expect_err("disabled refuses entries");
    assert!(matches!(error, VenueError::Refused(_)), "{error:?}");
    assert!(halted.route(true).is_ok(), "exits route past a halt");

    let reconcile = ReconcileHandle::new();
    reconcile.observe(&ReconcileReport {
        matched: false,
        mismatches: Vec::new(),
        alerts: Vec::new(),
    });
    let pinned =
        DecisionPipeline::new(StateHandle::new(TradingState::Enabled)).with_reconcile(reconcile);
    let error = pinned.route(false).expect_err("a mismatch refuses entries");
    assert!(matches!(error, VenueError::Reconciliation(_)), "{error:?}");
    assert!(pinned.route(true).is_ok());

    let backfill = BackfillHandle::new();
    backfill.suspend("hole".to_owned());
    let gapped =
        DecisionPipeline::new(StateHandle::new(TradingState::Enabled)).with_backfill(backfill);
    let error = gapped.route(false).expect_err("a hole refuses entries");
    assert!(matches!(error, VenueError::Health(_)), "{error:?}");
    assert!(gapped.route(true).is_ok());
}

/// A risk refusal still records the complete split: merit is evaluated
/// without breaker input and authorization reflects the reservation attempt,
/// so a caller can populate its decision without a second evaluation.
#[test]
fn risk_refusal_still_records_the_full_split() {
    let refused = Case {
        kill_switch: true,
        ..Case::default()
    };
    let split = evaluate_split(&refused);
    assert!(!split.allowed);
    assert_eq!(split.axis, Some(GateAxis::TaskValue));
    assert!(!split.risk_pass);
    assert!(!split.risk_reasons.is_empty(), "{:?}", split.risk_reasons);
    assert!(!split.approval);
    assert_eq!(split.merit, Some(GrantDecision::Allow));
    assert!(split.merit_reasons.is_empty());
    // The runner reserves on merit even when risk refuses; the dropped
    // reservation releases the hold, so authorization reads true here.
    assert!(split.authorized);
    assert!(split.reserved);
    assert!(split.authorization_reasons.is_empty());
}

/// A correctness refusal still records the task-value split instead of
/// defaulting it: the token is minted exactly when the engine passes, merit
/// is evaluated, and authorization tracks the reservation attempt.
#[test]
fn correctness_refusal_still_records_the_task_value_split() {
    let fractional = Case {
        quantity: "1.5",
        ..Case::default()
    };
    let split = evaluate_split(&fractional);
    assert!(!split.allowed);
    assert_eq!(split.axis, Some(GateAxis::Correctness));
    assert_eq!(
        split.risk_pass, split.approval,
        "the token is minted exactly when the engine passes"
    );
    assert!(split.merit.is_some(), "merit is evaluated, never defaulted");
    assert_eq!(split.authorized, split.reserved);
}

/// A tripped breaker records merit `Allow` but disarms authorization: the
/// merit verdict never carries the producer trip, and no budget is held.
#[test]
fn breaker_trip_records_merit_but_disarms_authorization() {
    let tripped = Case {
        trip_breaker: true,
        max_errors: 1,
        ..Case::default()
    };
    let split = evaluate_split(&tripped);
    assert!(!split.allowed);
    assert_eq!(split.axis, Some(GateAxis::ProducerConfidence));
    assert!(split.risk_pass);
    assert_eq!(split.merit, Some(GrantDecision::Allow));
    assert!(!split.authorized);
    assert!(!split.reserved);
    assert!(
        split
            .authorization_reasons
            .iter()
            .any(|reason| reason.contains("breaker")),
        "{:?}",
        split.authorization_reasons
    );
}

/// Simultaneous refusals: the first axis wins, later stages still run, and
/// the verdict carries the full split.
#[test]
fn multiple_refusals_first_axis_wins_but_split_stays_full() {
    let case = Case {
        quantity: "1.5",
        stale_health: true,
        kill_switch: true,
        ..Case::default()
    };
    let split = evaluate_split(&case);
    assert!(!split.allowed);
    assert_eq!(split.axis, Some(GateAxis::Correctness));
    assert!(
        split.reasons.iter().any(|reason| reason.contains("whole")),
        "{:?}",
        split.reasons
    );
    assert!(!split.risk_pass, "task-value stage still evaluated");
    assert!(!split.risk_reasons.is_empty(), "{:?}", split.risk_reasons);
    assert!(split.merit.is_some(), "merit evaluated, never defaulted");
}

// ── Rolls: the roll permission gates, the risk engine still judges ─────────

/// A roll grant: the shared fixture's shape with only the roll permission
/// flipped. The fixture baseline denies rolls; these tests arm them.
fn roll_grant(rolls: bool) -> Grant {
    grant_with(|grant| {
        grant.id = "roll-grant".to_owned();
        grant.scope.permissions.rolls = rolls;
        grant.signal.min_conviction = 0.5;
        grant.signal.min_activity_score = 0.5;
    })
}

fn evaluate_roll(rolls: bool, kill_switch: bool, max_contracts: u32) -> Split {
    let proposal = proposal("2", ProposalSide::Roll);
    let mut grant = roll_grant(rolls);
    grant.budgets.max_contracts = max_contracts;
    let ledger = GrantLedger::new(grant);
    let pipeline = DecisionPipeline::new(StateHandle::new(TradingState::Enabled));
    let state = health(false);
    let limits = HealthLimits::new().build();
    let risk = RiskEngine::new(RiskLimits::new().kill_switch(kill_switch).build());
    let risk_state = risk_state();
    let market = GrantMarket::new()
        .quote_age_secs(0)
        .spread_width(money("1"))
        .build();
    let verdict = pipeline.evaluate(&PipelineRequest {
        proposal: &proposal,
        shape: OrderShape::new(true, true),
        is_exit: false,
        health: Some((&state, &limits)),
        risk_state: &risk_state,
        market: &market,
        now: now(),
        risk: &risk,
        ledger: &ledger,
    });
    Split {
        allowed: verdict.allowed,
        axis: verdict.axis,
        reasons: verdict.reasons,
        risk_pass: verdict.risk_pass,
        risk_reasons: verdict.risk_reasons,
        approval: verdict.approval.is_some(),
        merit: verdict.grant_merit,
        merit_reasons: verdict.grant_merit_reasons,
        authorized: verdict.grant_authorized,
        authorization_reasons: verdict.grant_authorization_reasons,
        reserved: verdict.reservation.is_some(),
    }
}

#[test]
fn a_roll_is_denied_without_the_roll_permission_and_allowed_with_it() {
    // Without the permission the grant denies on scope — while the risk
    // engine still passes, so the denial is the grant's, not the engine's.
    let denied = evaluate_roll(false, false, 10_000);
    assert!(!denied.allowed);
    assert_eq!(denied.axis, Some(GateAxis::TaskValue));
    assert!(
        denied.reasons.iter().any(|reason| reason.contains("scope")),
        "{:?}",
        denied.reasons
    );
    assert!(denied.risk_pass, "the risk engine is consulted either way");
    // With the permission the same roll authorizes and reserves budget.
    let allowed = evaluate_roll(true, false, 10_000);
    assert!(allowed.allowed, "{:?}", allowed.reasons);
    assert!(allowed.risk_pass);
    assert!(allowed.approval);
    assert!(allowed.authorized);
    assert!(allowed.reserved);
    // A killed engine fails the roll even with the permission: risk still
    // judges rolls, and the verdict records both refusals' split.
    let risky = evaluate_roll(true, true, 10_000);
    assert!(!risky.allowed);
    assert!(
        !risky.risk_pass,
        "risk refusal recorded: {:?}",
        risky.risk_reasons
    );
    assert!(
        risky.merit.is_some_and(|decision| decision.is_allow()),
        "merit stays recorded even when risk refuses"
    );
}

#[test]
fn a_roll_past_the_budgets_is_denied_with_the_grant_own_reason() {
    // Two contracts against a one-contract budget: the grant's own contract
    // budget reason, with the risk engine still consulted.
    let split = evaluate_roll(true, false, 1);
    assert!(!split.allowed);
    assert_eq!(split.axis, Some(GateAxis::TaskValue));
    assert!(
        split
            .authorization_reasons
            .iter()
            .any(|reason| reason.contains("contract budget exhausted")),
        "{:?}",
        split.authorization_reasons
    );
    assert!(split.risk_pass, "risk still judges a budget-denied roll");
}

// ── Portfolio-drawdown breaker: disarm entries, never merit ────────────────

/// A grant with only the drawdown knob set: the shared fixture's shape plus
/// a configured cap. The fixture baseline leaves the arm inert.
fn drawdown_grant(cap: f64) -> Grant {
    grant_with(|grant| {
        grant.id = "drawdown-grant".to_owned();
        grant.signal.min_conviction = 0.5;
        grant.signal.min_activity_score = 0.5;
        grant.breakers.max_portfolio_drawdown_fraction = Some(cap);
    })
}

fn equity_facts(peak: &str, now: &str) -> PortfolioFacts {
    PortfolioFacts::observed(money("0"), None, None, None)
        .with_equity(Some(money(peak)), Some(money(now)))
}

fn evaluate_drawdown(cap: f64, facts: &PortfolioFacts, is_exit: bool) -> Split {
    let proposal = proposal("2", ProposalSide::Entry);
    let ledger = GrantLedger::new(drawdown_grant(cap));
    let pipeline = DecisionPipeline::new(StateHandle::new(TradingState::Enabled));
    let state = health(false);
    let limits = HealthLimits::new().build();
    let risk = RiskEngine::new(RiskLimits::new().build());
    let risk_state = RiskState {
        quote_age_secs: 0,
        clock_skew_secs: 1,
        reconciliation_healthy: true,
        facts: *facts,
    };
    let market = GrantMarket::new()
        .quote_age_secs(0)
        .spread_width(money("1"))
        .build();
    let verdict = pipeline.evaluate(&PipelineRequest {
        proposal: &proposal,
        shape: OrderShape::new(true, true),
        is_exit,
        health: Some((&state, &limits)),
        risk_state: &risk_state,
        market: &market,
        now: now(),
        risk: &risk,
        ledger: &ledger,
    });
    Split {
        allowed: verdict.allowed,
        axis: verdict.axis,
        reasons: verdict.reasons,
        risk_pass: verdict.risk_pass,
        risk_reasons: verdict.risk_reasons,
        approval: verdict.approval.is_some(),
        merit: verdict.grant_merit,
        merit_reasons: verdict.grant_merit_reasons,
        authorized: verdict.grant_authorized,
        authorization_reasons: verdict.grant_authorization_reasons,
        reserved: verdict.reservation.is_some(),
    }
}

#[test]
fn a_drawdown_past_the_cap_disarms_entries_but_not_exits_or_merit() {
    // (10000 − 8999) / 10000 = 0.1001: one basis point past a tenth.
    let facts = equity_facts("10000", "8999");
    // Entries disarm on the producer-confidence axis with the breaker's own
    // reason — while merit still records Allow for the otherwise-fine trade.
    let entry = evaluate_drawdown(0.10, &facts, false);
    assert!(!entry.allowed);
    assert_eq!(entry.axis, Some(GateAxis::ProducerConfidence));
    assert!(
        entry
            .authorization_reasons
            .iter()
            .any(|reason| reason.contains("drawdown")),
        "{:?}",
        entry.authorization_reasons
    );
    assert!(
        entry.risk_pass,
        "risk knows no drawdown cap: {:?}",
        entry.risk_reasons
    );
    assert!(
        matches!(&entry.merit, Some(decision) if decision.is_allow()),
        "a tripped breaker must not rewrite merit: {:?}",
        entry.merit
    );
    assert!(!entry.authorized);
    assert!(!entry.reserved);
    // Exits still route: the book must be closable in a drawdown.
    let exit = evaluate_drawdown(0.10, &facts, true);
    assert!(exit.allowed, "{:?}", exit.reasons);
    assert!(exit.authorized);
    assert!(exit.reserved);
}

#[test]
fn a_drawdown_at_the_cap_holds_and_an_unobserved_book_refuses() {
    // (1000 − 750) / 1000 = 0.25 exactly: the boundary holds.
    let edge = equity_facts("1000", "750");
    let held = evaluate_drawdown(0.25, &edge, false);
    assert!(held.allowed, "{:?}", held.reasons);
    assert!(held.authorized);
    // A configured cap with no equity refuses as unavailable — never a pass.
    let dark = evaluate_drawdown(0.25, &PortfolioFacts::unobserved(), false);
    assert!(!dark.allowed);
    assert_eq!(dark.axis, Some(GateAxis::ProducerConfidence));
    assert!(
        dark.reasons
            .iter()
            .any(|reason| reason.contains("PortfolioFactsUnavailable")),
        "{:?}",
        dark.reasons
    );
}

// ── Re-attestation: authorization-only, never merit ─────────────────────────

/// A grant with a daily cadence and an hour of grace, attested `ago_secs`
/// before `now()`.
/// A grant with a daily cadence and an hour of grace, live for the
/// test instant (attestation lapses against `now`, not the fixture window).
/// The window opens 30 h back so even an untouched usage is already lapsed —
/// the suite then moves attestation forward from there.
fn attested_grant() -> Grant {
    let mut grant = grant_with(|grant| {
        grant.id = "attested-grant".to_owned();
        grant.signal.min_conviction = 0.5;
        grant.signal.min_activity_score = 0.5;
        grant.re_attestation = Some(ReAttestationWindow {
            cadence: std::time::Duration::from_hours(24),
            grace: std::time::Duration::from_hours(1),
        });
    });
    grant.valid_from = now() - chrono::Duration::hours(30);
    grant
}

fn evaluate_attested(ago_secs: i64, is_exit: bool) -> Split {
    let proposal = proposal(
        "2",
        if is_exit {
            ProposalSide::Exit
        } else {
            ProposalSide::Entry
        },
    );
    let ledger = GrantLedger::new(attested_grant());
    // Seed the ledger the way the loop does after replaying attest rows:
    // last attested `ago_secs` before now, budgets untouched.
    ledger.observe_attested(now() - chrono::Duration::seconds(ago_secs));
    let pipeline = DecisionPipeline::new(StateHandle::new(TradingState::Enabled));
    let state = health(false);
    let limits = HealthLimits::new().build();
    let risk = RiskEngine::new(RiskLimits::new().build());
    let risk_state = risk_state();
    let market = GrantMarket::new()
        .quote_age_secs(0)
        .spread_width(money("1"))
        .build();
    let verdict = pipeline.evaluate(&PipelineRequest {
        proposal: &proposal,
        shape: OrderShape::new(true, true),
        is_exit,
        health: Some((&state, &limits)),
        risk_state: &risk_state,
        market: &market,
        now: now(),
        risk: &risk,
        ledger: &ledger,
    });
    Split {
        allowed: verdict.allowed,
        axis: verdict.axis,
        reasons: verdict.reasons,
        risk_pass: verdict.risk_pass,
        risk_reasons: verdict.risk_reasons,
        approval: verdict.approval.is_some(),
        merit: verdict.grant_merit,
        merit_reasons: verdict.grant_merit_reasons,
        authorized: verdict.grant_authorized,
        authorization_reasons: verdict.grant_authorization_reasons,
        reserved: verdict.reservation.is_some(),
    }
}

#[test]
fn a_lapsed_attestation_denies_entries_on_authorization_only() {
    // Attested 26 h ago against a 24 h cadence + 1 h grace: lapsed.
    let entry = evaluate_attested(26 * 3600, false);
    assert!(!entry.allowed);
    assert!(entry.risk_pass, "risk judges the trade, not the cadence");
    assert!(
        matches!(&entry.merit, Some(decision) if decision.is_allow()),
        "merit stays recorded for the otherwise-fine trade: {:?}",
        entry.merit
    );
    assert!(
        entry.merit_reasons.is_empty(),
        "NotAttested never lands in merit reasons: {:?}",
        entry.merit_reasons
    );
    assert!(
        entry
            .authorization_reasons
            .iter()
            .any(|reason| reason.contains("attestation")),
        "{:?}",
        entry.authorization_reasons
    );
    assert!(!entry.authorized);
    assert!(!entry.reserved);
    // Exits still route under the same lapsed grant.
    let exit = evaluate_attested(26 * 3600, true);
    assert!(exit.allowed, "{:?}", exit.reasons);
    assert!(exit.authorized);
}

#[test]
fn an_attestation_inside_cadence_plus_grace_allows_entries() {
    let entry = evaluate_attested(3600, false);
    assert!(entry.allowed, "{:?}", entry.reasons);
    assert!(entry.authorized);
    assert!(entry.reserved);
}
