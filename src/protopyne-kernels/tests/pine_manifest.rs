//! Indicator manifest: boot-time discovery over a Pine root, gated on
//! resolution.
//!
//! The builder walks `<root>/indicators/*.pine` (never the widget, never
//! `execute`): a script is `Verified` when it loads, declares the
//! supported version, and resolves every import against `<root>/libraries`.
//! Anything else is listed with its refusal reason — advertised, never
//! runnable. `Verified` is structural readiness, not harness proof: the
//! hermetic engine suites prove numerics over checked-in fixtures.

use super::*;
use crate::pine::manifest::ManifestParity;
use std::path::PathBuf;

fn data_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs")
}

/// A temp Pine root with the `indicators/` + `libraries/` layout the
/// builder reads.
fn temp_root(files: &[(&str, &str)]) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("temp root builds");
    for (relpath, text) in files {
        let path = root.path().join(relpath);
        std::fs::create_dir_all(path.parent().expect("file has a parent"))
            .expect("parent dirs build");
        std::fs::write(&path, text).expect("fixture writes");
    }
    root
}

const LIB_UTIL: &str = "//@version=6\nlibrary('libUtil')\nexport foo() => 1\n";

#[test]
fn manifest_marks_good_and_missing_imports() {
    let root = temp_root(&[
        (
            "indicators/_IntegrationLite.pine",
            "//@version=6\nindicator(\"Integration Lite\")\nimport showmethegrail/libUtil/12 as UTIL\n",
        ),
        (
            "indicators/_Broken.pine",
            "//@version=6\nindicator(\"Broken\")\nimport showmethegrail/libNope/1 as NOPE\n",
        ),
        ("libraries/_libUtil.pine", LIB_UTIL),
    ]);
    let manifest = build_indicator_manifest(root.path()).expect("the manifest builds");
    assert_eq!(manifest.len(), 2, "both scripts are listed: {manifest:?}");
    let good = manifest
        .iter()
        .find(|entry| entry.id == "pine:_IntegrationLite")
        .expect("good listed");
    assert_eq!(
        good.parity,
        ManifestParity::Verified,
        "good import verifies"
    );
    assert_eq!(
        good.title, "Integration Lite",
        "title comes from the header"
    );
    let broken = manifest
        .iter()
        .find(|entry| entry.id == "pine:_Broken")
        .expect("broken listed");
    match &broken.parity {
        ManifestParity::Unverified { reason } => {
            assert!(
                reason.contains("libNope"),
                "the refusal names the import: {reason}"
            );
        }
        ManifestParity::Verified => panic!("a missing import never verifies"),
    }
}

#[test]
fn manifest_refuses_version_skew_and_verifies_resolvable_stems() {
    let root = temp_root(&[
        (
            "indicators/_IntegrationLite.pine",
            "//@version=5\nindicator(\"Old Lite\")\nimport showmethegrail/libUtil/12 as UTIL\n",
        ),
        (
            "indicators/_Whatever.pine",
            "//@version=6\nindicator(\"Whatever\")\nimport showmethegrail/libUtil/12 as UTIL\n",
        ),
        ("libraries/_libUtil.pine", LIB_UTIL),
    ]);
    let manifest = build_indicator_manifest(root.path()).expect("the manifest builds");
    let skewed = manifest
        .iter()
        .find(|entry| entry.id == "pine:_IntegrationLite")
        .expect("listed");
    match &skewed.parity {
        ManifestParity::Unverified { reason } => {
            assert!(
                reason.contains('5'),
                "the refusal names the version: {reason}"
            );
        }
        ManifestParity::Verified => panic!("version skew never verifies"),
    }
    let uncovered = manifest
        .iter()
        .find(|entry| entry.id == "pine:_Whatever")
        .expect("listed");
    // No stem list gates verification: a resolvable script verifies
    // whatever it is named. Harness proof lives in the hermetic engine
    // suites over checked-in fixtures, never in this builder.
    assert_eq!(
        uncovered.parity,
        ManifestParity::Verified,
        "a resolvable script verifies: {uncovered:?}"
    );
}

#[test]
fn manifest_title_falls_back_to_stem() {
    let root = temp_root(&[(
        "indicators/_Mystery.pine",
        "//@version=6\n// no header here\n",
    )]);
    let manifest = build_indicator_manifest(root.path()).expect("the manifest builds");
    assert_eq!(manifest.len(), 1);
    assert_eq!(
        manifest[0].title, "_Mystery",
        "the stem stands in for a title"
    );
}

