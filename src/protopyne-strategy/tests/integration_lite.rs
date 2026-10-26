//! Strategy-group schema contract: every `GROUP_S_*` input of the shipped
//! `_IntegrationLiteStrategy.pine` resolves to a typed dial with the script
//! default, validates fail-closed, and renders in its Pine group.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use fluent_wvr::validate::Validate;
use fluent_wvr::Describable as _;
use fluent_wvr::FieldAccess as _;

use crate::integration_lite::IntegrationLiteStrategyConfig;

fn data_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs")
}

fn coverage_table() -> serde_json::Value {
    let text = std::fs::read_to_string(
        data_root().join("../../src/protopyne-egui/tests/data/pine_input_coverage.json"),
    )
    .expect("coverage table loads");
    serde_json::from_str(&text).expect("coverage table parses")
}

/// Live strategy-group inputs in the shipped script: `name = input.<kind>(`
/// plus the generic `input(color.new(...))` color form, in file order.
fn pine_inputs() -> Vec<(String, String, String)> {
    let text =
        std::fs::read_to_string(data_root().join("strategies/_IntegrationLiteStrategy.pine"))
            .expect("shipped strategy loads");
    let mut inputs = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("//") {
            continue;
        }
        let Some(eq) = line.find("= input") else {
            continue;
        };
        let (name, rest) = line.split_at(eq);
        let name = name.trim();
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let mut rest = rest["= input".len()..].trim_start();
        let kind = if let Some(dotted) = rest.strip_prefix('.') {
            let end = dotted.find('(').unwrap_or(dotted.len());
            let kind = dotted[..end].to_owned();
            rest = &dotted[end + 1..];
            kind
        } else if rest.starts_with('(') {
            rest = &rest[1..];
            let head = rest.split(',').next().unwrap_or("").trim();
            if head.starts_with("color.") {
                "color".to_owned()
            } else {
                continue;
            }
        } else {
            continue;
        };
        let default = rest.split(',').next().unwrap_or("").trim().to_owned();
        inputs.push((name.to_owned(), kind, default));
    }
    inputs
}

fn pine_default_value(kind: &str, default: &str) -> Option<String> {
    match kind {
        "bool" | "int" | "float" => Some(default.to_owned()),
        "string" => default
            .strip_prefix('"')?
            .strip_suffix('"')?
            .to_owned()
            .into(),
        "enum" => default.rsplit('.').next()?.to_ascii_lowercase().into(),
        _ => None,
    }
}

#[test]
fn strategy_rows_all_resolve() {
    let table = coverage_table();
    let rows = table.as_array().expect("table is an array");
    let config = IntegrationLiteStrategyConfig::default();
    assert!(config.validate().is_ok(), "defaults validate");
    let mut mapped = 0;
    for row in rows {
        if row["status"] != "mapped" {
            continue;
        }
        if !row["group"].as_str().unwrap_or("").starts_with("Strategy:") {
            continue;
        }
        // The oscillator source switch textually follows the strategy
        // sections in the script but lives on the indicator config; the
        // indicator-crate suite resolves it.
        if row["pine"] == "_src" {
            continue;
        }
        for native in row["native"]
            .as_str()
            .expect("native")
            .split(',')
            .map(str::trim)
        {
            assert!(
                config.get_field(native).is_ok(),
                "mapped field {native:?} resolves on the strategy config"
            );
        }
        mapped += 1;
    }
    assert_eq!(mapped, 87, "strategy mapped count is deliberate, not drift");
}

