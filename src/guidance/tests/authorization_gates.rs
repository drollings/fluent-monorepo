//! Authorization gates: advisory versus authorized steering, dataset
//! pinning, cold-start hold-for-review, and breaker-to-halted wiring.
//! Confidence (the model is sure) is never task value (the decision
//! is right); modes gate producer authority, precision and recall
//! certify outcome correctness.

use fluent_llm::decision::{
    authorize, authorize_advisory, authorize_pinned, CalibrationRegistry, CalibrationStatus,
    CaseOrigin, CaseProvenance, Corpus, CorpusProvenance, GateParams, RegistryEntry, ScoredCase,
    ThresholdSet,
};

fn provenance(dataset: &str) -> CorpusProvenance {
    CorpusProvenance {
        source: "authorization-gates".to_string(),
        labeller: "team".to_string(),
        date: "2026-11-10".to_string(),
        dataset: dataset.to_string(),
    }
}

fn case(score: f64, expected: bool, control: bool) -> ScoredCase {
    ScoredCase {
        score,
        expected,
        control,
        provenance: CaseProvenance {
            origin: CaseOrigin::Human,
            labeller: "team".to_string(),
            date: "2026-11-10".to_string(),
        },
    }
}

fn corpus(dataset: &str) -> Corpus {
    Corpus {
        point: "triage".to_string(),
        model_fingerprint: "model-a".to_string(),
        question_version: 1,
        provenance: provenance(dataset),
        cases: vec![
            case(0.9, true, false),
            case(0.85, true, false),
            case(0.8, true, false),
            case(0.2, false, false),
            case(0.1, false, false),
            case(0.05, false, true),
        ],
    }
}

fn calibrated(dataset: &str) -> RegistryEntry {
    RegistryEntry::calibrate("triage", "model-a", 1, &corpus(dataset), 0.5).expect("calibrate")
}

#[test]
fn control_set_never_authorizes() {
    let mut registry = CalibrationRegistry::empty();
    let entry = calibrated("ds-1");
    assert!(entry.gate.passed, "fixture corpus must pass its own gate");
    registry.upsert(entry);
    assert!(matches!(
        authorize(&registry, "triage", "model-a", 1),
        CalibrationStatus::Calibrated(_)
    ));
    assert!(matches!(
        authorize_advisory(&registry, "triage", "model-a", 1),
        CalibrationStatus::Advisory(_)
    ));
    assert!(authorize(&registry, "other-point", "model-a", 1).is_shadow_only());
    assert!(authorize(&registry, "triage", "model-b", 1).is_shadow_only());
    assert!(authorize_advisory(&registry, "triage", "model-b", 1).is_shadow_only());
}

#[test]
fn threshold_sweep_reproduces_the_pinned_entry() {
    let entry = calibrated("ds-1");
    let thresholds = ThresholdSet {
        act: 0.5,
        review: 0.5,
    };
    assert_eq!(entry.thresholds, thresholds);
    assert!(entry.gate.precision >= GateParams::default().min_precision);
    assert_eq!(entry.gate.control_fires, 0);
}

#[test]
fn version_bump_returns_to_measuring() {
    let mut registry = CalibrationRegistry::empty();
    registry.upsert(calibrated("ds-1"));
    assert!(authorize(&registry, "triage", "model-a", 1).is_calibrated());
    assert!(authorize(&registry, "triage", "model-a", 2).is_shadow_only());
    assert!(authorize_advisory(&registry, "triage", "model-a", 2).is_shadow_only());
    assert!(authorize_pinned(&registry, "triage", "model-a", 1, "ds-1").is_calibrated());
}

#[test]
fn dataset_rotation_returns_to_measuring() {
    let mut registry = CalibrationRegistry::empty();
    registry.upsert(calibrated("ds-1"));
    assert!(authorize_pinned(&registry, "triage", "model-a", 1, "ds-1").is_calibrated());
    let rotated = authorize_pinned(&registry, "triage", "model-a", 1, "ds-2");
    assert!(
        rotated.is_shadow_only(),
        "rotated dataset must re-measure, got {rotated:?}"
    );
}

#[test]
fn cold_start_holds_drafts_for_review() {
    use crate::triage::{draft_disposition, DraftDisposition};
    let measuring = authorize(&CalibrationRegistry::empty(), "triage", "model-a", 1);
    assert_eq!(
        draft_disposition(&measuring),
        DraftDisposition::HoldForReview
    );
    let mut registry = CalibrationRegistry::empty();
    registry.upsert(calibrated("ds-1"));
    let advisory = authorize_advisory(&registry, "triage", "model-a", 1);
    assert_eq!(
        draft_disposition(&advisory),
        DraftDisposition::HoldForReview
    );
    let authorized = authorize(&registry, "triage", "model-a", 1);
    assert_eq!(draft_disposition(&authorized), DraftDisposition::MayApply);
}

#[test]
fn breaker_trip_maps_to_halted_with_verified_work_kept() {
    use crate::maintenance::{is_breaker_trip, LoopReport};
    assert!(is_breaker_trip("repeat failure"));
    assert!(is_breaker_trip("revert rate"));
    assert!(is_breaker_trip("no progress"));
    assert!(!is_breaker_trip("max passes reached"));
    let mut report = LoopReport {
        stopped: Some("revert rate".to_string()),
        ..LoopReport::default()
    };
    report.verified.push("pkg/a.rs".to_string());
    assert_eq!(report.exit_code(), crate::seal::ExitCode::Halted);
    assert_eq!(report.verified, vec!["pkg/a.rs".to_string()]);
    let clean = LoopReport::default();
    assert_eq!(clean.exit_code(), crate::seal::ExitCode::Done);
    let stuck = LoopReport {
        escalated: vec![("a".to_string(), "needs_human".to_string())],
        ..LoopReport::default()
    };
    assert_eq!(stuck.exit_code(), crate::seal::ExitCode::Stuck);
}
