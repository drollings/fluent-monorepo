//! Live-loop decision-stream pin.
//!
//! A scripted feed run through the session's loop driver must decide exactly
//! as the batch run over the same bars. The existing loop test asserts full
//! verdict equality; this pin records the *before bytes* of the projected
//! decision vector (direction, submission, checked limit, legs, fees, equity
//! contribution), so a Phase 1 change shows up as a declared golden delta
//! rather than a silent drift.
//!
//! Record deliberately with `PROTOPYNE_RECORD=1`; a normal run only compares.

use protopyne_strategy::fixtures::grant_with;
use std::path::PathBuf;
use std::sync::Arc;

use protopyne_strategy::{
    elements::ExplicitStructure, Direction, FixedSignal, Grant, RiskEngine, RiskLimits,
    StructureKind,
};
use protopyne_types::{Bar, Money, Timeframe};
use protopyne_venue::{PaperConfig, SessionConfig, TradingSession, VenueRegistry, VenueStorage};
use rust_decimal::Decimal;

use crate::backtest::{BacktestConfig, BacktestVenue};
use crate::replay_session::ChainPolicy;
use crate::runner::{equity_curve_for, run_backtest, BarDecision, RunnerConfig};
use crate::trading_loop::TradingLoopExt;

fn data_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join(name)
}

fn history() -> Vec<Bar> {
    crate::csv::load_bars_csv(&data_path("SPY_15M.csv")).expect("vendored history loads")
}

fn money(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

fn strategy() -> protopyne_strategy::Strategy {
    protopyne_strategy::Strategy::with_signal(Arc::new(
        FixedSignal::new(Direction::Long, 0.8, 0.8).expect("valid signal"),
    ))
    .with_structure(Arc::new(ExplicitStructure {
        structure: StructureKind::Vertical,
    }))
}

fn risk() -> RiskEngine {
    RiskEngine::new(RiskLimits::new().build())
}

fn grant() -> Grant {
    // The shared fixture's shape; only the id and the signal floor differ.
    grant_with(|grant| {
        grant.id = "live-loop-baseline".to_owned();
        grant.signal.min_conviction = 0.5;
        grant.signal.min_activity_score = 0.5;
    })
}

fn quote_config() -> BacktestConfig {
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

/// One row per bar of the facts a later phase may move: direction,
/// submission, checked limit, executed legs, fees, and the bar's equity
/// contribution (the equity-curve step, so P&L drift is visible too).
fn decision_vector(bars: &[Bar], decisions: &[BarDecision]) -> serde_json::Value {
    let curve = equity_curve_for(bars, decisions);
    let rows: Vec<serde_json::Value> = decisions
        .iter()
        .enumerate()
        .map(|(row, decision)| {
            let equity = curve
                .get(row)
                .map_or(Decimal::ZERO, |level| level.as_decimal());
            let previous = if row == 0 {
                Decimal::ZERO
            } else {
                curve
                    .get(row - 1)
                    .map_or(Decimal::ZERO, |level| level.as_decimal())
            };
            let legs: Vec<serde_json::Value> = decision
                .legs
                .iter()
                .map(|leg| {
                    serde_json::json!({
                        "contract": leg.contract.to_string(),
                        "side": format!("{:?}", leg.side),
                        "quantity": leg.quantity,
                        "price": leg.price.to_string(),
                        "fee": leg.fee.to_string(),
                    })
                })
                .collect();
            serde_json::json!({
                "bar_index": decision.bar_index,
                "direction": decision.direction.map(|direction| format!("{direction:?}")),
                "submitted": decision.submitted,
                "checked_limit": decision.checked_limit.to_string(),
                "legs": legs,
                "fee": decision.fee.map(|fee| fee.to_string()),
                "filled_quantity": decision.filled_quantity,
                "fill_price": decision.fill_price.map(|price| price.to_string()),
                "equity_contribution": (equity - previous).to_string(),
            })
        })
        .collect();
    serde_json::Value::Array(rows)
}

fn golden_path() -> PathBuf {
    data_path("baseline/live_loop_vectors.json")
}

/// Compare against the checked-in pin; `PROTOPYNE_RECORD=1` writes it.
fn check_golden(actual: &str) {
    let path = golden_path();
    if std::env::var("PROTOPYNE_RECORD").is_ok() {
        std::fs::create_dir_all(path.parent().expect("golden has a parent"))
            .expect("baseline directory exists");
        std::fs::write(&path, actual).expect("golden records");
        return;
    }
    let recorded = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("golden {} records first", path.display()));
    assert_eq!(actual, recorded, "live-loop vector must match its baseline");
}

fn feed(bars: &[Bar]) -> tokio::sync::mpsc::Receiver<Bar> {
    let (tx, rx) = tokio::sync::mpsc::channel(bars.len().max(1));
    for bar in bars {
        tx.try_send(bar.clone()).expect("capacity fits every bar");
    }
    drop(tx);
    rx
}

#[tokio::test]
async fn live_loop_matches_batch_as_a_decision_vector() {
    let bars = history();
    let strategy = strategy();
    let risk = risk();
    let granted = grant();
    let quote = quote_config();
    let runner = runner_config();
    let timeframe = Timeframe::infer(&bars).unwrap_or(Timeframe::D1);

    let batch = run_backtest(&strategy, &risk, &granted, &quote, &runner, &bars, 11)
        .await
        .expect("batch run");

    let venue: Arc<dyn protopyne_venue::Venue> = Arc::new(
        BacktestVenue::new(quote.clone(), bars.clone(), PaperConfig::new().build()).expect("venue"),
    );
    let session = TradingSession::boot(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
        VenueStorage::new("loop-baseline.db".into()),
        None,
    )
    .expect("session boots");
    let mut receiver = feed(&bars);
    let looped = session
        .run_trading_loop(
            &mut receiver,
            Vec::new(),
            &strategy,
            &risk,
            &granted,
            &quote,
            &runner,
            timeframe,
            ChainPolicy::Unscreened,
            std::time::Duration::ZERO,
            11,
            None,
        )
        .await
        .expect("loop runs");
    session.shutdown().await;

    let batch_vector = decision_vector(&bars, &batch.decisions);
    let loop_vector = decision_vector(&bars, &looped);
    assert_eq!(
        loop_vector, batch_vector,
        "the loop and the batch must project to one vector"
    );
    assert!(
        looped.iter().any(|decision| decision.submitted),
        "the baseline must exercise a fill"
    );
    let body = serde_json::to_string_pretty(&loop_vector).expect("vector serializes");
    check_golden(&body);
}

/// A golden newer than this test source means the pin moved without the test
/// being revisited.
#[test]
fn live_loop_pin_is_regenerated_only_deliberately() {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/live_loop_baseline.rs");
    let source_time = std::fs::metadata(&source)
        .expect("test source exists")
        .modified()
        .expect("source mtime");
    let golden_time = std::fs::metadata(golden_path())
        .unwrap_or_else(|_| panic!("golden {} records first", golden_path().display()))
        .modified()
        .expect("golden mtime");
    assert!(
        golden_time <= source_time,
        "the live-loop golden is newer than its test source: revisit the pin deliberately"
    );
}
