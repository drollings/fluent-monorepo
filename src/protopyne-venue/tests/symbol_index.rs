use super::*;
use crate::order::{CorporateEvent, OrderIntent, OrderSide};
use crate::venue::Venue;
use chrono::{TimeZone, Utc};
use protopyne_types::Money;
use rust_decimal::Decimal;

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn quote() -> Quote {
    Quote::new()
        .bid(money("600.00"))
        .ask(money("600.10"))
        .bid_size(50)
        .ask_size(50)
        .ts(Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap())
        .build()
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap()
}

fn intent(id: &str, underlying: &str, side: OrderSide, limit: &str, qty: u32) -> OrderIntent {
    OrderIntent::new()
        .client_id(id.to_owned())
        .proposal_id("prop-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying(underlying.to_owned())
        .side(side)
        .quantity(qty)
        .limit_price(money(limit))
        .build()
}

fn venue() -> PaperVenue {
    PaperVenue::new(PaperConfig::new().build())
}

fn sorted_positions(venue: &PaperVenue) -> Vec<(String, i64)> {
    let mut rows: Vec<(String, i64)> = venue
        .positions()
        .into_iter()
        .map(|position| (position.underlying, position.quantity))
        .collect();
    rows.sort();
    rows
}

#[tokio::test]
async fn identical_fill_sequences_resolve_identically() {
    // Journal-replay equality golden: two independently owned venues (hence
    // two independent symbol tables) replaying the same fill sequence must
    // report byte-identical books — determinism never depends on process
    // history.
    let first = venue();
    let second = venue();
    for venue in [&first, &second] {
        venue
            .submit(intent("b#0", "SPY", OrderSide::Buy, "600.50", 2), &quote(), now())
            .await
            .expect("buy fills");
        venue
            .submit(intent("s#0", "AAPL", OrderSide::Sell, "599.50", 1), &quote(), now())
            .await
            .expect("sell fills");
    }
    assert_eq!(sorted_positions(&first), sorted_positions(&second));
    assert_eq!(
        sorted_positions(&first),
        vec![("AAPL".to_owned(), -1), ("SPY".to_owned(), 2)]
    );
}

#[tokio::test]
async fn venues_are_isolated() {
    let first = venue();
    let second = venue();
    first
        .submit(intent("b#0", "SPY", OrderSide::Buy, "600.50", 2), &quote(), now())
        .await
        .expect("buy fills");
    assert_eq!(sorted_positions(&first), vec![("SPY".to_owned(), 2)]);
    assert!(
        second.positions().is_empty(),
        "no cross-talk between venue books"
    );
}

#[tokio::test]
async fn corporate_events_resolve_through_keys() {
    let venue = venue();
    venue
        .submit(intent("b#0", "SPY", OrderSide::Buy, "600.50", 2), &quote(), now())
        .await
        .expect("buy fills");
    venue
        .apply_corporate_event("SPY", CorporateEvent::Split(2), now())
        .expect("split applies");
    assert_eq!(sorted_positions(&venue), vec![("SPY".to_owned(), 4)]);
    venue
        .apply_corporate_event("SPY", CorporateEvent::Expiry, now())
        .expect("expiry applies");
    assert!(venue.positions().is_empty());
    // Unknown underlyings behave as before: expiry closes nothing, split
    // refuses with a reason.
    venue
        .apply_corporate_event("MSFT", CorporateEvent::Expiry, now())
        .expect("unknown expiry is a no-op");
    assert!(
        venue
            .apply_corporate_event("MSFT", CorporateEvent::Split(2), now())
            .is_err()
    );
}

#[test]
fn registry_round_trip_by_name() {
    let registry = PortfolioRegistry::new();
    assert!(registry.is_empty());
    let portfolio = protopyne_types::Portfolio::new()
        .id(protopyne_types::PortfolioId::new("p1"))
        .name("paper".to_owned())
        .build();
    registry.register(portfolio);
    assert_eq!(registry.len(), 1);
    assert!(registry.get("paper").is_some());
    assert!(registry.get("unknown").is_none(), "reads never insert");
    assert_eq!(registry.names(), vec!["paper".to_owned()]);
}
