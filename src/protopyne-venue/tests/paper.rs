use super::*;
use crate::order::{CorporateEvent, OrderIntent, OrderSide, OrderStatus};
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

fn intent(id: &str, side: OrderSide, limit: &str, qty: u32) -> OrderIntent {
    OrderIntent::new()
        .client_id(id.to_owned())
        .proposal_id("prop-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(side)
        .quantity(qty)
        .limit_price(money(limit))
        .build()
}

fn venue() -> PaperVenue {
    PaperVenue::new(PaperConfig::new().build())
}

#[tokio::test]
async fn marketable_orders_fill_inside_the_limit() {
    let venue = venue();
    let buy = venue
        .submit(intent("b#0", OrderSide::Buy, "600.50", 2), &quote(), now())
        .await
        .expect("buy fills");
    assert_eq!(buy.status, OrderStatus::Filled);
    assert_eq!(buy.filled_quantity, 2);
    let price = buy.avg_price.expect("priced").as_decimal();
    assert!(price >= money("600.10").as_decimal());
    assert!(price <= money("600.50").as_decimal());
    assert_eq!(buy.fees_paid, money("1.30"));

    let sell = venue
        .submit(intent("s#0", OrderSide::Sell, "599.50", 2), &quote(), now())
        .await
        .expect("sell fills");
    let price = sell.avg_price.expect("priced").as_decimal();
    assert!(price <= money("600.00").as_decimal());
    assert!(price >= money("599.50").as_decimal());

    let positions = venue.positions();
    assert_eq!(positions.len(), 1);
    assert_eq!(positions[0].quantity, 0);
}

#[tokio::test]
async fn refusals_name_their_reason() {
    let venue = venue();
    let resting = venue
        .submit(intent("r#0", OrderSide::Buy, "599.00", 2), &quote(), now())
        .await
        .expect_err("would rest");
    assert!(matches!(resting, VenueError::Refused(_)));

    let wide = Quote::new()
        .bid(money("600.00"))
        .ask(money("660.00"))
        .bid_size(50)
        .ask_size(50)
        .ts(quote().ts)
        .build();
    let err = venue
        .submit(intent("w#0", OrderSide::Buy, "665.00", 2), &wide, now())
        .await
        .expect_err("wide");
    assert!(matches!(err, VenueError::Refused(_)));

    let stale = Utc.with_ymd_and_hms(2026, 6, 1, 13, 0, 0).unwrap();
    let err = venue
        .submit(intent("t#0", OrderSide::Buy, "615.00", 2), &quote(), stale)
        .await
        .expect_err("stale");
    assert!(matches!(err, VenueError::Refused(_)));
}

#[tokio::test]
async fn resubmit_returns_the_record_never_a_second_order() {
    let venue = venue();
    let first = venue
        .submit(intent("d#0", OrderSide::Buy, "600.50", 2), &quote(), now())
        .await
        .expect("first");
    let second = venue
        .submit(intent("d#0", OrderSide::Buy, "600.50", 2), &quote(), now())
        .await
        .expect("idempotent");
    assert_eq!(first, second);
    assert_eq!(venue.orders().len(), 1);
    assert_eq!(venue.query("d#0").await, Some(first));
}

#[tokio::test]
async fn injected_faults_fire_once_in_order() {
    let venue = venue();
    venue.inject(Fault::DropConnection);
    venue.inject(Fault::Status500);
    let first = venue
        .submit(intent("f#0", OrderSide::Buy, "600.50", 2), &quote(), now())
        .await
        .expect_err("drop");
    assert!(matches!(first, VenueError::Transport(_)));
    let second = venue
        .submit(intent("f#0", OrderSide::Buy, "600.50", 2), &quote(), now())
        .await
        .expect_err("500");
    assert!(matches!(second, VenueError::Server(_)));
    assert_eq!(venue.pending_faults(), 0);
    venue
        .submit(intent("f#0", OrderSide::Buy, "600.50", 2), &quote(), now())
        .await
        .expect("recovers");
    assert_eq!(venue.orders().len(), 1);
}

#[tokio::test]
async fn lost_ack_still_fills_exactly_once() {
    let venue = venue();
    venue.inject(Fault::LoseAck);
    let lost = venue
        .submit(intent("l#0", OrderSide::Buy, "600.50", 2), &quote(), now())
        .await
        .expect_err("ack lost");
    assert!(matches!(lost, VenueError::Transport(_)));
    let found = venue.query("l#0").await.expect("query finds it");
    assert_eq!(found.status, OrderStatus::Filled);
    venue
        .submit(intent("l#0", OrderSide::Buy, "600.50", 2), &quote(), now())
        .await
        .expect("retry dedups");
    assert_eq!(venue.orders().len(), 1);
}

#[tokio::test]
async fn dry_run_checks_buying_power() {
    let venue = venue();
    let ok = venue
        .dry_run(&intent("g#0", OrderSide::Buy, "600.50", 2), &quote())
        .expect("affordable");
    assert!(ok.allowed);
    let err = venue
        .dry_run(&intent("h#0", OrderSide::Buy, "600.50", 100_000), &quote())
        .expect_err("too big");
    assert!(matches!(err, VenueError::Refused(_)));
}

#[tokio::test]
async fn cancel_and_corporate_events() {
    let venue = venue();
    let err = venue.cancel("nope", now()).await.expect_err("unknown");
    assert!(matches!(err, VenueError::Invalid(_)));

    venue
        .submit(intent("c#0", OrderSide::Buy, "600.50", 4), &quote(), now())
        .await
        .expect("buy");
    let note = venue
        .apply_corporate_event("SPY", CorporateEvent::Split(2), now())
        .expect("split");
    assert!(note.contains('2'));
    let positions = venue.positions();
    assert_eq!(positions[0].quantity, 8);

    venue
        .apply_corporate_event("SPY", CorporateEvent::Expiry, now())
        .expect("expiry");
    assert!(venue.positions().is_empty());
}

#[tokio::test]
async fn cancel_on_terminal_orders_is_refused() {
    // Cancelling a filled order is a caller error, never a silent no-op:
    // quiet success would let a dead-order retry loop believe it acted.
    let venue = venue();
    let record = venue
        .submit(intent("t#0", OrderSide::Buy, "600.50", 2), &quote(), now())
        .await
        .expect("buy fills");
    assert_eq!(record.status, OrderStatus::Filled);
    let err = venue.cancel("t#0", now()).await.expect_err("terminal cancel must refuse");
    assert!(matches!(err, VenueError::Invalid(_)));
    assert!(err.to_string().contains("terminal"), "refusal names the state, got: {err}");
}

#[tokio::test]
async fn split_zero_is_refused() {
    let venue = venue();
    venue
        .submit(intent("z#0", OrderSide::Buy, "600.50", 4), &quote(), now())
        .await
        .expect("buy");
    let err = venue
        .apply_corporate_event("SPY", CorporateEvent::Split(0), now())
        .expect_err("zero split factor must refuse, never panic or mistreat as no-op");
    assert!(matches!(err, VenueError::Invalid(_)));
    let positions = venue.positions();
    assert_eq!(positions.len(), 1);
    assert_eq!(positions[0].quantity, 4);
    assert_eq!(positions[0].avg_price, money("600.50"));
}
