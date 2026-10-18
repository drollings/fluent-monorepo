use super::*;
use chrono::{NaiveDate, Utc};
use protopyne_types::{LegSide, Money, OptionContract, OptionKind, SpreadLeg, SpreadLegs};
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

fn position_at(quantity: i64, basis: &str) -> Position {
    Position::new()
        .underlying("SPY".to_owned())
        .quantity(quantity)
        .avg_price(money(basis))
        .build()
}

#[test]
fn positions_flip_long_to_short_reanchors_basis() {
    let mut position = position_at(2, "600.00");
    position.apply(-5, money("610.00"));
    assert_eq!(position.quantity, -3);
    assert_eq!(position.avg_price, money("610.00"));
}

#[test]
fn positions_flip_short_to_long_reanchors_basis() {
    let mut position = position_at(-2, "600.00");
    position.apply(5, money("590.00"));
    assert_eq!(position.quantity, 3);
    assert_eq!(position.avg_price, money("590.00"));
}

#[test]
fn positions_flip_back_restores_new_basis() {
    let mut position = position_at(0, "600.00");
    position.apply(2, money("600.00"));
    assert_eq!(position.avg_price, money("600.00"));
    position.apply(-4, money("610.00"));
    assert_eq!(position.quantity, -2);
    assert_eq!(position.avg_price, money("610.00"));
    position.apply(3, money("620.00"));
    assert_eq!(position.quantity, 1);
    assert_eq!(position.avg_price, money("620.00"));
}

#[test]
fn positions_partial_close_keeps_basis() {
    let mut position = position_at(4, "600.00");
    position.apply(-1, money("610.00"));
    assert_eq!(position.quantity, 3);
    assert_eq!(position.avg_price, money("600.00"));
    let mut short = position_at(-4, "600.00");
    short.apply(1, money("590.00"));
    assert_eq!(short.quantity, -3);
    assert_eq!(short.avg_price, money("600.00"));
}

#[test]
fn positions_same_direction_add_averages() {
    let mut position = position_at(2, "600.00");
    position.apply(2, money("610.00"));
    assert_eq!(position.quantity, 4);
    assert_eq!(position.avg_price, money("605.00"));
}

#[test]
fn status_to_active_covers_every_status() {
    use ActiveOrderState as A;
    use OrderStatus as S;
    assert_eq!(S::Pending.to_active(), Some(A::OpenInFlight));
    assert_eq!(S::Submitted.to_active(), Some(A::OpenInFlight));
    assert_eq!(S::Acknowledged.to_active(), Some(A::Open));
    assert_eq!(S::Partial.to_active(), Some(A::Open));
    assert_eq!(S::Filled.to_active(), None);
    assert_eq!(S::Cancelled.to_active(), None);
    assert_eq!(S::Rejected.to_active(), None);
    assert_eq!(S::Expired.to_active(), None);
    assert_eq!(S::OpenFailed.to_active(), None);
}

#[test]
fn active_states_round_trip_through_inactive() {
    use ActiveOrderState as A;
    assert_eq!(
        A::OpenInFlight.to_inactive().to_active(),
        Some(A::OpenInFlight)
    );
    assert_eq!(A::Open.to_inactive().to_active(), Some(A::Open));
    // A cancel request collapses to the last confirmed working state on the
    // status side: the venue has not confirmed anything yet.
    assert_eq!(A::CancelInFlight.to_inactive(), OrderStatus::Acknowledged);
}

#[test]
fn cancel_requests_convert_only_from_workable_states() {
    use ActiveOrderState as A;
    assert_eq!(A::OpenInFlight.to_request_cancel(), Some(A::CancelInFlight));
    assert_eq!(A::Open.to_request_cancel(), Some(A::CancelInFlight));
    assert_eq!(A::CancelInFlight.to_request_cancel(), None);
}

#[test]
fn partial_is_active_expired_and_open_failed_are_terminal() {
    assert!(OrderStatus::Partial.is_active());
    assert!(!OrderStatus::Expired.is_active());
    assert!(!OrderStatus::OpenFailed.is_active());
}

fn vertical_legs() -> SpreadLegs {
    let expiry = NaiveDate::from_ymd_opt(2026, 1, 16).expect("valid date");
    let long = SpreadLeg::new()
        .contract(
            OptionContract::new()
                .underlying("SPY".to_owned())
                .expiry(expiry)
                .strike(money("600"))
                .kind(OptionKind::Call)
                .build(),
        )
        .side(LegSide::Long)
        .ratio(1)
        .build();
    let short = SpreadLeg::new()
        .contract(
            OptionContract::new()
                .underlying("SPY".to_owned())
                .expiry(expiry)
                .strike(money("610"))
                .kind(OptionKind::Call)
                .build(),
        )
        .side(LegSide::Short)
        .ratio(1)
        .build();
    SpreadLegs::try_new(vec![long, short]).expect("vertical must build")
}

#[test]
fn defaulted_intent_keeps_single_leg_shape() {
    let built = intent();
    assert_eq!(built.legs, None);
    assert_eq!(built.time_in_force, TimeInForce::Day);
    assert_eq!(built.underlying, "SPY");
    assert_eq!(built.quantity, 2);
    assert_eq!(built.limit_price, money("600.50"));

    let legged = OrderIntent::new()
        .client_id(client_id_for("prop-1", 0))
        .proposal_id("prop-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(2)
        .limit_price(money("600.50"))
        .maybe_legs(Some(vertical_legs()))
        .time_in_force(TimeInForce::GoodTilCancelled)
        .build();
    assert_eq!(legged.legs, Some(vertical_legs()));
    assert_eq!(legged.time_in_force, TimeInForce::GoodTilCancelled);
    // The flat single-leg fields are untouched by opting into legs.
    assert_eq!(legged.underlying, built.underlying);
    assert_eq!(legged.quantity, built.quantity);
}