#[test]
fn strategy_defaults_match_the_script() {
    let table = coverage_table();
    let rows = table.as_array().expect("table is an array");
    let parsed = pine_inputs();
    let script: BTreeMap<&str, (&str, &str)> = parsed
        .iter()
        .map(|(name, kind, default)| (name.as_str(), (kind.as_str(), default.as_str())))
        .collect();
    let config = IntegrationLiteStrategyConfig::default();
    let mut compared = 0;
    for row in rows {
        if row["status"] != "mapped" {
            continue;
        }
        if !row["group"].as_str().unwrap_or("").starts_with("Strategy:") {
            continue;
        }
        let pine = row["pine"].as_str().expect("pine name");
        let (kind, default) = script.get(pine).unwrap_or_else(|| panic!("{pine} parses"));
        let Some(expected) = pine_default_value(kind, default) else {
            continue;
        };
        for field in row["native"]
            .as_str()
            .expect("native")
            .split(',')
            .map(str::trim)
        {
            let actual = config
                .get_field(field)
                .unwrap_or_else(|_| panic!("{field} resolves"));
            if *kind == "int" || *kind == "float" {
                let (actual_num, expected_num): (f64, f64) = (
                    actual.parse().unwrap_or_else(|_| panic!("{field} numeric")),
                    expected
                        .parse()
                        .unwrap_or_else(|_| panic!("{pine} numeric")),
                );
                assert!(
                    (actual_num - expected_num).abs() < 1e-12,
                    "{pine} -> {field}: {actual} reproduces {expected}"
                );
            } else {
                assert_eq!(
                    actual, expected,
                    "{pine} -> {field}: native default reproduces the script default"
                );
            }
            compared += 1;
        }
    }
    assert!(
        compared > 70,
        "the comparison covers the strategy dials, not an empty set"
    );
}

#[test]
fn strategy_dials_validate_fail_closed() {
    let mut config = IntegrationLiteStrategyConfig::default();
    assert!(config.validate().is_ok());
    config.risk_pct = 0.0;
    assert!(config.validate().is_err(), "risk below its floor refuses");
    let config = IntegrationLiteStrategyConfig {
        direction: "Sideways".to_owned(),
        ..IntegrationLiteStrategyConfig::default()
    };
    assert!(config.validate().is_err(), "unlisted direction refuses");
    let strategy = IntegrationLiteStrategyConfig {
        marker_long_color: "red".to_owned(),
        ..IntegrationLiteStrategyConfig::default()
    };
    assert!(strategy.validate().is_err(), "unhashed color refuses");
    let strategy = IntegrationLiteStrategyConfig {
        min_stop_atr: 5.0,
        max_stop_atr: 4.0,
        ..IntegrationLiteStrategyConfig::default()
    };
    assert!(
        strategy.validate().is_err(),
        "an inverted stop clamp refuses"
    );
    // TP-zone weights above 100% are the script's normalization case, not a
    // refusal: the backtester scales them, the schema stores them.
    let strategy = IntegrationLiteStrategyConfig {
        tp1_pct: 60.0,
        tp2_pct: 60.0,
        ..IntegrationLiteStrategyConfig::default()
    };
    assert!(
        strategy.validate().is_ok(),
        "over-100 TP weights store; the runner normalizes"
    );
}

#[test]
fn strategy_schema_groups_match_the_script_sections() {
    let schema = IntegrationLiteStrategyConfig::default().describe();
    let properties = schema["properties"].as_object().expect("object");
    assert!(properties.len() >= 80, "the full dial surface describes");
    let mut groups = BTreeSet::new();
    for (name, hint) in properties {
        assert!(
            hint.get("description")
                .and_then(|v| v.as_str())
                .is_some_and(|d| !d.is_empty()),
            "{name} carries a description"
        );
        let group = hint
            .get("x-group")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("{name} renders in its Pine group"));
        groups.insert(group.to_owned());
        // Every selector names its live catalog.
        if hint.get("enum").is_some() {
            assert!(
                !hint["enum"].as_array().expect("options").is_empty(),
                "{name} selects from nothing"
            );
        }
    }
    for group in [
        "Strategy: General",
        "Strategy: Entry Signals",
        "Strategy: Entry Filters",
        "Strategy: Position Size & Initial Stop",
        "Strategy: Take Profit Zones",
        "Strategy: Breakeven",
        "Strategy: Trailing Stop",
        "Strategy: Exhaustion & Exit Signals",
        "Strategy: Cooldowns & Daily Limits",
        "Strategy: Display & Alerts",
    ] {
        assert!(groups.contains(group), "{group} renders");
    }
    assert_eq!(groups.len(), 10, "ten strategy sections, no more");
}

#[test]
fn strategy_config_round_trips() {
    let config = IntegrationLiteStrategyConfig::default();
    let json = serde_json::to_value(&config).expect("serializes");
    let rerun: IntegrationLiteStrategyConfig =
        serde_json::from_value(json).expect("legacy-free documents decode");
    assert_eq!(
        serde_json::to_value(&rerun).expect("re-serializes"),
        serde_json::to_value(&config).expect("serializes"),
        "round-trip is byte-stable"
    );
    assert!(rerun.validate().is_ok());
}
