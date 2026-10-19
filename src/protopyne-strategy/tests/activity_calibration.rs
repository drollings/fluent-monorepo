//! Chain-activity calibration: a quiet control group, a labeled treatment
//! window, and the gate that keeps the proxy uninstalled until both agree.
//!
//! Controls are probes with a required outcome: no control may score above
//! the recorded floor. The treatment is a small synthetic window whose
//! activity labels are committed beside this test; the measure is rank
//! correlation, not precision/recall, because no external label exists. The
//! tally (control maxima, correlation, floor) is frozen in
//! `tests/data/chain_activity_tally.json`; record it with `PROTOPYNE_RECORD=1`
//! only after a deliberate change.

use super::*;
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use protopyne_types::{ChainQuote, Money, OptionContract, OptionKind, Timeframe};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::elements::Strategy;
use crate::view::{ChainSlice, MarketData, MarketView};
use crate::{Direction, FixedSignal};

/// A control may not score above this floor.
const CALIBRATION_FLOOR: f64 = 0.2;

/// The treatment must rank at least this well against the labels for an
/// operator to review enabling it.
const MIN_SPEARMAN: f64 = 0.5;

/// Recorded operator acceptance. Changing this is the operator's act.
const OPERATOR_ACCEPTED: bool = false;

/// The ship state the calibration currently supports.
const SHIP_STATE: &str = "disabled";

const CONTROL_BARS: i64 = 8;

fn money(value: f64) -> Money {
    Money::new(Decimal::from_f64_retain(value).expect("finite money"))
}

fn start() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 15, 14, 30, 0).unwrap()
}

fn at(minute: i64) -> DateTime<Utc> {
    start() + Duration::minutes(minute)
}

fn quote(
    strike: f64,
    kind: OptionKind,
    bid: f64,
    ask: f64,
    bid_size: u32,
    ask_size: u32,
    open_interest: u64,
    stamp: DateTime<Utc>,
) -> ChainQuote {
    ChainQuote::new()
        .contract(
            OptionContract::new()
                .underlying("SPY".to_owned())
                .expiry(NaiveDate::from_ymd_opt(2026, 7, 17).unwrap())
                .strike(money(strike))
                .kind(kind)
                .build(),
        )
        .bid(money(bid))
        .ask(money(ask))
        .bid_size(bid_size)
        .ask_size(ask_size)
        .open_interest(open_interest)
        .timestamp(stamp)
        .build()
}

/// One score over `quotes` at the view instant. `closes` is the spot source.
fn score(
    source: &ChainActivityScore,
    quotes: &[ChainQuote],
    as_of: DateTime<Utc>,
    closes: &[f64],
) -> Option<f64> {
    let chain = ChainSlice::new(quotes, as_of, "SPY").expect("quotes bind at the instant");
    let data = MarketData {
        bars: &[],
        chain: Some(chain),
        dte_days: 30,
        timeframe: Timeframe::D1,
    };
    let view = MarketView {
        data: &data,
        closes,
        as_of,
    };
    source.score(&view).expect("the proxy runs")
}

fn control_max(scores: &[Option<f64>]) -> Option<f64> {
    scores.iter().flatten().copied().reduce(f64::max)
}

fn control_tally(scores: &[Option<f64>]) -> Value {
    let max_score = control_max(scores);
    json!({
        "bars": scores.len(),
        "scored_bars": scores.iter().filter(|score| score.is_some()).count(),
        "max_score": max_score,
        "quiet": max_score.is_none_or(|max| max <= CALIBRATION_FLOOR),
    })
}

/// A live but never-changing book: symmetric sizes and even open interest.
fn constant_control() -> Vec<Option<f64>> {
    let source = ChainActivityScore::new();
    let closes = [100.0];
    (0..CONTROL_BARS)
        .map(|minute| {
            let stamp = at(minute);
            let quotes = [
                quote(100.0, OptionKind::Call, 2.0, 2.2, 20, 20, 50, stamp),
                quote(105.0, OptionKind::Call, 2.2, 2.4, 20, 20, 50, stamp),
            ];
            score(&source, &quotes, stamp, &closes)
        })
        .collect()
}

/// A book with no resting size and no open interest.
fn zero_size_control() -> Vec<Option<f64>> {
    let source = ChainActivityScore::new();
    let closes = [100.0];
    (0..CONTROL_BARS)
        .map(|minute| {
            let stamp = at(minute);
            let quotes = [
                quote(100.0, OptionKind::Call, 2.0, 2.2, 0, 0, 0, stamp),
                quote(105.0, OptionKind::Call, 2.2, 2.4, 0, 0, 0, stamp),
            ];
            score(&source, &quotes, stamp, &closes)
        })
        .collect()
}

