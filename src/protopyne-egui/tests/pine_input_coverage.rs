//! Pine-input coverage table: every live `input.*` of the shipped
//! `_IntegrationLiteStrategy.pine` is either `mapped` to a real native
//! settings field or `tracked` with the reason it has no dial yet.
//!
//! The table is the machine-checked gap list behind full-config parity: the
//! Pine side is re-parsed from the shipped script on every run, so a script
//! edit that adds, renames, or removes an input fails loudly here instead
//! of drifting silently. `mapped` rows name `IntegrationLiteConfig`
//! `FieldAccess` fields and are resolved against the live config, so a
//! renamed native field fails here too.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use fluent_wvr::FieldAccess as _;

use crate::integration_lite::IntegrationLiteConfig;

fn data_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs")
}

fn coverage_table() -> serde_json::Value {
    let text = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/pine_input_coverage.json"),
    )
    .expect("coverage table loads");
    serde_json::from_str(&text).expect("coverage table parses")
}

/// Live input names in the shipped script: non-comment lines shaped
/// `name = input.<kind>(...)` plus the generic `name = input(value, ...)`
/// form (kind inferred from the value head, exactly the two color calls
/// here), in file order.
fn pine_inputs() -> Vec<(String, String)> {
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
        let rest = rest["= input".len()..].trim_start();
        let kind = if let Some(dotted) = rest.strip_prefix('.') {
            dotted.split(['(', '.']).next().unwrap_or("").to_owned()
        } else if rest.starts_with('(') {
            // Generic form: infer from the value head like the engine does.
            let head = rest
                .strip_prefix('(')
                .expect("open paren tested")
                .split(',')
                .next()
                .unwrap_or("")
                .trim();
            if head.starts_with("color.") {
                "color".to_owned()
            } else {
                continue;
            }
        } else {
            continue;
        };
        inputs.push((name.to_owned(), kind));
    }
    inputs
}

#[test]
fn table_shape_is_closed_and_complete() {
    let table = coverage_table();
    let rows = table.as_array().expect("table is an array");
    assert_eq!(rows.len(), 141, "row count is deliberate, not drift");
    for row in rows {
        for key in ["pine", "kind", "group", "status", "native", "note"] {
            assert!(row.get(key).is_some(), "every row carries {key}: {row:?}");
        }
        assert!(
            ["mapped", "tracked"].contains(&row["status"].as_str().unwrap_or("")),
            "status is a closed vocabulary: {row:?}"
        );
        assert!(
            ["bool", "int", "float", "string", "enum", "color", "session", "source"]
                .contains(&row["kind"].as_str().unwrap_or("")),
            "kind is a closed vocabulary: {row:?}"
        );
        assert!(
            !row["pine"].as_str().unwrap_or("").is_empty(),
            "pine name is never empty: {row:?}"
        );
        assert!(
            !row["group"].as_str().unwrap_or("").is_empty(),
            "group is never empty: {row:?}"
        );
        assert!(
            !row["note"].as_str().unwrap_or("").is_empty(),
            "every row states its standing: {row:?}"
        );
        if row["status"] == "mapped" {
            assert!(
                row["native"].as_str().is_some_and(|n| !n.is_empty()),
                "mapped rows name the native field: {row:?}"
            );
        }
    }
}

#[test]
fn table_covers_the_script_exactly_no_silent_gaps() {
    let table = coverage_table();
    let rows = table.as_array().expect("table is an array");
    let table_names: BTreeSet<&str> = rows
        .iter()
        .map(|row| row["pine"].as_str().expect("pine name"))
        .collect();
    let script = pine_inputs();
    let script_names: BTreeSet<&str> = script.iter().map(|(name, _)| name.as_str()).collect();
    let missing: Vec<&&str> = script_names.difference(&table_names).collect();
    let stale: Vec<&&str> = table_names.difference(&script_names).collect();
    assert!(
        missing.is_empty(),
        "script inputs with no table row (silent gap): {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "table rows with no script input (stale row): {stale:?}"
    );
    // The script side of each row still carries the table's kind.
    let table_kinds: BTreeMap<&str, &str> = rows
        .iter()
        .map(|row| {
            (
                row["pine"].as_str().expect("pine"),
                row["kind"].as_str().expect("kind"),
            )
        })
        .collect();
    for (name, kind) in &script {
        assert_eq!(
            table_kinds.get(name.as_str()),
            Some(&kind.as_str()),
            "kind drift for {name}"
        );
    }
}

