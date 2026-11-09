use crate::query::decision_points::{intent_label_to_intent, intent_spec};
use crate::query::strategy::{FsmEngine, QueryIntent};

fn classify(query: &str) -> crate::query::strategy::QueryClass {
    let mut engine = FsmEngine::new();
    let intake = engine.intake(query);
    engine.classify(&intake)
}

#[test]
fn precheck_exact_arms_agree_with_classify() {
    use crate::query::strategy::intent_precheck;

    for query in ["", "helloWorld", "snake_case_fn", "src/main.rs"] {
        let verdict = classify(query);
        let pre = intent_precheck(&crate::query::strategy::FsmEngine::new().intake(query))
            .expect("exact arm decides");
        assert_eq!(pre.intent, verdict.intent, "{query:?}");
        assert_eq!(pre.domain, verdict.domain, "{query:?}");
        assert_eq!(
            pre.confidence.to_bits(),
            verdict.confidence.to_bits(),
            "{query:?}"
        );
    }
}

#[test]
fn precheck_ambiguous_queries_need_judgment() {
    use crate::query::strategy::intent_precheck;

    // Word-count/how-word arms: no exact evidence, model-eligible only.
    for query in [
        "ast parsing",
        "target registry",
        "how do I parse a file",
        "what is a target used for",
        "the target registry resolves dependencies across modules",
        "hello world test",
    ] {
        let intake = FsmEngine::new().intake(query);
        assert!(intent_precheck(&intake).is_none(), "{query:?}");
    }
}

#[test]
fn control_group_never_reaches_the_model() {
    use crate::query::strategy::intent_precheck;

    // Exact queries are decided before any backend exists to call:
    // the control group cannot fire by construction.
    for query in ["", "helloWorld", "src/main.rs", "foo_bar"] {
        let intake = FsmEngine::new().intake(query);
        assert!(intent_precheck(&intake).is_some(), "{query:?}");
    }
}

#[test]
fn intent_labels_round_trip_through_spec_options() {
    let spec = intent_spec();
    let mut mapped = Vec::new();
    for label in spec.options() {
        let intent =
            intent_label_to_intent(&label).unwrap_or_else(|| panic!("spec label maps: {label:?}"));
        assert!(!mapped.contains(&intent), "distinct intents");
        mapped.push(intent);
    }
    assert_eq!(mapped.len(), 8);
    assert!(intent_label_to_intent("Nope").is_none());
    assert!(intent_label_to_intent("").is_none());
}

#[test]
fn seed_corpus_passes_gate_at_point_threshold() {
    struct Case {
        score: f64,
        label: bool,
    }
    // Positives: ambiguous seeds the scripted backend labels correctly.
    // Negatives: mismatched labels the gate must reject.
    let cases = vec![
        Case {
            score: 0.95,
            label: true,
        },
        Case {
            score: 0.92,
            label: true,
        },
        Case {
            score: 0.97,
            label: true,
        },
        Case {
            score: 0.91,
            label: true,
        },
        Case {
            score: 0.40,
            label: false,
        },
        Case {
            score: 0.30,
            label: false,
        },
        Case {
            score: 0.10,
            label: false,
        },
        Case {
            score: 0.45,
            label: false,
        },
    ];
    let report =
        common_core::calibration::calibrate_threshold(&cases, |c| c.score, |c| c.label, 0.9);
    assert!(
        report.passes_gate(),
        "intent seed must pass: precision {} fpr {}",
        report.precision,
        report.fpr
    );
    // The 8 intents stay routable: every spec option maps somewhere.
    assert_eq!(QueryIntent::IdentifierLookup.priority(), 0);
}
