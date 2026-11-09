use super::{normalize_signature, staleness_eligible, staleness_spec};

#[test]
fn spec_pins_noul_shape() {
    let spec = staleness_spec();
    assert!(spec.validate().is_ok());
    assert_eq!(spec.kind, "noul");
}

#[test]
fn whitespace_only_diffs_are_ineligible() {
    // Producer self-doubt (clock granularity) resolved without a model:
    // identical bytes under whitespace are never nominated.
    assert!(!staleness_eligible(
        true,
        "fn foo(x: i32)",
        "fn  foo(x:  i32)"
    ));
    assert!(!staleness_eligible(
        true,
        "fn foo(x: i32) {\n    bar();\n}",
        "fn foo(x: i32) {\n\n    bar();\n}\n"
    ));
    assert_eq!(
        normalize_signature("fn  foo( x:i32 )"),
        normalize_signature("fn foo(x:i32)")
    );
}

#[test]
fn human_prose_is_never_nominated() {
    // Human-written commentary (`comment_generated == false`) survives
    // re-syncs untouched, even across behavior diffs.
    assert!(!staleness_eligible(
        false,
        "fn foo(x: i32)",
        "fn foo(x: i32, debug: bool)"
    ));
    assert!(!staleness_eligible(false, "fn foo()", "fn foo()"));
}

#[test]
fn behavior_diffs_on_generated_comments_are_eligible() {
    assert!(staleness_eligible(
        true,
        "fn foo(x: i32)",
        "fn foo(x: i32, debug: bool)"
    ));
    assert!(staleness_eligible(true, "fn foo()", "fn bar()"));
}

#[test]
fn seed_corpus_passes_gate_with_rename_controls() {
    struct Case {
        score: f64,
        label: bool,
    }
    // Positives: behavior diffs a verdict must catch. Negatives: the
    // rename/whitespace controls that must NOT fire (low scores).
    let cases = vec![
        Case {
            score: 0.96,
            label: true,
        },
        Case {
            score: 0.91,
            label: true,
        },
        Case {
            score: 0.94,
            label: true,
        },
        Case {
            score: 0.30,
            label: false,
        },
        Case {
            score: 0.20,
            label: false,
        },
        Case {
            score: 0.10,
            label: false,
        },
    ];
    let report =
        common_core::calibration::calibrate_threshold(&cases, |c| c.score, |c| c.label, 0.9);
    assert!(
        report.passes_gate(),
        "staleness seed must pass: precision {} fpr {}",
        report.precision,
        report.fpr
    );
    // Eligibility itself never fires on cosmetics: the gate below is the
    // verdict's future bar, and cosmetics never reach it.
    assert!(!staleness_eligible(
        true,
        "fn foo(x: i32)",
        "fn foo( x : i32 )"
    ));
}
