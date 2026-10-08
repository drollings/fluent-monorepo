use super::*;
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

fn filter() -> LiquidityFilter {
    LiquidityFilter::new()
        .max_spread_width(money("0.50"))
        .min_size(5)
        .max_age_secs(60)
        .build()
}

#[test]
fn mid_and_spread_reject_crossed_quotes() {
    let q = quote();
    assert_eq!(q.spread_width(), Some(money("0.10")));
    assert_eq!(q.mid(), Some(money("600.05")));

    let crossed = Quote::new()
        .bid(money("600.20"))
        .ask(money("600.10"))
        .bid_size(10)
        .ask_size(10)
        .ts(q.ts)
        .build();
    assert_eq!(crossed.spread_width(), None);
    assert_eq!(crossed.mid(), None);
}

#[test]
fn liquid_quote_passes_and_bad_markets_fail_with_reasons() {
    let now = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap();
    assert!(filter().check(&quote(), 2, true, now).is_ok());

    let wide = Quote::new()
        .bid(money("600.00"))
        .ask(money("602.00"))
        .bid_size(50)
        .ask_size(50)
        .ts(quote().ts)
        .build();
    assert!(filter().check(&wide, 2, true, now).is_err());

    let thin = Quote::new()
        .bid(money("600.00"))
        .ask(money("600.10"))
        .bid_size(1)
        .ask_size(1)
        .ts(quote().ts)
        .build();
    assert!(filter().check(&thin, 2, true, now).is_err());

    let stale = Utc.with_ymd_and_hms(2026, 6, 1, 12, 5, 0).unwrap();
    assert!(filter().check(&quote(), 2, true, stale).is_err());

    assert!(filter().check(&quote(), 500, true, now).is_err());
}

#[test]
fn slippage_moves_against_the_aggressor() {
    let tol = Slippage::new().bps(100).build();
    let touch = money("600.10");
    let buy = tol.apply(touch, true);
    let sell = tol.apply(touch, false);
    assert!(buy.as_decimal() > touch.as_decimal());
    assert!(sell.as_decimal() < touch.as_decimal());
}

#[test]
fn fees_scale_per_contract() {
    let fees = FeeSchedule::new().per_contract(money("0.65")).build();
    assert_eq!(fees.fee_for(2), Some(money("1.30")));
    assert_eq!(fees.fee_for(0), Some(money("0.00")));
}
