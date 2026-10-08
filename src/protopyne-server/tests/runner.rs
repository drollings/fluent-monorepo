use super::*;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use protopyne_strategy::{
    BreakerLimits, Direction, FixedSignal, Grant, GrantBudgets, GrantMarket, GrantPermission,
    GrantScope, PortfolioThresholds, ProposalSide, RiskEngine, RiskLimits, SignalThresholds,
    Strategy, StructureKind, TradeThresholds,
    elements::ExplicitStructure,
};
use protopyne_types::{Bar, Money};
use protopyne_venue::{PaperConfig, PaperVenue};

fn money(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

fn history() -> Vec<Bar> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    let bars = crate::load_bars_csv(&path).expect("vendored history loads");
    bars[..40].to_vec()
}

fn strategy() -> Arc<Strategy> {
    Arc::new(
        Strategy::with_signal(Arc::new(
            FixedSignal::new(Direction::Long, 0.8, 0.8).expect("valid signal"),
        ))
        .with_structure(Arc::new(ExplicitStructure { structure: StructureKind::Vertical })),
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
                .permissions(GrantPermission { entries: true, exits: true, rolls: false })
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
    RunnerConfig::new("SPY".to_owned(), "ribbon".to_owned(), money(400), money(2_000), 30, 0)
}

fn signature(decision: &BarDecision) -> (Option<Direction>, bool, bool, bool) {
    (decision.direction, decision.risk_pass, decision.grant_allow, decision.submitted)
}

#[tokio::test]
async fn backtest_and_paper_reach_identical_decisions() {
    let bars = history();
    let strategy = strategy();
    let risk = risk();
    let grant = grant_with(0.5);
    let run = run_backtest(&strategy, &risk, &grant, &venue_config(), &runner_config(), &bars, 11)
        .await
        .expect("backtest runs");
    let paper = PaperVenue::new(PaperConfig::new().build());
    let paper_decisions =
        run_paper_decisions(&strategy, &risk, &grant, &paper, &runner_config(), &bars, 11).await;
    assert_eq!(run.decisions.len(), bars.len());
    assert_eq!(paper_decisions.len(), bars.len());
    let backtest_json = serde_json::to_string(&run.decisions).expect("serializable");
    let paper_json = serde_json::to_string(&paper_decisions).expect("serializable");
    assert_eq!(backtest_json, paper_json, "same code path must decide identically");
    assert!(run.decisions.iter().any(|decision| decision.submitted), "liquid history must fill something");
    assert!(run.report.fills > 0);
    assert!(run.report.fill_assumptions.iter().any(|line| line.contains("touch")));
    assert!(run.report.greeks_note.contains("unavailable"));
}

#[tokio::test]
async fn rerun_is_byte_identical() {
    let bars = history();
    let strategy = strategy();
    let risk = risk();
    let grant = grant_with(0.5);
    let first =
        run_backtest(&strategy, &risk, &grant, &venue_config(), &runner_config(), &bars, 11)
            .await
            .expect("first run");
    let second =
        run_backtest(&strategy, &risk, &grant, &venue_config(), &runner_config(), &bars, 11)
            .await
            .expect("second run");
    assert_eq!(first.to_canonical_json(), second.to_canonical_json());
    assert_eq!(first.manifest.to_canonical_json(), second.manifest.to_canonical_json());
}

#[tokio::test]
async fn walk_forward_test_segment_matches_full_run() {
    let bars = history();
    let strategy = strategy();
    let risk = risk();
    let grant = grant_with(0.5);
    let full =
        run_backtest(&strategy, &risk, &grant, &venue_config(), &runner_config(), &bars, 11)
            .await
            .expect("full run");
    let (train, test) = walk_forward_split(&bars, 20).expect("split");
    assert_eq!(train.len(), 20);
    assert_eq!(test.len(), 20);
    assert!(walk_forward_split(&bars, 0).is_err());
    assert!(walk_forward_split(&bars, bars.len()).is_err());
    let segment =
        run_backtest(&strategy, &risk, &grant, &venue_config(), &runner_config(), test, 11)
            .await
            .expect("segment run");
    // Warmup is zero and budgets never bind, so each bar's verdict depends
    // only on its own quote: the test segment must decide like the full run's
    // tail up to proposal identity (ids embed run-local indices).
    for (segment_decision, full_decision) in segment.decisions.iter().zip(full.decisions[20..].iter()) {
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
    assert_eq!(outcomes[1].fills, 0, "strict grant must deny every proposal");
    assert!(run_sweep(strategy(), RiskLimits::new().build(), &venue_config(), &runner_config(), &bars, Vec::new(), 11).await.is_err());
}

#[tokio::test]
async fn runner_rejects_empty_history() {
    let strategy = strategy();
    let risk = risk();
    let grant = grant_with(0.5);
    assert!(run_backtest(&strategy, &risk, &grant, &venue_config(), &runner_config(), &[], 11).await.is_err());
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
    let note = expire_position(&venue, "SPY", now).expect("expiry journals");
    assert!(note.contains("SPY"));
    let quote = quote_for_bar(&history()[0], &venue_config()).expect("quotable");
    assert!(newest_quote(&[None, Some(&quote)]).is_some());
    let _ = (ProposalSide::Entry, GrantMarket::new, StructureKind::Vertical);
}
