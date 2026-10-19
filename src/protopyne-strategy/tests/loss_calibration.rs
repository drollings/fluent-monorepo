//! Loss-rule calibration: control groups plus frozen treatment
//! tallies.
//!
//! A loss rule is untrusted until its controls stay quiet and its treatment
//! tallies are frozen. The controls below are chosen so that a naive rule
//! would fire on them (a flat book, a winner below target, an absent mark,
//! a book exactly on a threshold); the calibrated rules must not. The
//! treatment tallies freeze the exact firing bars per rule over three
//! deterministic walks (a falling mark, a rising mark, a draining DTE);
//! a threshold edit moves a crossing bar and fails.
//!
//! Only the tallies are persisted (as a golden); the thresholds are literals
//! here, with a fingerprint recorded beside the tallies so that changing a
//! literal without re-freezing also fails. Deploying a calibrated rule set
//! as the default without the tally fails the ship gate at the bottom.

use super::*;
use std::path::PathBuf;

use rust_decimal::Decimal;
use serde_json::{json, Value};

// Thresholds under calibration. Editing any of these must move a tally.
const STOP_MAX_LOSS: i64 = 200;
const TARGET_GAIN: i64 = 400;
const TIME_FLOOR_DTE: u32 = 7;

fn money(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

fn book(dte_days: Option<u32>) -> PositionView {
    PositionView {
        underlying: "SPY".to_owned(),
        side: Direction::Long,
        quantity: 2,
        entry_avg: money(250),
        max_loss: money(600),
        dte_days,
    }
}

fn marked(price: i64) -> MarkContext {
    MarkContext {
        basis: protopyne_types::MarkBasis::ChainMid,
        mark: Some(money(price)),
    }
}

fn absent() -> MarkContext {
    MarkContext {
        basis: protopyne_types::MarkBasis::Absent,
        mark: None,
    }
}

fn rule_set() -> LossSet {
    LossSet::new(vec![
        Arc::new(StopLoss::new(money(STOP_MAX_LOSS)).expect("valid stop")),
        Arc::new(ProfitTarget::new(money(TARGET_GAIN)).expect("valid target")),
        Arc::new(TimeStop::new(TIME_FLOOR_DTE)),
    ])
}

fn thresholds_fingerprint() -> String {
    format!(
        "stop_max_loss={STOP_MAX_LOSS};target_gain={TARGET_GAIN};\
         time_floor_dte={TIME_FLOOR_DTE}"
    )
}

// ── Control group: must not fire ──────────────────────────────────────────

#[test]
fn a_flat_book_holds_every_rule() {
    assert!(
        rule_set().review(&book(Some(30)), &marked(250)).is_empty(),
        "no distance from entry means no trigger"
    );
}

#[test]
fn a_winner_below_target_and_a_loser_above_the_stop_hold() {
    let set = rule_set();
    // Up 200 against a 400 target; down 100 against a 200 stop.
    assert!(set.review(&book(Some(30)), &marked(350)).is_empty());
    assert!(set.review(&book(Some(30)), &marked(200)).is_empty());
}

#[test]
fn an_absent_mark_holds_every_rule() {
    // A book that would fire everything if it could be priced: deep past
    // the stop and below the time floor, with no mark to price it by.
    assert!(
        rule_set().review(&book(Some(1)), &absent()).is_empty(),
        "absence is not a trigger"
    );
}

#[test]
fn books_exactly_on_a_threshold_hold() {
    let set = rule_set();
    // Down exactly 200; up exactly 400; exactly 7 days left.
    assert!(
        set.review(&book(Some(30)), &marked(150)).is_empty(),
        "exactly on the stop is not past it"
    );
    assert!(
        set.review(&book(Some(30)), &marked(450)).is_empty(),
        "exactly on the target is not past it"
    );
    assert!(
        set.review(&book(Some(TIME_FLOOR_DTE)), &marked(250))
            .is_empty(),
        "exactly on the floor is not below it"
    );
}

// ── Treatment: frozen firing bars per rule ─────────────────────────────────

/// Review every bar of a walk through the full rule set, freezing which
/// bars fire which kind. Cross-talk is pinned too: a falling mark must not
/// trip the target or the time rule, and so on.
fn walk_tally(marks: &[MarkContext], dtes: &[Option<u32>]) -> Value {
    let set = rule_set();
    let mut firings: std::collections::BTreeMap<&str, Vec<usize>> = [
        ("stop", Vec::new()),
        ("profit-target", Vec::new()),
        ("time-stop", Vec::new()),
    ]
    .into_iter()
    .collect();
    for (bar, (mark, dte)) in marks.iter().zip(dtes.iter()).enumerate() {
        for action in set.review(&book(*dte), mark) {
            firings
                .get_mut(action.kind.as_str())
                .expect("a rule fires a known kind")
                .push(bar);
        }
    }
    json!({
        "stop": firings["stop"],
        "profit-target": firings["profit-target"],
        "time-stop": firings["time-stop"],
    })
}

fn treatment_tallies() -> Value {
    // Falling marks, expiry far away: the stop trips once at the crossing
    // bar (mark 149 puts the two-lot down 202, past the 200 stop).
    let stop_walk = walk_tally(
        &[marked(250), marked(200), marked(150), marked(149)],
        &[Some(30), Some(30), Some(30), Some(30)],
    );
    // Rising marks, expiry far away: the target trips once at its crossing
    // bar (mark 451 puts the two-lot up 402, past the 400 target).
    let target_walk = walk_tally(
        &[marked(250), marked(350), marked(450), marked(451)],
        &[Some(30), Some(30), Some(30), Some(30)],
    );
    // Flat mark, draining DTE: the time rule trips once below the floor.
    let flat = marked(250);
    let time_walk = walk_tally(
        &[flat, flat, flat, flat],
        &[Some(30), Some(8), Some(7), Some(6)],
    );
    // Every walk must trip exactly its own rule, once, at its last bar —
    // anything else is cross-talk or a moved crossing.
    for (name, walk) in [
        ("stop walk", &stop_walk),
        ("target walk", &target_walk),
        ("time walk", &time_walk),
    ] {
        let total: usize = ["stop", "profit-target", "time-stop"]
            .iter()
            .map(|kind| walk[kind].as_array().expect("bars").len())
            .sum();
        assert_eq!(total, 1, "{name} must fire exactly once");
    }
    json!({
        "stop_path": stop_walk,
        "target_path": target_walk,
        "time_path": time_walk,
        "thresholds": thresholds_fingerprint(),
        "ship_state": SHIP_STATE,
        "operator_accepted": OPERATOR_ACCEPTED,
    })
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("loss_tally.json")
}

fn recorded_tally() -> Value {
    let path = golden_path();
    let text = if std::env::var("PROTOPYNE_RECORD").is_ok() {
        let tally = treatment_tallies();
        let text = serde_json::to_string_pretty(&tally).expect("serializable");
        std::fs::create_dir_all(path.parent().expect("golden has a parent"))
            .expect("baseline directory exists");
        std::fs::write(&path, &text).expect("the tally records");
        text
    } else {
        std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("golden {} records first", path.display()))
    };
    serde_json::from_str(&text).expect("the tally is JSON")
}

