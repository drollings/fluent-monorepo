//! Calibration registry tests (tests-first).

use fluent_llm::decision::{authorize, CalibrationRegistry, CaseOrigin, GateParams, RegistryEntry};
use std::path::PathBuf;

fn seed_corpus_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("calibration/seed_intent.json")
}

#[test]
fn seed_corpus_loads_with_human_provenance_only() {
    let corpus = CalibrationRegistry::load_corpus(&seed_corpus_path()).expect("seed corpus");
    assert_eq!(corpus.point, "intent");
    assert!(!corpus.cases.is_empty());
    // Ground truth is never model output: every case carries a human-family
    // origin by type construction, pinned here against relabelling.
    for case in &corpus.cases {
        assert!(
            matches!(
                case.provenance.origin,
                CaseOrigin::Human | CaseOrigin::HumanReviewed | CaseOrigin::Synthetic
            ),
            "non-human origin: {:?}",
            case.provenance.origin
        );
    }
}

#[test]
fn empty_registry_fails_closed_to_shadow_only() {
    let registry = CalibrationRegistry::empty();
    let status = authorize(&registry, "intent", "fp-1", 1);
    assert!(status.is_shadow_only(), "got {status:?}");
}

#[test]
fn registry_round_trip_and_lookup() {
    let corpus = CalibrationRegistry::load_corpus(&seed_corpus_path()).expect("seed");
    let entry = RegistryEntry::calibrate("intent", "fp-test", 1, &corpus, 0.5).expect("calibrate");
    assert!(entry.gate.passed);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("registry.json");
    let mut registry = CalibrationRegistry::empty();
    registry.upsert(entry);
    registry.save(&path).expect("save");
    // Emission is real JSON, re-readable without this crate's types.
    let raw = std::fs::read_to_string(&path).expect("read back");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("json");
    assert!(parsed.get("entries").and_then(|e| e.as_array()).is_some());
    let loaded = CalibrationRegistry::load(&path).expect("load");
    let status = authorize(&loaded, "intent", "fp-test", 1);
    assert!(status.is_calibrated());
    // Wrong fingerprint or version fails closed.
    assert!(authorize(&loaded, "intent", "fp-other", 1).is_shadow_only());
    assert!(authorize(&loaded, "intent", "fp-test", 2).is_shadow_only());
    assert!(authorize(&loaded, "other-point", "fp-test", 1).is_shadow_only());
}

#[test]
fn gate_params_make_recall_floor_explicit() {
    use fluent_llm::decision::{gate_for_point_with, ScoredCase};
    let provenance = fluent_llm::decision::CaseProvenance::human("test", "2026-11-08");
    let mk = |score: f64, expected: bool| ScoredCase {
        score,
        expected,
        control: false,
        provenance: provenance.clone(),
    };
    // Recall 0.5 at threshold 0.5: default floor passes, explicit 0.9 blocks.
    let cases = vec![mk(0.95, true), mk(0.30, true), mk(0.10, false)];
    // Default floor (>0 recall) passes this firing threshold.
    assert!(fluent_llm::decision::gate_for_point(&cases, 0.5));
    // An explicit higher floor blocks it.
    let strict = GateParams {
        min_recall: 0.9,
        ..GateParams::default()
    };
    assert!(!gate_for_point_with(&cases, 0.5, &strict));
}
