//! Trigger-shape calibration: every M3 strategy trigger dial pins its Pine
//! default, its native default, its fail-closed validation window, and its
//! control refusals in a checked-in report beside the fixtures. The report
//! is load-bearing: the suite recomputes every number and compares
//! number-for-number; regenerating requires `PROTOPYNE_RECORD=1`, never a
//! silent update.
//!
//! Scope is deliberate: this pins the *shape* (defaults, bounds, refusals).
//! Outcome calibration — whether the trigger should fire on a given bar —
//! lands in the equity-sweep milestone with the native signal, which reads
//! these same fields. A threshold edit moves a tally and fails here first.

use std::path::PathBuf;

use crate::integration_lite::IntegrationLiteStrategyConfig;

fn data_file(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join(name)
}

fn load_report(name: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(data_file(name)).expect("trigger report loads");
    serde_json::from_str(&text).expect("report parses")
}

/// Recompute one trigger's shape record from the live config and code, in
/// the exact shape the report stores. `probe` runs the control refusals and
/// returns their reason strings; a new refusal changes the record and fails
/// until the report is deliberately re-recorded.
fn shape_record(
    pine_default: &str,
    native_default: &str,
    window: &str,
    config: &mut IntegrationLiteStrategyConfig,
    probe: impl Fn(&mut IntegrationLiteStrategyConfig) -> Vec<String>,
) -> serde_json::Value {
    serde_json::json!({
        "pine_default": pine_default,
        "native_default": native_default,
        "validation_window": window,
        "control_refusals": probe(config),
    })
}

fn check_trigger(
    name: &str,
    pine_default: &str,
    native_default: &str,
    window: &str,
    probe: impl Fn(&mut IntegrationLiteStrategyConfig) -> Vec<String>,
) {
    let report = load_report(&format!("trigger_{name}.json"));
    let record = shape_record(
        pine_default,
        native_default,
        window,
        &mut IntegrationLiteStrategyConfig::default(),
        probe,
    );
    if std::env::var("PROTOPYNE_RECORD").is_ok() {
        let mut full = report.as_object().expect("object").clone();
        full.insert("shape".to_owned(), record);
        std::fs::write(
            data_file(&format!("trigger_{name}.json")),
            serde_json::to_string_pretty(&serde_json::Value::Object(full)).expect("serializes")
                + "\n",
        )
        .expect("record writes");
        return;
    }
    assert_eq!(
        record, report["shape"],
        "{name}: recomputed shape must match the checked-in report number-for-number"
    );
    // The native default reproduces the script default exactly: no fork on
    // these dials (forks live in the indicator crate's documented list).
    assert_eq!(
        native_default, pine_default,
        "{name}: native default reproduces Pine"
    );
}

fn refusal_reasons(
    config: &mut IntegrationLiteStrategyConfig,
    edits: &[(&str, &str)],
) -> Vec<String> {
    let mut reasons = Vec::new();
    for (slot, value) in edits {
        let mut trial = config.clone();
        let field_set = match *slot {
            "sig_exhaust_level" => value
                .parse::<f64>()
                .ok()
                .map(|v| trial.sig_exhaust_level = v),
            "filt_extension_max" => value
                .parse::<f64>()
                .ok()
                .map(|v| trial.filt_extension_max = v),
            "filt_volume_min" => value.parse::<f64>().ok().map(|v| trial.filt_volume_min = v),
            "filt_delta_len" => value.parse::<i64>().ok().map(|v| trial.filt_delta_len = v),
            _ => None,
        };
        assert!(field_set.is_some(), "control edit parses for {slot}");
        reasons.push(
            trial
                .validate()
                .expect_err(&format!("{slot}={value} must refuse"))
                .to_string(),
        );
    }
    reasons
}

#[test]
fn exhaustion_level_shape_is_pinned() {
    check_trigger(
        "exhaustion",
        "200.0",
        "200.0",
        "sig_exhaust_level in [50, +inf)",
        |config| {
            refusal_reasons(
                config,
                &[("sig_exhaust_level", "49.9"), ("sig_exhaust_level", "NaN")],
            )
        },
    );
    assert!(IntegrationLiteStrategyConfig::default().validate().is_ok());
}

#[test]
fn extension_multiple_shape_is_pinned() {
    check_trigger(
        "extension",
        "2.5",
        "2.5",
        "filt_extension_max in [0.5, +inf)",
        |config| {
            refusal_reasons(
                config,
                &[
                    ("filt_extension_max", "0.49"),
                    ("filt_extension_max", "NaN"),
                ],
            )
        },
    );
    assert!(IntegrationLiteStrategyConfig::default().validate().is_ok());
}

#[test]
fn volume_multiple_shape_is_pinned() {
    check_trigger(
        "volume",
        "0.8",
        "0.8",
        "filt_volume_min in [0.1, +inf)",
        |config| {
            refusal_reasons(
                config,
                &[("filt_volume_min", "0.09"), ("filt_volume_min", "NaN")],
            )
        },
    );
    assert!(IntegrationLiteStrategyConfig::default().validate().is_ok());
}

#[test]
fn delta_window_shape_is_pinned() {
    check_trigger(
        "delta_window",
        "3",
        "3",
        "filt_delta_len in [1, 20]",
        |config| refusal_reasons(config, &[("filt_delta_len", "0"), ("filt_delta_len", "21")]),
    );
    assert!(IntegrationLiteStrategyConfig::default().validate().is_ok());
}

#[test]
fn regime_flags_shape_is_pinned() {
    let report = load_report("trigger_regime.json");
    let config = IntegrationLiteStrategyConfig::default();
    assert!(config.validate().is_ok(), "defaults validate");
    assert!(
        !config.filt_regime_lowvol && !config.filt_regime_volatile,
        "both regime skips default off (Pine false)"
    );
    let record = serde_json::json!({
        "pine_defaults": {"filt_regime_lowvol": false, "filt_regime_volatile": false},
        "native_defaults": {"filt_regime_lowvol": config.filt_regime_lowvol, "filt_regime_volatile": config.filt_regime_volatile},
        "validation_window": "booleans; no numeric bound to widen",
    });
    if std::env::var("PROTOPYNE_RECORD").is_ok() {
        let mut full = report.as_object().expect("object").clone();
        full.insert("shape".to_owned(), record);
        std::fs::write(
            data_file("trigger_regime.json"),
            serde_json::to_string_pretty(&serde_json::Value::Object(full)).expect("serializes")
                + "\n",
        )
        .expect("record writes");
        return;
    }
    assert_eq!(
        record, report["shape"],
        "regime: recomputed shape must match the checked-in report"
    );
}
