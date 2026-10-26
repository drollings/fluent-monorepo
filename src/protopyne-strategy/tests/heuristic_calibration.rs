//! Heuristic calibration: the template and its first application.
//!
//! # The recipe (copy for the next heuristic)
//!
//! 1. Name the axis the heuristic converts across. A confidence→task-value
//!    conversion (an observation becoming an action) is the risky shape:
//!    state it, then measure the conversion — never the detector underneath
//!    (the detector has its own calibration).
//! 2. Build one treatment corpus where the observation fires and two
//!    controls: the dial off (the action must never fire) and the
//!    observation absent (the action must never fire unprompted).
//! 3. Measure precision (`P(observed | acted)`) and recall (`P(acted |
//!    observed, dial armed)`) over the corpora. A deterministic gate must
//!    read exactly `1.0`/`1.0` — anything else is a leak in either
//!    direction, and the tally below fails.
//! 4. Freeze the per-case trace as a golden tally beside the thresholds
//!    fingerprint. Re-record deliberately with `PROTOPYNE_RECORD=1`; a
//!    threshold edit that moves the tally fails until re-recorded.
//! 5. Register the id in [`crate::axes::calibrated_heuristic_ids`]. Cache,
//!    persist, and sizing paths consult
//!    [`crate::axes::require_calibrated`]; an unlisted id refuses.
//!
//! # First application: trail tightening on exhaustion
//!
//! The observation is exhaustion (`PositionManager` marks the position
//! exhausted); the action is tightening the trailing stop. The dial is the
//! `trail_on_exhaustion` switch. Outcome P&L is out of scope here — that
//! needs a backtest corpus and is a documented follow-up. What is
//! calibrated: the conversion fires exactly when the observation holds and
//! the dial arms it, never unprompted, never when disarmed.

use crate::axes::{calibrated_heuristic_ids, require_calibrated};
use crate::integration_lite::IntegrationLiteStrategyConfig;
use crate::position::{PositionManager, PositionSpec};
use crate::tape::sine_fixture;
use protopyne_types::session::TradingSession;

fn manager(strategy: IntegrationLiteStrategyConfig) -> PositionManager {
    PositionManager::new(
        PositionSpec::from(strategy),
        TradingSession::parse("0930-1600").expect("session"),
        60_000,
        true,
    )
}

fn treatment_config() -> IntegrationLiteStrategyConfig {
    IntegrationLiteStrategyConfig {
        sig_exhaust_level: 50.0,
        exit_action: "Off".to_owned(),
        trail_on_exhaustion: true,
        ..IntegrationLiteStrategyConfig::default()
    }
}

/// Drive sixty sine bars through the exit pass, recording
/// (exhausted, tight) after each bar.
fn trace(mut manager: PositionManager) -> Vec<(bool, bool)> {
    let (bars, legs) = sine_fixture(60);
    manager.open(true, 100, 100.0, 0, 1.0, &bars);
    let mut out = Vec::new();
    for at in 0..60 {
        let tape = crate::tape::Tape::new(&bars, &legs, &legs.close, at).expect("tape");
        let _ = manager.review_exits(&tape, &bars, at);
        let position = manager.position().expect("position stays open");
        out.push((position.exhausted, position.tight));
    }
    out
}

fn precision_recall(predicted: &[bool], actual: &[bool]) -> (f64, f64) {
    assert_eq!(predicted.len(), actual.len());
    let mut true_positives = 0u64;
    let mut predicted_positives = 0u64;
    let mut actual_positives = 0u64;
    for (predicted, actual) in predicted.iter().zip(actual) {
        true_positives += u64::from(*predicted && *actual);
        predicted_positives += u64::from(*predicted);
        actual_positives += u64::from(*actual);
    }
    let precision = if predicted_positives == 0 {
        1.0
    } else {
        true_positives as f64 / predicted_positives as f64
    };
    let recall = if actual_positives == 0 {
        1.0
    } else {
        true_positives as f64 / actual_positives as f64
    };
    (precision, recall)
}

#[test]
fn tightening_fires_exactly_when_exhaustion_fires_and_the_dial_arms_it() {
    let dialed = trace(manager(treatment_config()));
    let exhausted: Vec<bool> = dialed.iter().map(|(exhausted, _)| *exhausted).collect();
    let tight: Vec<bool> = dialed.iter().map(|(_, tight)| *tight).collect();
    assert!(
        exhausted.iter().any(|flag| *flag),
        "the treatment corpus must observe exhaustion at least once"
    );
    let (precision, recall) = precision_recall(&tight, &exhausted);
    assert_eq!(
        (precision, recall),
        (1.0, 1.0),
        "tightening converts the observation exactly: no unprompted fires, no missed responses"
    );
}

