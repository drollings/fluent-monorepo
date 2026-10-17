use super::*;
use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use crate::order::OrderSide;
use protopyne_types::Money;

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn ts(secs: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).unwrap()
}

fn fill(side: OrderSide, quantity: u32, price: &str, fee: &str) -> PositionFill {
    PositionFill::new()
        .underlying("SPY".to_owned())
        .side(side)
        .quantity(quantity)
        .price(money(price))
        .fee(money(fee))
        .trade_id(format!("t-{quantity}-{price}-{fee}"))
        .at(ts(1_767_344_400))
        .build()
}

fn open_long() -> SpreadPosition {
    let (position, exited) =
        update_from_fill(None, &fill(OrderSide::Buy, 2, "100", "10")).expect("entry must open");
    assert!(exited.is_none());
    position.expect("entry must yield a position")
}

#[test]
fn entry_opens_with_fees_debited_and_no_unrealised() {
    let position = open_long();
    assert_eq!(position.underlying, "SPY");
    assert_eq!(position.side, PositionSide::Long);
    assert_eq!(position.price_entry_average, money("100"));
    assert_eq!(position.quantity_abs, 2);
    assert_eq!(position.quantity_abs_max, 2);
    // Entry fees debit realised immediately; unrealised waits for a mark.
    assert_eq!(position.pnl_realised, money("-10"));
    assert_eq!(position.pnl_unrealised, money("0"));
    assert_eq!(position.fees_enter, money("10"));
    assert_eq!(position.fees_exit, money("0"));
    assert_eq!(position.trade_ids.len(), 1);
}

#[test]
fn increase_averages_entry_and_accumulates_fees() {
    let (position, exited) = update_from_fill(
        Some(open_long()),
        &fill(OrderSide::Buy, 2, "120", "10"),
    )
    .expect("increase must apply");
    assert!(exited.is_none());
    let position = position.expect("increase keeps the position open");
    assert_eq!(position.price_entry_average, money("110"));
    assert_eq!(position.quantity_abs, 4);
    assert_eq!(position.quantity_abs_max, 4);
    assert_eq!(position.pnl_realised, money("-20"));
    assert_eq!(position.fees_enter, money("20"));
    assert_eq!(position.fees_exit, money("0"));
    assert_eq!(position.trade_ids.len(), 2);
}

#[test]
fn partial_reduce_keeps_basis_and_books_fee_inclusive_realised() {
    let (position, exited) = update_from_fill(
        Some(open_long()),
        &fill(OrderSide::Sell, 1, "150", "5"),
    )
    .expect("reduce must apply");
    assert!(exited.is_none());
    let position = position.expect("partial reduce keeps the position open");
    assert_eq!(position.quantity_abs, 1);
    assert_eq!(position.quantity_abs_max, 2);
    assert_eq!(position.price_entry_average, money("100"));
    // (150 - 100) * 1 - 5 fee on the closed leg, minus the 10 entry debit.
    assert_eq!(position.pnl_realised, money("35"));
    assert_eq!(position.fees_enter, money("10"));
    assert_eq!(position.fees_exit, money("5"));
}

#[test]
fn unrealised_includes_estimated_exit_fees() {
    let (position, _) = update_from_fill(
        Some(open_long()),
        &fill(OrderSide::Sell, 1, "150", "5"),
    )
    .expect("reduce must apply");
    let position = position.expect("position stays open");
    // Naive mark would be (150 - 100) * 1 = 50; the engine subtracts the
    // pro-rata exit estimate (1/2 of the 10 entry fees) = 5.
    assert_eq!(position.pnl_unrealised, money("45"));
    // Direct pure-function check on the same numbers.
    assert_eq!(
        calculate_pnl_unrealised(
            PositionSide::Long,
            money("100"),
            1,
            2,
            money("10"),
            money("150"),
        ),
        money("45")
    );
}

#[test]
fn exact_close_ends_the_position_with_a_closed_record() {
    let (position, exited) = update_from_fill(
        Some(open_long()),
        &fill(OrderSide::Sell, 2, "150", "5"),
    )
    .expect("exact close must apply");
    assert!(position.is_none(), "exact close leaves no open position");
    let exited = exited.expect("exact close yields a closed record");
    // -10 entry debit + (150 - 100) * 2 - 5 exit fee.
    assert_eq!(exited.pnl_realised, money("85"));
    assert_eq!(exited.quantity_abs_max, 2);
    assert_eq!(exited.fees_enter, money("10"));
    assert_eq!(exited.fees_exit, money("5"));
    assert_eq!(exited.side, PositionSide::Long);
    assert_eq!(exited.trade_ids.len(), 2);
}