#[test]
fn mapped_rows_resolve_to_real_config_fields() {
    let table = coverage_table();
    let config = IntegrationLiteConfig::default();
    let mut mapped = 0;
    for row in table.as_array().expect("table is an array") {
        if row["status"] != "mapped" {
            continue;
        }
        // Strategy-group dials live on the strategy-crate config, resolved
        // by its own suite; this suite pins the indicator-crate dials.
        let group = row["group"].as_str().unwrap_or("");
        if group.starts_with("Strategy:") {
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
                "mapped field {native:?} resolves on the live config"
            );
        }
        mapped += 1;
    }
    assert_eq!(mapped, 49, "mapped count is deliberate, not drift");
}

/// Parse one Pine `input.*` default into its native spelling: bools and
/// numbers verbatim, strings unquoted, enum members lowered past the last
/// dot (`UTIL.Smoothings.NA` -> `na`), `input.source(close)` -> `close`.
/// Colors, sessions, and timeframes have no native dial and return `None`.
fn pine_default(kind: &str, args: &str) -> Option<String> {
    let default = args.split(',').next()?.trim();
    match kind {
        "bool" | "int" | "float" | "source" => Some(default.to_owned()),
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
fn native_defaults_match_pine_or_document_the_fork() {
    let table = coverage_table();
    let rows = table.as_array().expect("table is an array");
    let script =
        std::fs::read_to_string(data_root().join("strategies/_IntegrationLiteStrategy.pine"))
            .expect("shipped strategy loads");
    let mut pine_defaults = BTreeMap::new();
    for line in script.lines() {
        let line = line.trim();
        if line.starts_with("//") {
            continue;
        }
        let Some(eq) = line.find("= input.") else {
            continue;
        };
        let (name, rest) = line.split_at(eq);
        let name = name.trim();
        let rest = &rest["= input.".len()..];
        let Some(dot) = rest.find('(') else {
            continue;
        };
        let (mut kind, args) = rest.split_at(dot);
        kind = kind.trim();
        // Generic `input(value, ...)` infers its kind from the value, like
        // the script engine does: only the color form occurs untyped here.
        let kind = if kind.is_empty() && args.trim_start_matches('(').starts_with("color.new") {
            "color"
        } else {
            kind
        };
        let args = args.trim_start_matches('(');
        // The closing paren of the outermost call: the args may hold
        // nested calls, so the default is the head up to the first comma
        // at depth zero — `pine_default` already reads only that head.
        if let Some(default) = pine_default(kind, args) {
            pine_defaults.insert(name.to_owned(), default);
        }
    }
    // Deliberate forks: native defaults that differ from the script on
    // purpose, each with its reason in the row note.
    let forks = [
        "i_delta_vol_unit",
        "i_pivots_threshold",
        "i_oscillator1_adaptive",
    ];
    // Blank-by-design: the dial follows the panel kind row, so the stored
    // default is empty while the script names its own number.
    let blanks = [
        "fast_period",
        "slow_period",
        "fast_smooth_period",
        "slow_smooth_period",
        "fast_k",
        "slow_k",
    ];
    let config = IntegrationLiteConfig::default();
    let mut compared = 0;
    for row in rows {
        if row["status"] != "mapped" {
            continue;
        }
        // Strategy-group dials are compared by the strategy-crate suite
        // against its own config; this suite pins the indicator dials.
        if row["group"].as_str().unwrap_or("").starts_with("Strategy:") {
            continue;
        }
        let pine = row["pine"].as_str().expect("pine name");
        let native = row["native"].as_str().expect("native field");
        let Some(expected) = pine_defaults.get(pine) else {
            continue;
        };
        // Multi-name natives (`fast_k, slow_k`) resolve field by field.
        for field in native.split(',').map(str::trim) {
            let actual = config
                .get_field(field)
                .unwrap_or_else(|_| panic!("{field} resolves"));
            if forks.contains(&pine) || blanks.contains(&field) {
                continue;
            }
            assert_eq!(
                actual, *expected,
                "{pine} -> {field}: native default reproduces the script default"
            );
            compared += 1;
        }
    }
    assert!(
        compared > 10,
        "the comparison covers the comparable dials, not an empty set"
    );
    // Every fork and blank names its standing in the row note.
    for row in rows {
        if row["status"] != "mapped" {
            continue;
        }
        let pine = row["pine"].as_str().expect("pine name");
        let note = row["note"].as_str().unwrap_or("");
        if forks.contains(&pine) {
            assert!(!note.is_empty(), "{pine} documents its fork: {row:?}");
        }
    }
}
