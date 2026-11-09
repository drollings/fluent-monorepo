use super::{
    apply_baseline_update, baseline_replays, baseline_to_value, score_metrics, Baseline,
    BaselineUpdateError, Measured,
};
use std::collections::HashMap;

// Ratchet discipline: tolerance bands pass, true regressions fail, the
// reasoned-update command requires a reason and refuses silent
// regressions, and the checked-in file proves every value went through it.

fn measured(name: &str, value: f64, higher: bool) -> Measured {
    Measured {
        name: name.to_string(),
        value,
        repro: "repro".to_string(),
        higher_is_better: higher,
    }
}

fn policy_baseline() -> Baseline {
    Baseline {
        metrics: [("m.ok".to_string(), 1.0), ("speed.ms".to_string(), 100.0)]
            .into_iter()
            .collect(),
        tolerances: [("speed.ms".to_string(), 10.0)].into_iter().collect(),
        bounds: [("speed.ms".to_string(), 200.0)].into_iter().collect(),
        history: Vec::new(),
    }
}

#[test]
fn tolerance_band_passes_true_regression_fails() {
    // Within 10 of 100: pass. Beyond it: violation.
    let within = score_metrics(
        &[measured("speed.ms", 109.0, false)],
        &Some(policy_baseline()),
    );
    assert_eq!(within.lines[0].status, super::MetricStatus::Pass);
    let beyond = score_metrics(
        &[measured("speed.ms", 111.0, false)],
        &Some(policy_baseline()),
    );
    assert_eq!(beyond.lines[0].status, super::MetricStatus::Violation);
    assert_eq!(beyond.exit_code, 2);
    // Absolute bound holds even inside tolerance: ceiling 200.
    let over_ceiling = score_metrics(
        &[measured("speed.ms", 250.0, false)],
        &Some(Baseline {
            tolerances: [("speed.ms".to_string(), 1000.0)].into_iter().collect(),
            ..policy_baseline()
        }),
    );
    assert_eq!(over_ceiling.lines[0].status, super::MetricStatus::Violation);
}

#[test]
fn update_needs_reason_and_refuses_silent_regression() {
    let base = policy_baseline();
    let err = apply_baseline_update(
        Some(&base),
        &[measured("m.ok", 0.5, true)],
        &[],
        &[],
        "",
        false,
        "2026-11-09",
    )
    .expect_err("empty reason fails");
    assert_eq!(err, BaselineUpdateError::EmptyReason);
    let err = apply_baseline_update(
        Some(&base),
        &[measured("m.ok", 0.5, true)],
        &[],
        &[],
        "lower the bar",
        false,
        "2026-11-09",
    )
    .expect_err("silent regression fails");
    assert_eq!(
        err,
        BaselineUpdateError::Regression {
            metric: "m.ok".to_string()
        }
    );
}

#[test]
fn reasoned_update_records_diff_and_replays() {
    let base = policy_baseline();
    let next = apply_baseline_update(
        Some(&base),
        &[
            measured("m.ok", 1.0, true),
            measured("speed.ms", 105.0, false),
        ],
        &[],
        &[],
        "tolerance covers timer jitter",
        false,
        "2026-11-09",
    )
    .expect("reasoned update passes");
    // Within tolerance: not a regression, recorded anyway as the new value.
    assert!((next.metrics["speed.ms"] - 105.0).abs() <= f64::EPSILON);
    let entry = next.history.last().expect("history appended");
    assert_eq!(entry.reason, "tolerance covers timer jitter");
    assert_eq!(entry.changes["speed.ms"], (Some(100.0), 105.0));
    assert!(baseline_replays(&next));
    // Allowed regressions are recorded explicitly, not smuggled.
    let allowed = apply_baseline_update(
        Some(&next),
        &[measured("m.ok", 0.9, true)],
        &[],
        &[],
        "documented methodology change",
        true,
        "2026-11-10",
    )
    .expect("allowed regression passes");
    assert_eq!(
        allowed.history.last().expect("entry").changes["m.ok"],
        (Some(1.0), 0.9)
    );
    assert!(baseline_replays(&allowed));
}

#[test]
fn hand_edit_without_history_fails_replay() {
    let base = policy_baseline();
    let mut edited = base.clone();
    edited.metrics.insert("m.ok".to_string(), 0.5);
    assert!(!baseline_replays(&edited), "unlogged edit must fail");
    let mut no_reason = base;
    no_reason.history.push(super::HistoryEntry {
        date: "2026-11-09".to_string(),
        reason: String::new(),
        changes: HashMap::new(),
        tolerances: HashMap::new(),
        bounds: HashMap::new(),
    });
    assert!(!baseline_replays(&no_reason), "empty reason must fail");
}

#[test]
fn checked_in_baseline_replays_with_reason() {
    let path = format!("{}/scorecard_baseline.json", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).expect("baseline reads");
    let value: serde_json::Value = serde_json::from_str(&text).expect("baseline parses");
    assert!(
        value
            .get("reason")
            .and_then(|reason| reason.as_str())
            .is_some_and(|reason| !reason.is_empty()),
        "baseline carries its reason"
    );
    let base = super::load_baseline(std::path::Path::new(&path)).expect("baseline loads");
    assert!(!base.history.is_empty(), "history present");
    assert!(
        baseline_replays(&base),
        "every value went through the command"
    );
}

#[test]
fn civil_date_anchors() {
    // Unix epoch and the 1234567890 anchor (2009-02-13).
    assert_eq!(super::days_to_civil(0), (1970, 1, 1));
    assert_eq!(super::days_to_civil(14288), (2009, 2, 13));
    assert_eq!(super::days_to_civil(10957), (2000, 1, 1));
    for days in [1, 30, 365, 1000, 10000, 20000] {
        let (year, month, day) = super::days_to_civil(days);
        assert!((1..=12).contains(&month), "{days}");
        assert!((1..=31).contains(&day), "{days}");
        assert!(year >= 1970, "{days}");
    }
}

#[test]
fn baseline_serializes_with_history() {
    let base = policy_baseline();
    let next = apply_baseline_update(
        Some(&base),
        &[measured("m.ok", 1.0, true)],
        &[],
        &[],
        "initial capture",
        false,
        "2026-11-09",
    )
    .expect("update");
    let value = baseline_to_value(&next, "initial capture", "2026-11-09");
    assert_eq!(value["history"].as_array().expect("history").len(), 2);
    assert_eq!(value["history"][1]["reason"], "initial capture");
    assert_eq!(value["metrics"]["m.ok"], 1.0);
}
