use super::{classify_finding, triage_spec, TriageClass, TRIAGE_POINT, TRIAGE_VERSION};
use fluent_llm::decision::Decision;

// Triage names the mechanical class or abstains. The mapping is closed:
// only allowlist labels at Act confidence auto-apply; everything else —
// unknown labels, low confidence, no decision — is Manual. A confident
// but wrong model still cannot fire: confidence never stands in for the
// gate's verdict.

fn decision(label: &str, top: f64) -> Decision {
    Decision::from_distribution(
        label,
        vec![(label.to_string(), top), ("manual".to_string(), 1.0 - top)],
        "test".to_string(),
        TRIAGE_VERSION,
        0,
        false,
    )
    .expect("decision")
}

#[test]
fn triage_spec_pins_version_and_options() {
    let spec = triage_spec();
    assert!(spec.validate().is_ok());
    assert_eq!(spec.id, TRIAGE_POINT);
    assert_eq!(spec.version, TRIAGE_VERSION);
    assert_eq!(spec.kind, "choice");
    assert!(!spec.cacheable);
}

#[test]
fn allowlisted_act_verdicts_auto_apply() {
    let thresholds = triage_spec().thresholds();
    assert_eq!(
        classify_finding("rustfmt", Some(&decision("rustfmt", 0.97)), &thresholds),
        TriageClass::Formatting
    );
    assert_eq!(
        classify_finding("clippy", Some(&decision("clippy-fix", 0.97)), &thresholds),
        TriageClass::ClippyFix
    );
}

#[test]
fn abstains_without_decision_or_confidence_or_listing_or_tool_match() {
    let thresholds = triage_spec().thresholds();
    // No decision: manual.
    assert_eq!(
        classify_finding("rustfmt", None, &thresholds),
        TriageClass::Manual
    );
    // Allowlisted label below Act: manual (would not have acted).
    assert_eq!(
        classify_finding("rustfmt", Some(&decision("rustfmt", 0.6)), &thresholds),
        TriageClass::Manual
    );
    // Confident but not allowlisted: manual — confidence never fires alone.
    assert_eq!(
        classify_finding(
            "clippy",
            Some(&decision("rewrite-logic", 0.99)),
            &thresholds
        ),
        TriageClass::Manual
    );
    // Confident and allowlisted but the finding came from another tool:
    // manual — the producing tool must agree with the class.
    assert_eq!(
        classify_finding("test", Some(&decision("clippy-fix", 0.97)), &thresholds),
        TriageClass::Manual
    );
    assert_eq!(
        classify_finding("clippy", Some(&decision("rustfmt", 0.97)), &thresholds),
        TriageClass::Manual
    );
}

#[test]
fn control_corpus_never_auto_applies() {
    // Every checked-in control finding — non-applicable lint,
    // behavior-changing patch, unknown-cause failure — must classify
    // Manual under its adversarial scripted decision. Any fire vetoes.
    let path = format!("{}/corpora/triage_seed.json", env!("CARGO_MANIFEST_DIR"));
    let corpus: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("corpus reads"))
            .expect("corpus parses");
    assert_eq!(
        corpus["provenance"]["origin"], "human",
        "ground truth is never model output"
    );
    let thresholds = triage_spec().thresholds();
    let findings = corpus["findings"].as_array().expect("findings");
    assert!(!findings.is_empty());
    let mut controls = 0;
    for finding in findings {
        if !finding["control"].as_bool().unwrap_or(false) {
            continue;
        }
        controls += 1;
        let id = finding["id"].as_str().expect("id");
        let scripted = Decision::from_distribution(
            finding["decision_label"].as_str().expect("label"),
            vec![
                (
                    finding["decision_label"]
                        .as_str()
                        .expect("label")
                        .to_string(),
                    finding["confidence"].as_f64().expect("confidence"),
                ),
                ("manual".to_string(), 0.0),
            ],
            "test".to_string(),
            TRIAGE_VERSION,
            0,
            false,
        )
        .expect("decision");
        assert_eq!(
            classify_finding(
                finding["tool"].as_str().expect("tool"),
                Some(&scripted),
                &thresholds
            ),
            TriageClass::Manual,
            "control must not auto-apply: {id}"
        );
    }
    assert!(controls >= 3, "corpus must carry controls");
}
