//! Reference-overlap matrix checks: every row cites both the reference
//! symbol and the engine symbol, refusal rows pin their exact strings by
//! executing the recorded probe, and the `ta.*` rows cover the full bridge
//! surface in both directions.
//!
//! Test path note: this file rides the `pine::egui_parity` unit module, so
//! the runnable filter is `pine::egui_parity`, not the file stem.

use super::*;
use crate::compat::{evidence_path_exists, ParityEvidence};
use crate::{parse_program, InputSet, PineEngine, PineRuntimeConfig, TaBridge};
use chrono::TimeZone as _;
use protopyne_types::Bar;

fn bar_at(index: i64) -> Bar {
    Bar::new()
        .ts(chrono::Utc
            .timestamp_opt(1_700_000_000 + index * 60, 0)
            .single()
            .expect("valid test timestamp"))
        .o("100.00".parse().expect("decimal"))
        .h("101.00".parse().expect("decimal"))
        .l("99.00".parse().expect("decimal"))
        .c(format!("{}.00", 100 + index).parse().expect("decimal"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .build()
}

fn bars(count: i64) -> Vec<Bar> {
    (0..count).map(bar_at).collect()
}

fn run_probe(script: &str) -> Result<crate::RunOutcome, crate::KernelError> {
    let program = parse_program(script).expect("probe parses");
    let mut engine = PineEngine::new(PineRuntimeConfig::new().build());
    engine.run(&program, &bars(24), InputSet::new())
}

#[test]
fn rows_cite_both_sides_with_reasons() {
    assert!(!EGUI_PARITY.is_empty(), "matrix is not a stub");
    let mut seen = std::collections::HashSet::new();
    for row in EGUI_PARITY {
        assert!(!row.construct.is_empty(), "construct named");
        assert!(
            seen.insert(row.construct),
            "duplicate construct row: {}",
            row.construct
        );
        assert!(
            !row.egui.is_empty(),
            "{} cites the reference side",
            row.construct
        );
        assert!(
            !row.protopyne.is_empty(),
            "{} cites the engine side",
            row.construct
        );
    }
}

/// Every row's standing must be the standing its evidence can carry. The
/// reference layer is not in this tree, so no row may claim a numeric match
/// without a checked-in vector, and a row with only its own probe must say
/// so (`MatchByInspection`); a citation must name a file that exists.
#[test]
fn every_row_standing_matches_its_evidence() {
    for row in EGUI_PARITY {
        match row.verdict {
            ParityVerdict::Match => assert!(
                matches!(
                    row.evidence,
                    ParityEvidence::GoldenVector(_) | ParityEvidence::ExternalExport(_)
                ),
                "{} claims a reference match with only {:?}",
                row.construct,
                row.evidence
            ),
            ParityVerdict::MatchByInspection => match row.probe {
                Some(_) => assert_eq!(
                    row.evidence,
                    ParityEvidence::ProbeRuns,
                    "{} runs a probe, so the probe is its evidence",
                    row.construct
                ),
                None => assert!(
                    matches!(row.evidence, ParityEvidence::StagedReference(_)),
                    "{} has no probe and must name the suite that exercises it",
                    row.construct
                ),
            },
            ParityVerdict::Correct => {
                assert!(
                    !row.reason.is_empty(),
                    "{} explains its divergence",
                    row.construct
                );
                assert_eq!(
                    row.evidence,
                    ParityEvidence::ProbeRuns,
                    "{} pins a divergence by running its probe",
                    row.construct
                );
            }
            ParityVerdict::Refuse => {
                assert!(
                    row.probe.is_some() && row.refusal.is_some(),
                    "{} pins probe plus exact refusal",
                    row.construct
                );
                assert_eq!(
                    row.evidence,
                    ParityEvidence::ProbeRuns,
                    "{} pins its refusal by running its probe",
                    row.construct
                );
            }
        }
        match row.evidence {
            ParityEvidence::GoldenVector(path) | ParityEvidence::StagedReference(path) => {
                assert!(
                    evidence_path_exists(path),
                    "{} cites missing evidence {path}",
                    row.construct
                );
            }
            ParityEvidence::ExternalExport(id) => {
                assert!(!id.is_empty(), "{} names an empty export", row.construct);
            }
            ParityEvidence::ProbeRuns => assert!(
                row.probe.is_some(),
                "{} cites a probe it does not carry",
                row.construct
            ),
        }
    }
}

#[test]
fn ta_surface_covered_both_directions() {
    let bridged: Vec<&str> = EGUI_PARITY
        .iter()
        .filter(|row| row.construct.starts_with("ta.") && row.verdict != ParityVerdict::Refuse)
        .filter_map(|row| row.construct.strip_prefix("ta."))
        .collect();
    for method in TaBridge::supported_methods() {
        assert!(
            bridged.contains(method),
            "bridged ta.{method} has a non-refuse row"
        );
    }
    let refused: Vec<&str> = EGUI_PARITY
        .iter()
        .filter(|row| row.construct.starts_with("ta.") && row.verdict == ParityVerdict::Refuse)
        .filter_map(|row| row.construct.strip_prefix("ta."))
        .collect();
    for missing in [
        "dmi",
        "adx",
        "sar",
        "kc",
        "dc",
        "mode",
        "percentrank",
        "linreg",
    ] {
        assert!(
            refused.contains(&missing),
            "unbridged ta.{missing} has a refuse row"
        );
    }
}

#[test]
fn refusal_probes_fail_with_exact_strings() {
    for row in EGUI_PARITY {
        let (Some(probe), Some(refusal)) = (row.probe, row.refusal) else {
            continue;
        };
        if row.verdict != ParityVerdict::Refuse {
            continue;
        }
        let err = run_probe(probe).expect_err("refuse probe fails");
        assert_eq!(
            err.to_string(),
            format!("pine evaluation refused: {refusal}"),
            "exact refusal for {}",
            row.construct
        );
    }
}

#[test]
fn inspection_probes_run_clean() {
    for row in EGUI_PARITY {
        let Some(probe) = row.probe else { continue };
        if !matches!(
            row.verdict,
            ParityVerdict::Match | ParityVerdict::MatchByInspection
        ) {
            continue;
        }
        run_probe(probe).expect("inspection probe runs");
    }
}

#[test]
fn lookup_finds_rows_by_construct() {
    let row = parity_for("ta.sma").expect("ta.sma row known");
    assert_eq!(row.verdict, ParityVerdict::MatchByInspection);
    assert!(parity_for("no.such.construct").is_none());
}
