//! Manifest-driven indicator catalog: manifest rows render under My
//! Scripts with their own titles, red entries refuse on instantiate, and the
//! widget never reads the directory (no manifest, no `pine:` rows).

use super::*;
use protopyne_kernels::pine::manifest::{IndicatorManifestEntry, ManifestParity};

fn green_backed() -> IndicatorManifestEntry {
    IndicatorManifestEntry {
        id: "pine:_IntegrationLite".to_owned(),
        title: "Integration Lite (manifest)".to_owned(),
        source_path: "indicators/_IntegrationLite.pine".to_owned(),
        version: Some(6),
        closure_hash: 1,
        parity: ManifestParity::Verified,
    }
}

fn red_entry() -> IndicatorManifestEntry {
    IndicatorManifestEntry {
        id: "pine:_Broken".to_owned(),
        title: "Broken Script".to_owned(),
        source_path: "indicators/_Broken.pine".to_owned(),
        version: Some(6),
        closure_hash: 2,
        parity: ManifestParity::Unverified {
            reason: "unknown import \"libNope\"".to_owned(),
        },
    }
}

fn green_unbacked() -> IndicatorManifestEntry {
    IndicatorManifestEntry {
        id: "pine:_ApexRSIMD".to_owned(),
        title: "Apex RSIMD".to_owned(),
        source_path: "indicators/_ApexRSIMD.pine".to_owned(),
        version: Some(6),
        closure_hash: 3,
        parity: ManifestParity::Verified,
    }
}

#[test]
fn catalog_from_manifest_renders_titles_under_my_scripts() {
    let rows = vec![green_backed(), red_entry()];
    let mut catalog = build_indicator_catalog_with_manifest(&rows).expect("the catalog builds");
    catalog.set_category(Some(IndicatorCategory::MyScripts));
    let titles: Vec<String> = catalog
        .filtered()
        .iter()
        .map(|entry| entry.title().to_owned())
        .collect();
    assert!(
        titles.contains(&"🌲 Integration Lite (manifest)".to_owned()),
        "the manifest title renders with its pine marker, not the id: {titles:?}"
    );
    assert!(
        titles.contains(&"🌲 Broken Script".to_owned()),
        "a red entry is listed, never hidden: {titles:?}"
    );
    assert!(
        !titles.iter().any(|title| title.starts_with("pine:")),
        "no id-as-title leaks: {titles:?}"
    );
}

#[test]
fn instantiate_with_manifest_refuses_red_and_unbacked() {
    let rows = vec![green_backed(), red_entry(), green_unbacked()];
    let plugin = instantiate_indicator_with_manifest("pine:_IntegrationLite", &rows)
        .expect("the backed script resolves");
    assert_eq!(
        plugin.id(),
        "integration_lite",
        "backed scripts ride the shipped plugin"
    );
    let Err(err) = instantiate_indicator_with_manifest("pine:_Broken", &rows) else {
        panic!("red parity refuses")
    };
    assert!(
        err.to_string().contains("libNope"),
        "the refusal carries the reason: {err}"
    );
    let Err(err) = instantiate_indicator_with_manifest("pine:_ApexRSIMD", &rows) else {
        panic!("an unbacked script refuses")
    };
    assert!(
        err.to_string().contains("no native plugin"),
        "the refusal names the gap: {err}"
    );
    assert!(
        instantiate_indicator_with_manifest("pine:_Absent", &rows).is_err(),
        "an unlisted manifest id refuses"
    );
}

#[test]
fn manifest_free_catalog_has_no_ambient_rows() {
    let mut catalog = build_indicator_catalog().expect("the catalog builds");
    catalog.set_category(Some(IndicatorCategory::MyScripts));
    let titles: Vec<String> = catalog
        .filtered()
        .iter()
        .map(|entry| entry.title().to_owned())
        .collect();
    assert!(
        !titles.iter().any(|title| title.starts_with("pine:")),
        "no directory read feeds the widget: {titles:?}"
    );
    assert!(
        instantiate_indicator("pine:_IntegrationLite").is_err(),
        "manifest ids refuse without their manifest"
    );
}

#[test]
fn pine_rows_render_with_a_pine_marker_and_checkbox() {
    let rows = vec![green_backed(), red_entry()];
    let catalog = build_indicator_catalog_with_manifest(&rows).expect("the catalog builds");
    assert!(!catalog.tag_only(), "the Pine checkbox installs unchecked");
    let titles: Vec<String> = catalog
        .filtered()
        .iter()
        .map(|entry| entry.title().to_owned())
        .collect();
    assert!(
        titles.iter().any(|title| title.starts_with("🌲 ")),
        "Pine rows carry the pine marker: {titles:?}"
    );
}
