//! Spread-economics identities, refusals, and property checks.

use chrono::{NaiveDate, TimeZone, Utc};
use rust_decimal::Decimal;

use super::*;
use protopyne_types::{
    ChainQuote, LegSide, Money, OptionContract, OptionKind, Qty, SpreadLeg, SpreadLegs,
};

fn date(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 7, day).expect("valid date")
}

fn contract(kind: OptionKind, strike: &str, expiry: NaiveDate) -> OptionContract {
    OptionContract::new()
        .underlying("SPY".to_owned())
        .expiry(expiry)
        .strike(strike.parse().expect("money"))
        .kind(kind)
        .build()
}

fn call(strike: &str) -> OptionContract {
    contract(OptionKind::Call, strike, date(17))
}

fn put(strike: &str) -> OptionContract {
    contract(OptionKind::Put, strike, date(17))
}

fn far_call(strike: &str) -> OptionContract {
    contract(OptionKind::Call, strike, date(21))
}

fn at(minute: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 1, 14, 30, 0).unwrap() + chrono::Duration::minutes(minute.into())
}

fn quote(contract: OptionContract, price: &str) -> ChainQuote {
    quote_book(contract, price, price)
}

fn quote_book(contract: OptionContract, bid: &str, ask: &str) -> ChainQuote {
    ChainQuote::new()
        .contract(contract)
        .bid(bid.parse().expect("bid"))
        .ask(ask.parse().expect("ask"))
        .timestamp(at(0))
        .build()
}

fn leg(contract: OptionContract, side: LegSide) -> SpreadLeg {
    SpreadLeg::new().contract(contract).side(side).build()
}

fn spread(legs: Vec<SpreadLeg>) -> SpreadLegs {
    SpreadLegs::try_new(legs).expect("a defined-risk leg set")
}

fn money(text: &str) -> Money {
    text.parse().expect("money")
}

fn fee(text: &str) -> Money {
    money(text)
}

fn qty(value: u64) -> Qty {
    Qty::new(Decimal::from(value))
}

fn long_vertical(high_strike: &str) -> (SpreadLegs, Vec<ChainQuote>) {
    let low = call("100");
    let high = call(high_strike);
    (
        spread(vec![
            leg(low.clone(), LegSide::Long),
            leg(high.clone(), LegSide::Short),
        ]),
        vec![quote(low, "3.00"), quote(high, "1.20")],
    )
}

fn short_vertical(high_strike: &str) -> (SpreadLegs, Vec<ChainQuote>) {
    let low = call("100");
    let high = call(high_strike);
    (
        spread(vec![
            leg(low.clone(), LegSide::Short),
            leg(high.clone(), LegSide::Long),
        ]),
        vec![quote(low, "3.00"), quote(high, "1.20")],
    )
}

/// The package's own executable width — what a taker pays between the
/// package bid and its ask. This, not the underlying's bar spread, is what a
/// spread threshold judges.
#[test]
fn the_package_reports_its_own_bid_ask_width() {
    let (low, high) = (call("100"), call("105"));
    let quotes = vec![
        quote_book(low.clone(), "3.50", "3.62"),
        quote_book(high.clone(), "1.50", "1.58"),
    ];
    let debit = spread(vec![
        leg(low.clone(), LegSide::Long),
        leg(high.clone(), LegSide::Short),
    ]);
    let credit = spread(vec![leg(low, LegSide::Short), leg(high, LegSide::Long)]);
    let priced = spread_economics(&debit, &quotes, qty(1), fee("0")).expect("priced");
    let sold = spread_economics(&credit, &quotes, qty(1), fee("0")).expect("priced");
    // Package bid 2.00 against ask 2.04: buying the wide long leg and selling
    // the narrower short one leaves the difference of their widths, and both
    // package sides measure the same crossing cost.
    assert_eq!(priced.net_per_unit, money("2.02"));
    assert_eq!(priced.bid_ask_width, money("0.04"));
    assert_eq!(sold.net_per_unit, money("-2.02"));
    assert_eq!(sold.bid_ask_width, money("0.04"));
}

