use super::*;
use crate::grant::{GrantBudgets, GrantMarket};
use crate::proposal::{Direction, Proposal, ProposalSide, ProposalState, StructureKind};
use chrono::{TimeZone, Utc};
use protopyne_types::{Money, Qty};
use rust_decimal::Decimal;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

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
        .state(ProposalState::Proposed)
        .created_at(now())
        .build()
}

fn grant(max_trades: u32, max_contracts: u32) -> Grant {
    // The shared fixture's shape with the ledger suite's budgets (the two knobs
    // this suite sweeps) and its two-error breaker.
    crate::fixtures::grant_with(|grant| {
        grant.id = "g1".to_owned();
        grant.budgets = GrantBudgets::new()
            .max_trades(max_trades)
            .max_contracts(max_contracts)
            .max_loss(money(5_000))
            .max_notional(money(100_000))
            .build();
        grant.signal.min_conviction = 0.5;
        grant.signal.min_activity_score = 0.5;
        grant.trade.min_dte_days = 7;
        grant.trade.max_dte_days = 60;
        grant.portfolio.max_open_risk = money(10_000);
        grant.breakers.max_consecutive_errors = 2;
        grant.valid_from = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    })
}

fn market() -> GrantMarket {
    GrantMarket::new()
        .quote_age_secs(5)
        .spread_width(money(10))
        .build()
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
}

fn filled() -> FillOutcome {
    FillOutcome {
        status: FillStatus::Filled,
        contracts: 2,
        loss: money(300),
        notional: money(1_000),
    }
}

#[test]
fn accounting_identity_holds_through_reserve_settle_and_drop() {
    let ledger = GrantLedger::new(grant(3, 100));
    assert_eq!(ledger.usage().used_trades + ledger.pending().used_trades, 0);

    let payroll = proposal();
    let first = ledger
        .reserve(&payroll, &market(), now())
        .expect("first reserves");
    assert_eq!(ledger.pending().used_trades, 1);
    assert_eq!(ledger.usage().used_trades, 0);
    assert!(matches!(first.settle(&filled()), Settled::Consumed));
    assert_eq!(ledger.pending().used_trades, 0);
    assert_eq!(ledger.usage().used_trades, 1);

    let reservation = ledger.reserve(&payroll, &market(), now());
    drop(reservation);
    assert_eq!(ledger.pending().used_trades, 0, "the drop released it");

    let reservation = ledger.reserve(&payroll, &market(), now()).expect("reserve");
    reservation.settle(&filled());
    assert_eq!(ledger.usage().used_trades, 2);
    assert_eq!(ledger.pending().used_trades, 0);
    // Used + pending never exceeds the cap over any operation sequence.
    assert!(ledger.usage().used_trades + ledger.pending().used_trades <= 3);
}

#[test]
fn an_operator_close_fill_books_without_a_prior_reservation() {
    // Operator closes are exits: no grant merit gates them, so there is no
    // claim to settle — but the fill still consumes the lifetime budgets
    // (trades, contracts, notional) exactly like a settled entry fill, and
    // commits no new loss (the position's loss was booked at entry).
    let ledger = GrantLedger::new(grant(3, 100));
    let settled = ledger.record_operator_fill(&FillOutcome {
        status: FillStatus::Filled,
        contracts: 2,
        loss: Money::zero(),
        notional: money(1_000),
    });
    assert_eq!(settled, Settled::Consumed);
    assert_eq!(ledger.pending(), GrantUsage::new().build());
    let usage = ledger.usage();
    assert_eq!(usage.used_trades, 1);
    assert_eq!(usage.used_contracts, 2);
    assert_eq!(usage.used_loss, Money::zero());
    assert_eq!(usage.used_notional, money(1_000));
}

#[test]
fn a_dropped_reservation_releases_its_budget() {
    let ledger = GrantLedger::new(grant(3, 100));
    let reservation = ledger
        .reserve(&proposal(), &market(), now())
        .expect("reserve");
    drop(reservation);
    // Attestation is not a budget: initializing it at valid_from and
    // dropping the reservation both leave the pending tally empty.
    let mut empty = GrantUsage::new().build();
    empty.last_attested = Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
    assert_eq!(ledger.pending(), GrantUsage::new().build());
    assert_eq!(ledger.usage(), empty);
}