#[test]
fn a_disarmed_dial_never_tightens() {
    let mut config = treatment_config();
    config.trail_on_exhaustion = false;
    let dialed = trace(manager(config));
    assert!(
        dialed.iter().all(|(_, tight)| !tight),
        "the dial gates the response: disarmed means never tight"
    );
}

#[test]
fn a_quiet_tape_never_tightens() {
    let (tape_bars, tape_legs) = sine_fixture(60);
    let _ = (tape_bars, tape_legs);
    let flat: Vec<protopyne_types::Bar> = (0..60)
        .map(|_| {
            use chrono::TimeZone as _;
            use rust_decimal::Decimal;
            let close = Decimal::from(100);
            protopyne_types::Bar::new()
                .ts(chrono::Utc
                    .with_ymd_and_hms(2026, 1, 5, 15, 0, 0)
                    .single()
                    .expect("time"))
                .o(close)
                .h(close)
                .l(close)
                .c(close)
                .volume(10_000)
                .maybe_adj(None)
                .build()
        })
        .collect();
    let mut manager = manager(treatment_config());
    let legs = protopyne_kernels::lite::lite_legs(
        &flat,
        &crate::signals_lite::LiteIndicatorParams::default(),
        14,
    )
    .expect("legs");
    manager.open(true, 100, 100.0, 0, 1.0, &flat);
    for at in 0..60 {
        let tape = crate::tape::Tape::new(&flat, &legs, &legs.close, at).expect("tape");
        let _ = manager.review_exits(&tape, &flat, at);
        assert!(
            !manager.position().expect("open").tight,
            "bar {at}: no exhaustion on a flat tape, so never tight"
        );
    }
}

#[test]
fn only_calibrated_heuristics_back_trust() {
    assert!(calibrated_heuristic_ids().contains(&"trail_on_exhaustion"));
    assert!(require_calibrated("trail_on_exhaustion").is_ok());
    assert!(
        require_calibrated("unproven_heuristic").is_err(),
        "an uncalibrated id refuses before it can back caching, persistence, or sizing"
    );
}

fn dials_fingerprint() -> String {
    let config = treatment_config();
    format!(
        "trail_on_exhaustion={};exit_action={};sig_exhaust_level={};sig_dual={};sig_dual_slow={};sig_dual_fast={};sig_dual_lookback={}",
        config.trail_on_exhaustion,
        config.exit_action,
        config.sig_exhaust_level,
        config.sig_dual,
        config.sig_dual_slow,
        config.sig_dual_fast,
        config.sig_dual_lookback,
    )
}

fn tightening_tally() -> serde_json::Value {
    let treatment: Vec<serde_json::Value> = trace(manager(treatment_config()))
        .iter()
        .enumerate()
        .map(|(bar, (exhausted, tight))| {
            serde_json::json!({"bar": bar, "exhausted": exhausted, "tight": tight})
        })
        .collect();
    let mut disarmed = treatment_config();
    disarmed.trail_on_exhaustion = false;
    let disarmed_tight: u64 = trace(manager(disarmed))
        .iter()
        .filter(|(_, tight)| *tight)
        .count() as u64;
    serde_json::json!({
        "method": "Each sine bar is read once by the exit pass; the tally freezes (exhausted, tight) per bar. Evidence, not code: the trace is the conversion's contract.",
        "scope": "trail tightening on exhaustion only: no trigger, filter, sizing, or exit-kind verdict reads these rows",
        "treatment": treatment,
        "disarmed_tight_bars": disarmed_tight,
        "dials": dials_fingerprint(),
    })
}

fn tally_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("exhaustion_tighten_tally.json")
}

#[test]
fn the_tightening_tally_matches_the_frozen_trace() {
    let actual = serde_json::to_string_pretty(&tightening_tally()).expect("serializable");
    let path = tally_path();
    if std::env::var("PROTOPYNE_RECORD").is_ok() {
        std::fs::create_dir_all(path.parent().expect("tally has a parent"))
            .expect("tally directory exists");
        std::fs::write(&path, format!("{actual}\n")).expect("tally records");
        return;
    }
    let recorded: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&path).expect("the tightening tally is checked in"),
    )
    .expect("tally is JSON");
    let actual_value: serde_json::Value = serde_json::from_str(&actual).expect("serializable");
    assert_eq!(
        actual_value, recorded,
        "an exit-path or dial change moved the tightening trace"
    );
}
