//! Portfolio aggregation fixtures: closed-form numbers first, no venue.
//!
//! Every expectation below is hand-derived from the cited helper's contract
//! (venue `update_from_fill` / `calculate_pnl_*`, server `metrics::*`), so a
//! failure means the wiring fed the wrong inputs, never that the math moved.
//! Stat comparisons use the frozen `EPS`: the summary must reproduce
//! hand-computed Sharpe/Sortino/Calmar within it before any number reaches
//! the report.

use super::*;
use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;

use protopyne_types::Money;
use protopyne_venue::{OrderSide, Position, PositionSide};

/// Frozen comparison tolerance for portfolio stats: hand-computed
/// Sharpe/Sortino/Calmar must reproduce within this eps.
const EPS: f64 = 1e-9;

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn ts(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).unwrap()
}

fn booked(
    side: OrderSide,
    quantity: u32,
    price: &str,
    fee: &str,
    trade_id: &str,
    at_secs: i64,
) -> BookedFill {
    BookedFill::new()
        .portfolio("paper".to_owned())
        .underlying("SPY".to_owned())
        .structure("Vertical".to_owned())
        .strategy("ribbon".to_owned())
        .grant("grant-1".to_owned())
        .side(side)
        .quantity(quantity)
        .price(money(price))
        .fee(money(fee))
        .trade_id(trade_id.to_owned())
        .at(ts(at_secs))
        .build()
}

fn key() -> BucketKey {
    BucketKey {
        portfolio: "paper".to_owned(),
        underlying: "SPY".to_owned(),
        structure: "Vertical".to_owned(),
        strategy: "ribbon".to_owned(),
        grant: "grant-1".to_owned(),
    }
}

fn bucket_of(aggregation: &Aggregation) -> BucketSummary {
    let buckets = aggregation.buckets();
    assert_eq!(buckets.len(), 1, "fixtures book one bucket");
    let bucket = buckets.into_iter().next().expect("one bucket");
    assert_eq!(bucket.key, key());
    bucket
}

fn close_enough(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() < EPS,
        "{what}: {actual} differs from hand-computed {expected}"
    );
}

#[test]
fn round_trip_fills_book_known_realized_pnl() {
    // Buy 10 @ 100 (fee 1) then sell 10 @ 110 (fee 1): the open debits 1,
    // the close earns (110 - 100) * 10 - 1 = 99, total 98. No new math —
    // `update_from_fill` owns every number; the fold only routes fills.
    let fills = vec![
        booked(OrderSide::Buy, 10, "100.00", "1.00", "a1", 100),
        booked(OrderSide::Sell, 10, "110.00", "1.00", "a2", 200),
    ];
    let bucket = bucket_of(&aggregate(&fills).expect("clean fills aggregate"));
    assert_eq!(bucket.fills, 2);
    assert_eq!(bucket.closed_trades, 1);
    assert_eq!(bucket.pnl_realised, money("98.00"));
    assert_eq!(bucket.fees, money("2.00"));
    assert_eq!(bucket.open_quantity, 0);
    assert_eq!(bucket.open_side, None);
    assert_eq!(bucket.pnl_unrealised, money("0"));
}

#[test]
fn flip_through_zero_reanchors_like_position_apply() {
    // Sell 15 @ 110 against a long 10 @ 100: the closing share earns
    // (110 - 100) * 10 - 1.00 = 99.00 (fee split 1.50 * 10/15), the flipped
    // short 5 re-anchors at the fill price 110 with the 0.50 opening share
    // debited. The average-cost `Position::apply` must agree on quantity and
    // basis: both models re-anchor a flip at the fill price.
    let fills = vec![
        booked(OrderSide::Buy, 10, "100", "1.00", "b1", 100),
        booked(OrderSide::Sell, 15, "110", "1.50", "b2", 200),
    ];
    let bucket = bucket_of(&aggregate(&fills).expect("flip aggregates"));
    assert_eq!(bucket.closed_trades, 1);
    assert_eq!(bucket.open_quantity, 5);
    assert_eq!(bucket.open_side, Some(PositionSide::Short));

    let mut simple = Position::new()
        .underlying("SPY".to_owned())
        .quantity(0)
        .avg_price(money("0"))
        .build();
    simple.apply(10, money("100"));
    simple.apply(-15, money("110"));
    assert_eq!(simple.quantity, -5);
    assert_eq!(
        simple.avg_price,
        money("110"),
        "flip re-anchors at the fill"
    );
}

#[test]
fn closed_trade_stats_match_hand_computation() {
    // Three round trips realizing exactly [100, -50, 50]: wins 2/3,
    // gross 150 over gross 50 for a profit factor of 3. Zero fees keep the
    // fixtures exact; fees are covered by the round-trip test above.
    let fills = vec![
        booked(OrderSide::Buy, 10, "100", "0", "c1", 100),
        booked(OrderSide::Sell, 10, "110", "0", "c2", 200),
        booked(OrderSide::Buy, 10, "100", "0", "c3", 300),
        booked(OrderSide::Sell, 10, "95", "0", "c4", 400),
        booked(OrderSide::Buy, 10, "100", "0", "c5", 500),
        booked(OrderSide::Sell, 10, "105", "0", "c6", 600),
    ];
    let aggregation = aggregate(&fills).expect("clean fills aggregate");
    assert_eq!(bucket_of(&aggregation).closed_trades, 3);
    let report = aggregation.summarize(&[]).expect("empty marks summarize");
    assert_eq!(report.portfolios.len(), 1);
    let totals = &report.portfolios[0];
    assert_eq!(totals.portfolio, "paper");
    assert_eq!(totals.pnl_realised, money("100"));
    close_enough(
        totals.win_rate.expect("decisive closes exist"),
        2.0 / 3.0,
        "win rate",
    );
    close_enough(
        totals.profit_factor.expect("losses exist"),
        3.0,
        "profit factor",
    );
    // No marks, no curve: return-based stats stay absent, never zero.
    assert!(totals.sharpe.is_none());
    assert!(totals.sortino.is_none());
    assert!(totals.calmar.is_none());
    assert!(report.unrealized_curve.is_empty());
}

