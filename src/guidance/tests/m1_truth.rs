//! Truth-model pins: sidecar header round-trip, component isolation, sweep verdicts.

use std::collections::BTreeMap;

use fluent_types::GuidanceDoc;
use fluent_wvr_testutil::tempdir;

use crate::components::{ComponentVector, SidecarReceipt, SweepDecision};
use crate::policy::MaintenancePolicy;
use crate::sync::json_store;

fn test_doc() -> GuidanceDoc {
    GuidanceDoc {
        meta: fluent_types::Meta {
            module: "m1".into(),
            source: "src/m1.zig".into(),
            language: "zig".into(),
        },
        members: vec![crate::tests::common::make_test_member(
            "m1_fn",
            "fn m1_fn() void",
            "m1hash",
            Some("M1 fixture"),
        )],
        ..GuidanceDoc::default()
    }
}

fn test_receipt(pairs: &[(&str, &str)]) -> SidecarReceipt {
    SidecarReceipt::new(
        ComponentVector {
            components: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        },
        None,
    )
}

#[test]
fn old_sidecar_without_header_loads() {
    let dir = tempdir();
    let path = dir.path().join("old.json");
    let doc = test_doc();
    let body = crate::sync::json_writer::doc_to_json_string(&doc);
    assert!(!body.lines().next().unwrap_or("").starts_with("GHDR1 "));
    std::fs::write(&path, &body).expect("write");
    let loaded = json_store::load_guidance(&path).expect("load").expect("doc");
    assert_eq!(loaded.meta.module.as_str(), "m1");
}

#[test]
fn header_round_trip_preserves_doc() {
    let dir = tempdir();
    let path = dir.path().join("new.json");
    let vector = test_receipt(&[("parser", "p1"), ("schema", "s1")]);
    json_store::save_guidance(&path, &test_doc(), &vector).expect("save");
    let raw = std::fs::read_to_string(&path).expect("read");
    assert!(raw.lines().next().unwrap_or("").starts_with("GHDR1 "));
    let loaded = json_store::load_guidance(&path).expect("load").expect("doc");
    assert_eq!(loaded.meta.module.as_str(), "m1");
}

#[test]
fn identical_write_with_header_skips_rewrite() {
    let dir = tempdir();
    let path = dir.path().join("idem.json");
    let vector = test_receipt(&[("parser", "p1")]);
    json_store::save_guidance(&path, &test_doc(), &vector).expect("first");
    let before = std::fs::metadata(&path).expect("stat").modified().expect("mtime");
    std::thread::sleep(std::time::Duration::from_millis(5));
    json_store::save_guidance(&path, &test_doc(), &vector).expect("second");
    let after = std::fs::metadata(&path).expect("stat").modified().expect("mtime");
    assert_eq!(before, after);
}

#[test]
fn policy_section_fingerprints_isolate() {
    let base = MaintenancePolicy::default();
    let mut mech = base.clone();
    mech.mechanical_classes.push("clippy-fix".to_string());
    let mut scope = base.clone();
    scope.scope.push("crates/foo".to_string());
    assert_ne!(
        crate::policy::section_fingerprint(&base, "mechanical"),
        crate::policy::section_fingerprint(&mech, "mechanical")
    );
    assert_eq!(
        crate::policy::section_fingerprint(&base, "scope"),
        crate::policy::section_fingerprint(&mech, "scope")
    );
    assert_ne!(
        crate::policy::section_fingerprint(&base, "scope"),
        crate::policy::section_fingerprint(&scope, "scope")
    );
    assert_eq!(
        crate::policy::section_fingerprint(&base, "mechanical"),
        crate::policy::section_fingerprint(&scope, "mechanical")
    );
}

