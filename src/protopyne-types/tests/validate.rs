use crate::{Adjustment, Bar, Money, OptionContract, OptionKind, Portfolio, PortfolioId};
use chrono::TimeZone as _;
use fluent_wvr::validate::{check_for_execute, valid_output, Validate};

fn sample_ts() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc
        .with_ymd_and_hms(2026, 1, 2, 14, 30, 0)
        .single()
        .expect("valid test timestamp")
}

fn dec(s: &str) -> rust_decimal::Decimal {
    s.parse().expect("valid test decimal")
}

fn sample_bar() -> Bar {
    Bar::new()
        .ts(sample_ts())
        .o(dec("100.00"))
        .h(dec("101.50"))
        .l(dec("99.25"))
        .c(dec("100.75"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .build()
}

#[test]
fn valid_output_shape() {
    let out = valid_output("ok");
    assert!(out.success);
}

#[test]
fn bar_validate_trait_parity_with_inherent() {
    let good = sample_bar();
    assert!(good.validate().is_ok());
    assert!(Validate::validate(&good).is_ok());
    // Adjustment import is exercised by the sample builder path.
    let _ = Adjustment::new().build();
    // Broken high: high below open/close/low.
    let bad = Bar::new()
        .ts(sample_ts())
        .o(dec("100.00"))
        .h(dec("50.00"))
        .l(dec("99.25"))
        .c(dec("100.75"))
        .volume(1_u64)
        .maybe_adj(None)
        .build();
    assert!(bad.validate().is_err());
    assert!(Validate::validate(&bad).is_err());
    // Broken low: low above open/close/high.
    let bad_low = Bar::new()
        .ts(sample_ts())
        .o(dec("100.00"))
        .h(dec("200.00"))
        .l(dec("300.00"))
        .c(dec("100.75"))
        .volume(1_u64)
        .maybe_adj(None)
        .build();
    assert!(bad_low.validate().is_err());
    assert!(Validate::validate(&bad_low).is_err());
}

#[test]
fn bridge_maps_invalid_to_execution_error() {
    let bad = Bar::new()
        .ts(sample_ts())
        .o(dec("100.00"))
        .h(dec("50.00"))
        .l(dec("99.25"))
        .c(dec("100.75"))
        .volume(1_u64)
        .maybe_adj(None)
        .build();
    let err = check_for_execute(&bad, "bar invalid").expect_err("must fail");
    match err {
        fluent_wvr::WorkError::Execution(msg) => assert!(msg.contains("bar invalid")),
        other => panic!("expected Execution, got {other:?}"),
    }
    let good = sample_bar();
    assert!(check_for_execute(&good, "bar invalid").is_ok());
}

#[test]
fn chain_validates_parity() {
    use chrono::NaiveDate;
    let contract = OptionContract::new()
        .underlying("SPY".to_owned())
        .expiry(NaiveDate::from_ymd_opt(2026, 1, 16).expect("date"))
        .strike(Money::new(dec("600")))
        .kind(OptionKind::Call)
        .build();
    assert!(contract.validate().is_ok());
    assert!(Validate::validate(&contract).is_ok());
    let bad_contract = OptionContract::new()
        .underlying(String::new())
        .expiry(NaiveDate::from_ymd_opt(2026, 1, 16).expect("date"))
        .strike(Money::new(dec("600")))
        .kind(OptionKind::Call)
        .build();
    assert!(bad_contract.validate().is_err());
    assert!(Validate::validate(&bad_contract).is_err());

    let portfolio = Portfolio::new()
        .id(PortfolioId::new("p1"))
        .name("paper".to_owned())
        .build();
    assert!(portfolio.validate().is_ok());
    assert!(Validate::validate(&portfolio).is_ok());
    let bad_portfolio = Portfolio::new()
        .id(PortfolioId::new("p1"))
        .name("   ".to_owned())
        .build();
    assert!(bad_portfolio.validate().is_err());
    assert!(Validate::validate(&bad_portfolio).is_err());
}