#[test]
fn mtm_curve_stats_match_hand_computation() {
    // Buy 10 @ 100 (no fee), marked 110 / 111 / 110: unrealized prints
    // 100 / 110 / 100 with nothing realized, so the MTM equity curve is
    // exactly [100, 110, 100]. Step returns are [0.1, -1/11] (mean 1/220),
    // giving closed forms: Sharpe = sqrt(252)/21, Sortino = sqrt(504)/20
    // (sole shortfall 1/11 over both steps), Calmar = (1/220)*252/(1/11)
    // = 12.6. Annualized with daily-equivalent steps, zero baseline — the
    // same convention `build_report` uses.
    let fills = vec![booked(OrderSide::Buy, 10, "100", "0", "d1", 100)];
    let marks = vec![
        (ts(150), money("110")),
        (ts(250), money("111")),
        (ts(350), money("110")),
    ];
    let aggregation = aggregate(&fills).expect("clean fill aggregates");
    let report = aggregation.summarize(&marks).expect("marks summarize");
    let curve: Vec<f64> = report
        .unrealized_curve
        .iter()
        .map(|mark| mark.as_decimal().to_f64().expect("in range"))
        .collect();
    assert_eq!(curve, vec![100.0, 110.0, 100.0]);
    let totals = &report.portfolios[0];
    assert_eq!(totals.pnl_unrealised, money("100"));
    close_enough(
        totals.sharpe.expect("variance exists"),
        (252.0_f64).sqrt() / 21.0,
        "sharpe",
    );
    close_enough(
        totals.sortino.expect("shortfall exists"),
        (504.0_f64).sqrt() / 20.0,
        "sortino",
    );
    close_enough(totals.calmar.expect("drawdown exists"), 12.6, "calmar");
    // Nothing ever closed: trade stats stay absent, never zero.
    assert!(totals.win_rate.is_none());
    assert!(totals.profit_factor.is_none());
    assert_eq!(
        report.greeks_note,
        crate::report::BacktestReport::unavailable_greeks_note(),
        "greeks stay explicitly unavailable until chain history exists",
    );
}

#[test]
fn tearsheet_golden() {
    use crate::report::{build_report, render_tearsheet, RunManifest};

    // One round trip (realized 98.00, fees 2.00) marked once at the exit:
    // the book is flat, so the curve reads [0] and every return-based stat
    // is honestly absent; the single winning close still reports win rate 1.
    let fills = vec![
        booked(OrderSide::Buy, 10, "100.00", "1.00", "e1", 100),
        booked(OrderSide::Sell, 10, "110.00", "1.00", "e2", 200),
    ];
    let portfolio = aggregate(&fills)
        .expect("clean fills aggregate")
        .summarize(&[(ts(200), money("110.00"))])
        .expect("mark summarizes");
    let manifest = RunManifest {
        data_version: "spy-15m-v1".to_owned(),
        code_version: env!("CARGO_PKG_VERSION").to_owned(),
        config_json: "{}".to_owned(),
        bar_count: 2,
        first_ts: "2026-01-02T09:00:00+00:00".to_owned(),
        last_ts: "2026-01-02T09:01:00+00:00".to_owned(),
        seed: 7,
        data_digest: None,
        data_first_ts: None,
        data_last_ts: None,
        data_bar_count: None,
        data_provenance: None,
    };
    let mut report = build_report(
        manifest,
        vec!["synthetic quotes".to_owned()],
        vec!["no chain data".to_owned()],
        2,
        1,
        money("2.00"),
        money("1100.00"),
        money("2000.00"),
        vec![money("0"), money("98.00")],
        None,
    );
    report.portfolio = Some(portfolio);
    let version = env!("CARGO_PKG_VERSION");
    let expected = format!(
        "backtest tearsheet\n\
         manifest: data=spy-15m-v1 code={version} bars=2 seed=7\n\
         proposals: 2 fills: 1 fill_rate: 0.5\n\
         fees: 2.00 notional: 1100.00 efficiency: 0.55\n\
         drawdown: 0 tail: 0 cum_return: 0 rsrs: n/a\n\
         sharpe: n/a sortino: n/a calmar: n/a win_rate: 1 profit_factor: n/a\n\
         portfolio paper: realized 98.00 unrealized 0 fees 2.00 fills 2 closed 1 open 0\n\
         bucket paper/SPY/Vertical/ribbon/grant-1: realized 98.00 unrealized 0 fees 2.00 fills 2 closed 1 open 0 flat\n\
         portfolio stats: win_rate 1 profit_factor n/a sharpe n/a sortino n/a calmar n/a\n\
         assumptions:\n\
         - synthetic quotes\n\
         limits:\n\
         - no chain data\n\
         greeks: greeks-over-time unavailable: prototype has no option-chain feed\n"
    );
    assert_eq!(render_tearsheet(&report), expected);
}