/// One listed strike, however active.
fn single_strike_control() -> Vec<Option<f64>> {
    let source = ChainActivityScore::new();
    let closes = [100.0];
    (0..CONTROL_BARS)
        .map(|minute| {
            let stamp = at(minute);
            let quotes = [
                quote(100.0, OptionKind::Call, 2.0, 2.2, 40, 0, 80, stamp),
                quote(100.0, OptionKind::Put, 2.0, 2.2, 40, 0, 80, stamp),
            ];
            score(&source, &quotes, stamp, &closes)
        })
        .collect()
}

/// A frozen tape: the view advances, the quotes never do.
fn stuck_timestamp_control() -> Vec<Option<f64>> {
    let source = ChainActivityScore::new();
    let closes = [100.0];
    let frozen = at(0);
    let quotes = [
        quote(100.0, OptionKind::Call, 2.0, 2.2, 40, 0, 80, frozen),
        quote(105.0, OptionKind::Call, 2.2, 2.4, 30, 0, 60, frozen),
    ];
    (0..CONTROL_BARS)
        .map(|minute| {
            let as_of = at(20 + minute);
            score(&source, &quotes, as_of, &closes)
        })
        .collect()
}

/// The candidate treatment window: activity rises from a balanced book to a
/// one-sided, concentrated one. Each row is `(bid sizes, ask sizes, OI)`.
const TREATMENT_ROWS: &[(u32, u32, u32, u32, u64, u64)] = &[
    (20, 20, 20, 20, 50, 50),
    (25, 20, 15, 20, 60, 40),
    (30, 20, 10, 20, 70, 30),
    (40, 25, 5, 15, 80, 20),
    (45, 30, 2, 10, 90, 10),
    (60, 40, 0, 0, 95, 5),
];

fn treatment_scores() -> Vec<f64> {
    let source = ChainActivityScore::new();
    let closes = [100.0];
    TREATMENT_ROWS
        .iter()
        .enumerate()
        .map(|(index, &(b1, b2, a1, a2, oi1, oi2))| {
            let stamp = at(index as i64);
            let quotes = [
                quote(100.0, OptionKind::Call, 2.0, 2.2, b1, a1, oi1, stamp),
                quote(105.0, OptionKind::Call, 2.2, 2.4, b2, a2, oi2, stamp),
            ];
            score(&source, &quotes, stamp, &closes).expect("a fresh treatment row scores")
        })
        .collect()
}

fn labels_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("chain_activity_labels.json")
}

fn treatment_labels() -> Vec<f64> {
    let recorded: Value = serde_json::from_str(
        &std::fs::read_to_string(labels_path()).expect("the labeled window is committed"),
    )
    .expect("labels are JSON");
    recorded["labels"]
        .as_array()
        .expect("labels are a list")
        .iter()
        .map(|label| label.as_f64().expect("labels are numbers"))
        .collect()
}

/// Average ranks, so tied labels share the mean position.
fn ranks(values: &[f64]) -> Vec<f64> {
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&left, &right| values[left].total_cmp(&values[right]));
    let mut ranked = vec![0.0; values.len()];
    let mut index = 0;
    while index < order.len() {
        let mut end = index + 1;
        while end < order.len()
            && values[order[end]].total_cmp(&values[order[index]]) == std::cmp::Ordering::Equal
        {
            end += 1;
        }
        let rank = (index + end - 1) as f64 / 2.0 + 1.0;
        for slot in &order[index..end] {
            ranked[*slot] = rank;
        }
        index = end;
    }
    ranked
}

fn spearman(left: &[f64], right: &[f64]) -> f64 {
    assert_eq!(left.len(), right.len(), "rank series agree in length");
    let left = ranks(left);
    let right = ranks(right);
    let mean = |values: &[f64]| values.iter().sum::<f64>() / values.len() as f64;
    let (left_mean, right_mean) = (mean(&left), mean(&right));
    let covariance: f64 = left
        .iter()
        .zip(&right)
        .map(|(l, r)| (l - left_mean) * (r - right_mean))
        .sum();
    let left_var: f64 = left.iter().map(|l| (l - left_mean).powi(2)).sum();
    let right_var: f64 = right.iter().map(|r| (r - right_mean).powi(2)).sum();
    if left_var <= 0.0 || right_var <= 0.0 {
        return 0.0;
    }
    covariance / (left_var * right_var).sqrt()
}

fn thresholds_fingerprint() -> String {
    let source = ChainActivityScore::new();
    format!(
        "band={};max_quote_age_secs={};floor={CALIBRATION_FLOOR}",
        source.near_money_band(),
        source.max_quote_age_secs()
    )
}