#[test]
fn a_permanent_refusal_consumes_only_the_attempted_trade() {
    let ledger = GrantLedger::new(grant(3, 100));
    let reservation = ledger
        .reserve(&proposal(), &market(), now())
        .expect("reserve");
    let settled = reservation.settle(&FillOutcome {
        status: FillStatus::Refused,
        contracts: 2,
        loss: money(300),
        notional: money(1_000),
    });
    assert_eq!(settled, Settled::Consumed);
    assert_eq!(ledger.usage().used_trades, 1);
    assert_eq!(ledger.usage().used_contracts, 0);
    assert_eq!(ledger.usage().used_loss, Money::zero());
}

#[test]
fn a_retryable_failure_retains_the_reservation_until_resolved() {
    let ledger = GrantLedger::new(grant(3, 100));
    let reservation = ledger
        .reserve(&proposal(), &market(), now())
        .expect("reserve");
    let settled = reservation.settle(&FillOutcome {
        status: FillStatus::Retryable,
        contracts: 0,
        loss: Money::zero(),
        notional: Money::zero(),
    });
    assert_eq!(settled, Settled::Retained);
    assert_eq!(ledger.usage().used_trades, 0);
    assert_eq!(ledger.pending().used_trades, 1, "budget stays reserved");
}

#[test]
fn two_concurrent_reservations_cannot_both_take_the_last_trade() {
    let ledger = Arc::new(GrantLedger::new(grant(1, 100)));
    let granted = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(std::sync::Barrier::new(2));
    std::thread::scope(|scope| {
        for _ in 0..2 {
            let ledger = Arc::clone(&ledger);
            let granted = Arc::clone(&granted);
            let barrier = Arc::clone(&barrier);
            scope.spawn(move || {
                barrier.wait();
                let reservation = ledger.reserve(&proposal(), &market(), now());
                if reservation.is_ok() {
                    granted.fetch_add(1, Ordering::SeqCst);
                }
                // Hold the reservation until both have attempted, so the
                // pending claim is live across the race.
                barrier.wait();
            });
        }
    });
    assert_eq!(granted.load(Ordering::SeqCst), 1, "exactly one wins");
}

#[test]
fn adopt_is_tighten_only() {
    let ledger = GrantLedger::new(grant(10, 100));
    assert_eq!(
        ledger.adopt(grant(20, 100)),
        Err(LedgerRefusal::NotTightening)
    );
    assert!(ledger.adopt(grant(5, 100)).is_ok());
    assert_eq!(ledger.grant().budgets.max_trades, 5);
}

#[test]
fn breaker_streaks_live_in_the_ledger() {
    let ledger = GrantLedger::new(grant(10, 100));
    ledger.observe_error(true);
    ledger.observe_error(false);
    assert_eq!(
        ledger.breakers().consecutive_errors,
        1,
        "only retries count"
    );
    ledger.observe_error(true);
    ledger.observe_error(true);
    assert!(ledger.breakers().tripped);

    let fresh = GrantLedger::new(grant(10, 100));
    fresh.observe_error(true);
    fresh.observe_success();
    assert_eq!(fresh.breakers().consecutive_errors, 0);
}

// ── Portfolio-drawdown breaker ─────────────────────────────────────────────

/// A grant with only the drawdown knob set: the fixture baseline plus a
/// configured cap. `None` (the baseline) leaves the arm inert.
fn capped_grant(cap: Option<f64>) -> Grant {
    crate::fixtures::grant_with(|grant| {
        grant.id = "drawdown-grant".to_owned();
        grant.breakers.max_portfolio_drawdown_fraction = cap;
    })
}