#[test]
fn a_debit_vertical_bounds_loss_at_the_debit_plus_fees() {
    let (legs, quotes) = long_vertical("105");
    let priced = spread_economics(&legs, &quotes, qty(2), fee("0.65")).expect("priced");
    assert_eq!(priced.net_per_unit, money("1.80"));
    assert_eq!(priced.package_width, Some(money("5")));
    assert_eq!(priced.max_loss_per_unit, money("3.10"));
    assert_eq!(priced.capital_requirement, money("6.20"));
    assert_eq!(priced.mid_limit, money("1.80"));
    assert_eq!(priced.side, PackageSide::Buy);
    assert!(priced.bounded);
}

#[test]
fn a_credit_vertical_bounds_loss_at_width_minus_credit_plus_fees() {
    let low = call("100");
    let high = call("105");
    let legs = spread(vec![
        leg(low.clone(), LegSide::Short),
        leg(high.clone(), LegSide::Long),
    ]);
    let quotes = vec![quote(low, "3.00"), quote(high, "1.20")];
    let priced = spread_economics(&legs, &quotes, qty(1), fee("0.65")).expect("priced");
    assert_eq!(priced.net_per_unit, money("-1.80"));
    assert_eq!(priced.package_width, Some(money("5")));
    assert_eq!(priced.max_loss_per_unit, money("4.50"));
    assert_eq!(priced.side, PackageSide::Sell);
}

#[test]
fn a_condor_bounds_loss_at_the_wider_wing() {
    let (p90, p95) = (put("90"), put("95"));
    let (c105, c110) = (call("105"), call("110"));
    let legs = spread(vec![
        leg(p90.clone(), LegSide::Long),
        leg(p95.clone(), LegSide::Short),
        leg(c105.clone(), LegSide::Short),
        leg(c110.clone(), LegSide::Long),
    ]);
    let quotes = vec![
        quote(p90, "0.50"),
        quote(p95, "1.00"),
        quote(c105, "1.00"),
        quote(c110, "0.50"),
    ];
    let priced = spread_economics(&legs, &quotes, qty(1), fee("0.65")).expect("priced");
    assert_eq!(priced.net_per_unit, money("-1.00"));
    assert_eq!(priced.package_width, Some(money("5")));
    assert_eq!(priced.max_loss_per_unit, money("6.60"));
    assert_eq!(priced.side, PackageSide::Sell);
}

#[test]
fn a_calendar_has_no_strike_width_and_bounds_at_the_debit() {
    let near = call("100");
    let far = far_call("100");
    let legs = spread(vec![
        leg(near.clone(), LegSide::Short),
        leg(far.clone(), LegSide::Long),
    ]);
    let quotes = vec![quote(near, "2.00"), quote(far, "5.00")];
    let priced = spread_economics(&legs, &quotes, qty(1), fee("0.65")).expect("priced");
    assert_eq!(priced.net_per_unit, money("3.00"));
    assert_eq!(priced.package_width, None);
    assert_eq!(priced.max_loss_per_unit, money("4.30"));
    assert_eq!(priced.side, PackageSide::Buy);
    assert!(priced.bounded);
}

#[test]
fn a_long_single_bounds_at_its_debit() {
    let long = call("100");
    let legs = spread(vec![leg(long.clone(), LegSide::Long)]);
    let priced =
        spread_economics(&legs, &[quote(long, "2.50")], qty(3), fee("0.10")).expect("priced");
    assert_eq!(priced.net_per_unit, money("2.50"));
    assert_eq!(priced.package_width, None);
    assert_eq!(priced.max_loss_per_unit, money("2.60"));
    assert_eq!(priced.capital_requirement, money("7.80"));
    assert_eq!(priced.side, PackageSide::Buy);
}

#[test]
fn max_loss_grows_with_strike_width() {
    let mut previous = None;
    for width in 5..=50 {
        let (legs, quotes) = short_vertical(&(100 + width).to_string());
        let priced = spread_economics(&legs, &quotes, qty(1), fee("0.50")).expect("priced");
        if let Some(previous) = previous {
            assert!(
                priced.max_loss_per_unit > previous,
                "width {width} did not increase max loss"
            );
        }
        previous = Some(priced.max_loss_per_unit);
    }
}

