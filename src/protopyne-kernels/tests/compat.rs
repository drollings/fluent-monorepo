use crate::compat::{
    assert_matrix_covers, compat_for, evidence_path_exists, external_evidence, CompatStatus,
    ExternalArtifact, ExternalRow, ParityEvidence, COMPATIBILITY, NATIVE_MATRIX_ROWS,
};
use crate::component::ComponentRegistry;
use crate::indicator::builtin_registry;

#[test]
fn matrix_covers_builtins_with_positive_finite_eps() {
    let registry = builtin_registry().expect("builtins register");
    assert_matrix_covers(&registry).expect("matrix covers registry");
    for id in registry.ids() {
        let row = compat_for(&id).expect("row present");
        assert!(row.eps.is_finite() && row.eps > 0.0);
    }
}

#[test]
fn registry_matches_matrix_exactly_with_no_orphans() {
    // Count equality in both directions: every kernel has exactly one row
    // and no row survives its kernel — plus every native-table library has
    // exactly one `native-*` row. `assert_matrix_covers` proves the
    // mapping; the lengths prove there is no padding on either side.
    let registry = builtin_registry().expect("builtins register");
    assert_matrix_covers(&registry).expect("matrix covers registry");
    assert_eq!(
        registry.len() + NATIVE_MATRIX_ROWS.len(),
        COMPATIBILITY.len(),
        "registry plus native rows equals matrix length, no orphans either way"
    );
    let components = ComponentRegistry::from_builtins().expect("components load");
    assert_eq!(
        components.ids(),
        registry.ids(),
        "component registry mirrors the builtin registry exactly"
    );
}

/// Every row's claim must be the claim its evidence can carry: a numeric
/// standing (parity at eps) never rests on "the probe ran", an external
/// standing names the artifact it was minted from, and every cited vector is
/// a file that exists. This is the board, not a comment.
#[test]
fn every_row_standing_matches_its_evidence() {
    for row in COMPATIBILITY {
        match row.status {
            CompatStatus::ExprParityVerified => assert!(
                matches!(
                    row.evidence,
                    ParityEvidence::GoldenVector(_)
                        | ParityEvidence::StagedReference(_)
                        | ParityEvidence::ExternalExport(_)
                ),
                "{:?} claims parity at eps with only {:?}",
                row.id,
                row.evidence
            ),
            CompatStatus::FormulaVerified(reason) => {
                assert!(
                    !reason.is_empty(),
                    "{:?} claims a formula standing with no reason",
                    row.id
                );
                assert!(
                    !matches!(row.evidence, ParityEvidence::ProbeRuns),
                    "{:?} claims eps on hand vectors but cites only probe runs",
                    row.id
                );
            }
            CompatStatus::ExternalParityVerified(artifact) => {
                let ParityEvidence::ExternalExport(named) = row.evidence else {
                    panic!(
                        "{:?} claims an external standing over {:?}",
                        row.id, row.evidence
                    );
                };
                assert_eq!(
                    artifact.as_str(),
                    named,
                    "{:?} names a different artifact in its standing and its evidence",
                    row.id
                );
            }
        }
        if let ParityEvidence::GoldenVector(path) = row.evidence {
            assert!(
                evidence_path_exists(path),
                "{:?} cites missing vector {path}",
                row.id
            );
        }
        if let ParityEvidence::StagedReference(path) = row.evidence {
            assert!(
                evidence_path_exists(path),
                "{:?} cites missing gate {path}",
                row.id
            );
        }
        if let ParityEvidence::ExternalExport(id) = row.evidence {
            assert!(
                !id.is_empty(),
                "{:?} cites an unnamed external export",
                row.id
            );
        }
    }
}

/// A complete provenance record, the shape an operator supplies for one
/// exported indicator.
fn complete_artifact() -> ExternalArtifact {
    ExternalArtifact {
        id: "tv-sma-1m-2026-10-22",
        source: "TradingView",
        source_version: "Pine v6",
        exported_at: "2026-10-22T14:00:00Z",
        symbol: "AMEX:SPY",
        timeframe: "1",
        script_sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        rows: &[ExternalRow {
            bar: 0,
            reference: 100.0,
            ours: 100.0,
        }],
    }
}

#[test]
fn a_complete_artifact_mints_an_external_standing() {
    let artifact = complete_artifact();
    match external_evidence(&artifact) {
        CompatStatus::ExternalParityVerified(id) => assert_eq!(id.as_str(), artifact.id),
        other => panic!("expected an external standing, got {other:?}"),
    }
}

#[test]
#[should_panic(expected = "script_sha256")]
fn external_evidence_refuses_an_artifact_without_script_sha() {
    let artifact = ExternalArtifact {
        script_sha256: "",
        ..complete_artifact()
    };
    let _ = external_evidence(&artifact);
}

#[test]
#[should_panic(expected = "rows")]
fn external_evidence_refuses_an_artifact_without_rows() {
    let artifact = ExternalArtifact {
        rows: &[],
        ..complete_artifact()
    };
    let _ = external_evidence(&artifact);
}

#[test]
#[should_panic(expected = "source")]
fn external_evidence_refuses_an_artifact_without_a_source() {
    let artifact = ExternalArtifact {
        source: "",
        ..complete_artifact()
    };
    let _ = external_evidence(&artifact);
}

#[test]
#[should_panic(expected = "script_sha256")]
fn external_evidence_refuses_a_malformed_script_sha() {
    let artifact = ExternalArtifact {
        script_sha256: "deadbeef",
        ..complete_artifact()
    };
    let _ = external_evidence(&artifact);
}
