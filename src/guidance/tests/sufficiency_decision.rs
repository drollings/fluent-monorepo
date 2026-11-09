use super::{route_sufficiency, sufficiency_spec, Sufficiency, SUFFICIENCY_VERSION};
use crate::query::shadow::ShadowRecord;
use fluent_llm::decision::{Decision, ThresholdSet};

fn thresholds() -> ThresholdSet {
    sufficiency_spec().thresholds()
}

fn sufficient(top: f64) -> Decision {
    Decision::from_distribution(
        "Answer",
        vec![
            ("Answer".to_string(), top),
            ("Synthesize".to_string(), (1.0 - top) / 2.0),
            ("Unanswerable".to_string(), (1.0 - top) / 2.0),
        ],
        "test".to_string(),
        SUFFICIENCY_VERSION,
        7,
        false,
    )
    .expect("decision")
}

fn labelled(label: &str) -> Decision {
    Decision::from_distribution(
        label,
        vec![
            ("Answer".to_string(), 0.1),
            ("Synthesize".to_string(), 0.1),
            ("Unanswerable".to_string(), 0.1),
            (label.to_string(), 0.7),
        ],
        "test".to_string(),
        SUFFICIENCY_VERSION,
        7,
        false,
    )
    .expect("decision")
}

#[test]
fn ungrounded_stays_silent_despite_confident_model() {
    // Grounded-or-silent: no excerpts, no synthesis — the verdict
    // cannot override the hard gate.
    let (route, record) = route_sufficiency(0, false, Some(&sufficient(0.99)), &thresholds());
    assert_eq!(route, Sufficiency::Unanswerable);
    let record: ShadowRecord = record.expect("shadow still logged");
    assert_eq!(record.point, "sufficiency");
    assert_eq!(record.authoritative, "evidence:0");
    // The verdict would have acted — that disagreement with the silent
    // route is precisely what the shadow log measures.
    assert!(record.would_have_acted);
}

#[test]
fn grounded_without_decision_answers_from_excerpts() {
    // Deterministic fallback: excerpts present, no verdict — answer.
    let (route, record) = route_sufficiency(3, true, None, &thresholds());
    assert_eq!(route, Sufficiency::Answer);
    assert!(record.is_none());
}

#[test]
fn grounded_verdict_routes_three_ways() {
    let (answer, _) = route_sufficiency(2, true, Some(&labelled("Answer")), &thresholds());
    assert_eq!(answer, Sufficiency::Answer);
    let (synth, _) = route_sufficiency(2, true, Some(&labelled("Synthesize")), &thresholds());
    assert_eq!(synth, Sufficiency::Synthesize);
    let (silent, _) = route_sufficiency(2, true, Some(&labelled("Unanswerable")), &thresholds());
    assert_eq!(silent, Sufficiency::Unanswerable);
}

#[test]
fn unknown_label_falls_back_to_answering() {
    // A label outside the spec can never silence grounded evidence.
    let odd = Decision::from_distribution(
        "Maybe",
        vec![("Maybe".to_string(), 1.0)],
        "test".to_string(),
        SUFFICIENCY_VERSION,
        7,
        false,
    )
    .expect("decision");
    let (route, record) = route_sufficiency(2, true, Some(&odd), &thresholds());
    assert_eq!(route, Sufficiency::Answer);
    assert!(record.is_some());
}

#[test]
fn seed_corpus_passes_gate_at_point_threshold() {
    struct Case {
        score: f64,
        label: bool,
    }
    let cases = vec![
        Case {
            score: 0.95,
            label: true,
        },
        Case {
            score: 0.93,
            label: true,
        },
        Case {
            score: 0.90,
            label: true,
        },
        Case {
            score: 0.50,
            label: false,
        },
        Case {
            score: 0.20,
            label: false,
        },
        Case {
            score: 0.05,
            label: false,
        },
    ];
    let report =
        common_core::calibration::calibrate_threshold(&cases, |c| c.score, |c| c.label, 0.9);
    assert!(
        report.passes_gate(),
        "sufficiency seed must pass: precision {} fpr {}",
        report.precision,
        report.fpr
    );
}
