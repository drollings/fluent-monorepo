//! Crate-graph guards for the decision layer.
//!
//! A decision crate must not reach the venue, the paint edge, or a dataframe
//! closure: the venue depends on strategy (not the reverse), the terminal is
//! a paint edge, and a scorer is a slice function, not a frame builder. The
//! guards read `cargo metadata` (workspace dependency declarations), so they
//! see the graph as Cargo resolves it rather than as a source scan guesses.
//! The negative controls below prove each guard fires on a synthetic
//! violation, so a guard that silently stopped matching would fail here.

use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

/// Names the decision crates may never depend on.
const FORBIDDEN_DECISION_DEPS: &[&str] = &[
    "protopyne-venue",
    "protopyne-server",
    "protopyne-egui",
    "egui",
    "eframe",
    "polars",
];

/// Adapter features that must stay off unless a caller opts in by name.
const OPT_IN_FEATURES: &[&str] = &["pine", "signal-adapters", "research"];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn workspace_metadata() -> Value {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(workspace_root().join("Cargo.toml"))
        .output()
        .expect("cargo metadata runs");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("metadata is JSON")
}

fn package<'a>(metadata: &'a Value, name: &str) -> &'a Value {
    metadata["packages"]
        .as_array()
        .expect("metadata has packages")
        .iter()
        .find(|package| package["name"] == name)
        .unwrap_or_else(|| panic!("workspace member {name} is present"))
}

fn dependency<'a>(package: &'a Value, name: &str) -> Option<&'a Value> {
    package["dependencies"]
        .as_array()
        .expect("package has dependencies")
        .iter()
        .find(|dependency| dependency["name"] == name)
}

fn requested_features(package: &Value, name: &str) -> Vec<String> {
    dependency(package, name)
        .and_then(|dependency| dependency["features"].as_array())
        .map(|features| {
            features
                .iter()
                .map(|feature| feature.as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn default_features(package: &Value) -> Vec<String> {
    package["features"]["default"]
        .as_array()
        .map(|features| {
            features
                .iter()
                .map(|feature| feature.as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// A decision crate reaches only the types vocabulary and a lean kernels
/// closure. Returns the reason on the first violation.
fn check_decision_layering(package: &Value) -> Result<(), String> {
    for name in FORBIDDEN_DECISION_DEPS {
        if dependency(package, name).is_some() {
            return Err(format!("decision crate must not depend on {name}"));
        }
    }
    for feature in requested_features(package, "protopyne-kernels") {
        if OPT_IN_FEATURES.contains(&feature.as_str()) {
            return Err(format!(
                "decision crate must not enable the {feature} kernels feature"
            ));
        }
    }
    for feature in default_features(package) {
        if OPT_IN_FEATURES.contains(&feature.as_str()) {
            return Err(format!("default features must not include {feature}"));
        }
    }
    Ok(())
}

/// Lean kernels keep the dataframe behind an opt-in feature.
fn check_lean_kernels(package: &Value) -> Result<(), String> {
    let polars = dependency(package, "polars")
        .ok_or_else(|| "kernels declares no polars dependency".to_owned())?;
    if polars["optional"] != Value::Bool(true) {
        return Err("polars must be an optional kernels dependency".to_owned());
    }
    for feature in default_features(package) {
        if OPT_IN_FEATURES.contains(&feature.as_str()) {
            return Err(format!(
                "kernels default features must not include {feature}"
            ));
        }
    }
    let research = package["features"]["research"]
        .as_array()
        .is_some_and(|features| features.iter().any(|f| f == "dep:polars"));
    if !research {
        return Err("the research feature must be the only polars gate".to_owned());
    }
    Ok(())
}

#[test]
fn strategy_reaches_neither_the_venue_nor_the_paint_edge() {
    let metadata = workspace_metadata();
    let strategy = package(&metadata, "protopyne-strategy");
    check_decision_layering(strategy).unwrap_or_else(|reason| panic!("{reason}"));
}

#[test]
fn lean_kernels_keep_polars_behind_the_research_feature() {
    let metadata = workspace_metadata();
    let kernels = package(&metadata, "protopyne-kernels");
    check_lean_kernels(kernels).unwrap_or_else(|reason| panic!("{reason}"));
}

#[test]
fn the_layering_guards_fire_on_a_synthetic_violation() {
    let metadata = workspace_metadata();
    let mut strategy = package(&metadata, "protopyne-strategy").clone();
    strategy["dependencies"]
        .as_array_mut()
        .expect("dependencies list")
        .push(serde_json::json!({
            "name": "protopyne-venue",
            "features": [],
            "optional": false,
            "uses_default_features": true,
        }));
    assert!(
        check_decision_layering(&strategy).is_err(),
        "a forged venue edge must be refused"
    );

    let mut kernels = package(&metadata, "protopyne-kernels").clone();
    for dependency in kernels["dependencies"]
        .as_array_mut()
        .expect("dependencies list")
    {
        if dependency["name"] == "polars" {
            dependency["optional"] = Value::Bool(false);
        }
    }
    assert!(
        check_lean_kernels(&kernels).is_err(),
        "a mandatory polars edge must be refused"
    );
}