#[test]
fn sweep_reports_current_when_recorded_match() {
    let dir = tempdir();
    let sidecars = dir.path().join("src");
    std::fs::create_dir_all(&sidecars).expect("mkdir");
    let source = dir.path().join("a.zig");
    std::fs::write(&source, "pub fn a() void {}\n").expect("write");
    let vector = test_receipt(&[("parser", "p1"), ("schema", "s1")]);
    json_store::save_guidance(&sidecars.join("a.zig.json"), &test_doc(), &vector)
        .expect("save");
    let outcomes = crate::components::sweep_sidecars(&sidecars, dir.path(), &vector.components);
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].decision, SweepDecision::Current);
}

#[test]
fn sweep_backdates_only_on_recorded_change() {
    let dir = tempdir();
    let sidecars = dir.path().join("src");
    std::fs::create_dir_all(&sidecars).expect("mkdir");
    let source = dir.path().join("a.zig");
    std::fs::write(&source, "pub fn a() void {}\n").expect("write");
    let saved = test_receipt(&[("parser", "p1"), ("schema", "s1")]);
    let sidecar = sidecars.join("a.zig.json");
    json_store::save_guidance(&sidecar, &test_doc(), &saved).expect("save");
    // Unrecorded component change never backdates the receipt.
    let mut changed: BTreeMap<String, String> = saved.components.components.clone();
    changed.insert("prompt".to_string(), "p9".to_string());
    changed.insert("model".to_string(), "m9".to_string());
    let outcomes = crate::components::sweep_sidecars(
        &sidecars,
        dir.path(),
        &ComponentVector { components: changed },
    );
    assert_eq!(outcomes[0].decision, SweepDecision::Current);
    // Recorded component change backdates exactly the affected receipt.
    let evolved = test_receipt(&[("parser", "p2"), ("schema", "s1")]);
    let outcomes =
        crate::components::sweep_sidecars(&sidecars, dir.path(), &evolved.components);
    assert_eq!(outcomes[0].decision, SweepDecision::Backdated);
    let sidecar_mtime = std::fs::metadata(&sidecar).expect("stat").modified().expect("mtime");
    let source_mtime = std::fs::metadata(&source).expect("stat").modified().expect("mtime");
    assert!(sidecar_mtime < source_mtime);
}

#[test]
fn sweep_marks_headerless_sidecars_unknown() {
    let dir = tempdir();
    let sidecars = dir.path().join("src");
    std::fs::create_dir_all(&sidecars).expect("mkdir");
    let body = crate::sync::json_writer::doc_to_json_string(&test_doc());
    let sidecar = sidecars.join("old.json");
    std::fs::write(&sidecar, &body).expect("write");
    let before = std::fs::metadata(&sidecar).expect("stat").modified().expect("mtime");
    let outcomes = crate::components::sweep_sidecars(
        &sidecars,
        dir.path(),
        &test_receipt(&[("parser", "p1")]).components,
    );
    assert_eq!(outcomes[0].decision, SweepDecision::Unknown);
    let after = std::fs::metadata(&sidecar).expect("stat").modified().expect("mtime");
    assert_eq!(before, after);
}

#[test]
fn node_index_rebuilds_from_sidecars() {
    let dir = tempdir();
    let sidecars = dir.path().join("src");
    std::fs::create_dir_all(&sidecars).expect("mkdir");
    let vector = test_receipt(&[("parser", "p1")]);
    json_store::save_guidance(&sidecars.join("a.json"), &test_doc(), &vector).expect("save");
    let db_a = dir.path().join("a.db");
    let first = search_vector::GuidanceDb::open(&db_a).expect("open");
    let synced = first.sync_from_dir(&sidecars).expect("sync");
    assert!(synced > 0);
    let count = first.get_node_count().expect("count");
    // Second sync converges to a no-op through the watermark gate.
    assert_eq!(first.sync_from_dir(&sidecars).expect("resync"), 0);
    drop(first);
    // A deleted index rebuilds the same node set from sidecars alone.
    std::fs::remove_file(&db_a).expect("delete index");
    let second = search_vector::GuidanceDb::open(&db_a).expect("reopen");
    assert!(second.sync_from_dir(&sidecars).expect("rebuild") > 0);
    assert_eq!(second.get_node_count().expect("count"), count);
}
