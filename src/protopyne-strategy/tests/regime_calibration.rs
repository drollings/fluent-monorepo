//! Regime-gate corpus: every label arm reachable, absence fail-closed.
//!
//! The detector is untrusted until each of its five arms fires on a named
//! input and its quiet arm holds on absent evidence. The corpus below names
//! one input triple per arm plus the all-absent control; the tally freezes
//! the exact label each triple reads, so a threshold edit moves a label and
//! fails here first. The stability wrapper is measured in its own table: it
//! may hold a label the detector would drop, which is label hysteresis, not
//! a second detector, so its rows never assert parity with the detector.

use std::path::PathBuf;

use protopyne_kernels::market_context::{detect_regime, detect_regime_stable, MarketRegime};
use serde_json::{json, Value};

fn data_file(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join(name)
}

fn thresholds_fingerprint() -> String {
    format!(
        "trend_floor={};volatility_floor={};volume_floor={};volatility_scale={};volume_scale={}",
        protopyne_kernels::market_context::TREND_SCORE_FLOOR,
        protopyne_kernels::market_context::VOLATILITY_SCORE_FLOOR,
        protopyne_kernels::market_context::VOLUME_SCORE_FLOOR,
        protopyne_kernels::market_context::VOLATILITY_SCALE,
        protopyne_kernels::market_context::VOLUME_SCALE,
    )
}

type Surfaces = (Option<f64>, Option<f64>, Option<f64>);

/// One detector row: the three optional surfaces and the label they read.
/// `None` is an absent surface (`na` on a warming bar), never a zero.
fn corpus() -> Vec<(Surfaces, MarketRegime)> {
    vec![
        // Strong positive health wins outright, siblings absent.
        ((Some(30.0), None, None), MarketRegime::TrendingBull),
        // Strong negative health wins outright, siblings absent.
        ((Some(-30.0), None, None), MarketRegime::TrendingBear),
        // Dispersion wins the tie: trend 10 below its floor, 8x3 = 24 the
        // max and past the volatility floor, volume 10 present but beaten.
        ((Some(10.0), Some(3.0), Some(1.0)), MarketRegime::Volatile),
        // Participation wins the tie: trend 5, 8x1 = 8 dispersion, 10x3 =
        // 30 participation past the volume floor.
        ((Some(5.0), Some(1.0), Some(3.0)), MarketRegime::Ranging),
        // Control: all absent reads the fail-closed quiet label.
        ((None, None, None), MarketRegime::LowVolume),
        // Control: a present-but-beaten dispersion with an absent trend still
        // reads quiet (the volatility arm needs the tie and the floor).
        ((None, Some(1.0), Some(0.5)), MarketRegime::LowVolume),
    ]
}

/// Stability rows: (held label, triple) and the label the wrapper keeps.
/// The wrapper may hold where the detector would switch; that hysteresis is
/// the measured behaviour, pinned here and never confused with detection.
type HeldTriple = (MarketRegime, Surfaces, MarketRegime);

fn stable_rows() -> Vec<HeldTriple> {
    vec![
        // A fresh signal from quiet is never vetoed.
        (
            MarketRegime::LowVolume,
            (Some(10.0), Some(3.0), Some(1.0)),
            MarketRegime::Volatile,
        ),
        // Same-surface sign flips switch immediately: the sign is signal.
        (
            MarketRegime::TrendingBull,
            (Some(-30.0), None, None),
            MarketRegime::TrendingBear,
        ),
        // A 14-point cross-surface win beats the 5-point band: switch.
        (
            MarketRegime::TrendingBull,
            (Some(10.0), Some(3.0), Some(0.5)),
            MarketRegime::Volatile,
        ),
        // A decayed hold below floor-minus-band releases to quiet.
        (
            MarketRegime::Ranging,
            (Some(10.0), Some(2.0), Some(1.0)),
            MarketRegime::LowVolume,
        ),
        // A hold inside floor-minus-band stays: the flicker is not earned.
        (
            MarketRegime::Ranging,
            (Some(5.0), Some(1.0), Some(2.0)),
            MarketRegime::Ranging,
        ),
    ]
}

fn treatment_tally() -> Value {
    let rows: Vec<Value> = corpus()
        .iter()
        .map(|((health, dispersion, participation), _)| {
            let label = detect_regime(*health, *dispersion, *participation);
            json!({
                "health": health,
                "volatility_ratio": dispersion,
                "vol_ratio": participation,
                "label": label.to_string(),
            })
        })
        .collect();
    let stable: Vec<Value> = stable_rows()
        .iter()
        .map(|(prev, (health, dispersion, participation), _)| {
            let label = detect_regime_stable(*prev, *health, *dispersion, *participation);
            json!({
                "held": prev.to_string(),
                "health": health,
                "volatility_ratio": dispersion,
                "vol_ratio": participation,
                "label": label.to_string(),
            })
        })
        .collect();
    json!({
        "method": "Each corpus triple is read once by the frozen detector (exact label equality) and each stability row once by the hysteresis wrapper; the tally freezes every label. Evidence, not code: the labels are the gate's contract.",
        "scope": "regime gate only: no trigger, filter, sizing, or exit reads these rows",
        "detector": rows,
        "stability": stable,
        "thresholds": thresholds_fingerprint(),
    })
}

fn golden_path() -> PathBuf {
    data_file("regime_corpus_tally.json")
}

#[test]
fn every_regime_arm_reads_its_label_and_absence_reads_quiet() {
    for ((health, dispersion, participation), expected) in corpus() {
        assert_eq!(
            detect_regime(health, dispersion, participation),
            expected,
            "health={health:?} dispersion={dispersion:?} participation={participation:?}"
        );
    }
}

#[test]
fn the_stability_wrapper_holds_and_releases_on_its_own_table() {
    for (prev, (health, dispersion, participation), expected) in stable_rows() {
        assert_eq!(
            detect_regime_stable(prev, health, dispersion, participation),
            expected,
            "held={prev} health={health:?} dispersion={dispersion:?} participation={participation:?}"
        );
    }
}

#[test]
fn the_corpus_tally_matches_the_frozen_labels() {
    let actual = serde_json::to_string_pretty(&treatment_tally()).expect("serializable");
    let path = golden_path();
    if std::env::var("PROTOPYNE_RECORD").is_ok() {
        std::fs::create_dir_all(path.parent().expect("tally has a parent"))
            .expect("tally directory exists");
        std::fs::write(&path, format!("{actual}\n")).expect("tally records");
        return;
    }
    let recorded: Value = serde_json::from_str(
        &std::fs::read_to_string(&path).expect("the corpus tally is checked in"),
    )
    .expect("tally is JSON");
    let actual_value: Value = serde_json::from_str(&actual).expect("serializable");
    assert_eq!(
        actual_value, recorded,
        "a detector or wrapper change moved a corpus label"
    );
}

#[test]
fn the_threshold_fingerprint_matches_the_tally() {
    if std::env::var("PROTOPYNE_RECORD").is_ok() {
        return;
    }
    let recorded: Value = serde_json::from_str(
        &std::fs::read_to_string(golden_path()).expect("the corpus tally is checked in"),
    )
    .expect("tally is JSON");
    assert_eq!(
        recorded["thresholds"]
            .as_str()
            .expect("fingerprint present"),
        thresholds_fingerprint(),
        "a threshold changed without re-freezing the corpus"
    );
}
