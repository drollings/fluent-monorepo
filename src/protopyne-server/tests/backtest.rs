use super::*;
use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use protopyne_types::{Bar, Money};
use protopyne_venue::{OrderIntent, OrderSide, PaperConfig, PaperVenue, Quote, Venue};

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn bar_at(close: &str, ts_secs: i64) -> Bar {
    let close = close.parse::<Decimal>().expect("decimal");
    Bar::new()
        .ts(Utc.timestamp_opt(ts_secs, 0).unwrap())
        .o(close)
        .h(close)
        .l(close)
        .c(close)
        .volume(1_000)
        .maybe_adj(None)
        .build()
}

fn config() -> BacktestConfig {
    BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("spy-15m-v1".to_owned())
        .build()
}

fn test_data() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv")
}

#[test]
fn quote_synthesis_centers_mid_on_close() {
    let bar = bar_at("680.00", 1_767_344_400);
    let quote = quote_for_bar(&bar, &config()).expect("quotable bar");
    let mid = quote.mid().expect("mid exists");
    assert_eq!(mid, money("680.00"));
    assert_eq!(quote.bid_size, 100);
    assert_eq!(quote.ask_size, 100);
    assert_eq!(quote.ts, bar.ts);
    let width = quote.spread_width().expect("width exists");
    assert_eq!(width, money("0.34"));
}

#[test]
fn quote_synthesis_fails_closed_on_bad_input() {
    let zero = bar_at("0.00", 1_767_344_400);
    assert!(quote_for_bar(&zero, &config()).is_none());
    let wide = BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("v".to_owned())
        .quote_spread_bps(10_001)
        .build();
    assert!(quote_for_bar(&bar_at("680.00", 1_767_344_400), &wide).is_none());
}

#[test]
fn config_validation_is_fail_closed() {
    let blank_symbol = BacktestConfig::new()
        .underlying(String::new())
        .data_version("v".to_owned())
        .build();
    assert!(blank_symbol.validate().is_err());
    let blank_version = BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("  ".to_owned())
        .build();
    assert!(blank_version.validate().is_err());
    assert!(config().validate().is_ok());
}

#[test]
fn empty_history_is_rejected() {
    let venue = BacktestVenue::new(config(), Vec::new(), PaperVenue::new(PaperConfig::new().build()));
    assert!(venue.is_err());
}

#[tokio::test]
async fn backtest_venue_shares_paper_fill_behavior() {
    let bars = crate::load_bars_csv(&test_data()).expect("vendored history loads");
    let bar = &bars[0];
    let venue = BacktestVenue::new(
        config(),
        bars[..8].to_vec(),
        PaperVenue::new(PaperConfig::new().build()),
    )
    .expect("venue pins history");
    assert_eq!(venue.data_version(), "spy-15m-v1");
    assert_eq!(venue.bars().len(), 8);
    assert_eq!(venue.first_ts(), bars[0].ts);
    assert_eq!(venue.last_ts(), bars[7].ts);

    let quote = quote_for_bar(bar, venue.config()).expect("quotable");
    assert!(quote.mid().is_some());
    let intent = OrderIntent::new()
        .client_id("bt-fill-0".to_owned())
        .proposal_id("bt-fill".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(1)
        .limit_price(quote.ask)
        .build();
    let dry = venue.dry_run(&intent, &quote).expect("dry run passes");
    assert!(dry.allowed);
    let record = venue.submit(intent.clone(), &quote, bar.ts).await.expect("fills");
    assert_eq!(record.filled_quantity, 1);
    // Idempotent resubmit returns the same record, never a second order.
    let again = venue.submit(intent, &quote, bar.ts).await.expect("idempotent");
    assert_eq!(again.filled_quantity, 1);
    assert_eq!(venue.orders().len(), 1);
    assert_eq!(venue.positions().len(), 1);
    let missing: Option<Quote> = None;
    assert!([missing.as_ref()].into_iter().find_map(|entry| entry).is_none());
}

#[test]
fn fallback_loader_prefers_first_usable_source() {
    let missing = PathBuf::from("/nonexistent/history-a.csv");
    let bars = load_bars_with_fallback(&[missing.clone(), test_data()]).expect("falls through");
    assert!(!bars.is_empty());
    let none: Vec<PathBuf> = vec![missing];
    assert!(load_bars_with_fallback(&none).is_err());
    assert!(load_bars_with_fallback(&[]).is_err());
    assert!(load_bars_single(&test_data()).is_ok());
}