#[test]
fn manifest_is_sorted_and_deterministic() {
    let root = temp_root(&[
        (
            "indicators/_IntegrationLite.pine",
            "//@version=6\nindicator(\"Integration Lite\")\nimport showmethegrail/libUtil/12 as UTIL\n",
        ),
        ("libraries/_libUtil.pine", LIB_UTIL),
    ]);
    let first = build_indicator_manifest(root.path()).expect("the manifest builds");
    let second = build_indicator_manifest(root.path()).expect("the manifest rebuilds");
    assert_eq!(first, second, "a rerun is byte-identical");
    let ids: Vec<&str> = first.iter().map(|entry| entry.id.as_str()).collect();
    assert!(
        ids.windows(2).all(|pair| pair[0] <= pair[1]),
        "ids sort: {ids:?}"
    );
}

#[test]
fn manifest_missing_root_refuses() {
    let root = temp_root(&[]);
    assert!(
        build_indicator_manifest(&root.path().join("absent")).is_err(),
        "a missing root refuses instead of yielding an empty catalog"
    );
}

#[test]
fn manifest_real_root_lists_every_file_with_a_verdict() {
    // Directory-driven over the volatile live root: one row per file on
    // disk, rows sorted by id, every row carrying a verdict. A `Verified`
    // row resolves (its discovery closure links); an `Unverified` row
    // names its reason. Adding, removing, or renaming a script changes
    // only which per-file rows appear here, never the verdict shape.
    use crate::{closure_inputs, discover_closures, resolve_imports, ClosureOutcome};
    let (indicators, libraries) = load_pine_sources(&data_root()).expect("the real root builds");
    let manifest = build_indicator_manifest(&data_root()).expect("the manifest builds");
    assert_eq!(
        manifest.len(),
        indicators.len(),
        "one row per indicator file on disk"
    );
    let ids: Vec<&str> = manifest.iter().map(|entry| entry.id.as_str()).collect();
    assert!(
        ids.windows(2).all(|pair| pair[0] <= pair[1]),
        "rows sort: {ids:?}"
    );
    for outcome in discover_closures(&indicators, &libraries) {
        let stem = outcome.stem().to_owned();
        let entry = manifest
            .iter()
            .find(|entry| entry.id == format!("pine:{stem}"))
            .unwrap_or_else(|| panic!("{stem} is listed"));
        match (&entry.parity, &outcome) {
            (ManifestParity::Verified, ClosureOutcome::Linked(closure)) => {
                let inputs = closure_inputs(closure).expect("verified closure parses");
                resolve_imports(inputs).expect("verified closure links");
                assert!(!entry.title.is_empty(), "{stem} carries a title");
            }
            (ManifestParity::Verified, ClosureOutcome::Refused { reason, .. }) => {
                panic!("{stem} verifies but discovery refuses: {reason}")
            }
            (ManifestParity::Unverified { reason }, _) => {
                assert!(!reason.is_empty(), "{stem} refuses with a reason");
            }
        }
    }
}

#[test]
fn load_pine_sources_returns_both_dirs_byte_identical() {
    let root = temp_root(&[
        (
            "indicators/_Foo.pine",
            "//@version=6\nindicator(\"Foo\")\nplot(close)\n",
        ),
        ("libraries/_libUtil.pine", LIB_UTIL),
    ]);
    let (indicators, libraries) = load_pine_sources(root.path()).expect("the sources load");
    assert_eq!(indicators.len(), 1);
    assert_eq!(indicators[0].relpath, "indicators/_Foo.pine");
    assert!(indicators[0].text.contains("plot(close)"));
    assert_eq!(libraries.len(), 1);
    assert_eq!(libraries[0].relpath, "libraries/_libUtil.pine");
}

#[test]
fn load_pine_sources_tolerates_a_missing_libraries_dir() {
    let root = temp_root(&[(
        "indicators/_Solo.pine",
        "//@version=6\nindicator(\"Solo\")\nplot(close)\n",
    )]);
    let (indicators, libraries) = load_pine_sources(root.path()).expect("loads");
    assert_eq!(indicators.len(), 1);
    assert!(libraries.is_empty(), "no libraries dir reads empty");
}

#[test]
fn load_pine_sources_refuses_a_missing_root() {
    assert!(
        load_pine_sources(&temp_root(&[]).path().join("absent")).is_err(),
        "a missing root refuses"
    );
}