#[test]
fn flip_splits_fees_proportionally_and_opens_the_other_side() {
    let (position, exited) = update_from_fill(
        Some(open_long()),
        &fill(OrderSide::Sell, 5, "150", "15"),
    )
    .expect("flip must apply");
    let exited = exited.expect("flip closes the old side");
    // Closing 2 of 5: exit share 15 * 2/5 = 6; realised -10 + (50 * 2 - 6).
    assert_eq!(exited.fees_exit, money("6"));
    assert_eq!(exited.pnl_realised, money("84"));
    let position = position.expect("flip opens the new side");
    assert_eq!(position.side, PositionSide::Short);
    assert_eq!(position.quantity_abs, 3);
    assert_eq!(position.quantity_abs_max, 3);
    assert_eq!(position.price_entry_average, money("150"));
    // Entering 3 of 5: enter share 15 * 3/5 = 9, debited from realised.
    assert_eq!(position.fees_enter, money("9"));
    assert_eq!(position.pnl_realised, money("-9"));
    // The split is conservative: both shares sum to the fill fee exactly.
    assert_eq!(
        exited.fees_exit.as_decimal() + position.fees_enter.as_decimal(),
        money("15").as_decimal()
    );
}

#[test]
fn short_side_mirrors_long_with_inverted_pnl() {
    let (position, _) = update_from_fill(None, &fill(OrderSide::Sell, 2, "100", "10"))
        .expect("short entry must open");
    let position = position.expect("short opens");
    assert_eq!(position.side, PositionSide::Short);
    let (position, _) = update_from_fill(Some(position), &fill(OrderSide::Buy, 1, "80", "5"))
        .expect("cover must apply");
    let position = position.expect("partial cover keeps the short open");
    // (100 - 80) * 1 - 5 cover fee, minus the 10 entry debit.
    assert_eq!(position.pnl_realised, money("5"));
    assert_eq!(position.price_entry_average, money("100"));
}

#[test]
fn zero_quantity_and_wrong_instrument_refuse_without_panic() {
    let zero = PositionFill::new()
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(0)
        .price(money("100"))
        .fee(money("1"))
        .trade_id("t-zero".to_owned())
        .at(ts(1_767_344_400))
        .build();
    assert!(update_from_fill(None, &zero).is_err());
    assert!(update_from_fill(Some(open_long()), &zero).is_err());

    let other = PositionFill::new()
        .underlying("QQQ".to_owned())
        .side(OrderSide::Buy)
        .quantity(1)
        .price(money("100"))
        .fee(money("1"))
        .trade_id("t-other".to_owned())
        .at(ts(1_767_344_400))
        .build();
    let err = update_from_fill(Some(open_long()), &other).expect_err("wrong instrument must refuse");
    assert!(err.to_string().contains("QQQ"));
}

#[test]
fn entry_average_is_volume_weighted() {
    assert_eq!(
        calculate_price_entry_average(money("100"), 2, money("120"), 2),
        money("110")
    );
    assert_eq!(
        calculate_price_entry_average(money("100"), 0, money("120"), 0),
        money("0")
    );
}

#[test]
fn multi_fill_entry_average_weights_by_quantity_not_fill_count() {
    // Two increases of unequal size: the entry is volume-weighted, so it
    // cannot be the two-price arithmetic mean.
    let (position, _) = update_from_fill(None, &fill(OrderSide::Buy, 1, "100", "1"))
        .expect("entry must open");
    let (position, _) = update_from_fill(position, &fill(OrderSide::Buy, 3, "120", "1"))
        .expect("increase must apply");
    let position = position.expect("increase keeps the position open");
    assert_eq!(position.price_entry_average, money("115"));
    assert_ne!(
        position.price_entry_average,
        money("110"),
        "the entry must not be the arithmetic mean of the two prices"
    );
    assert_eq!(position.quantity_abs, 4);
    // The pure formula agrees over the same fills.
    assert_eq!(
        calculate_price_entry_average(money("100"), 1, money("120"), 3),
        money("115")
    );
}

#[test]
fn realised_pnl_is_fee_inclusive_on_both_sides() {
    assert_eq!(
        calculate_pnl_realised(PositionSide::Long, money("100"), 2, money("150"), money("5")),
        money("95")
    );
    assert_eq!(
        calculate_pnl_realised(PositionSide::Short, money("100"), 2, money("80"), money("5")),
        money("35")
    );
}
