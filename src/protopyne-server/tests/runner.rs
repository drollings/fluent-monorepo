use super::*;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::prelude::ToPrimitive as _;
use rust_decimal::Decimal;

use protopyne_strategy::{
    elements::{ExplicitStructure, FixedContracts},
    BreakerLimits, BreakerState, Direction, FixedSignal, Grant, GrantBudgets, GrantLedger,
    GrantMarket, GrantPermission, GrantScope, MarketData, MarketView, PortfolioThresholds,
    Proposal, ProposalSide, ProposalState, RiskEngine, RiskLimits, SignalThresholds, Strategy,
    StructureKind, TradeThresholds,
};
use protopyne_types::{Bar, Money, Qty};
use protopyne_venue::{
    Fault, Journal, PaperConfig, PaperVenue, Recorder, StreamConfig, SupervisedStream,
    TransportError, VenueStorage, SUBMIT_RETRIES,
};

/// A minimal decision view for direct `decide_and_submit` tests: no bars and
/// no chain, exactly the economics-only shape those tests exercise.
const BARE_DATA: MarketData<'static> = MarketData::empty(30);

fn bare_view(closes: &[f64], at: DateTime<Utc>) -> MarketView<'_> {
    MarketView {
        data: &BARE_DATA,
        closes,
        as_of: at,
    }
}

fn money(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

fn history() -> Vec<Bar> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    crate::csv::load_bars_csv(&path).expect("vendored history loads")
}

/// A permit value the ephemeral gate ignores: ephemeral reads need a permit
/// value but no installed capability (proven in the venue gate tests).
fn ephemeral_permit() -> VenueStorage {
    VenueStorage::new(PathBuf::from("ephemeral-test"))
}

fn strategy() -> Arc<Strategy> {
    Arc::new(
        Strategy::with_signal(Arc::new(
            FixedSignal::new(Direction::Long, 0.8, 0.8).expect("valid signal"),
        ))
        .with_structure(Arc::new(ExplicitStructure {
            structure: StructureKind::Vertical,
        })),
    )
}

fn risk() -> RiskEngine {
    RiskEngine::new(RiskLimits::new().build())
}

fn grant_with(min_conviction: f64) -> Grant {
    Grant::new()
        .id(format!("sweep-{min_conviction}"))
        .scope(
            GrantScope::new()
                .strategies(vec!["ribbon".to_owned()])
                .underlyings(vec!["SPY".to_owned()])
                .structures(vec![StructureKind::Vertical])
                .directions(vec![Direction::Long, Direction::Short])
                .permissions(GrantPermission {
                    entries: true,
                    exits: true,
                    rolls: false,
                })
                .build(),
        )
        .budgets(
            GrantBudgets::new()
                .max_trades(1_000)
                .max_contracts(10_000)
                .max_loss(money(1_000_000))
                .max_notional(money(100_000_000))
                .build(),
        )
        .signal(
            SignalThresholds::new()
                .min_conviction(min_conviction)
                .min_activity_score(0.5)
                .build(),
        )
        .trade(
            TradeThresholds::new()
                .max_loss_per_trade(money(1_000))
                .max_spread_width(money(50))
                .min_dte_days(1)
                .max_dte_days(90)
                .build(),
        )
        .portfolio(
            PortfolioThresholds::new()
                .max_open_risk(money(10_000_000))
                .quote_max_age_secs(60)
                .build(),
        )
        .breakers(BreakerLimits::new().max_consecutive_errors(3).build())
        .valid_from(Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap())
        .valid_until(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap())
        .build()
}

fn venue_config() -> BacktestConfig {
    BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("spy-15m-v1".to_owned())
        .build()
}

fn runner_config() -> RunnerConfig {
    RunnerConfig::new(
        "SPY".to_owned(),
        "ribbon".to_owned(),
        money(400),
        money(2_000),
        30,
        0,
    )
}

fn signature(decision: &BarDecision) -> (Option<Direction>, bool, bool, bool) {
    (
        decision.direction,
        decision.risk_pass,
        decision.grant_merit,
        decision.submitted,
    )
}

fn strategy_with_contracts(contracts: u32) -> Arc<Strategy> {
    Arc::new(
        Strategy::with_signal(Arc::new(
            FixedSignal::new(Direction::Long, 0.8, 0.8).expect("valid signal"),
        ))
        .with_structure(Arc::new(ExplicitStructure {
            structure: StructureKind::Vertical,
        }))
        .with_sizing(Arc::new(FixedContracts { contracts })),
    )
}

fn directional_strategy(direction: Direction) -> Arc<Strategy> {
    Arc::new(
        Strategy::with_signal(Arc::new(
            FixedSignal::new(direction, 0.8, 0.8).expect("valid signal"),
        ))
        .with_structure(Arc::new(ExplicitStructure {
            structure: StructureKind::Vertical,
        })),
    )
}

