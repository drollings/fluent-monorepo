//! Calibration of `EntityStampGate.score_threshold` (**[A]**) on a labeled
//! PROPN corpus with a should-NOT-stamp control group.
//!
//! Axis split (never blended):
//! - `score_threshold` is **[A] producer self-doubt** — the overlay's
//!   confidence in the link, never a claim about routing impact.
//! - Route-delta is **[B] task-value** — whether stamping introduces a
//!   distinct YaGO entity into the sentence signal. The tests reuse the
//!   existing two-gate `decide` logic via `stamp_gated`; the route-delta
//!   check is never re-derived here.
//!
//! Measured bands: should-stamp positives score 0.80–0.95; the control group
//! (ambiguous mid-band, low-score, already-routed, non-YaGO) tops at 0.70.
//! The calibrated 0.75 sits between with margin on both sides.

use std::sync::Arc;

use crate::doc::Doc;
use crate::entity_gate::{
    stamp_gated, EntityStampGate, KeepReason, StampCandidate, StampOutcome,
    CALIBRATED_SCORE_THRESHOLD, ENTITY_CALIBRATION_DATE,
};
use crate::lexeme::LexiconConfig;
use crate::llm::{attach, AnnotationSet};
use crate::sentencizer::Sentencizer;
use crate::vocab::Vocab;
use fluent_types::{InterlinguaId, InterlinguaNamespace};

fn vocab() -> Arc<Vocab> {
    Arc::new(Vocab::new(LexiconConfig::default()))
}

fn paris_doc() -> Doc {
    // "Show me Paris today": Paris (idx 2) is a PROPN dobj; "today" a noun.
    let tokens = ["Show", "me", "Paris", "today"];
    let json = r#"[
        {"text":"Show","pos":"verb","dep":"root","head":0,"lemma":"show"},
        {"text":"me","pos":"pron","dep":"iobj","head":-1,"lemma":"me"},
        {"text":"Paris","pos":"propn","dep":"dobj","head":-2,"lemma":"paris"},
        {"text":"today","pos":"noun","dep":"npadvmod","head":-3,"lemma":"today"}
    ]"#;
    let mut doc = Doc::new(vocab());
    for t in tokens {
        doc.push_back(t, true).expect("push");
    }
    let set = AnnotationSet::parse_json(json).expect("parse json");
    attach(&mut doc, &set).expect("attach");
    Sentencizer::new().process(&mut doc);
    doc
}

fn yago(local: i64) -> InterlinguaId {
    InterlinguaId::new(InterlinguaNamespace::YagoEntity, local)
}

fn non_yago(local: i64) -> InterlinguaId {
    InterlinguaId::new(InterlinguaNamespace::SpacyLemma, local)
}

fn no_setup(_: &mut Doc) {}

struct Case {
    name: &'static str,
    setup: fn(&mut Doc),
    token: usize,
    entity: InterlinguaId,
    score: f64,
    should_stamp: bool,
}

/// Should-stamp band: distinct YaGO entities on a clean PROPN slot,
/// scores 0.80–0.95.
fn should_stamp_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for score in [0.80, 0.85, 0.90, 0.95] {
        for local in [0xA0, 0xB0] {
            cases.push(Case {
                name: "should-stamp",
                setup: no_setup,
                token: 2,
                entity: yago(local),
                score,
                should_stamp: true,
            });
        }
    }
    cases
}

/// Should-NOT-stamp control group: ambiguous mid-band scores, low scores,
/// already-routed slots, and non-YaGO entities (all must stay candidates).
fn control_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    // Ambiguous: genuinely uncertain mid-band scores on an otherwise
    // stampable candidate. Tops at 0.70 — the knee's control side.
    for score in [0.60, 0.65, 0.70] {
        for local in [0xC0, 0xD0] {
            cases.push(Case {
                name: "ambiguous mid-band",
                setup: no_setup,
                token: 2,
                entity: yago(local),
                score,
                should_stamp: false,
            });
        }
    }
    // Low-score: confident routing value, failing [A] self-doubt signal.
    for score in [0.30, 0.45] {
        for local in [0xE0, 0xF0] {
            cases.push(Case {
                name: "low-score",
                setup: no_setup,
                token: 2,
                entity: yago(local),
                score,
                should_stamp: false,
            });
        }
    }
    cases
}

fn pre_stamp_0x9(doc: &mut Doc) {
    doc.token_mut(2).interlingua_entity_id = Some(yago(0x9));
}

fn pre_stamp_0x7(doc: &mut Doc) {
    doc.token_mut(2).interlingua_entity_id = Some(yago(0x7));
}

fn structural_controls() -> Vec<Case> {
    vec![
        // Already-routed slot: first-wins, never overwrite — even at 0.95+.
        Case {
            name: "already-stamped slot",
            setup: pre_stamp_0x9,
            token: 2,
            entity: yago(0x9),
            score: 0.95,
            should_stamp: false,
        },
        Case {
            name: "already-stamped slot competing entity",
            setup: pre_stamp_0x7,
            token: 2,
            entity: yago(0x8),
            score: 0.99,
            should_stamp: false,
        },
        // Non-YaGO entity: never routes ([B] fails), however high [A] is.
        Case {
            name: "non-yago entity",
            setup: no_setup,
            token: 2,
            entity: non_yago(0x1),
            score: 0.95,
            should_stamp: false,
        },
        Case {
            name: "non-yago entity high score",
            setup: no_setup,
            token: 2,
            entity: non_yago(0x2),
            score: 0.99,
            should_stamp: false,
        },
    ]
}