#[test]
fn a_side_flip_mirrors_the_package_and_completes_the_width() {
    let low = call("100");
    let high = call("105");
    let quotes = vec![quote(low.clone(), "3.00"), quote(high.clone(), "1.20")];
    let debit = spread_economics(
        &spread(vec![
            leg(low.clone(), LegSide::Long),
            leg(high.clone(), LegSide::Short),
        ]),
        &quotes,
        qty(1),
        fee("0.65"),
    )
    .expect("debit priced");
    let credit = spread_economics(
        &spread(vec![leg(low, LegSide::Short), leg(high, LegSide::Long)]),
        &quotes,
        qty(1),
        fee("0.65"),
    )
    .expect("credit priced");
    assert_eq!(
        credit.net_per_unit.as_decimal(),
        -debit.net_per_unit.as_decimal()
    );
    assert_eq!(debit.side, PackageSide::Buy);
    assert_eq!(credit.side, PackageSide::Sell);
    assert_eq!(debit.package_width, credit.package_width);
    let two_legs = fee("1.30");
    assert_eq!(
        debit.max_loss_per_unit.as_decimal() + credit.max_loss_per_unit.as_decimal(),
        money("5").as_decimal() + two_legs.as_decimal() + two_legs.as_decimal()
    );
}

#[test]
fn a_fee_increase_never_lowers_max_loss() {
    let (legs, quotes) = long_vertical("105");
    let mut previous = None;
    for per_contract in ["0.00", "0.25", "0.65", "1.00"] {
        let priced = spread_economics(&legs, &quotes, qty(1), fee(per_contract)).expect("priced");
        if let Some(previous) = previous {
            assert!(
                priced.max_loss_per_unit >= previous,
                "fee {per_contract} lowered loss"
            );
        }
        previous = Some(priced.max_loss_per_unit);
    }
}

#[test]
fn the_side_follows_the_sign_of_the_net() {
    let low = call("100");
    let high = call("105");
    let debit = spread_economics(
        &spread(vec![
            leg(low.clone(), LegSide::Long),
            leg(high.clone(), LegSide::Short),
        ]),
        &[quote(low.clone(), "3.00"), quote(high.clone(), "1.20")],
        qty(1),
        fee("0"),
    )
    .expect("debit");
    assert_eq!(debit.side, PackageSide::Buy);
    let credit = spread_economics(
        &spread(vec![
            leg(low.clone(), LegSide::Short),
            leg(high.clone(), LegSide::Long),
        ]),
        &[quote(low.clone(), "3.00"), quote(high.clone(), "1.20")],
        qty(1),
        fee("0"),
    )
    .expect("credit");
    assert_eq!(credit.side, PackageSide::Sell);
    let flat = spread_economics(
        &spread(vec![
            leg(low.clone(), LegSide::Long),
            leg(high.clone(), LegSide::Short),
        ]),
        &[quote(low, "2.00"), quote(high, "2.00")],
        qty(1),
        fee("0"),
    )
    .expect("flat");
    assert_eq!(flat.net_per_unit, Money::zero());
    assert_eq!(flat.side, PackageSide::Buy);
}

#[test]
fn the_newest_quote_wins() {
    let long = call("100");
    let mut old = quote(long.clone(), "1.00");
    old.timestamp = at(0);
    let mut newest = quote(long.clone(), "2.50");
    newest.timestamp = at(5);
    let priced = spread_economics(
        &spread(vec![leg(long, LegSide::Long)]),
        &[newest, old],
        qty(1),
        fee("0"),
    )
    .expect("priced");
    assert_eq!(priced.net_per_unit, money("2.50"));
}

#[test]
fn a_missing_quote_refuses() {
    let (legs, _) = long_vertical("105");
    assert!(matches!(
        spread_economics(&legs, &[], qty(1), fee("0")),
        Err(EconRefusal::MissingQuote(_))
    ));
}

#[test]
fn a_crossed_quote_refuses() {
    let (legs, quotes) = long_vertical("105");
    let mut crossed = quotes.clone();
    crossed[0] = quote_book(call("100"), "3.00", "2.00");
    assert!(matches!(
        spread_economics(&legs, &crossed, qty(1), fee("0")),
        Err(EconRefusal::CrossedQuote(_))
    ));
}