fn equity_facts(peak: i64, now: i64) -> PortfolioFacts {
    PortfolioFacts::observed(money(0), None, None, None)
        .with_equity(Some(money(peak)), Some(money(now)))
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

// Control: a thousand rising equity curves never trip the configured arm.
#[test]
fn a_thousand_rising_curves_never_trip_the_drawdown_arm() {
    let ledger = GrantLedger::new(capped_grant(Some(0.10)));
    let mut lcg = Lcg(0xd9a0);
    for _ in 0..1_000 {
        // A rising curve: start at 1,000, climb 0–1% a step for 20–60 steps.
        let mut value = 1_000i64;
        let mut peak = value;
        for _ in 0..20 + (lcg.next() % 40) {
            value += (lcg.next() % 11) as i64;
            peak = peak.max(value);
        }
        let facts = PortfolioFacts::observed(money(0), None, None, None)
            .with_equity(Some(money(peak)), Some(money(value)));
        assert!(
            matches!(ledger.observe_drawdown(&facts), DrawdownStatus::Within(_)),
            "a rising curve at its peak is within, never breached"
        );
    }
    assert!(
        !ledger.breakers().tripped,
        "a thousand quiet curves bank no trip"
    );
}

// Boundary: drawdown exactly equal to the cap holds (strictly-past rule).
#[test]
fn a_drawdown_exactly_at_the_cap_holds() {
    let ledger = GrantLedger::new(capped_grant(Some(0.25)));
    // (1000 − 750) / 1000 = 0.25 exactly — quarters are exact in binary and
    // decimal alike, so this equality is honest, not a representation gap.
    let status = ledger.observe_drawdown(&equity_facts(1000, 750));
    assert!(
        matches!(status, DrawdownStatus::Within(f) if f == rust_decimal::Decimal::new(25, 2)),
        "exactly at the cap is within: {status:?}"
    );
    assert!(!ledger.breakers().tripped);
}

// Trigger: one basis point past trips, latches, and banks the reason once.
#[test]
fn a_drawdown_past_the_cap_trips_and_banks_the_reason_once() {
    let ledger = GrantLedger::new(capped_grant(Some(0.10)));
    // (10000 − 8999) / 10000 = 0.1001: one basis point past a tenth.
    let breached = ledger.observe_drawdown(&equity_facts(10_000, 8999));
    assert!(
        matches!(&breached, DrawdownStatus::Breached(_)),
        "one basis point past must breach: {breached:?}"
    );
    assert!(ledger.breakers().tripped);
    let reason = ledger
        .breakers()
        .trip_reason
        .expect("a trip banks its reason");
    assert!(
        reason.contains("drawdown"),
        "the reason names the trip: {reason}"
    );
    // A second breach does not overwrite the banked reason.
    let _ = ledger.observe_drawdown(&equity_facts(10_000, 8000));
    assert_eq!(ledger.breakers().trip_reason.expect("still banked"), reason);
}

// Unobserved: a configured cap with no equity refuses, never passes, and
// never latches.
#[test]
fn unobserved_equity_refuses_without_latching() {
    let ledger = GrantLedger::new(capped_grant(Some(0.10)));
    for facts in [
        PortfolioFacts::unobserved(),
        PortfolioFacts::observed(money(0), None, None, None),
        PortfolioFacts::observed(money(0), None, None, None).with_equity(Some(money(1000)), None),
        PortfolioFacts::observed(money(0), None, None, None)
            .with_equity(Some(money(0)), Some(money(0))),
    ] {
        assert!(
            matches!(ledger.observe_drawdown(&facts), DrawdownStatus::Unobserved),
            "missing equity is unobserved, never a pass"
        );
    }
    assert!(
        !ledger.breakers().tripped,
        "refusing as unobserved must not latch a trip"
    );
}

// Inert: without a configured cap even a halved book moves nothing.
#[test]
fn an_unconfigured_cap_is_inert() {
    let ledger = GrantLedger::new(crate::fixtures::grant());
    let status = ledger.observe_drawdown(&equity_facts(1000, 500));
    assert!(
        matches!(status, DrawdownStatus::Inert),
        "no cap, no arm: {status:?}"
    );
    assert!(!ledger.breakers().tripped);
}

// Garbage caps never trip: arming refuses them (see the grant suite), and
// the observer treats them as inert belt-and-braces.
#[test]
fn a_garbage_cap_never_trips() {
    for cap in [0.0, -0.1, 1.5, f64::NAN, f64::INFINITY] {
        let ledger = GrantLedger::new(capped_grant(Some(cap)));
        let status = ledger.observe_drawdown(&equity_facts(1000, 100));
        assert!(
            matches!(status, DrawdownStatus::Inert),
            "cap {cap} must never trip: {status:?}"
        );
        assert!(!ledger.breakers().tripped);
    }
}
