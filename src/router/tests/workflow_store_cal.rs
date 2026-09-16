//! Calibration of the workflow **write** gate (`gated_insert_allowed`).
//!
//! Axis split (never blended):
//! - `assembler_confidence` is **[A] producer self-doubt** — the assembler
//!   reporting a well-formed decomposition, not a claim about task value.
//! - novelty distance is **[B] task-value/duplication** — whether the library
//!   gains anything, even when the producer is confident.
//! - `verified` is provenance (not a number) — human review or paraphrase
//!   self-consistency, never inferred from confidence.
//!
//! The corpus pairs a should-store band with a should-NOT-store control
//! group (confident-but-unverified, near-duplicate, low-confidence). The
//! constants sit between the bands with margin; the knee proof shows the
//! controls leak below the band.

use crate::ledger::workflow_store::{
    gated_insert_allowed, CALIBRATED_WRITE_CONFIDENCE, CALIBRATED_WRITE_NOVELTY, CALIBRATION_DATE,
};
use common_core::calibration::{calibrate_threshold, sweep_thresholds};

/// One calibration case: the gate inputs plus ground truth.
struct Case {
    name: &'static str,
    /// [A] producer self-doubt signal.
    confidence: f64,
    /// [B] novelty distance (`1 - cosine` to nearest stored entry).
    novelty: f64,
    /// Provenance: human review or paraphrase self-consistency.
    verified: bool,
    /// Ground truth: should this workflow be stored?
    should_store: bool,
}

/// Should-store band: verified, genuinely reusable, non-duplicate.
fn should_store_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for (i, confidence) in [0.90, 0.92, 0.95, 0.99].iter().enumerate() {
        for (j, novelty) in [0.20, 0.30, 0.50, 1.00].iter().enumerate() {
            cases.push(Case {
                name: "should-store",
                confidence: *confidence,
                novelty: *novelty,
                verified: true,
                should_store: true,
            });
            let _ = (i, j);
        }
    }
    cases
}

/// Should-NOT-store control group.
fn control_cases() -> Vec<Case> {
    vec![
        // Confident-but-unverified: high [A], failing provenance.
        Case {
            name: "confident-but-unverified",
            confidence: 0.95,
            novelty: 0.50,
            verified: false,
            should_store: false,
        },
        Case {
            name: "confident-but-unverified-2",
            confidence: 0.99,
            novelty: 0.80,
            verified: false,
            should_store: false,
        },
        // Near-duplicate: verified and confident, failing [B].
        Case {
            name: "near-duplicate",
            confidence: 0.95,
            novelty: 0.00,
            verified: true,
            should_store: false,
        },
        Case {
            name: "near-duplicate-2",
            confidence: 0.90,
            novelty: 0.05,
            verified: true,
            should_store: false,
        },
        Case {
            name: "near-duplicate-3",
            confidence: 0.99,
            novelty: 0.10,
            verified: true,
            should_store: false,
        },
        Case {
            name: "near-duplicate-edge",
            confidence: 0.92,
            novelty: 0.14,
            verified: true,
            should_store: false,
        },
        // Low self-doubt signal: verified and novel, failing [A].
        Case {
            name: "low-confidence",
            confidence: 0.50,
            novelty: 0.50,
            verified: true,
            should_store: false,
        },
        Case {
            name: "low-confidence-2",
            confidence: 0.65,
            novelty: 0.40,
            verified: true,
            should_store: false,
        },
        Case {
            name: "low-confidence-3",
            confidence: 0.75,
            novelty: 0.60,
            verified: true,
            should_store: false,
        },
        Case {
            name: "low-confidence-edge",
            confidence: 0.80,
            novelty: 0.30,
            verified: true,
            should_store: false,
        },
        // Fails everything.
        Case {
            name: "fails-all",
            confidence: 0.40,
            novelty: 0.02,
            verified: false,
            should_store: false,
        },
    ]
}

fn corpus() -> Vec<Case> {
    let mut cases = should_store_cases();
    cases.extend(control_cases());
    cases
}

fn gate_predicted(c: &Case) -> bool {
    gated_insert_allowed(c.confidence, c.novelty, c.verified)
}

