//! Risk-adjusted metric calibration vectors.
//!
//! Confidence axis (must-NOT-fire controls): flat and empty curves report
//! `None`, never an invented zero — a metric with no variation or no input
//! has nothing to say. Task-value axis (economics): hand-computed Sharpe /
//! Sortino / Calmar vectors below must match within `1e-9`; win-rate and
//! profit-factor count only decisive steps (flat steps excluded).

use super::*;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive as _;

fn decimal(value: &str) -> Decimal {
    value.parse::<Decimal>().expect("decimal")
}

fn returns(values: &[&str]) -> Vec<Decimal> {
    values.iter().map(|value| decimal(value)).collect()
}

fn close_enough(actual: Decimal, expected: f64) {
    let actual = actual
        .to_f64()
        .expect("metric values stay in f64 range");
    assert!(
        (actual - expected).abs() < 1e-9,
        "metric {actual} differs from hand-computed {expected}"
    );
}

#[test]
fn flat_and_empty_curves_report_none_never_zero() {
    let flat_returns = returns(&["0", "0", "0"]);
    assert!(sharpe(&flat_returns, Decimal::ZERO, decimal("252")).is_none());
    assert!(sortino(&flat_returns, Decimal::ZERO, decimal("252")).is_none());
    assert!(win_rate(&flat_returns).is_none());
    assert!(profit_factor(&flat_returns).is_none());

    let flat_equity = returns(&["100", "100", "100"]);
    assert!(calmar(&flat_equity, Decimal::ZERO, decimal("252")).is_none());

    let empty: Vec<Decimal> = Vec::new();
    assert!(sharpe(&empty, Decimal::ZERO, decimal("252")).is_none());
    assert!(sortino(&empty, Decimal::ZERO, decimal("252")).is_none());
    assert!(calmar(&empty, Decimal::ZERO, decimal("252")).is_none());
    assert!(win_rate(&empty).is_none());
    assert!(profit_factor(&empty).is_none());
}

#[test]
fn sharpe_matches_hand_computation() {
    // mean 0.0125, population std 0.014790199457749039, × sqrt(252).
    let sample = returns(&["0.01", "0.02", "-0.01", "0.03"]);
    let ratio = sharpe(&sample, Decimal::ZERO, decimal("252")).expect("defined");
    close_enough(ratio, 13.41640786499874);
}

#[test]
fn sortino_matches_hand_computation() {
    // Downside deviation over all four steps: sqrt(0.0001 / 4) = 0.005.
    let sample = returns(&["0.01", "0.02", "-0.01", "0.03"]);
    let ratio = sortino(&sample, Decimal::ZERO, decimal("252")).expect("defined");
    close_enough(ratio, 39.68626966596886);
}

#[test]
fn sortino_with_no_downside_reports_none() {
    let all_up = returns(&["0.01", "0.02", "0.03"]);
    assert!(sortino(&all_up, Decimal::ZERO, decimal("252")).is_none());
}

#[test]
fn calmar_matches_hand_computation() {
    // Mean step return 0.0658008658008658 × 252 over a 5/110 drawdown.
    let equity = returns(&["100", "110", "105", "120"]);
    let ratio = calmar(&equity, Decimal::ZERO, decimal("252")).expect("defined");
    close_enough(ratio, 364.8);
}

#[test]
fn win_rate_counts_only_decisive_steps() {
    let steps = returns(&["10", "-5", "0", "15", "0"]);
    let rate = win_rate(&steps).expect("two wins of three decisive");
    close_enough(rate, 2.0 / 3.0);
    assert!(win_rate(&returns(&["0", "0"])).is_none());
    close_enough(win_rate(&returns(&["1", "2"])).expect("all-win"), 1.0);
    close_enough(win_rate(&returns(&["-1", "-2"])).expect("all-loss"), 0.0);
}

#[test]
fn profit_factor_handles_one_sided_books() {
    let steps = returns(&["10", "-5", "15"]);
    let factor = profit_factor(&steps).expect("defined");
    close_enough(factor, 5.0);
    assert!(profit_factor(&[]).is_none(), "empty book is undefined");
    assert!(
        profit_factor(&returns(&["10", "5"])).is_none(),
        "all-win book is unbounded, never invented"
    );
    close_enough(
        profit_factor(&returns(&["-10", "-5"])).expect("all-loss is zero"),
        0.0,
    );
}

#[test]
fn step_returns_skip_pre_deployment_zeros() {
    let equity = returns(&["0", "0", "100", "110"]);
    assert_eq!(step_returns(&equity), vec![decimal("0.1")]);
    assert!(step_returns(&[]).is_empty());
    assert!(step_returns(&returns(&["0", "0"])).is_empty());
}

fn money(value: &str) -> protopyne_types::Money {
    protopyne_types::Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn manifest() -> crate::report::RunManifest {
    crate::report::RunManifest {
        data_version: "metrics-v1".to_owned(),
        code_version: env!("CARGO_PKG_VERSION").to_owned(),
        config_json: "{}".to_owned(),
        bar_count: 5,
        first_ts: String::new(),
        last_ts: String::new(),
        seed: 0,
    }
}

fn report_for(curve: &[&str]) -> crate::report::BacktestReport {
    crate::report::build_report(
        manifest(),
        Vec::new(),
        Vec::new(),
        0,
        0,
        money("0"),
        money("0"),
        money("0"),
        curve.iter().map(|value| money(value)).collect(),
        None,
    )
}

#[test]
fn flat_report_carries_none_not_zero() {
    let report = report_for(&["100", "100", "100"]);
    assert_eq!(report.sharpe, None);
    assert_eq!(report.sortino, None);
    assert_eq!(report.calmar, None);
    assert_eq!(report.win_rate, None);
    assert_eq!(report.profit_factor, None);
}

#[test]
fn moving_report_populates_all_five() {
    let report = report_for(&["0", "0", "100", "110", "105", "120"]);
    assert!(report.sharpe.is_some(), "sharpe wired");
    assert!(report.sortino.is_some(), "sortino wired");
    assert!(report.calmar.is_some(), "calmar wired");
    assert!(report.win_rate.is_some(), "win rate wired");
    assert!(report.profit_factor.is_some(), "profit factor wired");
}

#[test]
fn report_rerun_is_byte_identical() {
    let first = report_for(&["0", "0", "100", "110", "105", "120"]);
    let second = report_for(&["0", "0", "100", "110", "105", "120"]);
    assert_eq!(first.to_canonical_json(), second.to_canonical_json());
}

#[test]
fn legacy_report_json_reads_with_none_metrics() {
    // Pre-metrics JSON (no risk-adjusted keys) must still deserialize, with
    // the new fields absent rather than zeroed.
    let report = report_for(&["0", "0", "100", "110", "105", "120"]);
    let mut value = serde_json::to_value(&report).expect("serializable");
    let object = value.as_object_mut().expect("report is an object");
    for key in ["sharpe", "sortino", "calmar", "win_rate", "profit_factor"] {
        assert!(object.remove(key).is_some(), "new key present: {key}");
    }
    let legacy: crate::report::BacktestReport =
        serde_json::from_value(value).expect("old JSON reads");
    assert_eq!(legacy.sharpe, None);
    assert_eq!(legacy.sortino, None);
    assert_eq!(legacy.calmar, None);
    assert_eq!(legacy.win_rate, None);
    assert_eq!(legacy.profit_factor, None);
    assert_eq!(legacy.drawdown, report.drawdown, "old fields intact");
}
