use super::*;
use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use protopyne_types::{Bar, Money};
use protopyne_venue::{OrderIntent, OrderSide, PaperConfig, Quote, Venue};

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
fn spread_above_full_close_is_refused_at_the_boundary() {
    let wide = BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("v".to_owned())
        .quote_spread_bps(10_001)
        .build();
    let err = wide.validate().expect_err("spread above 100% must refuse at validate");
    assert!(err.to_string().contains("quote_spread_bps"), "refusal names the bound, got: {err}");
    let edge = BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("v".to_owned())
        .quote_spread_bps(10_000)
        .build();
    assert!(edge.validate().is_ok(), "exactly 100% stays admissible at the boundary");
}

#[test]
fn empty_history_is_rejected() {
    let venue = BacktestVenue::new(config(), Vec::new(), PaperConfig::new().build());
    assert!(venue.is_err());
}

#[tokio::test]
async fn backtest_venue_shares_paper_fill_behavior() {
    let bars = crate::csv::load_bars_csv(&test_data()).expect("vendored history loads");
    let bar = &bars[0];
    let venue = BacktestVenue::new(
        config(),
        bars[..8].to_vec(),
        PaperConfig::new().build(),
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

#[tokio::test]
async fn backtest_venue_honors_explicit_paper_tuning() {
    use protopyne_venue::FeeSchedule;
    let bars = crate::csv::load_bars_csv(&test_data()).expect("vendored history loads");
    let bar = &bars[0];
    let tuned = PaperConfig::new()
        .fees(FeeSchedule::new().per_contract(money("1.30")).build())
        .build();
    let venue =
        BacktestVenue::new(config(), bars[..8].to_vec(), tuned).expect("tuned venue pins history");
    let quote = quote_for_bar(bar, venue.config()).expect("quotable");
    let intent = OrderIntent::new()
        .client_id("bt-tuned-0".to_owned())
        .proposal_id("bt-tuned".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(2)
        .limit_price(quote.ask)
        .build();
    let record = venue.submit(intent, &quote, bar.ts).await.expect("fills");
    assert_eq!(record.filled_quantity, 2);
    assert_eq!(
        record.fees_paid,
        money("2.60"),
        "custom per-contract fees must flow into fills, not the default 0.65"
    );
}

#[test]
fn backtest_snapshot_delegates_to_inner_venue() {
    use protopyne_venue::PaperVenue;
    let bars = crate::csv::load_bars_csv(&test_data()).expect("vendored history loads");
    let paper_cfg = PaperConfig::new().build();
    let paper = PaperVenue::new(paper_cfg.clone());
    let venue = BacktestVenue::new(config(), bars[..8].to_vec(), paper_cfg)
        .expect("venue pins history");
    // Delegation equality on the same inputs: the backtest venue adds no
    // account logic of its own, it mirrors its inner paper venue.
    assert_eq!(venue.account_snapshot(), paper.account_snapshot());
    assert_eq!(venue.account_stream(), paper.account_stream());
    assert_eq!(venue.fetch_balances(), paper.fetch_balances());
    assert_eq!(venue.fetch_open_orders(), paper.fetch_open_orders());
    assert_eq!(venue.fetch_trades(), paper.fetch_trades());
}

#[test]
fn fallback_loader_prefers_first_usable_source() {
    use crate::history::{load_bars_candidates, load_bars_file};
    let missing = PathBuf::from("/nonexistent/history-a.csv");
    let bars = load_bars_candidates(&[missing.clone(), test_data()]).expect("falls through");
    assert!(!bars.is_empty());
    let none: Vec<PathBuf> = vec![missing];
    assert!(load_bars_candidates(&none).is_err());
    assert!(load_bars_candidates(&[]).is_err());
    assert!(load_bars_file(&test_data()).is_ok());
}

#[test]
fn dynamic_history_loads_beside_files() {
    use crate::backtest::{HistorySource, load_history};

    // Same tape, two sources: the dynamic variant must reproduce the files
    // path bar for bar — the harness treats them interchangeably.
    let from_files =
        load_history(&HistorySource::Files(vec![test_data()]), None).expect("files load").bars;
    assert!(!from_files.is_empty());
    let replayed = load_history(&HistorySource::Dynamic(from_files.clone()), None)
        .expect("dynamic loads")
        .bars;
    assert_eq!(replayed, from_files);
    // Fail-closed mirrors the files path: no candidates and no bars both
    // refuse instead of running on an empty tape.
    assert!(load_history(&HistorySource::Files(Vec::new()), None).is_err());
    assert!(load_history(&HistorySource::Dynamic(Vec::new()), None).is_err());
    assert!(
        load_history(&HistorySource::Files(vec![PathBuf::from("/nonexistent/x.csv")]), None).is_err()
    );
}