#[test]
fn write_gate_confidence_knee_with_margin() {
    // [A] axis in isolation: novelty high, provenance held. The low-signal
    // control band tops at 0.80, the should-store band starts at 0.90.
    let positives: Vec<f64> = should_store_cases().iter().map(|c| c.confidence).collect();
    let controls: Vec<f64> = control_cases()
        .iter()
        .filter(|c| c.verified && c.novelty > CALIBRATED_WRITE_NOVELTY)
        .map(|c| c.confidence)
        .collect();
    assert!(!controls.is_empty(), "control band must be non-empty");
    let control_max = controls.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));
    let positive_min = positives.iter().fold(f64::INFINITY, |a, &b| a.min(b));
    assert!(
        control_max < CALIBRATED_WRITE_CONFIDENCE && CALIBRATED_WRITE_CONFIDENCE < positive_min,
        "[A] knee: control {control_max:.2} < {CALIBRATED_WRITE_CONFIDENCE} < {positive_min:.2}"
    );

    // Knee proof via the shared harness: below the band the low-signal
    // controls leak (FPR > 0); at the calibrated floor the gate is exact.
    let axis_cases: Vec<Case> = [0.90, 0.92, 0.95, 0.99]
        .iter()
        .map(|&confidence| Case {
            name: "should-store",
            confidence,
            novelty: 0.50,
            verified: true,
            should_store: true,
        })
        .chain(controls.iter().map(|&confidence| Case {
            name: "low-signal-control",
            confidence,
            novelty: 0.50,
            verified: true,
            should_store: false,
        }))
        .collect();
    let below = calibrate_threshold(&axis_cases, |c| c.confidence, |c| c.should_store, 0.80);
    assert!(
        below.fpr > 0.0,
        "below the [A] band the controls must leak (knee proof)"
    );
    let at_floor = calibrate_threshold(
        &axis_cases,
        |c| c.confidence,
        |c| c.should_store,
        CALIBRATED_WRITE_CONFIDENCE,
    );
    assert_eq!((at_floor.tp, at_floor.fp, at_floor.r#fn), (4, 0, 0));
    assert!(
        at_floor.passes_gate(),
        "calibrated [A] floor passes the harness gate"
    );
}

#[test]
fn write_gate_novelty_knee_with_margin() {
    // [B] axis in isolation: confidence high, provenance held. The
    // near-duplicate control band tops at 0.14, the should-store band starts
    // at 0.20.
    let positives: Vec<f64> = [0.20, 0.30, 0.50, 1.00].to_vec();
    let controls: Vec<f64> = control_cases()
        .iter()
        .filter(|c| c.verified && c.confidence >= CALIBRATED_WRITE_CONFIDENCE)
        .map(|c| c.novelty)
        .collect();
    assert!(!controls.is_empty(), "control band must be non-empty");
    let control_max = controls.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));
    let positive_min = positives.iter().fold(f64::INFINITY, |a, &b| a.min(b));
    assert!(
        control_max < CALIBRATED_WRITE_NOVELTY && CALIBRATED_WRITE_NOVELTY < positive_min,
        "[B] knee: control {control_max:.2} < {CALIBRATED_WRITE_NOVELTY} < {positive_min:.2}"
    );

    let axis_cases: Vec<Case> = positives
        .iter()
        .map(|&novelty| Case {
            name: "should-store",
            confidence: 0.95,
            novelty,
            verified: true,
            should_store: true,
        })
        .chain(controls.iter().map(|&novelty| Case {
            name: "near-duplicate-control",
            confidence: 0.95,
            novelty,
            verified: true,
            should_store: false,
        }))
        .collect();
    let below = calibrate_threshold(&axis_cases, |c| c.novelty, |c| c.should_store, 0.10);
    assert!(
        below.fpr > 0.0,
        "below the [B] band the controls must leak (knee proof)"
    );
    let at_floor = calibrate_threshold(
        &axis_cases,
        |c| c.novelty,
        |c| c.should_store,
        // The gate uses strict `>`: probe just above the floor.
        CALIBRATED_WRITE_NOVELTY + f64::EPSILON,
    );
    assert_eq!((at_floor.tp, at_floor.fp, at_floor.r#fn), (4, 0, 0));
    assert!(
        at_floor.passes_gate(),
        "calibrated [B] floor passes the harness gate"
    );
}

#[test]
fn control_group_must_not_store() {
    let mut leaked = Vec::new();
    for c in control_cases() {
        if gate_predicted(&c) {
            leaked.push(c.name);
        }
    }
    assert!(
        leaked.is_empty(),
        "control leaks (false positives): {leaked:?}"
    );
}

#[test]
fn calibrated_write_gate_is_exact_on_corpus() {
    let cases = corpus();
    let mut tp = 0usize;
    let mut fp = 0usize;
    let mut failed = Vec::new();
    for c in &cases {
        match (gate_predicted(c), c.should_store) {
            (true, true) => tp += 1,
            (true, false) => {
                fp += 1;
                failed.push(format!("CONTROL LEAK {}", c.name));
            }
            (false, true) => failed.push(format!("MISS {}", c.name)),
            (false, false) => {}
        }
    }
    let r#fn = cases.iter().filter(|c| c.should_store).count() - tp;
    assert!(failed.is_empty(), "gate failures: {failed:?}");
    let precision = tp as f64 / (tp + fp).max(1) as f64;
    let recall = tp as f64 / (tp + r#fn).max(1) as f64;
    assert_eq!((precision, recall), (1.0, 1.0));

    // Joint knee proof end-to-end: with the confidence floor lowered to the
    // top of the control band, a low-signal control passes the full
    // conjunction — the floor placement is load-bearing, not incidental.
    // (Single-axis harness sweeps cannot separate the conjunction's other
    // legs; the isolated knee proofs above cover those.)
    let relaxed_leaks = cases
        .iter()
        .filter(|c| !c.should_store)
        .filter(|c| c.confidence >= 0.80 && c.novelty > CALIBRATED_WRITE_NOVELTY && c.verified)
        .count();
    assert!(
        relaxed_leaks > 0,
        "lowering the [A] floor must admit a control"
    );
    let _sweep = sweep_thresholds(
        &cases,
        |c| c.confidence,
        |c| c.should_store,
        &[0.80, CALIBRATED_WRITE_CONFIDENCE, 0.90],
    );
}

#[test]
fn write_gate_constants_carry_the_calibration_date() {
    // The shipped gate pins the measured numbers with their date (matching
    // the replay-side date): a change to any constant without
    // re-calibration fails here on purpose.
    assert_eq!(CALIBRATION_DATE, "2026-09-15");
    assert_eq!(CALIBRATED_WRITE_CONFIDENCE.to_bits(), 0.85f64.to_bits());
    assert_eq!(CALIBRATED_WRITE_NOVELTY.to_bits(), 0.15f64.to_bits());
}
