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

use chrono::NaiveDate;
use protopyne_types::Money;
use protopyne_types::{ChainQuote, ContractId, OptionContract, OptionKind};
use protopyne_venue::{LegExecution, OrderSide, Position, PositionSide};

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

/// One long package unit booked at 1.50 with an explicit mark basis and the
/// executed legs a package carries.
fn booked_marked(basis: MarkBasis) -> BookedFill {
    BookedFill::new()
        .portfolio("paper".to_owned())
        .underlying("SPY".to_owned())
        .structure("Vertical".to_owned())
        .strategy("ribbon".to_owned())
        .grant("grant-1".to_owned())
        .side(OrderSide::Buy)
        .quantity(1)
        .price(money("1.50"))
        .fee(money("0"))
        .trade_id("m1".to_owned())
        .at(ts(100))
        .mark_basis(basis)
        .legs(vec![
            LegExecution {
                contract: ContractId::new("SPY   260220C00690000"),
                side: OrderSide::Buy,
                quantity: 1,
                price: money("1.50"),
                fee: money("0"),
            },
            LegExecution {
                contract: ContractId::new("SPY   260220C00695000"),
                side: OrderSide::Sell,
                quantity: 1,
                price: money("0"),
                fee: money("0"),
            },
        ])
        .build()
}

/// The same two legs quoted at a 2.00 package mid (2.50 long less 0.50
/// short) at the mark's instant.
fn covering_chain() -> Vec<ChainQuote> {
    let contract = |occ: &str| {
        OptionContract::new()
            .underlying("SPY".to_owned())
            .expiry(NaiveDate::from_ymd_opt(2026, 2, 20).expect("valid expiry"))
            .strike(money(match occ {
                "SPY   260220C00690000" => "690",
                _ => "695",
            }))
            .kind(OptionKind::Call)
            .build()
    };
    vec![
        ChainQuote::new()
            .contract(contract("SPY   260220C00690000"))
            .bid(money("2.49"))
            .ask(money("2.51"))
            .timestamp(ts(150))
            .build(),
        ChainQuote::new()
            .contract(contract("SPY   260220C00695000"))
            .bid(money("0.49"))
            .ask(money("0.51"))
            .timestamp(ts(150))
            .build(),
    ]
}

/// One mark over the tape price legless books mark against, with the run's
/// chain for legged ones.
fn mark<'a>(at_secs: i64, price: &str, chain: &'a [ChainQuote]) -> MarkPoint<'a> {
    MarkPoint {
        at: ts(at_secs),
        price: money(price),
        chain,
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
    assert_eq!(bucket.pnl_unrealised, Some(money("0")));
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
fn an_unmarked_book_reports_absent_unrealized_never_zero() {
    // A book with no mark source reports `None`, not a zero that never
    // existed; a portfolio total goes absent with it.
    let aggregation =
        aggregate(&[booked_marked(MarkBasis::Absent)]).expect("clean fill aggregates");
    let bucket = bucket_of(&aggregation);
    assert_eq!(bucket.mark_basis, MarkBasis::Absent);
    assert_eq!(bucket.pnl_unrealised, None, "absence is not zero");
    let report = aggregation
        .summarize(&[MarkPoint::tape(ts(200), money("2.00"))])
        .expect("marks summarize");
    assert_eq!(report.portfolios[0].pnl_unrealised, None);
}

#[test]
fn a_chain_mark_differs_from_the_underlying_close_proxy_by_the_frozen_delta() {
    // One long package unit booked at 1.50, marked at the same instant twice:
    // once with the run's chain covering both legs (mid 2.00, so 0.50
    // unrealized) and once without it, where the book's own fill price stands
    // in (1.50, so 0.00). The tape price never moves — a package has no share
    // price to ride it on — so the two arms differ by the chain alone, and the
    // delta is frozen so a silent mark swap names itself.
    let chain = covering_chain();
    let aggregation = aggregate(&[booked_marked(MarkBasis::ChainMid)]).expect("clean fills");
    let marked = aggregation
        .summarize(&[mark(200, "100.00", &chain)])
        .expect("chain marks");
    let unmarkable = aggregation
        .summarize(&[mark(200, "100.00", &[])])
        .expect("no chain marks");
    assert_eq!(marked.portfolios[0].pnl_unrealised, Some(money("0.50")));
    assert_eq!(
        unmarkable.portfolios[0].pnl_unrealised,
        Some(money("0")),
        "an unanswered leg falls back to the package's own fill prices"
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
        MarkPoint::tape(ts(150), money("110")),
        MarkPoint::tape(ts(250), money("111")),
        MarkPoint::tape(ts(350), money("110")),
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
    assert_eq!(totals.pnl_unrealised, Some(money("100")));
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
        crate::report::BacktestReport::unavailable_greeks_note(false),
        "greeks stay explicitly unavailable, and a chainless fold says so",
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
        .summarize(&[MarkPoint::tape(ts(200), money("110.00"))])
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
        None,
        MarkBasis::FillPrice,
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
