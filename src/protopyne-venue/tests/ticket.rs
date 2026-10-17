//! Manual-ticket workflow: exact human tickets with confirmation + drift.
//!
//! Protocol: tickets are `OrderIntent`s the operator places by hand — the
//! platform never submits them. Issue tracks them as unconfirmed; the
//! operator's fill report confirms them; anything the fill disagrees on
//! flags drift. A failing read-only import changes nothing.

use super::*;
use crate::journal::{Journal, VenueStorage};
use crate::order::{OrderIntent, OrderSide};
use chrono::{TimeZone, Utc};
use protopyne_types::{LegSide, Money, OptionContract, OptionKind, SpreadLeg, SpreadLegs};
use rust_decimal::Decimal;

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap()
}

fn intent(id: &str) -> OrderIntent {
    OrderIntent::new()
        .client_id(id.to_owned())
        .proposal_id("prop-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(2)
        .limit_price(money("600.50"))
        .build()
}

fn spread_intent(id: &str) -> OrderIntent {
    let legs = SpreadLegs::try_new(vec![
        SpreadLeg::new()
            .contract(
                OptionContract::new()
                    .underlying("SPY".to_owned())
                    .expiry(chrono::NaiveDate::from_ymd_opt(2026, 1, 16).expect("date"))
                    .strike(money("600"))
                    .kind(OptionKind::Call)
                    .build(),
            )
            .side(LegSide::Long)
            .ratio(1)
            .build(),
        SpreadLeg::new()
            .contract(
                OptionContract::new()
                    .underlying("SPY".to_owned())
                    .expiry(chrono::NaiveDate::from_ymd_opt(2026, 1, 16).expect("date"))
                    .strike(money("610"))
                    .kind(OptionKind::Call)
                    .build(),
            )
            .side(LegSide::Short)
            .ratio(1)
            .build(),
    ])
    .expect("defined-risk vertical");
    OrderIntent::new()
        .client_id(id.to_owned())
        .proposal_id("prop-2".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(1)
        .limit_price(money("2.50"))
        .legs(legs)
        .build()
}

/// The ticket text is the whole order: side, quantity, underlying, limit,
/// good-until, and every leg by OCC symbol. A ticket missing any of these
/// is not exact.
#[test]
fn ticket_renders_the_exact_human_ticket() {
    let ticket = ManualTicket::from_intent(intent("rh-1"), now());
    let text = ticket.render();
    for part in ["buy", "2", "SPY", "600.50", "day", "rh-1"] {
        assert!(text.contains(part), "ticket names {part}, got: {text}");
    }

    let spread = ManualTicket::from_intent(spread_intent("rh-2"), now());
    let text = spread.render();
    for occ in ["SPY   260116C00600000", "SPY   260116C00610000"] {
        assert!(text.contains(occ), "ticket lists leg {occ}, got: {text}");
    }
    assert!(text.contains("long") && text.contains("short"), "ticket sides each leg, got: {text}");
}

/// Issued tickets wait as unconfirmed; a matching fill confirms cleanly and
/// leaves the other ticket waiting.
#[test]
fn unconfirmed_tracking_issue_then_confirm() {
    let mut tracker = TicketTracker::new();
    tracker.issue(intent("rh-1"), now()).expect("issue");
    tracker.issue(intent("rh-2"), now()).expect("issue");
    assert_eq!(tracker.unconfirmed().len(), 2);

    let drift = tracker
        .confirm("rh-1", ConfirmedFill { quantity: 2, price: money("600.40") }, now())
        .expect("confirm");
    assert!(drift.is_none(), "a matching fill reports no drift");
    assert_eq!(tracker.unconfirmed().len(), 1);
    assert_eq!(tracker.unconfirmed()[0].ticket_id(), "rh-2");
    assert!(tracker.drifts().is_empty());
}

/// A fill for fewer (or more) contracts than the ticket flags drift — but
/// the ticket still confirms, so it never waits twice.
#[test]
fn quantity_mismatch_flags_drift() {
    let mut tracker = TicketTracker::new();
    tracker.issue(intent("rh-1"), now()).expect("issue");
    let drift = tracker
        .confirm("rh-1", ConfirmedFill { quantity: 1, price: money("600.40") }, now())
        .expect("confirm still lands");
    let drift = drift.expect("short fill flags drift");
    assert_eq!(drift.kind, DriftKind::QuantityMismatch);
    assert!(tracker.unconfirmed().is_empty(), "confirmed tickets never wait twice");
    assert_eq!(tracker.drifts().len(), 1);
}

/// Paying worse than the ticket limit flags drift; fills at or better than
/// the limit confirm cleanly on both sides of the book.
#[test]
fn price_outside_limit_flags_drift() {
    let mut tracker = TicketTracker::new();
    tracker.issue(intent("rh-buy"), now()).expect("issue");
    let drift = tracker
        .confirm("rh-buy", ConfirmedFill { quantity: 2, price: money("600.60") }, now())
        .expect("confirm lands");
    assert!(
        drift.is_some_and(|drift| drift.kind == DriftKind::PriceOutsideLimit),
        "a buy above the limit flags drift"
    );

    let mut tracker = TicketTracker::new();
    let mut sell = intent("rh-sell");
    sell.side = OrderSide::Sell;
    tracker.issue(sell, now()).expect("issue");
    let drift = tracker
        .confirm("rh-sell", ConfirmedFill { quantity: 2, price: money("600.40") }, now())
        .expect("confirm lands");
    assert!(
        drift.is_some_and(|drift| drift.kind == DriftKind::PriceOutsideLimit),
        "a sell below the limit flags drift"
    );

    let mut tracker = TicketTracker::new();
    tracker.issue(intent("rh-even"), now()).expect("issue");
    let drift = tracker
        .confirm("rh-even", ConfirmedFill { quantity: 2, price: money("600.50") }, now())
        .expect("confirm");
    assert!(drift.is_none(), "a fill at the limit is clean");
}

/// Confirming a ticket that was never issued refuses fail-closed: nothing
/// confirms, nothing drifts.
#[test]
fn unknown_confirm_refuses() {
    let mut tracker = TicketTracker::new();
    tracker.issue(intent("rh-1"), now()).expect("issue");
    tracker
        .confirm("rh-ghost", ConfirmedFill { quantity: 2, price: money("600.40") }, now())
        .expect_err("unknown tickets refuse");
    assert_eq!(tracker.unconfirmed().len(), 1);
    assert!(tracker.drifts().is_empty());
}

/// Issuing the same ticket twice refuses: one ticket, one wait.
#[test]
fn duplicate_issue_refuses() {
    let mut tracker = TicketTracker::new();
    tracker.issue(intent("rh-1"), now()).expect("issue");
    tracker.issue(intent("rh-1"), now()).expect_err("double issue refuses");
    assert_eq!(tracker.unconfirmed().len(), 1);
}

/// Journal-first issue/confirm write rows a restart replays: the confirmed
/// ticket is gone, the waiting one survives. Ticket rows never read as
/// orders in the monitor view.
#[tokio::test]
async fn journaled_issue_and_confirm_replay() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let journal = Journal::open(&path).expect("open");
    let permit = VenueStorage::new(path.clone());

    let caps = fluent_wvr::CapabilitySet::new().with(permit.clone());
    let events = fluent_wvr::CURRENT_CAPS
        .scope(
            caps,
            async {
                let mut tracker = TicketTracker::new();
                tracker
                    .issue_journaled(&journal, &permit, intent("rh-1"), now())
                    .expect("issue");
                tracker
                    .issue_journaled(&journal, &permit, intent("rh-2"), now())
                    .expect("issue");
                tracker
                    .confirm_journaled(
                        &journal,
                        &permit,
                        "rh-1",
                        ConfirmedFill { quantity: 2, price: money("600.40") },
                        now(),
                    )
                    .expect("confirm");
                journal.replay(&permit).expect("replay").events
            },
        )
        .await;
    let kinds: Vec<&str> = events.iter().map(|event| event.kind.as_str()).collect();
    assert!(
        kinds.contains(&TICKET_ISSUED_KIND) && kinds.contains(&TICKET_CONFIRMED_KIND),
        "both ticket rows journal, got: {kinds:?}"
    );

    let rebuilt = TicketTracker::replay(&events);
    assert_eq!(rebuilt.unconfirmed().len(), 1);
    assert_eq!(rebuilt.unconfirmed()[0].ticket_id(), "rh-2");

    let view = crate::command::StateReplica::apply(&events);
    assert!(view.orders.is_empty(), "ticket rows never read as orders: {:?}", view.orders);
    assert!(view.leg_fills.is_empty());
}

/// The read-only import is failure-tolerant: a dark adapter changes
/// nothing — open tickets, drifts, and the last good snapshot survive.
#[test]
fn failing_import_leaves_the_tracker_untouched() {
    struct Dark;
    impl TicketRead for Dark {
        fn snapshot(&self) -> Result<Vec<ImportedPosition>, crate::VenueError> {
            Err(crate::VenueError::Transport("robinhood dark: scripted".to_owned()))
        }
    }

    let mut tracker = TicketTracker::new();
    tracker.issue(intent("rh-1"), now()).expect("issue");
    tracker
        .apply_import(&Serving(vec![ImportedPosition { underlying: "SPY".to_owned(), quantity: 0 }]))
        .expect("good import lands");
    assert_eq!(tracker.imported().len(), 1);

    tracker.apply_import(&Dark).expect_err("a dark import reports its failure");
    assert_eq!(tracker.unconfirmed().len(), 1, "open tickets survive a dark import");
    assert!(tracker.drifts().is_empty(), "a dark import invents no drift");
    assert_eq!(tracker.imported().len(), 1, "the last good snapshot survives");
}

/// A clean import agrees with the confirmed book; a position the human
/// never confirmed flags import drift without touching the ticket set.
#[test]
fn import_mismatch_flags_drift() {
    let mut tracker = TicketTracker::new();
    tracker.issue(intent("rh-1"), now()).expect("issue");
    tracker
        .confirm("rh-1", ConfirmedFill { quantity: 2, price: money("600.40") }, now())
        .expect("confirm");

    tracker
        .apply_import(&Serving(vec![ImportedPosition { underlying: "SPY".to_owned(), quantity: 2 }]))
        .expect("agreement lands");
    assert!(tracker.drifts().is_empty(), "an agreeing import stays silent");

    tracker
        .apply_import(&Serving(vec![ImportedPosition { underlying: "SPY".to_owned(), quantity: 5 }]))
        .expect("disagreement lands");
    assert!(
        tracker.drifts().iter().any(|drift| drift.kind == DriftKind::ImportMismatch),
        "an unconfirmed position flags drift, got: {:?}",
        tracker.drifts()
    );
    assert!(tracker.unconfirmed().is_empty(), "imports never reopen confirmed tickets");
}

/// Scripted read-only rung: serves canned positions, never mutates.
struct Serving(Vec<ImportedPosition>);

impl TicketRead for Serving {
    fn snapshot(&self) -> Result<Vec<ImportedPosition>, crate::VenueError> {
        Ok(self.0.clone())
    }
}