#[test]
fn zero_quantity_and_negative_fees_refuse() {
    let (legs, quotes) = long_vertical("105");
    assert!(matches!(
        spread_economics(&legs, &quotes, Qty::zero(), fee("0")),
        Err(EconRefusal::ZeroQuantity)
    ));
    assert!(matches!(
        spread_economics(&legs, &quotes, qty(1), money("-0.01")),
        Err(EconRefusal::NegativeFee)
    ));
}

#[test]
fn a_leg_set_without_a_long_leg_refuses_with_the_shared_vocabulary() {
    let short = leg(call("100"), LegSide::Short);
    let text = serde_json::to_string(&short).expect("leg serializes");
    let legs: SpreadLegs =
        serde_json::from_str(&format!("{{\"legs\":[{text}]}}")).expect("invalid rows deserialize");
    let quotes = [quote(call("100"), "3.00")];
    assert!(matches!(
        spread_economics(&legs, &quotes, qty(1), fee("0")),
        Err(EconRefusal::DefinedRisk(
            protopyne_types::TypesError::InvalidSpread(_)
        ))
    ));
}

#[test]
fn unsupported_shapes_refuse() {
    let three = spread(vec![
        leg(call("95"), LegSide::Long),
        leg(call("100"), LegSide::Long),
        leg(call("105"), LegSide::Long),
    ]);
    let quotes: Vec<_> = ["95", "100", "105"]
        .into_iter()
        .map(|strike| quote(call(strike), "1.00"))
        .collect();
    assert!(matches!(
        spread_economics(&three, &quotes, qty(1), fee("0")),
        Err(EconRefusal::UnsupportedShape(_))
    ));

    let low = call("100");
    let high = call("105");
    let unequal = SpreadLegs::try_new(vec![
        leg(low.clone(), LegSide::Long),
        SpreadLeg::new()
            .contract(high.clone())
            .side(LegSide::Short)
            .ratio(2)
            .build(),
    ])
    .expect("structurally valid");
    assert!(matches!(
        spread_economics(
            &unequal,
            &[quote(low, "3"), quote(high, "1")],
            qty(1),
            fee("0")
        ),
        Err(EconRefusal::UnsupportedShape(_))
    ));
}

#[test]
fn a_credit_calendar_refuses_as_unbounded() {
    let near = call("100");
    let far = far_call("100");
    let legs = spread(vec![
        leg(near.clone(), LegSide::Long),
        leg(far.clone(), LegSide::Short),
    ]);
    let quotes = [quote(near, "2.00"), quote(far, "5.00")];
    assert!(matches!(
        spread_economics(&legs, &quotes, qty(1), fee("0")),
        Err(EconRefusal::UnboundedRisk(_))
    ));
}

#[test]
fn a_condor_with_inverted_wings_refuses() {
    let (p90, p95) = (put("90"), put("95"));
    let (c105, c110) = (call("105"), call("110"));
    let legs = spread(vec![
        leg(p90.clone(), LegSide::Short),
        leg(p95.clone(), LegSide::Long),
        leg(c105.clone(), LegSide::Short),
        leg(c110.clone(), LegSide::Long),
    ]);
    let quotes = [
        quote(p90, "0.50"),
        quote(p95, "1.00"),
        quote(c105, "1.00"),
        quote(c110, "0.50"),
    ];
    assert!(matches!(
        spread_economics(&legs, &quotes, qty(1), fee("0")),
        Err(EconRefusal::UnsupportedShape(_))
    ));
}

#[test]
fn a_unit_times_quantity_is_the_capital_requirement() {
    let (legs, quotes) = long_vertical("105");
    let one = spread_economics(&legs, &quotes, qty(1), fee("0.65")).expect("priced");
    let sized = spread_economics(&legs, &quotes, qty(4), fee("0.65")).expect("priced");
    let unit = one.unit();
    assert_eq!(unit.price_per_unit, money("1.80"));
    assert_eq!(unit.max_loss_per_unit, money("3.10"));
    assert_eq!(unit.bid_ask_width, one.bid_ask_width);
    assert_eq!(unit.side, PackageSide::Buy);
    // The per-unit loss times the quantity is the sized requirement, to the
    // decimal: rescaling needs no second pricing.
    assert_eq!(
        Money::new(unit.max_loss_per_unit.as_decimal() * Decimal::from(4)),
        sized.capital_requirement,
    );
    assert_eq!(unit.price_per_unit, sized.net_per_unit);
}
