use super::*;
use fluent_wvr::prelude::*;
use protopyne_types::{Money, Validate, check_for_execute};
use rust_decimal::Decimal;

fn money(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

#[test]
fn server_config_trait_parity() {
    let good = ServerConfig::default();
    assert!(good.validate().is_ok());
    assert!(Validate::validate(&good).is_ok());
    let bad_bind = ServerConfig::new()
        .bind("not-a-socket".to_owned())
        .max_concurrent_requests(32)
        .feed_capacity(256)
        .feed_interval_ms(50)
        .build();
    assert!(bad_bind.validate().is_err());
    assert!(Validate::validate(&bad_bind).is_err());
    let bad_cap = ServerConfig::new()
        .bind("127.0.0.1:9060".to_owned())
        .max_concurrent_requests(0)
        .feed_capacity(256)
        .feed_interval_ms(50)
        .build();
    assert!(bad_cap.validate().is_err());
    assert!(Validate::validate(&bad_cap).is_err());
}

#[test]
fn backtest_and_runner_trait_parity() {
    let good_bt = BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("spy-15m-v1".to_owned())
        .build();
    assert!(good_bt.validate().is_ok());
    assert!(Validate::validate(&good_bt).is_ok());
    let bad_bt = BacktestConfig::new()
        .underlying(String::new())
        .data_version("spy-15m-v1".to_owned())
        .build();
    assert!(bad_bt.validate().is_err());
    assert!(Validate::validate(&bad_bt).is_err());

    let good_run = RunnerConfig::new(
        "SPY".to_owned(),
        "ribbon".to_owned(),
        money(400),
        money(2_000),
        30,
        0,
    );
    assert!(good_run.validate().is_ok());
    assert!(Validate::validate(&good_run).is_ok());
    let bad_run = RunnerConfig::new(String::new(), "ribbon".to_owned(), money(400), money(2_000), 30, 0);
    assert!(bad_run.validate().is_err());
    assert!(Validate::validate(&bad_run).is_err());
}

#[test]
fn invalid_server_config_fails_execute_bridge() {
    let bad = ServerConfig::new()
        .bind("not-a-socket".to_owned())
        .max_concurrent_requests(32)
        .feed_capacity(256)
        .feed_interval_ms(50)
        .build();
    assert!(check_for_execute(&bad, "server config invalid").is_err());
    let ctx = WorkContext::default();
    assert!(bad.execute(&ctx).is_err());
    assert!(ServerConfig::default().execute(&ctx).is_ok());
}