#[test]
fn the_frozen_tally_matches() {
    let actual = serde_json::to_string_pretty(&treatment_tallies()).expect("serializable");
    let recorded = recorded_tally();
    assert_eq!(
        serde_json::to_string_pretty(&recorded).expect("serializable"),
        actual,
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

// ── Ship gate: no default deployment without the tally ─────────────────────

/// Recorded operator acceptance. Changing this is the operator's act.
const OPERATOR_ACCEPTED: bool = false;

/// The ship state the calibration currently supports.
const SHIP_STATE: &str = "unshipped";

/// The ship gate: the default installation state and the recorded ship state
/// must agree; shipping requires confirmed acceptance and a quiet control
/// group. A bare flip of either side fails.
#[test]
fn the_ship_state_matches_the_default_installation() {
    let recorded = recorded_tally();
    let strategy = Strategy::with_signal(std::sync::Arc::new(
        FixedSignal::new(Direction::Long, 0.8, 0.7).expect("valid scores"),
    ));
    let installed = strategy.loss_name();
    let shipped = recorded["ship_state"].as_str().expect("ship state") == "shipped";
    assert_eq!(
        installed == "hold",
        !shipped,
        "the default strategy installation must match the calibrated ship state"
    );
    if shipped {
        assert!(
            recorded["operator_accepted"].as_bool().unwrap_or(false),
            "shipping needs recorded operator acceptance"
        );
    }
}