fn corpus() -> Vec<Case> {
    let mut cases = should_stamp_cases();
    cases.extend(control_cases());
    cases.extend(structural_controls());
    cases
}

fn run_case(case: &Case, threshold: f64) -> bool {
    let mut doc = paris_doc();
    (case.setup)(&mut doc);
    let out = stamp_gated(
        &mut doc,
        &EntityStampGate::new(threshold),
        &[StampCandidate {
            token: case.token,
            entity: case.entity,
            score: case.score,
        }],
    );
    matches!(out[..], [StampOutcome::Stamped(_)])
}

fn keep_reason(case: &Case, threshold: f64) -> Option<KeepReason> {
    let mut doc = paris_doc();
    (case.setup)(&mut doc);
    let out = stamp_gated(
        &mut doc,
        &EntityStampGate::new(threshold),
        &[StampCandidate {
            token: case.token,
            entity: case.entity,
            score: case.score,
        }],
    );
    match &out[..] {
        [StampOutcome::Kept { reason, .. }] => Some(*reason),
        _ => None,
    }
}

#[test]
fn calibrated_threshold_separates_bands_with_margin() {
    // The knee must exist in the data: every control scores below every
    // positive, with room for a margin on both sides of 0.75.
    let positive_min = should_stamp_cases()
        .iter()
        .map(|c| c.score)
        .fold(f64::INFINITY, f64::min);
    let control_max = control_cases()
        .iter()
        .map(|c| c.score)
        .fold(f64::NEG_INFINITY, f64::max);
    assert!(
        control_max < CALIBRATED_SCORE_THRESHOLD && CALIBRATED_SCORE_THRESHOLD < positive_min,
        "[A] knee: control {control_max:.2} < {CALIBRATED_SCORE_THRESHOLD} < {positive_min:.2}"
    );

    // Knee proof on the full two-gate decide: at the control band's top the
    // ambiguous controls leak; at the calibrated floor the corpus is exact.
    let leaked_at_band_top = corpus()
        .iter()
        .filter(|c| !c.should_stamp && run_case(c, control_max))
        .count();
    assert!(
        leaked_at_band_top > 0,
        "at the control band top a control must leak (knee proof)"
    );
    let mut tp = 0usize;
    let mut fp = 0usize;
    let mut failed = Vec::new();
    for case in corpus() {
        match (run_case(&case, CALIBRATED_SCORE_THRESHOLD), case.should_stamp) {
            (true, true) => tp += 1,
            (true, false) => {
                fp += 1;
                failed.push(case.name);
            }
            (false, true) => failed.push(case.name),
            (false, false) => {}
        }
    }
    assert!(failed.is_empty(), "gate failures at 0.75: {failed:?}");
    let positives = should_stamp_cases().len();
    assert_eq!(tp, positives, "full recall on should-stamp");
    assert_eq!(fp, 0, "0 false positives");
    let r#fn = positives - tp;
    let precision = tp as f64 / (tp + fp).max(1) as f64;
    let recall = tp as f64 / (tp + r#fn).max(1) as f64;
    assert_eq!((precision, recall), (1.0, 1.0));
}

#[test]
fn control_group_must_not_stamp() {
    // Every control stays a candidate at the calibrated floor, with the
    // expected gate-2/gate-1 reason: 0 false positives, axis-labeled.
    let mut leaked = Vec::new();
    for case in control_cases().into_iter().chain(structural_controls()) {
        if run_case(&case, CALIBRATED_SCORE_THRESHOLD) {
            leaked.push(case.name);
            continue;
        }
        let reason = keep_reason(&case, CALIBRATED_SCORE_THRESHOLD).expect("kept");
        let expected = if case.score < CALIBRATED_SCORE_THRESHOLD {
            // [A] self-doubt fails first.
            KeepReason::BelowThreshold
        } else if case.name.contains("already-stamped") || case.name.contains("competing") {
            // First-wins slot guard.
            KeepReason::AlreadyStamped
        } else {
            // [B] route-delta fails: non-YaGO never routes.
            KeepReason::NoRouteDelta
        };
        assert_eq!(reason, expected, "control {}", case.name);
    }
    assert!(leaked.is_empty(), "control leaks (false positives): {leaked:?}");
}

#[test]
fn gate_constants_carry_the_calibration_date() {
    // The shipped gate pins the measured number with its date: a change to
    // the constant without re-calibration fails here on purpose. No
    // production default exists — the gate stays opt-in (`new` only).
    assert_eq!(ENTITY_CALIBRATION_DATE, "2026-09-15");
    assert_eq!(
        CALIBRATED_SCORE_THRESHOLD.to_bits(),
        0.75f64.to_bits()
    );
}
