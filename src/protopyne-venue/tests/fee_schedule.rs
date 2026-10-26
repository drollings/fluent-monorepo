//! Commission schedule: the flat arm, the percentage arm, and the promise that
//! opting out of the percentage changes nothing.

use protopyne_types::Money;

use crate::quote::FeeSchedule;

fn money(text: &str) -> Money {
    Money::new(text.parse().expect("decimal"))
}

#[test]
fn a_zero_percentage_reproduces_the_flat_schedule_exactly() {
    let schedule = FeeSchedule::new().per_contract(money("0.65")).build();
    assert_eq!(schedule.percent_bps, 0);
    let flat = schedule.fee_for(100).expect("no overflow");
    assert_eq!(
        schedule
            .fee_for_notional(money("20000"), 100)
            .expect("no overflow"),
        flat,
        "a schedule that did not opt into a percentage pays exactly the flat fee"
    );
    // The rendered form is identical too: no zero-scale artefact from the
    // arithmetic that did not run.
    assert_eq!(flat.to_string(), "65.00");
}

#[test]
fn the_percentage_arm_charges_notional_in_basis_points() {
    // 0.03 % of 20 000 = 6.00, plus nothing per contract.
    let schedule = FeeSchedule::new()
        .per_contract(money("0"))
        .percent_bps(3)
        .build();
    let fee = schedule
        .fee_for_notional(money("20000"), 100)
        .expect("no overflow");
    assert_eq!(fee, money("6.00"));
}

#[test]
fn the_two_arms_add() {
    let schedule = FeeSchedule::new()
        .per_contract(money("0.65"))
        .percent_bps(3)
        .build();
    let fee = schedule
        .fee_for_notional(money("20000"), 100)
        .expect("no overflow");
    assert_eq!(fee, money("71.00"), "65.00 flat plus 6.00 proportional");
}

#[test]
fn a_credit_fill_is_charged_on_magnitude() {
    let schedule = FeeSchedule::new()
        .per_contract(money("0"))
        .percent_bps(3)
        .build();
    let fee = schedule
        .fee_for_notional(money("-20000"), 1)
        .expect("no overflow");
    assert_eq!(fee, money("6.00"), "a sale is commissioned on notional");
}

/// A hand-computed notional-fee tape: 0.03 % of notional plus $0.65 per
/// contract, charged on magnitude so sales commission on notional.
///
/// The schedule under test: $0.65 flat plus 3 bps. Every row is arithmetic
/// done by hand, so a change to either arm moves a row here first.
#[test]
fn a_hand_computed_notional_fee_tape() {
    let schedule = FeeSchedule::new()
        .per_contract(money("0.65"))
        .percent_bps(3)
        .build();
    // (notional, contracts, hand-computed fee)
    let tape = [
        ("20000", 100, "71.00"), // 65.00 flat plus 0.0003 x 20000 = 6.00
        ("10500", 50, "35.65"),  // 32.50 flat plus 0.0003 x 10500 = 3.15
        ("-8000", 40, "28.40"),  // sale: 26.00 flat plus 0.0003 x 8000 = 2.40
        ("100", 1, "0.68"),      // 0.65 flat plus 0.0003 x 100 = 0.03
    ];
    let mut total = money("0");
    for (notional, contracts, expected) in tape {
        let fee = schedule
            .fee_for_notional(money(notional), contracts)
            .expect("no overflow");
        assert_eq!(fee, money(expected), "notional {notional} x {contracts}");
        total = Money::new(total.as_decimal() + fee.as_decimal());
    }
    // 71.00 + 35.65 + 28.40 + 0.68 = 135.73 by hand.
    assert_eq!(total, money("135.73"), "the tape total is hand-computed");
}