fn tallies() -> Value {
    let constant = constant_control();
    let zero_size = zero_size_control();
    let single_strike = single_strike_control();
    let stuck = stuck_timestamp_control();
    let treatment = treatment_scores();
    let labels = treatment_labels();
    json!({
        "floor": CALIBRATION_FLOOR,
        "thresholds": thresholds_fingerprint(),
        "controls": {
            "constant": control_tally(&constant),
            "zero_size": control_tally(&zero_size),
            "single_strike": control_tally(&single_strike),
            "stuck_timestamps": control_tally(&stuck),
        },
        "treatment": {
            "bars": treatment.len(),
            "spearman": spearman(&treatment, &labels),
            "min_spearman": MIN_SPEARMAN,
        },
        "operator_accepted": OPERATOR_ACCEPTED,
        "ship_state": SHIP_STATE,
    })
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("chain_activity_tally.json")
}

fn recorded_tally() -> Value {
    let path = golden_path();
    if std::env::var("PROTOPYNE_RECORD").is_ok() {
        let text = serde_json::to_string_pretty(&tallies()).expect("serializable");
        std::fs::create_dir_all(path.parent().expect("golden has a parent"))
            .expect("the data directory exists");
        std::fs::write(&path, &text).expect("the tally records");
        return tally_from_text(&text);
    }
    tally_from_text(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("golden {} records first", path.display())),
    )
}

fn tally_from_text(text: &str) -> Value {
    serde_json::from_str(text).expect("the tally is JSON")
}

#[test]
fn every_control_stays_at_or_below_the_floor() {
    for (name, scores) in [
        ("constant", constant_control()),
        ("zero size", zero_size_control()),
        ("single strike", single_strike_control()),
        ("stuck timestamps", stuck_timestamp_control()),
    ] {
        let max = control_max(&scores);
        assert!(
            max.is_none_or(|max| max <= CALIBRATION_FLOOR),
            "control {name} scored {max:?}, above the floor {CALIBRATION_FLOOR}"
        );
    }
}

#[test]
fn the_treatment_correlation_meets_the_review_bar() {
    let correlation = spearman(&treatment_scores(), &treatment_labels());
    assert!(
        correlation >= MIN_SPEARMAN,
        "the labeled window ranks at {correlation}, below {MIN_SPEARMAN}"
    );
}

#[test]
fn the_frozen_tally_matches() {
    let actual = serde_json::to_string_pretty(&tallies()).expect("serializable");
    let recorded = recorded_tally();
    assert_eq!(
        recorded,
        tally_from_text(&actual),
        "the calibration tally must match the frozen golden"
    );
}

#[test]
fn the_threshold_fingerprint_matches_the_golden() {
    if std::env::var("PROTOPYNE_RECORD").is_ok() {
        return;
    }
    let recorded = recorded_tally();
    assert_eq!(
        recorded["thresholds"]
            .as_str()
            .expect("fingerprint present"),
        thresholds_fingerprint(),
        "a threshold changed without re-freezing the tally"
    );
}

/// The ship gate: the default installation state and the recorded ship state
/// must agree; enabling requires confirmed acceptance and a quiet control
/// group. A bare flip of either side fails.
#[test]
fn the_ship_state_matches_the_default_installation() {
    let recorded = recorded_tally();
    let installed = Strategy::with_signal(std::sync::Arc::new(
        FixedSignal::new(Direction::Long, 0.8, 0.7).expect("valid scores"),
    ))
    .has_options_activity();
    let shipped = recorded["ship_state"].as_str().expect("ship state") == "enabled";
    assert_eq!(
        installed, shipped,
        "the default strategy installation must match the calibrated ship state"
    );
    if shipped {
        assert!(
            recorded["operator_accepted"].as_bool().unwrap_or(false),
            "enabling needs recorded operator acceptance"
        );
        for control in recorded["controls"].as_object().expect("controls").values() {
            assert_eq!(
                control["quiet"].as_bool(),
                Some(true),
                "enabling needs every control quiet, got {control}"
            );
        }
        let spearman = recorded["treatment"]["spearman"]
            .as_f64()
            .expect("correlation recorded");
        assert!(
            spearman >= MIN_SPEARMAN,
            "enabling needs the treatment above the review bar"
        );
    }
}

#[test]
fn a_bound_band_and_age_stay_positive() {
    let source = ChainActivityScore::new()
        .with_max_quote_age_secs(60)
        .expect("a positive age");
    assert_eq!(source.max_quote_age_secs(), 60);
    assert!(source.with_max_quote_age_secs(0).is_err());
    assert!(ChainActivityScore::with_band(0.5).is_ok());
}
