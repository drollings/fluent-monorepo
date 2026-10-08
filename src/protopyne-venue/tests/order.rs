use super::*;
use chrono::Utc;
use protopyne_types::Money;
use rust_decimal::Decimal;

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn intent() -> OrderIntent {
    OrderIntent::new()
        .client_id(client_id_for("prop-1", 0))
        .proposal_id("prop-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(2)
        .limit_price(money("600.50"))
        .build()
}

#[test]
fn client_ids_are_deterministic_per_attempt() {
    assert_eq!(client_id_for("p", 0), "p#0");
    assert_eq!(client_id_for("p", 1), "p#1");
    assert_ne!(client_id_for("p", 0), client_id_for("q", 0));
}

#[test]
fn only_live_states_count_as_active() {
    assert!(OrderStatus::Pending.is_active());
    assert!(OrderStatus::Submitted.is_active());
    assert!(OrderStatus::Acknowledged.is_active());
    assert!(!OrderStatus::Filled.is_active());
    assert!(!OrderStatus::Cancelled.is_active());
    assert!(!OrderStatus::Rejected.is_active());
}

#[test]
fn fills_keep_a_quantity_weighted_average() {
    let now = Utc::now();
    let mut record = OrderRecord::pending(intent(), now);
    record.apply_fill(
        Fill::new()
            .quantity(1)
            .price(money("600.00"))
            .fee(money("0.65"))
            .build(),
        now,
    );
    record.apply_fill(
        Fill::new()
            .quantity(1)
            .price(money("602.00"))
            .fee(money("0.65"))
            .build(),
        now,
    );
    assert_eq!(record.filled_quantity, 2);
    assert_eq!(record.avg_price, Some(money("601.00")));
    assert_eq!(record.fees_paid, money("1.30"));
}

#[test]
fn positions_flatten_to_zero() {
    let mut position = Position::new()
        .underlying("SPY".to_owned())
        .quantity(0)
        .avg_price(Money::zero())
        .build();
    position.apply(2, money("600.00"));
    assert_eq!(position.quantity, 2);
    position.apply(-2, money("601.00"));
    assert_eq!(position.quantity, 0);
}
