use super::*;
use crate::grant::{
    BreakerLimits, GrantBudgets, GrantMarket, GrantPermission, GrantScope, PortfolioThresholds,
    SignalThresholds, TradeThresholds,
};
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
    Grant::new()
        .id("g1".to_owned())
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
                .max_trades(max_trades)
                .max_contracts(max_contracts)
                .max_loss(money(5_000))
                .max_notional(money(100_000))
                .build(),
        )
        .signal(
            SignalThresholds::new()
                .min_conviction(0.5)
                .min_activity_score(0.5)
                .build(),
        )
        .trade(
            TradeThresholds::new()
                .max_loss_per_trade(money(1_000))
                .max_spread_width(money(50))
                .min_dte_days(7)
                .max_dte_days(60)
                .build(),
        )
        .portfolio(
            PortfolioThresholds::new()
                .max_open_risk(money(10_000))
                .quote_max_age_secs(60)
                .build(),
        )
        .breakers(BreakerLimits::new().max_consecutive_errors(2).build())
        .valid_from(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap())
        .valid_until(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap())
        .build()
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
    assert_eq!(ledger.pending(), GrantUsage::new().build());
    assert_eq!(ledger.usage(), GrantUsage::new().build());
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
