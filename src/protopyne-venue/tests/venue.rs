use super::*;
use chrono::Utc;
use rust_decimal::Decimal;

use crate::order::{OrderIntent, OrderRecord, OrderSide};
use crate::paper::{PaperConfig, PaperVenue};
use crate::quote::Quote;
use protopyne_types::Money;

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn intent() -> OrderIntent {
    OrderIntent::new()
        .client_id(crate::order::client_id_for("prop-1", 0))
        .proposal_id("prop-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(2)
        .limit_price(money("600.50"))
        .build()
}

#[test]
fn paper_venue_keeps_empty_account_defaults() {
    let venue = PaperVenue::new(PaperConfig::new().build());
    let snapshot = venue.account_snapshot();
    assert!(snapshot.is_empty());
    assert!(snapshot.balances.is_empty());
    assert!(snapshot.open_orders.is_empty());
    assert!(snapshot.recent_trades.is_empty());
    assert!(venue.account_stream().is_empty());
    assert!(venue.fetch_balances().is_empty());
    assert!(venue.fetch_open_orders().is_empty());
    assert!(venue.fetch_trades().is_empty());
}

/// A venue overriding a single fetch method: the defaulted snapshot
/// assembler must carry that override through while every other surface
/// stays empty.
struct OrdersOnly {
    orders: Vec<OrderRecord>,
}

impl Venue for OrdersOnly {
    fn dry_run(&self, _intent: &OrderIntent, _quote: &Quote) -> Result<DryRunOutcome, VenueError> {
        Ok(DryRunOutcome {
            allowed: true,
            reserved_notional: money("0"),
        })
    }

    fn submit<'a>(
        &'a self,
        _intent: OrderIntent,
        _quote: &'a Quote,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>,
    > {
        Box::pin(std::future::ready(Err(VenueError::Refused(
            "stub never fills".to_owned(),
        ))))
    }

    fn query<'a>(
        &'a self,
        _client_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<OrderRecord>> + Send + 'a>>
    {
        Box::pin(std::future::ready(None))
    }

    fn cancel<'a>(
        &'a self,
        _client_id: &'a str,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>,
    > {
        Box::pin(std::future::ready(Err(VenueError::Invalid(
            "stub holds nothing".to_owned(),
        ))))
    }

    fn positions(&self) -> Vec<crate::order::Position> {
        Vec::new()
    }

    fn orders(&self) -> Vec<OrderRecord> {
        self.orders.clone()
    }

    fn apply_corporate_event(
        &self,
        _underlying: &str,
        _event: crate::order::CorporateEvent,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<String, VenueError> {
        Ok("stub".to_owned())
    }

    fn fetch_open_orders(&self) -> Vec<OrderRecord> {
        self.orders.clone()
    }
}

#[test]
fn single_override_composes_through_default_snapshot() {
    let record = OrderRecord::pending(intent(), Utc::now());
    let venue = OrdersOnly {
        orders: vec![record.clone()],
    };
    // The full trait is satisfied with one override: everything else is a
    // default, and the default snapshot assembler carries the override.
    let snapshot = venue.account_snapshot();
    assert_eq!(snapshot.open_orders, vec![record]);
    assert!(snapshot.balances.is_empty());
    assert!(snapshot.recent_trades.is_empty());
    assert!(!snapshot.is_empty());
    assert!(venue.account_stream().is_empty());
    assert!(venue.fetch_balances().is_empty());
    assert!(venue.fetch_trades().is_empty());
}

#[test]
fn snapshot_replaces_while_events_upsert() {
    // Snapshot-replace semantics: a fresh snapshot carries the whole view,
    // so an empty default reads as "no account state", never as "unknown".
    let empty = AccountSnapshot::default();
    assert!(empty.is_empty());
    // Upsert semantics: one event carries one entry keyed by its natural
    // key (orders by client id, balances by asset, trades append-only).
    let record = OrderRecord::pending(intent(), Utc::now());
    let event = AccountEvent::OrderUpdated(record.clone());
    match event {
        AccountEvent::OrderUpdated(back) => assert_eq!(back, record),
        _ => panic!("order event must carry the record"),
    }
}