/// Every submitted decision must carry the touch limit for its direction:
/// the limit the venue actually received, not the indicative bar close.
fn assert_touch_limits(decisions: &[BarDecision], bars: &[Bar], direction: Direction) {
    let mut checked = 0;
    for decision in decisions {
        if decision.submitted {
            let quote =
                quote_for_bar(&bars[decision.bar_index], &venue_config()).expect("quotable bar");
            let touch = match direction {
                Direction::Long => quote.ask,
                Direction::Short => quote.bid,
            };
            assert_eq!(
                decision.checked_limit, touch,
                "submitted limit must be the touch for {direction:?}"
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "must verify at least one submitted limit");
}

#[tokio::test]
async fn equity_and_notional_scale_with_sizer_quantity() {
    let bars = history();
    let risk = risk();
    let grant = grant_with(0.5);
    let one = run_backtest(
        &strategy_with_contracts(1),
        &risk,
        &grant,
        &venue_config(),
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("one-contract run");
    let three = run_backtest(
        &strategy_with_contracts(3),
        &risk,
        &grant,
        &venue_config(),
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("three-contract run");
    assert!(one.report.fills > 0, "liquid history must fill something");
    assert_eq!(
        one.report.fills, three.report.fills,
        "sizer must not change which bars fill"
    );
    assert_eq!(
        one.report.equity_curve.len(),
        three.report.equity_curve.len()
    );
    for (single, triple) in one
        .report
        .equity_curve
        .iter()
        .zip(three.report.equity_curve.iter())
    {
        assert_eq!(
            triple.as_decimal(),
            single.as_decimal() * Decimal::from(3),
            "every booked flow must scale with filled quantity"
        );
    }
    assert_eq!(
        three.report.filled_notional.as_decimal(),
        one.report.filled_notional.as_decimal() * Decimal::from(3),
    );
}

#[tokio::test]
async fn backtest_and_paper_reach_identical_decisions() {
    let bars = history();
    let strategy = strategy();
    let risk = risk();
    let grant = grant_with(0.5);
    let run = run_backtest(
        &strategy,
        &risk,
        &grant,
        &venue_config(),
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("backtest runs");
    let paper = PaperVenue::new(PaperConfig::new().build());
    let paper_decisions = run_paper_decisions(
        &strategy,
        &risk,
        &grant,
        &paper,
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("the paper run decides");
    assert_eq!(run.decisions.len(), bars.len());
    assert_eq!(paper_decisions.len(), bars.len());
    let backtest_json = serde_json::to_string(&run.decisions).expect("serializable");
    let paper_json = serde_json::to_string(&paper_decisions).expect("serializable");
    assert_eq!(
        backtest_json, paper_json,
        "same code path must decide identically"
    );
    assert!(
        run.decisions.iter().any(|decision| decision.submitted),
        "liquid history must fill something"
    );
    assert_touch_limits(&run.decisions, &bars, Direction::Long);
    assert_touch_limits(&paper_decisions, &bars, Direction::Long);
    assert!(run.report.fills > 0);
    assert!(run
        .report
        .fill_assumptions
        .iter()
        .any(|line| line.contains("touch")));
    assert!(
        run.report
            .fill_assumptions
            .iter()
            .any(|line| line.contains("fill-model assumption")),
        "fills must declare slippage as a fill-model assumption"
    );
    assert!(run.report.greeks_note.contains("unavailable"));
}

#[tokio::test]
async fn submitted_limits_match_touch_by_direction() {
    let bars = history();
    for direction in [Direction::Long, Direction::Short] {
        let strategy = directional_strategy(direction);
        let run = run_backtest(
            &strategy,
            &risk(),
            &grant_with(0.5),
            &venue_config(),
            &runner_config(),
            &bars,
            11,
        )
        .await
        .expect("directional run");
        assert_touch_limits(&run.decisions, &bars, direction);
    }
}

#[tokio::test]
async fn rerun_is_byte_identical() {
    let bars = history();
    let strategy = strategy();
    let risk = risk();
    let grant = grant_with(0.5);
    let first = run_backtest(
        &strategy,
        &risk,
        &grant,
        &venue_config(),
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("first run");
    let second = run_backtest(
        &strategy,
        &risk,
        &grant,
        &venue_config(),
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("second run");
    assert_eq!(first.to_canonical_json(), second.to_canonical_json());
    assert_eq!(
        first.manifest.to_canonical_json(),
        second.manifest.to_canonical_json()
    );
}

#[tokio::test]
async fn walk_forward_test_segment_matches_full_run() {
    let bars = history();
    let strategy = strategy();
    let risk = risk();
    let grant = grant_with(0.5);
    let full = run_backtest(
        &strategy,
        &risk,
        &grant,
        &venue_config(),
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("full run");
    let (train, test) = walk_forward_split(&bars, bars.len() / 2).expect("split");
    assert_eq!(train.len(), bars.len() / 2);
    assert_eq!(test.len(), bars.len() - bars.len() / 2);
    assert!(walk_forward_split(&bars, 0).is_err());
    assert!(walk_forward_split(&bars, bars.len()).is_err());
    let segment = run_backtest(
        &strategy,
        &risk,
        &grant,
        &venue_config(),
        &runner_config(),
        test,
        11,
    )
    .await
    .expect("segment run");
    // Warmup is zero and budgets never bind, so each bar's verdict depends
    // only on its own quote: the test segment must decide like the full run's
    // tail up to proposal identity (ids embed run-local indices).
    for (segment_decision, full_decision) in segment
        .decisions
        .iter()
        .zip(full.decisions[bars.len() / 2..].iter())
    {
        assert_eq!(signature(segment_decision), signature(full_decision));
    }
}

#[tokio::test]
async fn sweep_separates_loose_from_strict_grants() {
    let bars = history();
    let outcomes = run_sweep(
        strategy(),
        RiskLimits::new().build(),
        &venue_config(),
        &runner_config(),
        &bars,
        vec![grant_with(0.5), grant_with(0.95)],
        11,
    )
    .await
    .expect("sweep runs");
    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[0].index, 0);
    assert_eq!(outcomes[1].index, 1);
    assert!(outcomes[0].fills > 0, "loose grant must fill");
    assert_eq!(
        outcomes[1].fills, 0,
        "strict grant must deny every proposal"
    );
    assert!(run_sweep(
        strategy(),
        RiskLimits::new().build(),
        &venue_config(),
        &runner_config(),
        &bars,
        Vec::new(),
        11
    )
    .await
    .is_err());
}

/// The strategy crosses `run_sweep`'s worker pool behind an `Arc`; a
/// stateless signal must decide identically in every worker, so identical
/// grants yield identical decisions. A signal that stored per-bar state
/// behind `&self` would diverge under the pool.
#[tokio::test]
async fn sweep_workers_share_a_stateless_strategy_without_leak() {
    let bars = history();
    let outcomes = run_sweep(
        strategy(),
        RiskLimits::new().build(),
        &venue_config(),
        &runner_config(),
        &bars,
        vec![grant_with(0.5); 8],
        11,
    )
    .await
    .expect("sweep runs");
    assert_eq!(outcomes.len(), 8);
    let first = &outcomes[0];
    for outcome in &outcomes {
        assert_eq!(
            outcome.decisions, first.decisions,
            "worker {} must decide like worker 0",
            outcome.index
        );
        assert_eq!(outcome.manifest, first.manifest);
    }
    assert!(first.fills > 0, "the shared strategy must fill");
}

#[tokio::test]
async fn runner_rejects_empty_history() {
    let strategy = strategy();
    let risk = risk();
    let grant = grant_with(0.5);
    assert!(run_backtest(
        &strategy,
        &risk,
        &grant,
        &venue_config(),
        &runner_config(),
        &[],
        11
    )
    .await
    .is_err());
}

#[test]
fn replay_graph_tracks_and_rewinds_research_steps() {
    let bars = history();
    let mut graph = build_replay(&bars, 5).expect("replay builds");
    assert_eq!(graph.step_count(), bars.len());
    assert!(graph.is_checkpoint(&"bar-0".to_owned()));
    assert!(graph.is_checkpoint(&"bar-5".to_owned()));
    assert!(!graph.is_checkpoint(&"bar-1".to_owned()));
    complete_replay(&mut graph);
    assert_eq!(graph.completed_count(), bars.len());
    assert!(graph.ready_steps().contains(&"bar-1".to_owned()));
    let reset = graph.rewind_to(&"bar-5".to_owned()).expect("rewind works");
    assert_eq!(reset.len(), bars.len() - 5);
    assert!(!graph.is_completed(&"bar-5".to_owned()));
    assert!(!graph.is_checkpoint(&"bar-5".to_owned()));
    assert!(build_replay(&[], 5).is_err());
    assert!(build_replay(&bars, 0).is_err());
}

#[test]
fn runner_helpers_stay_on_the_venue_trait() {
    let venue = PaperVenue::new(PaperConfig::new().build());
    assert!(snapshot_positions(&venue).is_empty());
    let now = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap();
    let outcome = expire_position(&venue, "SPY", now).expect("expiry journals");
    assert!(outcome.note.contains("SPY"));
    let quote = quote_for_bar(&history()[0], &venue_config()).expect("quotable");
    assert!([None, Some(&quote)]
        .into_iter()
        .find_map(|entry| entry)
        .is_some());
    let _ = (
        ProposalSide::Entry,
        GrantMarket::new,
        StructureKind::Vertical,
    );
}

fn decimal_money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn synthetic_bar(close: &str, secs: i64) -> Bar {
    let price = close.parse::<Decimal>().expect("decimal");
    Bar::new()
        .ts(Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap() + chrono::Duration::minutes(secs))
        .o(price)
        .h(price)
        .l(price)
        .c(price)
        .volume(1_000)
        .maybe_adj(None)
        .build()
}

fn filled_decision(
    bar_index: usize,
    ts: String,
    direction: Direction,
    quantity: u32,
    price: &str,
    fee: &str,
) -> BarDecision {
    BarDecision {
        bar_index,
        ts,
        proposal_id: format!("synthetic-{bar_index}"),
        direction: Some(direction),
        structure: Some(StructureKind::Vertical),
        strategy: "ribbon".to_owned(),
        underlying: "SPY".to_owned(),
        conviction: 0.8,
        risk_pass: true,
        risk_reasons: Vec::new(),
        grant_merit: true,
        grant_merit_reasons: Vec::new(),
        grant_authorized: true,
        grant_authorization_reasons: Vec::new(),
        submitted: true,
        limit_price: decimal_money(price),
        checked_limit: decimal_money(price),
        submitted_quantity: quantity,
        filled_quantity: quantity,
        fill_price: Some(decimal_money(price)),
        fee: Some(decimal_money(fee)),
        legs: Vec::new(),
        transitions: Vec::new(),
        lifecycle_error: None,
        skipped: None,
    }
}

fn flat_decision(bar_index: usize, ts: String) -> BarDecision {
    BarDecision {
        bar_index,
        ts,
        proposal_id: format!("synthetic-{bar_index}"),
        direction: None,
        structure: None,
        strategy: String::new(),
        underlying: String::new(),
        conviction: 0.0,
        risk_pass: false,
        risk_reasons: Vec::new(),
        grant_merit: false,
        grant_merit_reasons: Vec::new(),
        grant_authorized: false,
        grant_authorization_reasons: Vec::new(),
        submitted: false,
        limit_price: Money::new(Decimal::ZERO),
        checked_limit: Money::new(Decimal::ZERO),
        submitted_quantity: 0,
        filled_quantity: 0,
        fill_price: None,
        fee: None,
        legs: Vec::new(),
        transitions: Vec::new(),
        lifecycle_error: None,
        skipped: Some("warmup".to_owned()),
    }
}

#[test]
fn equity_curve_leaves_pre_fill_bars_flat() {
    let bars = vec![
        synthetic_bar("110.00", 0),
        synthetic_bar("111.00", 1),
        synthetic_bar("112.00", 2),
        synthetic_bar("113.00", 3),
        synthetic_bar("114.00", 4),
    ];
    let decisions = vec![
        flat_decision(0, bars[0].ts.to_rfc3339()),
        flat_decision(1, bars[1].ts.to_rfc3339()),
        flat_decision(2, bars[2].ts.to_rfc3339()),
        flat_decision(3, bars[3].ts.to_rfc3339()),
        filled_decision(
            4,
            bars[4].ts.to_rfc3339(),
            Direction::Long,
            2,
            "100.00",
            "0.00",
        ),
    ];
    let curve = equity_curve_for(&bars, &decisions);
    assert_eq!(curve.len(), 5);
    let zero = Decimal::ZERO;
    assert_eq!(
        curve[0].as_decimal(),
        zero,
        "bars before any fill must read flat zero"
    );
    assert_eq!(curve[1].as_decimal(), zero);
    assert_eq!(curve[2].as_decimal(), zero);
    assert_eq!(curve[3].as_decimal(), zero);
    assert_eq!(
        curve[4].as_decimal(),
        Decimal::from(-200) + Decimal::from(2) * Decimal::from(114),
        "final bar marks cash plus open position at its close"
    );
}

#[test]
fn equity_curve_ramps_only_after_each_fill() {
    let bars = vec![
        synthetic_bar("100.00", 0),
        synthetic_bar("100.00", 1),
        synthetic_bar("100.00", 2),
        synthetic_bar("100.00", 3),
    ];
    let decisions = vec![
        flat_decision(0, bars[0].ts.to_rfc3339()),
        filled_decision(
            1,
            bars[1].ts.to_rfc3339(),
            Direction::Long,
            1,
            "10.00",
            "1.00",
        ),
        flat_decision(2, bars[2].ts.to_rfc3339()),
        filled_decision(
            3,
            bars[3].ts.to_rfc3339(),
            Direction::Long,
            1,
            "20.00",
            "1.00",
        ),
    ];
    let curve = equity_curve_for(&bars, &decisions);
    assert_eq!(curve.len(), 4);
    assert_eq!(
        curve[0].as_decimal(),
        Decimal::ZERO,
        "pre-fill bar must stay flat"
    );
    assert_eq!(
        curve[1].as_decimal(),
        Decimal::from(-11) + Decimal::from(100),
        "first fill marks its own bar"
    );
    assert_eq!(
        curve[2].as_decimal(),
        curve[1].as_decimal(),
        "bars with no fill carry the previous mark forward"
    );
    assert_eq!(
        curve[3].as_decimal(),
        Decimal::from(-32) + Decimal::from(2) * Decimal::from(100),
        "second fill accumulates on the running state"
    );
}

#[test]
fn equity_curve_marks_shorts_against_later_closes() {
    let bars = vec![
        synthetic_bar("100.00", 0),
        synthetic_bar("110.00", 1),
        synthetic_bar("120.00", 2),
        synthetic_bar("130.00", 3),
    ];
    let decisions = vec![
        flat_decision(0, bars[0].ts.to_rfc3339()),
        flat_decision(1, bars[1].ts.to_rfc3339()),
        filled_decision(
            2,
            bars[2].ts.to_rfc3339(),
            Direction::Short,
            1,
            "120.00",
            "0.00",
        ),
        flat_decision(3, bars[3].ts.to_rfc3339()),
    ];
    let curve = equity_curve_for(&bars, &decisions);
    assert_eq!(curve[0].as_decimal(), Decimal::ZERO);
    assert_eq!(
        curve[1].as_decimal(),
        Decimal::ZERO,
        "pre-fill bars stay flat on shorts too"
    );
    assert_eq!(
        curve[2].as_decimal(),
        Decimal::ZERO,
        "short filled at its own close marks zero"
    );
    assert_eq!(
        curve[3].as_decimal(),
        Decimal::from(120) - Decimal::from(130),
        "later bars mark the open short at their close"
    );
}

#[test]
fn equity_curve_with_no_decisions_stays_flat() {
    let bars = vec![synthetic_bar("100.00", 0), synthetic_bar("101.00", 1)];
    let curve = equity_curve_for(&bars, &[]);
    assert_eq!(curve.len(), 2);
    assert!(curve.iter().all(|mark| mark.as_decimal() == Decimal::ZERO));
}

#[test]
fn equity_curve_prices_a_package_from_its_legs_once() {
    let bars = vec![synthetic_bar("100.00", 0)];
    let mut decision = filled_decision(
        0,
        bars[0].ts.to_rfc3339(),
        Direction::Long,
        1,
        "1.5039",
        "1.30",
    );
    // A debit vertical's executed legs back the package: the curve prices the
    // legs once (buy legs pay, sell legs receive) and marks no share position.
    // `fill_price` (the package net) and `filled_quantity` (package units) are
    // never multiplied together — that product was the double count.
    decision.legs = vec![
        LegExecution {
            contract: protopyne_types::ContractId::new("SPY   260717C00600000".to_owned()),
            side: OrderSide::Buy,
            quantity: 1,
            price: decimal_money("2.7027"),
            fee: decimal_money("0.65"),
        },
        LegExecution {
            contract: protopyne_types::ContractId::new("SPY   260717C00610000".to_owned()),
            side: OrderSide::Sell,
            quantity: 1,
            price: decimal_money("1.1988"),
            fee: decimal_money("0.65"),
        },
    ];
    let curve = equity_curve_for(&bars, &[decision]);
    let expected = -(decimal_money("2.7027").as_decimal() - decimal_money("1.1988").as_decimal())
        - decimal_money("1.30").as_decimal();
    assert_eq!(curve.len(), 1);
    assert_eq!(curve[0].as_decimal(), expected);
}

#[tokio::test]
async fn the_report_carries_and_names_its_mark_basis() {
    let bars = history();
    let run = run_backtest(
        &strategy(),
        &risk(),
        &grant_with(0.5),
        &venue_config(),
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("run");
    // A config-supplied batch run installs no chain, so the book keeps the
    // fill-price basis; the report states it rather than leaving it implicit.
    assert_eq!(run.report.mark_basis, MarkBasis::FillPrice);
    assert!(
        run.report
            .fill_assumptions
            .iter()
            .any(|line| line.contains("fill price")),
        "the report names its mark basis: {:?}",
        run.report.fill_assumptions
    );
}

#[tokio::test]
async fn a_corporate_action_tape_is_named_in_data_limits() {
    let bars = history();
    let tape = vec![protopyne_venue::CorporateEventArgs::new()
        .underlying("SPY".to_owned())
        .at(bars[2].ts)
        .event(protopyne_venue::CorporateEvent::Dividend)
        .amount(Money::from(1))
        .build()];
    let config = BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("spy-15m-v1".to_owned())
        .corporate_actions(tape)
        .build();
    let run = run_backtest(
        &strategy(),
        &risk(),
        &grant_with(0.5),
        &config,
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("taped run");
    assert!(
        run.report.data_limits.iter().any(|line| {
            line.contains(
                "corporate-action tape: 1 event(s) (expiry 0, assignment 0, \
                 exercise 0, dividend 1, split 0)",
            )
        }),
        "the report names its tape: {:?}",
        run.report.data_limits
    );
}

#[tokio::test]
async fn quote_config_flows_into_submitted_touch() {
    let bars = history();
    let wide_config = BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("spy-15m-wide".to_owned())
        .quote_spread_bps(200)
        .quote_size(100)
        .build();
    let run = run_backtest(
        &strategy(),
        &risk(),
        &grant_with(0.5),
        &wide_config,
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("wide-spread run");
    let mut checked = 0;
    for decision in &run.decisions {
        if decision.submitted {
            let expected =
                quote_for_bar(&bars[decision.bar_index], &wide_config).expect("quotable bar");
            let touch = match decision.direction {
                Some(Direction::Short) => expected.bid,
                _ => expected.ask,
            };
            assert_eq!(
                decision.checked_limit, touch,
                "submitted limit must use the configured 200 bps spread, not the default"
            );
            let default_touch = quote_for_bar(&bars[decision.bar_index], &venue_config())
                .expect("quotable bar")
                .ask;
            assert_ne!(
                decision.checked_limit, default_touch,
                "200 bps touch must differ from the 5 bps default on real closes"
            );
            checked += 1;
        }
    }
    assert!(
        checked > 0,
        "liquid history must fill something under a wide spread"
    );
}

/// A misconfigured paper run reports the refusal instead of unwinding: the
/// public entry never panics, and the refusal names the config that failed.
#[tokio::test]
async fn a_misconfigured_paper_run_reports_its_refusal_never_a_panic() {
    let paper = PaperVenue::new(PaperConfig::new().build());
    let bars = vec![synthetic_bar("690", 0)];
    for (label, config) in [
        (
            "underlying must be named",
            RunnerConfig::new(
                " ".to_owned(),
                "ribbon".to_owned(),
                money(400),
                money(2_000),
                30,
                0,
            ),
        ),
        (
            "strategy_name must be named",
            RunnerConfig::new(
                "SPY".to_owned(),
                " ".to_owned(),
                money(400),
                money(2_000),
                30,
                0,
            ),
        ),
    ] {
        let outcome = run_paper_decisions(
            &strategy(),
            &risk(),
            &grant_with(0.5),
            &paper,
            &config,
            &bars,
            11,
        )
        .await;
        assert!(
            matches!(&outcome, Err(ServerError::Backtest(reason)) if reason == label),
            "{label} must refuse with its own reason, got {outcome:?}"
        );
    }
    // The valid config on the same venue and bars decides, so the refusal
    // above is the config's and not the paper path's.
    assert_eq!(
        run_paper_decisions(
            &strategy(),
            &risk(),
            &grant_with(0.5),
            &paper,
            &runner_config(),
            &bars,
            11,
        )
        .await
        .expect("a valid config decides")
        .len(),
        bars.len(),
        "the same call decides every bar once the config is valid"
    );
    // Empty history still reads as an empty run, not a refusal.
    assert!(run_paper_decisions(
        &strategy(),
        &risk(),
        &grant_with(0.5),
        &paper,
        &runner_config(),
        &[],
        11,
    )
    .await
    .expect("empty history decides nothing")
    .is_empty());
}

#[tokio::test]
async fn paper_path_prices_at_its_labeled_default_spread() {
    let bars = history();
    let paper = PaperVenue::new(PaperConfig::new().build());
    let decisions = run_paper_decisions(
        &strategy(),
        &risk(),
        &grant_with(0.5),
        &paper,
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("the paper run decides");
    let labeled = BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("paper-synthetic".to_owned())
        .build();
    let mut checked = 0;
    for decision in &decisions {
        if decision.submitted {
            let touch = quote_for_bar(&bars[decision.bar_index], &labeled)
                .expect("quotable bar")
                .ask;
            assert_eq!(
                decision.checked_limit, touch,
                "paper fills must price at the labeled paper-synthetic default spread"
            );
            checked += 1;
        }
    }
    assert!(
        checked > 0,
        "liquid history must fill something on the paper path"
    );
}

fn transport_skipped(decision: &BarDecision) -> &str {
    decision.skipped.as_deref().unwrap_or("<submitted>")
}

#[tokio::test]
async fn breaker_trips_past_the_configured_max_and_disarms_entries() {
    let bars = history();
    let venue = PaperVenue::new(PaperConfig::new().build());
    // The shared submission protocol retries a retryable venue failure
    // `SUBMIT_RETRIES` times, so one failing bar burns that whole budget.
    // max_consecutive_errors is 3: the first three failing bars still arm
    // (each reaches the venue), the fourth trips, the rest disarm untouched.
    let attempts = SUBMIT_RETRIES as usize;
    for _ in 0..4 * attempts {
        venue.inject(Fault::DropConnection);
    }
    let decisions = run_paper_decisions(
        &strategy(),
        &risk(),
        &grant_with(0.5),
        &venue,
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("the paper run decides");
    for (index, decision) in decisions.iter().enumerate().take(4) {
        assert!(
            !decision.submitted,
            "bar {index} must fail on its injected fault"
        );
        let skipped = transport_skipped(decision);
        assert!(
            skipped.contains("connection dropped"),
            "bar {index} must bank the transport failure, got: {skipped:?}"
        );
        assert!(
            !skipped.contains("breaker"),
            "bar {index} (error {} of 3+1) must still be armed",
            index + 1
        );
        assert!(
            decision.risk_pass && decision.grant_merit,
            "merit still allows on bar {index}"
        );
    }
    for (index, decision) in decisions.iter().enumerate().skip(4) {
        assert!(
            !decision.submitted,
            "bar {index} must stay disarmed after the trip"
        );
        assert!(
            transport_skipped(decision).contains("breaker tripped"),
            "bar {index} must name the disarm, got: {:?}",
            decision.skipped
        );
        assert!(
            decision.risk_pass && decision.grant_merit,
            "bar {index}: disarm must not rewrite the merit verdict"
        );
    }
    assert_eq!(
        venue.pending_faults(),
        0,
        "the four failing bars burned the whole retry budget, and disarmed bars touch nothing"
    );
}

#[tokio::test]
async fn errors_capped_at_the_max_still_arm_and_reset_on_success() {
    let bars = history();
    let venue = PaperVenue::new(PaperConfig::new().build());
    // Each failing bar burns the protocol's whole retry budget.
    let attempts = SUBMIT_RETRIES as usize;
    for _ in 0..3 * attempts {
        venue.inject(Fault::DropConnection);
    }
    let decisions = run_paper_decisions(
        &strategy(),
        &risk(),
        &grant_with(0.5),
        &venue,
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("the paper run decides");
    for decision in decisions.iter().take(3) {
        assert!(transport_skipped(decision).contains("connection dropped"));
        assert!(
            !transport_skipped(decision).contains("breaker"),
            "exactly max errors must still arm"
        );
    }
    assert!(
        decisions.iter().any(|decision| decision.submitted),
        "the first clean bar must submit once the streak resets"
    );
    assert!(
        !decisions
            .iter()
            .any(|decision| transport_skipped(decision).contains("breaker")),
        "a run that never exceeds the max must never disarm"
    );
}

#[tokio::test]
async fn a_retryable_submission_failure_retains_its_budget_reservation() {
    let bars = history();
    let bar = &bars[0];
    let venue = PaperVenue::new(PaperConfig::new().build());
    // Burn the protocol's whole retry budget so the verdict is unknown.
    for _ in 0..SUBMIT_RETRIES {
        venue.inject(Fault::DropConnection);
    }
    let ledger = GrantLedger::new(grant_with(0.5));
    let closes = vec![bar.c.to_f64().expect("representable close")];
    let view = bare_view(&closes, bar.ts);
    let journal = Journal::ephemeral();
    let recorder = Recorder::ephemeral(&journal);
    let decision = decide_and_submit(
        &strategy(),
        &risk(),
        &ledger,
        &venue,
        &recorder,
        &venue_config(),
        BarHealth::rehearsed(),
        0,
        bar,
        &view,
        &runner_config(),
        11,
    )
    .await;
    assert!(!decision.submitted);
    assert!(transport_skipped(&decision).contains("connection dropped"));
    assert_eq!(
        ledger.usage().used_trades,
        0,
        "an unresolved verdict consumes no budget"
    );
    assert_eq!(
        ledger.pending().used_trades,
        1,
        "the reservation stays pending until query-by-ID resolves it"
    );
}

#[tokio::test]
async fn merit_denials_never_feed_the_breaker() {
    let bars = history();
    let venue = PaperVenue::new(PaperConfig::new().build());
    for _ in 0..2 {
        venue.inject(Fault::DropConnection);
    }
    let decisions = run_paper_decisions(
        &strategy(),
        &risk(),
        &grant_with(0.95),
        &venue,
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("the paper run decides");
    assert!(decisions.iter().all(|decision| !decision.submitted));
    assert_eq!(
        venue.pending_faults(),
        2,
        "merit-denied proposals must never reach the venue, so faults stay queued"
    );
    assert!(
        !decisions
            .iter()
            .any(|decision| transport_skipped(decision).contains("breaker")),
        "denials are task-value information, never producer doubt"
    );
}

/// The runner's decision path records each submitted proposal's intent in
/// the run-scoped in-memory log — the same protocol the durable journal runs,
/// with no file and no fsync.
#[tokio::test]
async fn runner_records_in_memory_intents_for_submitted_proposals() {
    let bars = history();
    let bar = &bars[0];
    let venue = PaperVenue::new(PaperConfig::new().build());
    let strategy = strategy();
    let ledger = GrantLedger::new(grant_with(0.5));
    let closes = vec![bar.c.to_f64().expect("representable close")];
    let view = bare_view(&closes, bar.ts);
    let journal = Journal::ephemeral();
    let recorder = Recorder::ephemeral(&journal);
    let decision = decide_and_submit(
        &strategy,
        &risk(),
        &ledger,
        &venue,
        &recorder,
        &venue_config(),
        BarHealth::rehearsed(),
        0,
        bar,
        &view,
        &runner_config(),
        11,
    )
    .await;
    assert!(decision.submitted, "liquid first bar must submit");
    let replayed = journal
        .replay(&ephemeral_permit())
        .expect("in-memory replay");
    assert_eq!(replayed.intents.len(), 1, "one intent recorded");
    assert!(
        replayed.events.iter().any(|event| event.kind == "intent"),
        "the protocol journals the intent before any venue call"
    );
    assert!(
        replayed.events.iter().any(|event| event.kind == "filled"),
        "the verdict lands in the run log"
    );
    assert_eq!(
        journal
            .intent_status(&ephemeral_permit(), &replayed.intents[0].client_id)
            .expect("status")
            .as_deref(),
        Some("filled")
    );
}

/// Every decided bar carries the lifecycle chain the decision actually
/// walked: contiguous, starting from `Proposed`, ending at the venue
/// outcome. A grant-covered entry auto-approves; a submitted order reaches
/// `Filled`.
#[tokio::test]
async fn decisions_carry_the_proposal_lifecycle() {
    let bars = history();
    let run = run_backtest(
        &strategy(),
        &risk(),
        &grant_with(0.5),
        &venue_config(),
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("run");
    let mut submitted = 0;
    for decision in &run.decisions {
        if decision.direction.is_none() {
            assert!(
                decision.transitions.is_empty(),
                "a bar with no proposal records no lifecycle"
            );
            continue;
        }
        let first = decision
            .transitions
            .first()
            .expect("a proposal has a chain");
        assert_eq!(first.from, ProposalState::Proposed);
        for pair in decision.transitions.windows(2) {
            assert_eq!(pair[0].to, pair[1].from, "the chain must be contiguous");
        }
        let last = decision.transitions.last().expect("chain");
        assert!(decision.submitted, "the permissive grant must submit");
        assert_eq!(last.to, ProposalState::Filled, "a filled order ends Filled");
        assert!(
            decision
                .transitions
                .iter()
                .any(|entry| entry.to == ProposalState::AutoApproved),
            "a grant-covered entry auto-approves"
        );
        assert_eq!(decision.strategy, "ribbon", "carried attribution");
        assert_eq!(decision.underlying, "SPY", "carried attribution");
        submitted += 1;
    }
    assert!(submitted > 0, "the fixture must submit something");
}

/// A grant that does not cover the proposal routes it to the human approval
/// queue, and the recorded chain says so — never a fabricated rejection.
#[tokio::test]
async fn uncovered_grant_requires_human_in_the_chain() {
    let bars = history();
    let run = run_backtest(
        &strategy(),
        &risk(),
        &grant_with(0.95),
        &venue_config(),
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("run");
    let mut pending = 0;
    for decision in &run.decisions {
        if decision.direction.is_none() {
            continue;
        }
        assert!(decision.risk_pass, "the risk engine approves");
        assert!(!decision.grant_merit, "the strict grant denies");
        assert!(!decision.submitted, "no uncovered entry is submitted");
        let last = decision.transitions.last().expect("chain");
        assert_eq!(last.to, ProposalState::PendingHuman);
        assert!(decision
            .transitions
            .iter()
            .any(|entry| entry.from == ProposalState::RiskChecked
                && entry.to == ProposalState::PendingHuman));
        pending += 1;
    }
    assert!(
        pending > 0,
        "the strict grant must leave pending-human rows"
    );
}

/// A risk refusal is a terminal `Rejected`, recorded as `FailRisk`.
#[tokio::test]
async fn risk_refusal_terminalizes_the_chain() {
    let bars = history();
    let bar = &bars[0];
    let venue = PaperVenue::new(PaperConfig::new().build());
    let ledger = GrantLedger::new(grant_with(0.5));
    let health = BarHealth {
        quote_age_secs: 3_600,
        clock_skew_secs: 0,
        reconciliation_healthy: true,
    };
    let closes = vec![bar.c.to_f64().expect("representable close")];
    let view = bare_view(&closes, bar.ts);
    let journal = Journal::ephemeral();
    let recorder = Recorder::ephemeral(&journal);
    let decision = decide_and_submit(
        &strategy(),
        &risk(),
        &ledger,
        &venue,
        &recorder,
        &venue_config(),
        health,
        0,
        bar,
        &view,
        &runner_config(),
        11,
    )
    .await;
    assert!(!decision.risk_pass);
    let last = decision.transitions.last().expect("chain");
    assert_eq!(last.to, ProposalState::Rejected);
    assert_eq!(decision.transitions.len(), 1, "a risk refusal is one move");
    assert_eq!(decision.transitions[0].from, ProposalState::Proposed);
    assert_eq!(decision.transitions[0].to, ProposalState::Rejected);
}

/// A filled submission walks `AutoApproved -> Submitted -> Working -> Filled`,
/// and the merit outcome never trips the confidence breaker.
#[tokio::test]
async fn submitted_chain_walks_to_filled_without_tripping_the_breaker() {
    let bars = history();
    let bar = &bars[0];
    let venue = PaperVenue::new(PaperConfig::new().build());
    let ledger = GrantLedger::new(grant_with(0.5));
    let closes = vec![bar.c.to_f64().expect("representable close")];
    let view = bare_view(&closes, bar.ts);
    let journal = Journal::ephemeral();
    let recorder = Recorder::ephemeral(&journal);
    let decision = decide_and_submit(
        &strategy(),
        &risk(),
        &ledger,
        &venue,
        &recorder,
        &venue_config(),
        BarHealth::rehearsed(),
        0,
        bar,
        &view,
        &runner_config(),
        11,
    )
    .await;
    assert!(decision.submitted, "the liquid first bar fills");
    let states: Vec<ProposalState> = decision.transitions.iter().map(|entry| entry.to).collect();
    assert_eq!(
        states,
        vec![
            ProposalState::RiskChecked,
            ProposalState::AutoApproved,
            ProposalState::Submitted,
            ProposalState::Working,
            ProposalState::Filled,
        ],
        "the filled chain walks one legal move per state"
    );
    assert!(
        decision
            .transitions
            .iter()
            .all(|entry| entry.proposal_id == decision.proposal_id),
        "every transition names this bar's proposal"
    );
    assert!(
        !ledger.breakers().tripped,
        "a merit outcome never trips the breaker"
    );
}

#[tokio::test]
async fn stale_quote_refuses_on_risk_and_grant_paths() {
    let bars = history();
    let bar = &bars[0];
    let venue = PaperVenue::new(PaperConfig::new().build());
    let strategy = strategy();
    let ledger = GrantLedger::new(grant_with(0.5));
    let health = BarHealth {
        quote_age_secs: 3_600,
        clock_skew_secs: 0,
        reconciliation_healthy: true,
    };
    let closes = vec![bar.c.to_f64().expect("representable close")];
    let view = bare_view(&closes, bar.ts);
    let journal = Journal::ephemeral();
    let recorder = Recorder::ephemeral(&journal);
    let decision = decide_and_submit(
        &strategy,
        &risk(),
        &ledger,
        &venue,
        &recorder,
        &venue_config(),
        health,
        0,
        bar,
        &view,
        &runner_config(),
        11,
    )
    .await;
    assert!(!decision.risk_pass, "stale data must fail the risk gate");
    assert!(
        decision
            .risk_reasons
            .iter()
            .any(|reason| reason.contains("StaleQuote")),
        "risk refusal names the gate, got: {:?}",
        decision.risk_reasons
    );
    assert!(!decision.grant_merit, "stale data must fail the grant gate");
    assert!(
        decision
            .grant_merit_reasons
            .iter()
            .any(|reason| reason.contains("stale")),
        "grant refusal names the gate, got: {:?}",
        decision.grant_merit_reasons
    );
    assert!(!decision.submitted);
    assert!(
        venue.orders().is_empty(),
        "refused bars never reach the venue"
    );
}

#[tokio::test]
async fn tripped_breaker_disarms_entries_without_touching_merit_verdicts() {
    let bars = history();
    let bar = &bars[0];
    let venue = PaperVenue::new(PaperConfig::new().build());
    let strategy = strategy();
    let ledger = GrantLedger::with_breakers(
        grant_with(0.5),
        BreakerState::new()
            .tripped(true)
            .trip_reason("test trip".to_owned())
            .build(),
    );
    let closes = vec![bar.c.to_f64().expect("representable close")];
    let view = bare_view(&closes, bar.ts);
    let journal = Journal::ephemeral();
    let recorder = Recorder::ephemeral(&journal);
    let decision = decide_and_submit(
        &strategy,
        &risk(),
        &ledger,
        &venue,
        &recorder,
        &venue_config(),
        BarHealth::rehearsed(),
        0,
        bar,
        &view,
        &runner_config(),
        11,
    )
    .await;
    assert!(
        decision.risk_pass,
        "merit verdict must stay Allow while tripped"
    );
    assert!(
        decision.grant_merit,
        "merit verdict must stay Allow while tripped"
    );
    assert!(decision.risk_reasons.is_empty() && decision.grant_merit_reasons.is_empty());
    assert!(!decision.submitted, "tripped entries disarm");
    assert!(
        transport_skipped(&decision).contains("breaker tripped"),
        "disarm names its cause, got: {:?}",
        decision.skipped
    );
    assert!(
        venue.orders().is_empty(),
        "disarmed bars never reach the venue"
    );
}

/// A hand-built proposal for exercising one side of the lifecycle directly.
fn manual_proposal(side: ProposalSide, bar: &Bar) -> Proposal {
    Proposal::new()
        .id("manual-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(Direction::Long)
        .side(side)
        .structure(StructureKind::Vertical)
        .quantity(Qty::new(Decimal::from(1)))
        .limit_price(money(694))
        .max_loss(money(300))
        .notional(money(1_000))
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.7)
        .created_at(bar.ts)
        .build()
}

fn tripped_breaker() -> BreakerState {
    BreakerState::new()
        .tripped(true)
        .trip_reason("test trip".to_owned())
        .build()
}

#[tokio::test]
async fn a_tripped_breaker_denies_entries_but_routes_exits() {
    let bars = history();
    let bar = &bars[0];
    let venue = PaperVenue::new(PaperConfig::new().build());
    let quote = bar_quote(bar, &venue_config()).expect("quoted bar");

    let entry_ledger = GrantLedger::with_breakers(grant_with(0.5), tripped_breaker());
    let journal = Journal::ephemeral();
    let recorder = Recorder::ephemeral(&journal);
    let entry = submit_decided(
        manual_proposal(ProposalSide::Entry, bar),
        quote,
        &risk(),
        &entry_ledger,
        &venue,
        &recorder,
        BarHealth::rehearsed(),
        None,
        0,
        bar,
        &runner_config(),
    )
    .await;
    assert!(entry.grant_merit, "a trip never rewrites merit");
    assert!(!entry.grant_authorized, "a trip denies entry authorization");
    assert!(!entry.submitted);
    assert!(
        entry
            .grant_authorization_reasons
            .iter()
            .any(|reason| reason.contains("disarmed")),
        "authorization names the trip, got: {:?}",
        entry.grant_authorization_reasons
    );

    let exit_ledger = GrantLedger::with_breakers(grant_with(0.5), tripped_breaker());
    let journal = Journal::ephemeral();
    let recorder = Recorder::ephemeral(&journal);
    let exit = submit_decided(
        manual_proposal(ProposalSide::Exit, bar),
        quote,
        &risk(),
        &exit_ledger,
        &venue,
        &recorder,
        BarHealth::rehearsed(),
        None,
        0,
        bar,
        &runner_config(),
    )
    .await;
    assert!(exit.grant_merit, "an exit keeps its merit");
    assert!(exit.grant_authorized, "exits route through a trip");
    assert!(exit.submitted, "the exit reaches the venue");
}

#[tokio::test]
async fn an_exhausted_budget_denies_merit_and_authorization() {
    let bars = history();
    let bar = &bars[0];
    let venue = PaperVenue::new(PaperConfig::new().build());
    let quote = bar_quote(bar, &venue_config()).expect("quoted bar");
    let market = GrantMarket::new()
        .quote_age_secs(1)
        .spread_width(money(10))
        .build();
    let mut grant = grant_with(0.5);
    grant.budgets.max_trades = 1;
    let ledger = GrantLedger::new(grant);
    // Hold the grant's only trade reserved across the decision.
    let held = ledger
        .reserve(&manual_proposal(ProposalSide::Entry, bar), &market, bar.ts)
        .expect("the last trade reserves");

    let journal = Journal::ephemeral();
    let recorder = Recorder::ephemeral(&journal);
    let decision = submit_decided(
        manual_proposal(ProposalSide::Entry, bar),
        quote,
        &risk(),
        &ledger,
        &venue,
        &recorder,
        BarHealth::rehearsed(),
        None,
        0,
        bar,
        &runner_config(),
    )
    .await;
    assert!(!decision.grant_merit, "the budget binds merit");
    assert!(!decision.grant_authorized, "and authorization");
    assert!(decision
        .grant_merit_reasons
        .iter()
        .any(|reason| reason.contains("trade budget exhausted")));
    assert!(!decision.submitted);
    assert!(venue.orders().is_empty());
    drop(held);
}

struct TripCase {
    label: &'static str,
    faults: Vec<Fault>,
    seed: u64,
    should_trip: bool,
}

fn trip_observed(decisions: &[BarDecision]) -> bool {
    decisions.iter().any(|decision| {
        decision
            .skipped
            .as_deref()
            .unwrap_or("")
            .contains("breaker tripped")
    })
}

#[tokio::test]
#[ignore = "soak: 9-case breaker trip/no-trip matrix with calibration artifact; the disarm path is pinned by smoke tests every run; run via `make test-heavy`"]
async fn soak_breaker_trip_precision_recall_with_clean_control() {
    // Soak gate: the breaker must trip exactly when consecutive producer
    // failures exceed the bound — never on clean runs, never at-or-below
    // the max — with merit verdicts intact on every disarmed bar. Scores
    // and labels feed the shared calibration harness; the table lands under
    // `target/calibration/` for the audit trail.
    let cases = vec![
        TripCase {
            label: "clean-11",
            faults: vec![],
            seed: 11,
            should_trip: false,
        },
        TripCase {
            label: "clean-12",
            faults: vec![],
            seed: 12,
            should_trip: false,
        },
        // Each failing bar burns the shared protocol's whole retry budget.
        TripCase {
            label: "at-max-3-drops",
            faults: vec![Fault::DropConnection; 3 * SUBMIT_RETRIES as usize],
            seed: 11,
            should_trip: false,
        },
        TripCase {
            label: "over-max-4-drops",
            faults: vec![Fault::DropConnection; 4 * SUBMIT_RETRIES as usize],
            seed: 11,
            should_trip: true,
        },
        TripCase {
            label: "over-max-8-drops",
            faults: vec![Fault::DropConnection; 8 * SUBMIT_RETRIES as usize],
            seed: 11,
            should_trip: true,
        },
        TripCase {
            label: "over-max-4-status500",
            faults: vec![Fault::Status500; 4 * SUBMIT_RETRIES as usize],
            seed: 11,
            should_trip: true,
        },
        TripCase {
            label: "over-max-mixed",
            faults: [Fault::DropConnection, Fault::Status500]
                .into_iter()
                .cycle()
                .take(4 * SUBMIT_RETRIES as usize)
                .collect(),
            seed: 11,
            should_trip: true,
        },
        // A lost acknowledgement is not a sustained failure: the protocol's
        // query-by-ID recovers the fill, so the breaker never trips.
        TripCase {
            label: "recovered-lost-acks",
            faults: vec![Fault::LoseAck; 4],
            seed: 11,
            should_trip: false,
        },
    ];
    let bars = history();
    let mut scores: Vec<(f64, bool)> = Vec::with_capacity(cases.len());
    for case in &cases {
        let venue = PaperVenue::new(PaperConfig::new().build());
        for fault in &case.faults {
            venue.inject(*fault);
        }
        let decisions = run_paper_decisions(
            &strategy(),
            &risk(),
            &grant_with(0.5),
            &venue,
            &runner_config(),
            &bars,
            case.seed,
        )
        .await
        .expect("the paper run decides");
        let tripped = trip_observed(&decisions);
        assert_eq!(
            tripped,
            case.should_trip,
            "case {} must {}trip",
            case.label,
            if case.should_trip { "" } else { "not " }
        );
        if case.should_trip {
            assert!(
                decisions.iter().all(|d| d.risk_pass && d.grant_merit),
                "case {}: disarm must never rewrite a merit verdict",
                case.label
            );
            assert!(
                decisions.iter().all(|d| !d.submitted),
                "case {}: nothing submits once producer health fails",
                case.label
            );
        } else {
            assert!(
                decisions.iter().any(|d| d.submitted),
                "case {} (clean control) must keep trading",
                case.label
            );
        }
        scores.push((if tripped { 1.0 } else { 0.0 }, case.should_trip));
    }
    let reports = common_core::calibration::sweep_thresholds(
        &scores,
        |(score, _)| *score,
        |(_, label)| *label,
        &[0.0, 0.5, 1.0],
    );
    common_core::calibration::emit_markdown_artifact("protopyne-breaker-trip", &reports);
    let operating = &reports[1].1;
    assert_eq!(operating.support, cases.len());
    assert!(
        (operating.precision - 1.0).abs() < f64::EPSILON,
        "no false trips: {operating:?}"
    );
    assert!(
        (operating.recall - 1.0).abs() < f64::EPSILON,
        "no missed trips: {operating:?}"
    );
    assert!(
        (operating.fpr - 0.0).abs() < f64::EPSILON,
        "clean controls never fire: {operating:?}"
    );
    assert!(
        operating.passes_gate(),
        "trip detector passes the shared gate: {operating:?}"
    );
}

#[tokio::test]
#[ignore = "soak: full-run temporal-marking and exact-fee audit; pre-fill flatness is pinned by smoke tests every run; run via `make test-heavy`"]
async fn soak_real_run_marks_temporally_with_exact_fees() {
    // Re-soak under the corrected equity and quote-config paths: a real run
    // over history must mark pre-fill bars at zero, price the first fill at
    // the configured touch, and book exact per-contract fees.
    let bars = history();
    let run = run_backtest(
        &strategy(),
        &risk(),
        &grant_with(0.5),
        &venue_config(),
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("soak run");
    let first_fill = run
        .decisions
        .iter()
        .position(|decision| decision.submitted)
        .expect("liquid history must fill something");
    for (index, mark) in run.report.equity_curve.iter().enumerate().take(first_fill) {
        assert_eq!(
            mark.as_decimal(),
            Decimal::ZERO,
            "bar {index} precedes the first fill and must read zero"
        );
    }
    let first = &run.decisions[first_fill];
    let price = first.fill_price.expect("priced").as_decimal();
    let fee = first.fee.expect("fee").as_decimal();
    let qty = Decimal::from(first.filled_quantity);
    // Cash after the first (long) fill plus the open position marked at
    // that bar's close: -price*qty - fee + qty*close.
    let expected = -(price * qty) - fee + qty * bars[first_fill].c;
    assert_eq!(
        run.report.equity_curve[first_fill].as_decimal(),
        expected,
        "first fill marks cash plus position at its own close"
    );
    let filled_contracts: u32 = run
        .decisions
        .iter()
        .map(|decision| decision.filled_quantity)
        .sum();
    let expected_fees = Decimal::new(65, 2) * Decimal::from(filled_contracts);
    assert_eq!(
        run.report.total_fees.as_decimal(),
        expected_fees,
        "every filled contract books exactly 0.65 commission"
    );
    assert!(filled_contracts > 0);
}

#[tokio::test]
async fn report_states_dividend_exclusion_next_to_pnl() {
    // Cash dividends are not tracked in positions or portfolio: the report
    // must say so beside the numbers, never silently omit them.
    let bars = history();
    let run = run_backtest(
        &strategy(),
        &risk(),
        &grant_with(0.5),
        &venue_config(),
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("soak run");
    assert!(
        run.report
            .data_limits
            .iter()
            .any(|line| line.contains("dividends excluded")),
        "exclusion sits beside P&L, got: {:?}",
        run.report.data_limits
    );
}

fn barrier_bars(spec: &[(f64, f64, f64)]) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let (mut closes, mut highs, mut lows) = (Vec::new(), Vec::new(), Vec::new());
    for (c, h, l) in spec {
        closes.push(*c);
        highs.push(*h);
        lows.push(*l);
    }
    (closes, highs, lows)
}

#[test]
fn triple_barrier_touch_paths() {
    use crate::runner::triple_barrier;
    // The reference docstring example: NaN entry skips, take-profit hits,
    // stop-loss hits, and the trailing bar has no window left.
    let (closes, highs, lows) = barrier_bars(&[
        (f64::NAN, f64::NAN, f64::NAN),
        (1.0, 1.0, 1.0),
        (1.0, 1.1, 1.0),
        (1.0, 1.0, 0.95),
    ]);
    assert_eq!(
        triple_barrier(&closes, &highs, &lows, 2, 0.1, 0.05),
        vec![None, Some(1.0), Some(-1.0), None]
    );
    // Timeout settles on the horizon bar's signed drift, not zero.
    let (closes, highs, lows) = barrier_bars(&[
        (100.0, 101.0, 99.0),
        (102.0, 103.0, 101.0),
        (101.0, 102.0, 100.0),
    ]);
    assert_eq!(
        triple_barrier(&closes, &highs, &lows, 2, 0.5, 0.5),
        vec![Some(1.0), Some(-1.0), None]
    );
    // Both barriers on one bar: the farther-from-close side wins.
    let (closes, highs, lows) = barrier_bars(&[(100.0, 100.0, 100.0), (100.0, 112.0, 96.0)]);
    assert_eq!(
        triple_barrier(&closes, &highs, &lows, 2, 0.1, 0.05),
        vec![Some(1.0), None]
    );
    let (closes, highs, lows) = barrier_bars(&[(100.0, 100.0, 100.0), (100.0, 104.0, 90.0)]);
    assert_eq!(
        triple_barrier(&closes, &highs, &lows, 2, 0.1, 0.05),
        vec![Some(-1.0), None]
    );
    // Flat market never touches a barrier: every label expires at zero.
    let (closes, highs, lows) = barrier_bars(&[(50.0, 50.0, 50.0); 6]);
    assert_eq!(
        triple_barrier(&closes, &highs, &lows, 5, 0.1, 0.05),
        vec![Some(0.0), Some(0.0), None, None, None, None]
    );
    // Degenerate windows refuse instead of indexing out of bounds.
    assert!(triple_barrier(&closes, &highs, &lows, 1, 0.1, 0.05)
        .iter()
        .all(Option::is_none));
    assert!(triple_barrier(&closes, &highs, &lows, 0, 0.1, 0.05)
        .iter()
        .all(Option::is_none));
}

fn rsrs_history(n: usize) -> Vec<Bar> {
    use chrono::TimeZone as _;
    let price = |x: f64| rust_decimal::Decimal::from_f64_retain(x).expect("finite test price");
    (0..n)
        .map(|i| {
            let k = f64::from(i as i32);
            let close = 100.0 + k * 0.2 + (k * 0.7).sin() * 2.0;
            let spread = 0.5 + (k * 0.3).cos().abs() * 2.0;
            Bar::new()
                .ts(chrono::Utc
                    .with_ymd_and_hms(2026, 1, 2, 9, 0, 0)
                    .single()
                    .expect("ts")
                    + chrono::Duration::minutes(k as i64))
                .o(price(close))
                .h(price(close + spread))
                .l(price(close - spread))
                .c(price(close))
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

/// The stream-breaker wire: consecutive transport errors trip the shared
/// stream breaker, the trip merges into `BreakerState` through `apply_to`,
/// and the existing tripped path disarms entries with merit verdicts
/// intact. No runner logic changes — the merge is the wire.
#[tokio::test]
async fn stream_breaker_trip_disarms_entries_through_breaker_state() {
    let stream = SupervisedStream::new(StreamConfig::new().max_consecutive_errors(2).build());
    for _ in 0..3 {
        stream
            .breaker()
            .record_error(&TransportError::Transient("unavailable: storm".to_owned()));
    }
    assert!(
        stream.breaker().tripped(),
        "the storm trips the stream breaker"
    );
    let mut breakers = BreakerState::new().build();
    assert!(!breakers.tripped);
    stream.breaker().apply_to(&mut breakers);
    assert!(breakers.tripped, "the trip merges into BreakerState");
    assert!(
        breakers
            .trip_reason
            .as_deref()
            .unwrap_or("")
            .contains("consecutive"),
        "the merged reason names the budget, got: {:?}",
        breakers.trip_reason
    );

    let bars = history();
    let bar = &bars[0];
    let venue = PaperVenue::new(PaperConfig::new().build());
    let strategy = strategy();
    let ledger = GrantLedger::with_breakers(grant_with(0.5), breakers);
    let closes = vec![bar.c.to_f64().expect("representable close")];
    let view = bare_view(&closes, bar.ts);
    let journal = Journal::ephemeral();
    let recorder = Recorder::ephemeral(&journal);
    let decision = decide_and_submit(
        &strategy,
        &risk(),
        &ledger,
        &venue,
        &recorder,
        &venue_config(),
        BarHealth::rehearsed(),
        0,
        bar,
        &view,
        &runner_config(),
        11,
    )
    .await;
    assert!(
        decision.risk_pass,
        "merit verdict must stay Allow while tripped"
    );
    assert!(
        decision.grant_merit,
        "merit verdict must stay Allow while tripped"
    );
    assert!(!decision.submitted, "stream-tripped entries disarm");
    assert!(
        transport_skipped(&decision).contains("breaker tripped"),
        "disarm names its cause, got: {:?}",
        decision.skipped
    );
    assert!(
        venue.orders().is_empty(),
        "disarmed bars never reach the venue"
    );
}

#[test]
fn report_rsrs_is_honest_about_history_length() {
    use crate::runner::report_rsrs;
    // Under the default 600-bar scoring window there is nothing to score.
    assert_eq!(report_rsrs(&history()), None);
    assert_eq!(report_rsrs(&rsrs_history(100)), None);
    // A full window on varying data scores finite values.
    assert!(report_rsrs(&rsrs_history(650)).is_some());
}

/// Rising synthetic tape for the second symbol: no second CSV ships, so the
/// harness proves the dynamic source beside the file path.
fn rising_bars(start: &str, count: usize) -> Vec<Bar> {
    let start = start.parse::<Decimal>().expect("decimal");
    (0..count)
        .map(|step| {
            let close = start + Decimal::from(step as u64);
            Bar::new()
                .ts(Utc
                    .timestamp_opt(1_767_344_400 + step as i64 * 900, 0)
                    .unwrap())
                .o(close)
                .h(close)
                .l(close)
                .c(close)
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

fn grant_covering(underlyings: &[&str]) -> Grant {
    let mut grant = grant_with(0.5);
    grant.scope.underlyings = underlyings.iter().map(|name| (*name).to_owned()).collect();
    grant
}

fn multi_input(
    underlying: &str,
    data_version: &str,
    history: crate::backtest::HistorySource,
) -> crate::runner::MultiSymbolInput {
    crate::runner::MultiSymbolInput::new()
        .venue_config(
            BacktestConfig::new()
                .underlying(underlying.to_owned())
                .data_version(data_version.to_owned())
                .build(),
        )
        .runner_config(RunnerConfig::new(
            underlying.to_owned(),
            "ribbon".to_owned(),
            money(400),
            money(2_000),
            30,
            0,
        ))
        .history(history)
        .build()
}

#[tokio::test]
async fn multi_symbol_run_matches_single_symbol_runs() {
    use crate::backtest::HistorySource;

    // One symbol from the CSV path, one from the dynamic path: the harness
    // must add no perturbation — per-symbol decisions equal standalone
    // `run_backtest` runs on the same inputs, and the combined manifest pins
    // each symbol's data version beside the shared code version.
    let spy_bars = crate::csv::load_bars_csv(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("data")
            .join("SPY_15M.csv"),
    )
    .expect("vendored history loads");
    let qqq_bars = rising_bars("500", spy_bars.len());
    let grant = grant_covering(&["SPY", "QQQ"]);
    let inputs = vec![
        multi_input(
            "SPY",
            "spy-15m-v1",
            HistorySource::Dynamic(spy_bars.clone()),
        ),
        multi_input(
            "QQQ",
            "qqq-synth-v1",
            HistorySource::Dynamic(qqq_bars.clone()),
        ),
    ];
    let paper = PaperConfig::new().build();
    let multi =
        crate::runner::run_multi_backtest(&strategy(), &risk(), &grant, paper.clone(), inputs, 11)
            .await
            .expect("multi-symbol harness runs");
    assert_eq!(multi.runs.len(), 2);
    assert_eq!(multi.manifest.symbols.len(), 2);
    assert_eq!(multi.manifest.symbols[0].data_version, "spy-15m-v1");
    assert_eq!(multi.manifest.symbols[1].data_version, "qqq-synth-v1");
    assert_eq!(multi.manifest.code_version, env!("CARGO_PKG_VERSION"));

    for (run, (underlying, bars)) in multi
        .runs
        .iter()
        .zip([("SPY", &spy_bars), ("QQQ", &qqq_bars)])
    {
        let venue_config = BacktestConfig::new()
            .underlying((*underlying).to_owned())
            .data_version(format!("{underlying}-pin"))
            .build();
        let runner_config = RunnerConfig::new(
            (*underlying).to_owned(),
            "ribbon".to_owned(),
            money(400),
            money(2_000),
            30,
            0,
        );
        let single = run_backtest(
            &strategy(),
            &risk(),
            &grant,
            &venue_config,
            &runner_config,
            bars,
            11,
        )
        .await
        .expect("standalone run");
        // Config JSON pins input configs, so only decisions must match
        // exactly — the same decide path over the same bars.
        assert_eq!(
            run.decisions, single.decisions,
            "{underlying} decisions identical"
        );
        assert!(
            run.decisions.iter().any(|decision| decision.submitted),
            "{underlying} fills"
        );
    }

    // Rerun equality over the combined pin.
    let again = crate::runner::run_multi_backtest(
        &strategy(),
        &risk(),
        &grant,
        paper,
        vec![
            multi_input("SPY", "spy-15m-v1", HistorySource::Dynamic(spy_bars)),
            multi_input("QQQ", "qqq-synth-v1", HistorySource::Dynamic(qqq_bars)),
        ],
        11,
    )
    .await
    .expect("second harness run");
    assert_eq!(
        multi.manifest.to_canonical_json(),
        again.manifest.to_canonical_json()
    );
}

#[tokio::test]
async fn multi_symbol_reruns_are_identical_under_parallelism() {
    // The per-symbol legs fan out over a bounded pool: reruns must still be
    // identical wholesale (outcomes re-sorted by input index before return,
    // so completion order never leaks into results), and per-symbol
    // decisions must still equal standalone runs bit for bit.
    use crate::backtest::HistorySource;

    let spy_bars = history();
    let qqq_bars = rising_bars("500", spy_bars.len());
    let grant = grant_covering(&["SPY", "QQQ"]);
    let paper = PaperConfig::new().build();
    let inputs = || {
        vec![
            multi_input(
                "SPY",
                "spy-15m-v1",
                HistorySource::Dynamic(spy_bars.clone()),
            ),
            multi_input(
                "QQQ",
                "qqq-synth-v1",
                HistorySource::Dynamic(qqq_bars.clone()),
            ),
        ]
    };
    let first = crate::runner::run_multi_backtest(
        &strategy(),
        &risk(),
        &grant,
        paper.clone(),
        inputs(),
        11,
    )
    .await
    .expect("first harness run");
    let second =
        crate::runner::run_multi_backtest(&strategy(), &risk(), &grant, paper, inputs(), 11)
            .await
            .expect("second harness run");
    assert_eq!(first, second, "parallel reruns identical wholesale");
    assert_eq!(
        first.manifest.to_canonical_json(),
        second.manifest.to_canonical_json(),
        "combined pin byte-identical"
    );
    for (run, (underlying, bars)) in first
        .runs
        .iter()
        .zip([("SPY", &spy_bars), ("QQQ", &qqq_bars)])
    {
        let venue_config = BacktestConfig::new()
            .underlying((*underlying).to_owned())
            .data_version(format!("{underlying}-pin"))
            .build();
        let runner_config = RunnerConfig::new(
            (*underlying).to_owned(),
            "ribbon".to_owned(),
            money(400),
            money(2_000),
            30,
            0,
        );
        let single = run_backtest(
            &strategy(),
            &risk(),
            &grant,
            &venue_config,
            &runner_config,
            bars,
            11,
        )
        .await
        .expect("standalone run");
        assert_eq!(
            run.decisions, single.decisions,
            "{underlying} decisions identical"
        );
    }
}

#[tokio::test]
async fn sweep_drains_more_variants_than_workers() {
    // Workers clamp at 8: twelve variants oversubscribe the pool and must
    // still all complete, sorted by index — the exactly-sized queue never
    // observes `Full`, no matter the oversubscription.
    let bars = history();
    let grants: Vec<Grant> = (0..12)
        .map(|variant| grant_with(0.4 + f64::from(variant) * 0.01))
        .collect();
    let outcomes = run_sweep(
        strategy(),
        RiskLimits::new().build(),
        &venue_config(),
        &runner_config(),
        &bars,
        grants,
        11,
    )
    .await
    .expect("oversubscribed sweep drains");
    assert_eq!(outcomes.len(), 12);
    for (index, outcome) in outcomes.iter().enumerate() {
        assert_eq!(outcome.index, index, "sweep outcomes return in index order");
    }
}

#[tokio::test]
async fn bench_workload_smoke() {
    // Smoke for the criterion benches below: the tiny synthetic workload
    // (decide path plus a two-variant sweep) must execute without the
    // criterion harness, so a broken fixture fails here, not in `cargo bench`.
    let bars = rising_bars("600", 32);
    let config = BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("bench-smoke".to_owned())
        .build();
    let runner = RunnerConfig::new(
        "SPY".to_owned(),
        "ribbon".to_owned(),
        money(400),
        money(2_000),
        0,
        0,
    );
    let run = run_backtest(
        &strategy(),
        &risk(),
        &grant_covering(&["SPY"]),
        &config,
        &runner,
        &bars,
        3,
    )
    .await
    .expect("tiny decide-path run executes");
    assert_eq!(run.decisions.len(), bars.len());
    let outcomes = run_sweep(
        strategy(),
        RiskLimits::new().build(),
        &config,
        &runner,
        &bars,
        vec![grant_covering(&["SPY"]), grant_covering(&["SPY"])],
        3,
    )
    .await
    .expect("tiny sweep executes");
    assert_eq!(outcomes.len(), 2);
}

#[tokio::test]
async fn run_report_carries_decision_consistent_portfolio() {
    use crate::portfolio::PAPER_PORTFOLIO;

    let bars = history();
    let run = run_backtest(
        &strategy(),
        &risk(),
        &grant_with(0.5),
        &venue_config(),
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("run with fills");
    let summary = run
        .report
        .portfolio
        .as_ref()
        .expect("run attaches a portfolio");
    assert_eq!(summary.portfolios.len(), 1, "one run books one portfolio");
    let totals = &summary.portfolios[0];
    assert_eq!(totals.portfolio, PAPER_PORTFOLIO);
    // The fold books exactly the priced submitted fills: same count, same
    // fees, and the same curve length as the bar tape it marked.
    let priced: Vec<&crate::runner::BarDecision> = run
        .decisions
        .iter()
        .filter(|decision| {
            decision.submitted && decision.filled_quantity > 0 && decision.fill_price.is_some()
        })
        .collect();
    assert!(!priced.is_empty(), "liquid history must fill something");
    assert_eq!(totals.fills, priced.len() as u64);
    let decision_fees = priced
        .iter()
        .filter_map(|decision| decision.fee)
        .fold(Decimal::ZERO, |sum, fee| sum + fee.as_decimal());
    assert_eq!(totals.fees, Money::new(decision_fees));
    assert_eq!(summary.unrealized_curve.len(), bars.len());
    // Single strategy, single grant, single structure: one bucket, and the
    // totals are exactly its numbers (no second book to roll up).
    assert_eq!(totals.buckets.len(), 1);
    let bucket = &totals.buckets[0];
    assert_eq!(bucket.key.underlying, "SPY");
    assert_eq!(bucket.key.strategy, "ribbon");
    assert_eq!(totals.pnl_realised, bucket.pnl_realised);
    assert_eq!(totals.pnl_unrealised, bucket.pnl_unrealised);
    // The tearsheet renders from these same structs: it must name the pin,
    // the portfolio, and the unavailable greeks — never an empty page.
    let sheet = crate::report::render_tearsheet(&run.report);
    assert!(sheet.contains("spy-15m"), "pin present, got:\n{sheet}");
    assert!(
        sheet.contains(PAPER_PORTFOLIO),
        "portfolio present, got:\n{sheet}"
    );
    assert!(
        sheet.contains("unavailable"),
        "greeks honesty present, got:\n{sheet}"
    );
}

#[tokio::test]
async fn fixed_history_run_records_its_shape_for_efficiency_work() {
    let bars = history();
    let run = run_backtest(
        &strategy(),
        &risk(),
        &grant_with(0.5),
        &venue_config(),
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("fixed history runs");
    assert_eq!(
        run.decisions.len(),
        bars.len(),
        "one verdict per replayed bar"
    );
    let canonical = run.to_canonical_json();
    let sheet = crate::report::render_tearsheet(&run.report);
    eprintln!(
        "backtest baseline: bars={} decisions={} fills={} canonical_bytes={} tearsheet_bytes={} equity_points={}",
        bars.len(),
        run.decisions.len(),
        run.report.fills,
        canonical.len(),
        sheet.len(),
        run.report.equity_curve.len(),
    );
    assert!(run.report.fills > 0, "liquid fixture must fill something");
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join(name)
}

#[tokio::test]
async fn backtest_output_identity_pins_report_and_tearsheet() {
    // Frozen-output golden: the fixed-history report JSON and tearsheet must
    // match byte-for-byte. The decisions now carry the real proposal
    // lifecycle and attribution, so this golden was re-baselined once for
    // that stated change; after that, any move is a revert, never an
    // accepted diff. Regenerate only with `OVERWRITE_GOLDENS=1` on reviewed
    // code, exactly as the route-payload goldens do.
    let bars = history();
    let run = run_backtest(
        &strategy(),
        &risk(),
        &grant_with(0.5),
        &venue_config(),
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("fixed history runs");
    let canonical = run.to_canonical_json();
    let sheet = crate::report::render_tearsheet(&run.report);
    if std::env::var("OVERWRITE_GOLDENS").is_ok() {
        std::fs::write(golden_path("backtest_identity_canonical.json"), &canonical)
            .expect("canonical golden writes");
        std::fs::write(golden_path("backtest_identity_tearsheet.txt"), &sheet)
            .expect("tearsheet golden writes");
        return;
    }
    let frozen_canonical = std::fs::read_to_string(golden_path("backtest_identity_canonical.json"))
        .expect("identity golden loads");
    let frozen_sheet = std::fs::read_to_string(golden_path("backtest_identity_tearsheet.txt"))
        .expect("tearsheet golden loads");
    assert_eq!(canonical, frozen_canonical, "report JSON byte-identical");
    assert_eq!(sheet, frozen_sheet, "tearsheet byte-identical");
}

#[test]
fn per_bar_clone_sites_are_gone() {
    // Allocation pin, grep-level by design (no timing assertions — they
    // flake): the backtest hot path must carry no per-bar `closes.clone()`
    // and no per-construction history `Vec` clone. The only history copies
    // left are the two sanctioned owned constructions from borrowed inputs
    // (single-run venue history, sweep-shared history); the Dynamic arm's
    // borrowed-source copy is the single sanctioned one in `backtest.rs`.
    const RUNNER_SRC: &str = include_str!("../src/runner.rs");
    const BACKTEST_SRC: &str = include_str!("../src/backtest.rs");
    assert!(
        !RUNNER_SRC.contains("closes.clone()"),
        "no per-bar closes clone survives on the decide path"
    );
    assert!(
        !RUNNER_SRC.contains("job.bars.clone()"),
        "multi-symbol legs move their bars into the venue"
    );
    assert!(
        !RUNNER_SRC.contains("bars.clone()"),
        "loaded histories move into the venue, never clone"
    );
    assert_eq!(
        RUNNER_SRC.matches("bars.to_vec()").count(),
        2,
        "exactly the two sanctioned owned constructions (run venue, sweep share)"
    );
    assert_eq!(
        BACKTEST_SRC.matches("bars.clone()").count(),
        1,
        "only the Dynamic arm copies its borrowed source"
    );
}

/// A disarmed bar terminalizes instead of parking: the chain ends
/// `Cancelled` with the disarm reason journaled, so the served inbox state
/// is terminal and no human waits on a proposal the run will never submit.
#[tokio::test]
async fn disarmed_entries_terminalize_as_cancelled() {
    use protopyne_strategy::ProposalEvent;
    let bars = history();
    let bar = &bars[0];
    let venue = PaperVenue::new(PaperConfig::new().build());
    let ledger = GrantLedger::with_breakers(
        grant_with(0.5),
        BreakerState::new()
            .tripped(true)
            .trip_reason("test trip".to_owned())
            .build(),
    );
    let closes = vec![bar.c.to_f64().expect("representable close")];
    let view = bare_view(&closes, bar.ts);
    let journal = Journal::ephemeral();
    let recorder = Recorder::ephemeral(&journal);
    let decision = decide_and_submit(
        &strategy(),
        &risk(),
        &ledger,
        &venue,
        &recorder,
        &venue_config(),
        BarHealth::rehearsed(),
        0,
        bar,
        &view,
        &runner_config(),
        11,
    )
    .await;
    assert!(!decision.submitted, "disarmed bars never submit");
    let last = decision.transitions.last().expect("chain");
    assert_eq!(last.to, ProposalState::Cancelled, "disarm terminalizes");
    assert_eq!(last.from, ProposalState::AutoApproved);
    assert!(
        last.reason.contains("disarmed"),
        "the journal names the cause: {}",
        last.reason
    );
    for pair in decision.transitions.windows(2) {
        assert_eq!(pair[0].to, pair[1].from, "the chain stays contiguous");
    }
    let replayed = journal
        .replay(&ephemeral_permit())
        .expect("run log replays");
    assert!(
        replayed.events.iter().any(|event| event
            .detail
            .contains(&format!("{:?}", ProposalEvent::Cancel))),
        "the withdrawal is journaled, not just served"
    );
}

/// A fractional quantity can never reach the venue: the risk engine refuses
/// an unrepresentable quantity first, so the chain terminalizes as
/// `Rejected` and the venue sees nothing. (The runner's own quantity gate
/// below the risk check keeps a withdrawing `Cancel` as defense in depth
/// for a path that cannot be reached through the checked proposal.)
#[tokio::test]
async fn unrepresentable_quantity_refuses_terminal() {
    use protopyne_strategy::{elements::Sizing, PortfolioFacts, SignalOutput};
    use protopyne_types::Qty;
    struct FractionalSizing;
    impl Sizing for FractionalSizing {
        fn name(&self) -> &str {
            "fractional"
        }

        fn size(&self, _signal: &SignalOutput, _facts: &PortfolioFacts) -> Qty {
            Qty::new(Decimal::new(15, 1))
        }
    }
    let bars = history();
    let bar = &bars[0];
    let venue = PaperVenue::new(PaperConfig::new().build());
    let fractional = Arc::new(
        Strategy::with_signal(Arc::new(
            FixedSignal::new(Direction::Long, 0.8, 0.8).expect("valid signal"),
        ))
        .with_sizing(Arc::new(FractionalSizing)),
    );
    let ledger = GrantLedger::new(grant_with(0.5));
    let closes = vec![bar.c.to_f64().expect("representable close")];
    let view = bare_view(&closes, bar.ts);
    let journal = Journal::ephemeral();
    let recorder = Recorder::ephemeral(&journal);
    let decision = decide_and_submit(
        &fractional,
        &risk(),
        &ledger,
        &venue,
        &recorder,
        &venue_config(),
        BarHealth::rehearsed(),
        0,
        bar,
        &view,
        &runner_config(),
        11,
    )
    .await;
    assert!(!decision.submitted, "fractional quantities never submit");
    assert!(!decision.risk_pass, "the risk engine refuses first");
    let last = decision.transitions.last().expect("chain");
    assert_eq!(
        last.to,
        ProposalState::Rejected,
        "unrepresentable terminalizes"
    );
    assert_eq!(
        decision.skipped, None,
        "a risk refusal carries reasons, not a skip"
    );
    assert!(venue.orders().is_empty(), "the venue sees nothing");
}

/// A recorder that fails partway through the verdict: the venue already
/// reported, so the fill still books — but the decision says so in
/// `lifecycle_error` instead of dropping the failure.
#[tokio::test]
async fn verdict_journal_failure_surfaces_without_losing_the_fill() {
    use protopyne_venue::{
        Journal, LegExecution, OrderStatus, Recorder, SubmissionRecorder, VenueError,
        PROPOSAL_TRANSITION_KIND,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct FailLate {
        journal: Journal,
        lifecycle_transitions: AtomicUsize,
    }
    impl SubmissionRecorder for FailLate {
        fn record_intent(
            &self,
            intent: &protopyne_venue::OrderIntent,
            now: chrono::DateTime<chrono::Utc>,
        ) -> Result<bool, VenueError> {
            Recorder::ephemeral(&self.journal).record_intent(intent, now)
        }
        fn mark_status(
            &self,
            client_id: &str,
            status: OrderStatus,
            now: chrono::DateTime<chrono::Utc>,
        ) -> Result<(), VenueError> {
            Recorder::ephemeral(&self.journal).mark_status(client_id, status, now)
        }
        fn record_leg_fill(
            &self,
            client_id: &str,
            leg: &LegExecution,
            now: chrono::DateTime<chrono::Utc>,
        ) -> Result<(), VenueError> {
            Recorder::ephemeral(&self.journal).record_leg_fill(client_id, leg, now)
        }
        fn record_transition(
            &self,
            client_id: &str,
            kind: &str,
            detail: &str,
            now: chrono::DateTime<chrono::Utc>,
        ) -> Result<(), VenueError> {
            // Only the lifecycle kind fails, and only once the gates and the
            // submission are journaled: the protocol's own `verdict` row and
            // every status row succeed, so the failure lands in the
            // post-fill verdict drive rather than inside the submission.
            if kind == PROPOSAL_TRANSITION_KIND
                && self.lifecycle_transitions.fetch_add(1, Ordering::SeqCst) >= 3
            {
                return Err(VenueError::Journal(
                    "injected verdict journal failure".to_owned(),
                ));
            }
            Recorder::ephemeral(&self.journal).record_transition(client_id, kind, detail, now)
        }
    }
    let bars = history();
    let bar = &bars[0];
    let venue = PaperVenue::new(PaperConfig::new().build());
    let ledger = GrantLedger::new(grant_with(0.5));
    let closes = vec![bar.c.to_f64().expect("representable close")];
    let view = bare_view(&closes, bar.ts);
    let recorder = FailLate {
        journal: Journal::ephemeral(),
        lifecycle_transitions: AtomicUsize::new(0),
    };
    let decision = decide_and_submit(
        &strategy(),
        &risk(),
        &ledger,
        &venue,
        &recorder,
        &venue_config(),
        BarHealth::rehearsed(),
        0,
        bar,
        &view,
        &runner_config(),
        11,
    )
    .await;
    assert!(decision.submitted, "the venue verdict still books");
    assert_eq!(decision.filled_quantity, 1, "the fill is not lost");
    let error = decision
        .lifecycle_error
        .as_deref()
        .expect("failure surfaces");
    assert!(
        error.contains("injected verdict journal failure"),
        "the cause rides along: {error}"
    );
}

/// An approved proposal routes into the ledger + submit path: the grant's
/// budgets are reserved and consumed, and the venue books the order.
#[tokio::test]
async fn approving_submits_through_the_ledger() {
    use protopyne_strategy::{ProposalEvent, ProposalState};
    use protopyne_venue::Quote;
    let at = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap();
    let mut proposal = Proposal::new()
        .id("approved-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(Direction::Long)
        .side(ProposalSide::Entry)
        .structure(StructureKind::Vertical)
        .quantity(Qty::new(Decimal::from(2)))
        .limit_price(money(600))
        .max_loss(money(300))
        .notional(money(1_000))
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.7)
        .created_at(at)
        .build();
    proposal
        .apply(ProposalEvent::PassRisk, "risk approved", at)
        .expect("pass risk");
    proposal
        .apply(ProposalEvent::RequireHuman, "no grant covers entry", at)
        .expect("ask human");
    proposal
        .apply(ProposalEvent::Approve, "operator approved", at)
        .expect("human approve");
    assert_eq!(proposal.state, ProposalState::Approved);

    let venue = PaperVenue::new(PaperConfig::new().build());
    let journal = Journal::ephemeral();
    let recorder = Recorder::ephemeral(&journal);
    let ledger = GrantLedger::new(grant_with(0.5));
    let quote = Quote::new()
        .bid(money(600))
        .ask(money(601))
        .bid_size(50)
        .ask_size(50)
        .ts(at)
        .build();
    let risk_state = RiskState {
        quote_age_secs: 0,
        clock_skew_secs: 0,
        reconciliation_healthy: true,
        facts: PortfolioFacts::unobserved(),
    };
    let market = GrantMarket::new()
        .quote_age_secs(0)
        .spread_width(money(1))
        .build();
    let record = submit_approved(
        &mut proposal,
        &risk(),
        &risk_state,
        &ledger,
        &market,
        &venue,
        &recorder,
        &quote,
        &runner_config(),
        at,
    )
    .await
    .expect("an approved proposal submits");
    assert_eq!(record.filled_quantity, 2);
    assert_eq!(proposal.state, ProposalState::Filled);
    assert_eq!(
        ledger.usage().used_trades,
        1,
        "the ledger consumed the trade"
    );
    assert_eq!(venue.orders().len(), 1, "the venue booked the order");
}

/// A proposal the grant does not cover is journaled as a body and the bar
/// continues: a pending proposal never blocks the loop's next bar.
#[tokio::test]
async fn a_pending_proposal_does_not_block_the_next_bar() {
    use protopyne_venue::{
        LegExecution, OrderStatus, SubmissionRecorder, VenueError, PROPOSAL_BODY_KIND,
    };
    use std::sync::Mutex;

    struct BodyCapture {
        journal: Journal,
        bodies: Mutex<Vec<String>>,
    }

    impl SubmissionRecorder for BodyCapture {
        fn record_intent(
            &self,
            intent: &protopyne_venue::OrderIntent,
            now: DateTime<Utc>,
        ) -> Result<bool, VenueError> {
            Recorder::ephemeral(&self.journal).record_intent(intent, now)
        }
        fn mark_status(
            &self,
            client_id: &str,
            status: OrderStatus,
            now: DateTime<Utc>,
        ) -> Result<(), VenueError> {
            Recorder::ephemeral(&self.journal).mark_status(client_id, status, now)
        }
        fn record_leg_fill(
            &self,
            client_id: &str,
            leg: &LegExecution,
            now: DateTime<Utc>,
        ) -> Result<(), VenueError> {
            Recorder::ephemeral(&self.journal).record_leg_fill(client_id, leg, now)
        }
        fn record_transition(
            &self,
            client_id: &str,
            kind: &str,
            detail: &str,
            now: DateTime<Utc>,
        ) -> Result<(), VenueError> {
            if kind == PROPOSAL_BODY_KIND {
                self.bodies
                    .lock()
                    .expect("bodies lock")
                    .push(detail.to_owned());
            }
            Recorder::ephemeral(&self.journal).record_transition(client_id, kind, detail, now)
        }
    }

    let bars = history();
    let venue = PaperVenue::new(PaperConfig::new().build());
    // A strict grant: no bar's proposal is covered, so each is handed to the
    // human.
    let ledger = GrantLedger::new(grant_with(0.95));
    let recorder = BodyCapture {
        journal: Journal::ephemeral(),
        bodies: Mutex::new(Vec::new()),
    };
    let config = runner_config();
    for (index, bar) in bars.iter().take(2).enumerate() {
        let closes: Vec<f64> = bars[..=index].iter().filter_map(|b| b.c.to_f64()).collect();
        let view = bare_view(&closes, bar.ts);
        let decision = decide_and_submit(
            &strategy(),
            &risk(),
            &ledger,
            &venue,
            &recorder,
            &venue_config(),
            BarHealth::rehearsed(),
            index,
            bar,
            &view,
            &config,
            11,
        )
        .await;
        assert!(!decision.grant_merit, "a strict grant asks the human");
        assert!(!decision.submitted, "a pending proposal never submits");
    }
    assert_eq!(
        recorder.bodies.lock().expect("bodies lock").len(),
        2,
        "each bar journals its pending proposal; the next bar is not blocked"
    );
}

/// M3 runner plumbing: legs onto the intent, batch byte-identical.
///
/// A stub chain source serving both the selection snapshot and per-leg
/// fills. Selection reads `chain_snapshot`; fills read `leg_quote_at` —
/// one handle, two reads, no second fill math.
struct SnapshotChain {
    quotes: Vec<protopyne_types::ChainQuote>,
}

impl protopyne_venue::LegQuoteSource for SnapshotChain {
    fn leg_quote(
        &self,
        contract: &protopyne_types::OptionContract,
    ) -> Option<protopyne_venue::Quote> {
        self.quotes
            .iter()
            .rfind(|quote| quote.contract == *contract)
            .map(fill_quote_of)
    }

    fn leg_quote_at(
        &self,
        contract: &protopyne_types::OptionContract,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Option<protopyne_venue::Quote> {
        self.quotes
            .iter()
            .rfind(|quote| quote.contract == *contract && quote.timestamp <= at)
            .map(fill_quote_of)
    }

    fn chain_snapshot(
        &self,
        underlying: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Vec<protopyne_types::ChainQuote> {
        self.quotes
            .iter()
            .filter(|quote| quote.contract.underlying == underlying && quote.timestamp <= at)
            .cloned()
            .collect()
    }
}

fn fill_quote_of(chain: &protopyne_types::ChainQuote) -> protopyne_venue::Quote {
    protopyne_venue::Quote::new()
        .bid(chain.bid)
        .ask(chain.ask)
        .bid_size(50)
        .ask_size(50)
        .ts(chain.timestamp)
        .build()
}

fn chain_contract(strike: i64) -> protopyne_types::OptionContract {
    protopyne_types::OptionContract::new()
        .underlying("SPY".to_owned())
        .expiry(chrono::NaiveDate::from_ymd_opt(2026, 2, 20).expect("valid expiry"))
        .strike(Money::new(Decimal::from(strike)))
        .kind(protopyne_types::OptionKind::Call)
        .build()
}

fn chain_quote_for(
    strike: i64,
    bid: i64,
    ask: i64,
    ts: chrono::DateTime<chrono::Utc>,
) -> protopyne_types::ChainQuote {
    protopyne_types::ChainQuote::new()
        .contract(chain_contract(strike))
        .bid(Money::new(Decimal::from(bid)))
        .ask(Money::new(Decimal::from(ask)))
        .timestamp(ts)
        .build()
}

/// Chain snapshot covering `bar`: 690/695 call pair around its ~694 close.
fn covering_chain(bar: &Bar) -> Vec<protopyne_types::ChainQuote> {
    vec![
        chain_quote_for(690, 3, 4, bar.ts),
        chain_quote_for(695, 2, 3, bar.ts),
    ]
}

async fn submit_with_chain(venue: &PaperVenue, bar: &Bar, seed: u64) -> BarDecision {
    let ledger = GrantLedger::new(grant_with(0.5));
    let closes = vec![bar.c.to_f64().expect("representable close")];
    let view = bare_view(&closes, bar.ts);
    let journal = Journal::ephemeral();
    let recorder = Recorder::ephemeral(&journal);
    decide_and_submit(
        &strategy(),
        &risk(),
        &ledger,
        venue,
        &recorder,
        &venue_config(),
        BarHealth::rehearsed(),
        0,
        bar,
        &view,
        &runner_config(),
        seed,
    )
    .await
}

/// The config arm never reads the chain: an installed source whose quotes
/// all post-date the bar changes nothing, because only the chain-derived arm
/// prices a package. Two legs reach the venue only through that arm (pinned
/// by the `chain_derived_*` cases).
#[tokio::test]
async fn the_config_arm_never_reads_the_chain() {
    let bars = history();
    let bar = &bars[0];
    let mut venue = PaperVenue::new(PaperConfig::new().build());
    venue.set_leg_quotes(Arc::new(SnapshotChain {
        quotes: covering_chain(bar),
    }));
    let decision = submit_with_chain(&venue, bar, 11).await;
    assert!(
        decision.submitted,
        "the config arm submits on its own numbers: {:?}",
        decision.skipped
    );
    assert!(
        decision.legs.is_empty(),
        "no unpriced legs attach under the config arm"
    );
    assert!(
        venue
            .executed_legs(&protopyne_venue::client_id_for("run-11-0", 0))
            .is_empty(),
        "the venue fills one legless order"
    );
    assert!(
        !decision.transitions.is_empty(),
        "the lifecycle journals the fill"
    );
}

#[tokio::test]
async fn chainless_multileg_keeps_legacy_single_leg_booking() {
    // Batch divergence, pinned not fixed: without a chain handle a spread
    // label still books one stock fill, exactly as before this milestone.
    let bars = history();
    let bar = &bars[0];
    let venue = PaperVenue::new(PaperConfig::new().build());
    let decision = submit_with_chain(&venue, bar, 13).await;
    assert!(decision.submitted, "legacy path still submits");
    assert!(
        venue
            .executed_legs(&protopyne_venue::client_id_for("run-13-0", 0))
            .is_empty(),
        "no legs attach without a chain handle"
    );
    assert!(
        decision.legs.is_empty(),
        "a legless decision keeps its empty leg shape"
    );
}

/// A signal whose verdict alternates with its own per-run call count. If a
/// sweep variant shared another variant's instance, the alternation phase
/// would shift and its decisions would drift.
struct AlternatingSignal;

struct Alternation {
    calls: u64,
}

impl protopyne_strategy::Signal for AlternatingSignal {
    fn name(&self) -> &str {
        "alternating"
    }

    fn bind(
        &self,
        _cfg: &protopyne_strategy::SignalConfig,
    ) -> Result<Box<dyn protopyne_strategy::SignalInstance>, protopyne_strategy::StrategyError>
    {
        Ok(Box::new(Alternation { calls: 0 }))
    }

    fn evaluate(
        &self,
        _view: &MarketView<'_>,
    ) -> Result<protopyne_strategy::SignalOutput, protopyne_strategy::StrategyError> {
        Ok(protopyne_strategy::SignalOutput {
            direction: Direction::Long,
            conviction: 0.8,
            activity_score: 1.0,
        })
    }
}

impl protopyne_strategy::SignalInstance for Alternation {
    fn evaluate(
        &mut self,
        _view: &MarketView<'_>,
    ) -> Result<protopyne_strategy::SignalOutput, protopyne_strategy::StrategyError> {
        self.calls += 1;
        let odd = self.calls % 2 == 1;
        Ok(protopyne_strategy::SignalOutput {
            direction: if odd {
                Direction::Long
            } else {
                Direction::Short
            },
            conviction: if odd { 0.8 } else { 0.4 },
            activity_score: 1.0,
        })
    }

    fn warmup(&self) -> usize {
        0
    }
}

fn alternating_strategy() -> Arc<Strategy> {
    Arc::new(
        Strategy::with_signal(Arc::new(AlternatingSignal)).with_structure(Arc::new(
            ExplicitStructure {
                structure: StructureKind::Vertical,
            },
        )),
    )
}

#[tokio::test]
async fn sweep_variants_never_share_stateful_signal_state() {
    let bars = history();
    let grants = vec![grant_with(0.5); 8];
    let concurrent = run_sweep(
        alternating_strategy(),
        RiskLimits::new().build(),
        &venue_config(),
        &runner_config(),
        &bars,
        grants.clone(),
        11,
    )
    .await
    .expect("sweep runs");
    let strategy = alternating_strategy();
    let sequential = run_backtest(
        &strategy,
        &risk(),
        &grants[0],
        &venue_config(),
        &runner_config(),
        &bars,
        11,
    )
    .await
    .expect("sequential run");
    for outcome in &concurrent {
        assert_eq!(
            outcome.decisions, sequential.decisions,
            "variant {} diverged from the sequential run",
            outcome.index
        );
    }
    assert!(
        concurrent.iter().any(|outcome| outcome.fills > 0),
        "the alternation must still fire on its strong bars"
    );
}

/// Chain-derived economics: selection prices the package, and the proposal,
/// intent, and dry run all read the same numbers.
use protopyne_strategy::{ChainSlice, EconomicsPolicy, LegSelector};
use protopyne_types::{ChainQuote, OptionContract, OptionKind, Timeframe};

fn kind_quote(
    kind: OptionKind,
    strike: i64,
    bid: &str,
    ask: &str,
    ts: chrono::DateTime<Utc>,
) -> ChainQuote {
    ChainQuote::new()
        .contract(
            OptionContract::new()
                .underlying("SPY".to_owned())
                // Inside the fixture grant's DTE window (1..90 days after the
                // pinned January bars).
                .expiry(chrono::NaiveDate::from_ymd_opt(2026, 2, 20).expect("expiry"))
                .strike(Money::new(Decimal::from(strike)))
                .kind(kind)
                .build(),
        )
        .bid(bid.parse().expect("bid"))
        .ask(ask.parse().expect("ask"))
        .timestamp(ts)
        .build()
}

fn chain_strategy() -> Arc<Strategy> {
    Arc::new(
        Strategy::with_signal(Arc::new(
            FixedSignal::new(Direction::Long, 0.8, 0.8).expect("valid signal"),
        ))
        .with_structure(Arc::new(LegSelector::new().build())),
    )
}

fn grant_for_structure(structure: StructureKind) -> Grant {
    Grant::new()
        .id("chain-derived".to_owned())
        .scope(
            GrantScope::new()
                .strategies(vec!["ribbon".to_owned()])
                .underlyings(vec!["SPY".to_owned()])
                .structures(vec![structure])
                .directions(vec![Direction::Long, Direction::Short])
                .permissions(GrantPermission {
                    entries: true,
                    exits: true,
                    rolls: false,
                })
                .build(),
        )
        .budgets(
            GrantBudgets::new()
                .max_trades(1_000)
                .max_contracts(10_000)
                .max_loss(money(1_000_000))
                .max_notional(money(100_000_000))
                .build(),
        )
        .signal(
            SignalThresholds::new()
                .min_conviction(0.5)
                .min_activity_score(0.5)
                .build(),
        )
        .trade(
            TradeThresholds::new()
                .max_loss_per_trade(money(1_000))
                .max_spread_width(money(50))
                .min_dte_days(1)
                .max_dte_days(90)
                .build(),
        )
        .portfolio(
            PortfolioThresholds::new()
                .max_open_risk(money(10_000_000))
                .quote_max_age_secs(60)
                .build(),
        )
        .breakers(BreakerLimits::new().max_consecutive_errors(3).build())
        .valid_from(Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap())
        .valid_until(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap())
        .build()
}

async fn chain_derived_decision(
    structure: StructureKind,
    quotes: &[ChainQuote],
    grant_structure: StructureKind,
) -> (BarDecision, PaperVenue) {
    chain_derived_decision_with(grant_for_structure(grant_structure), structure, quotes).await
}

async fn chain_derived_decision_with(
    grant: Grant,
    structure: StructureKind,
    quotes: &[ChainQuote],
) -> (BarDecision, PaperVenue) {
    let bars = history();
    let bar = &bars[0];
    let mut venue = PaperVenue::new(PaperConfig::new().build());
    let source = Arc::new(SnapshotChain {
        quotes: quotes.to_vec(),
    });
    venue.set_leg_quotes(Arc::clone(&source) as Arc<dyn protopyne_venue::LegQuoteSource>);
    let chain = ChainSlice::new(quotes, bar.ts, "SPY").expect("quotes bind");
    let closes = vec![bar.c.to_f64().expect("representable close")];
    let data = MarketData {
        bars: &[],
        chain: Some(chain),
        dte_days: 30,
        timeframe: Timeframe::D1,
    };
    let view = MarketView {
        data: &data,
        closes: &closes,
        as_of: bar.ts,
    };
    let ledger = GrantLedger::new(grant);
    let journal = Journal::ephemeral();
    let recorder = Recorder::ephemeral(&journal);
    let config = runner_config()
        .with_economics(EconomicsPolicy::ChainDerived)
        .with_structure(structure);
    let decision = decide_and_submit(
        &chain_strategy(),
        &risk(),
        &ledger,
        &venue,
        &recorder,
        &venue_config(),
        BarHealth::rehearsed(),
        0,
        bar,
        &view,
        &config,
        21,
    )
    .await;
    (decision, venue)
}

/// The spread threshold judges the package, not the underlying's bar: a
/// vertical whose own bid/ask width is a dollar is refused under a $0.50
/// threshold even though the synthesized bar quote is a third of a dollar
/// wide.
#[tokio::test]
async fn a_wide_package_is_refused_by_the_spread_threshold() {
    let bars = history();
    let bar = &bars[0];
    // Wide books on a deep debit package: crossing costs a dollar against a
    // 99.40 mid, well inside the 200 bps tolerance, so the package is
    // marketable and the spread threshold alone refuses it.
    let quotes = vec![
        kind_quote(OptionKind::Call, 690, "99.30", "100.70", bar.ts),
        kind_quote(OptionKind::Call, 695, "0.40", "0.80", bar.ts),
    ];
    let mut grant = grant_for_structure(StructureKind::Vertical);
    // Above the 0.347-wide synthesized bar touch, below the package's 1.00.
    grant.trade.max_spread_width = Money::new(Decimal::new(50, 2));
    let (decision, venue) =
        chain_derived_decision_with(grant, StructureKind::Vertical, &quotes).await;
    assert!(
        !decision.submitted,
        "the package width refuses: {:?}",
        decision.skipped
    );
    assert!(venue.orders().is_empty(), "a refused package books nothing");
    assert!(
        decision
            .grant_merit_reasons
            .iter()
            .any(|reason| reason.contains("spread width")),
        "the refusal names the spread threshold: {:?}",
        decision.grant_merit_reasons
    );
}

#[tokio::test]
async fn chain_derived_vertical_books_the_debit_package() {
    let bars = history();
    let bar = &bars[0];
    // One-tick quotes: mid equals touch, so the fill clears the tolerance.
    let quotes = vec![
        kind_quote(OptionKind::Call, 690, "3.499", "3.501", bar.ts),
        kind_quote(OptionKind::Call, 695, "2.499", "2.501", bar.ts),
    ];
    let (decision, venue) =
        chain_derived_decision(StructureKind::Vertical, &quotes, StructureKind::Vertical).await;
    assert!(
        decision.submitted,
        "a marketable package submits: {:?}",
        decision.skipped
    );
    assert_eq!(decision.structure, Some(StructureKind::Vertical));
    // Mid debit 1.00 plus the 200 bps package tolerance.
    assert_eq!(decision.checked_limit, Money::new(Decimal::new(102, 2)));
    assert_eq!(decision.filled_quantity, 1, "the package is one unit");
    assert_eq!(decision.legs.len(), 2, "both legs are recorded");
    let orders = venue.orders();
    assert_eq!(orders.len(), 1);
    let intent = &orders[0].intent;
    assert_eq!(intent.side, OrderSide::Buy, "a debit package buys");
    assert_eq!(intent.limit_price, Money::new(Decimal::new(102, 2)));
    assert_eq!(
        intent.legs.as_ref().map(protopyne_types::SpreadLegs::len),
        Some(2)
    );
    // The intent the venue received is the one the engine judged: the
    // checked limit and the derived shape match the submitted intent.
    assert_eq!(intent.limit_price, decision.checked_limit);
    assert_eq!(
        OrderShape::of(intent),
        OrderShape::new(true, true),
        "the submitted spread is a limit spread"
    );
    // Debit 1.00 plus two 0.65 fees.
    assert_eq!(
        intent.capital_requirement,
        Some(Money::new(Decimal::new(230, 2)))
    );
}

#[tokio::test]
async fn chain_derived_condor_flips_the_intent_to_sell() {
    let bars = history();
    let bar = &bars[0];
    let quotes = vec![
        kind_quote(OptionKind::Put, 685, "0.999", "1.001", bar.ts),
        kind_quote(OptionKind::Put, 690, "1.999", "2.001", bar.ts),
        kind_quote(OptionKind::Call, 700, "0.999", "1.001", bar.ts),
        kind_quote(OptionKind::Call, 705, "0.499", "0.501", bar.ts),
    ];
    let (decision, venue) =
        chain_derived_decision(StructureKind::Condor, &quotes, StructureKind::Condor).await;
    assert_eq!(decision.structure, Some(StructureKind::Condor));
    let orders = venue.orders();
    assert_eq!(
        orders.len(),
        1,
        "a covered condor reaches the venue: {:?}",
        decision.skipped
    );
    let intent = &orders[0].intent;
    assert_eq!(intent.side, OrderSide::Sell, "a credit package sells");
    assert!(
        intent.limit_price.as_decimal().is_sign_negative(),
        "the credit limit is signed"
    );
    assert_eq!(
        intent.legs.as_ref().map(protopyne_types::SpreadLegs::len),
        Some(4)
    );
}

#[tokio::test]
async fn chain_derived_package_outside_tolerance_names_the_limit() {
    let bars = history();
    let bar = &bars[0];
    let quotes = covering_chain(bar);
    let (decision, venue) =
        chain_derived_decision(StructureKind::Vertical, &quotes, StructureKind::Vertical).await;
    assert!(!decision.submitted, "the touch is far above the mid limit");
    assert!(venue.orders().is_empty(), "a refused package books nothing");
    let skipped = decision.skipped.as_deref().expect("a reason");
    assert!(
        skipped.contains("outside limit"),
        "the refusal names the limit: {skipped}"
    );
}

/// One vertical package proposal: the shape a caller would hand over with
/// legs attached under the config arm.
fn legged_proposal(bar: &Bar, legs: Option<protopyne_types::SpreadLegs>) -> Proposal {
    Proposal::new()
        .id("run-11-0".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(Direction::Long)
        .side(ProposalSide::Entry)
        .structure(StructureKind::Vertical)
        .quantity(Qty::new(Decimal::ONE))
        .limit_price(Money::new(bar.c))
        .max_loss(money(400))
        .notional(money(2_000))
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.8)
        .maybe_legs(legs)
        .state(ProposalState::Proposed)
        .created_at(bar.ts)
        .build()
}

/// A 690/695 call package around the bar's close, quoted tight.
fn package_legs(bar: &Bar) -> protopyne_types::SpreadLegs {
    use protopyne_types::{LegSide, OptionContract, OptionKind, SpreadLeg};

    let contract = |strike: i64| {
        OptionContract::new()
            .underlying("SPY".to_owned())
            .expiry(bar.ts.date_naive() + chrono::Duration::days(30))
            .strike(money(strike))
            .kind(OptionKind::Call)
            .build()
    };
    protopyne_types::SpreadLegs::try_new(vec![
        SpreadLeg::new()
            .contract(contract(690))
            .side(LegSide::Long)
            .build(),
        SpreadLeg::new()
            .contract(contract(695))
            .side(LegSide::Short)
            .build(),
    ])
    .expect("a defined-risk package")
}

/// Config-supplied economics are the caller's constants: they cannot check a
/// structure's own numbers, so a proposal carrying legs refuses naming the
/// arm instead of booking legs the gates never priced. The same proposal
/// without legs is the only config-supplied shape, and it flows unchanged.
#[tokio::test]
async fn a_config_supplied_proposal_with_legs_refuses_naming_the_arm() {
    let bars = history();
    let bar = &bars[0];
    let venue = PaperVenue::new(PaperConfig::new().build());
    let journal = Journal::ephemeral();
    let recorder = Recorder::ephemeral(&journal);
    let ledger = GrantLedger::new(grant_with(0.5));
    let quote = bar_quote(bar, &venue_config()).expect("the bar quotes");
    let refused = submit_decided(
        legged_proposal(bar, Some(package_legs(bar))),
        quote,
        &risk(),
        &ledger,
        &venue,
        &recorder,
        BarHealth::rehearsed(),
        None,
        0,
        bar,
        &runner_config(),
    )
    .await;
    assert!(
        !refused.submitted,
        "a priced package needs the chain-derived arm: {:?}",
        refused.skipped
    );
    let reason = refused.skipped.as_deref().expect("a named refusal");
    assert!(
        reason.contains("ChainDerived"),
        "the refusal names the arm that can price it: {reason}"
    );
    assert!(
        venue.orders().is_empty(),
        "a refused proposal books nothing"
    );

    let flowed = submit_decided(
        legged_proposal(bar, None),
        quote,
        &risk(),
        &ledger,
        &venue,
        &recorder,
        BarHealth::rehearsed(),
        None,
        0,
        bar,
        &runner_config(),
    )
    .await;
    assert!(
        flowed.submitted,
        "the legless config-supplied shape is unchanged: {:?}",
        flowed.skipped
    );
    let orders = venue.orders();
    assert_eq!(orders.len(), 1, "the refused bar booked nothing");
    assert!(
        orders[0].intent.legs.is_none(),
        "the legless path still books one legless order"
    );
}
