//! Indicator manifest: boot-time discovery over a Pine root, gated on
//! resolution and harness coverage.
//!
//! The builder walks `<root>/indicators/*.pine` (never the widget, never
//! `execute`): a script is `Verified` only when it loads, declares the
//! supported version, resolves every import against `<root>/libraries`, and
//! names a stem the parity suites cover. Anything else is listed with its
//! refusal reason — advertised, never runnable.

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
fn manifest_refuses_version_skew_and_uncovered_stems() {
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
    match &uncovered.parity {
        ManifestParity::Unverified { reason } => {
            assert!(
                reason.contains("harness"),
                "the refusal names coverage: {reason}"
            );
        }
        ManifestParity::Verified => panic!("an uncovered script never verifies"),
    }
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
fn manifest_real_root_covers_the_proven_set() {
    let manifest = build_indicator_manifest(&data_root()).expect("the real root builds");
    assert!(
        manifest.len() >= 5,
        "every checked-in script is listed: {}",
        manifest.len()
    );
    for stem in [
        "_IntegrationLite",
        "_FastMAs",
        "_DeltaVolumeProfileProtopyne",
        "_ApexPPAS",
        "_ApexRSIMD",
    ] {
        let entry = manifest
            .iter()
            .find(|entry| entry.id == format!("pine:{stem}"))
            .unwrap_or_else(|| panic!("{stem} is listed"));
        assert_eq!(
            entry.parity,
            ManifestParity::Verified,
            "{stem} is harness-proven: {entry:?}"
        );
        assert!(!entry.title.is_empty(), "{stem} carries a title");
        assert_ne!(entry.title, stem, "{stem} titles never echo the id");
    }
}
