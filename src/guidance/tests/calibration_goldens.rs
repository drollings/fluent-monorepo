//! Calibration corpora goldens: the checked-in triage and routing
//! sets load, their control slices agree with the static table, and
//! the static table alone cannot ace the full sets (parrot-proof:
//! a classifier that merely repeats the table scores below perfect,
//! so future model entries must earn their bars).

use std::path::PathBuf;

use crate::triage::{
    auto_apply_allowed, calibrate, load_gold_corpus, load_route_corpus, static_classify,
    static_route, TriageVerdict,
};

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("corpora")
        .join(name)
}

fn static_only(diagnostic: &str) -> TriageVerdict {
    static_classify(diagnostic).unwrap_or(TriageVerdict::NeedsHuman)
}

#[test]
fn triage_corpus_controls_agree_and_discriminate() {
    let gold = load_gold_corpus(&corpus("triage_calibration.json")).expect("loads");
    assert!(!gold.cases.is_empty(), "corpus is not empty");
    assert!(
        !gold.controls.is_empty() && gold.controls.len() * 2 <= gold.cases.len(),
        "control is a slice, not the bulk: {} of {}",
        gold.controls.len(),
        gold.cases.len()
    );
    // Control slice: the static table already decides every one.
    for diagnostic in &gold.controls {
        assert!(
            static_classify(diagnostic).is_some(),
            "control must be static-covered: {diagnostic}"
        );
    }
    // Full set: the static-only classifier computes a report but does
    // not ace it — the off-table cases discriminate the table.
    let report = calibrate(
        &gold
            .cases
            .iter()
            .map(|case| crate::triage::GoldCase {
                diagnostic: case.diagnostic.clone(),
                expected: case.expected,
            })
            .collect::<Vec<_>>(),
        static_only,
    );
    assert_eq!(report.total, gold.cases.len());
    assert!(!report.classes.is_empty());
    assert!(
        (report.accuracy - 1.0).abs() > f64::EPSILON,
        "a parroting table must not ace the corpus"
    );
    // The bar helper runs either way; the corpus pins its inputs here.
    let _ = auto_apply_allowed(&report);
}

#[test]
fn route_corpus_controls_agree_and_discriminate() {
    let cases = load_route_corpus(&corpus("route_calibration.json")).expect("loads");
    let controls: Vec<_> = cases.iter().filter(|case| case.control).collect();
    assert!(!controls.is_empty(), "control slice present");
    assert!(
        controls.len() * 2 <= cases.len(),
        "control is a slice, not the bulk"
    );
    for case in &controls {
        assert_eq!(
            static_route(&case.question).map(|plan| plan.primitive),
            case.expected,
            "control agrees: {}",
            case.question
        );
    }
    let misses = cases
        .iter()
        .filter(|case| !case.control)
        .filter(|case| static_route(&case.question).map(|plan| plan.primitive) != case.expected)
        .count();
    assert!(
        misses > 0,
        "off-table cases must discriminate the static router"
    );
}

#[test]
fn corrupt_corpus_labels_refuse() {
    let dir = fluent_wvr_testutil::tempdir();
    let bad = dir.path().join("bad.json");
    std::fs::write(
        &bad,
        r#"{"cases": [{"diagnostic": "x", "expected": "probably fine", "control": false}]}"#,
    )
    .expect("write");
    assert!(
        load_gold_corpus(&bad).is_err(),
        "open-vocabulary verdicts never load"
    );
    let bad_route = dir.path().join("bad_route.json");
    std::fs::write(
        &bad_route,
        r#"{"cases": [{"question": "q", "expected": "teleport", "control": false}]}"#,
    )
    .expect("write");
    assert!(
        load_route_corpus(&bad_route).is_err(),
        "open-vocabulary primitives never load"
    );
    let empty = dir.path().join("empty.json");
    std::fs::write(&empty, r#"{"cases": []}"#).expect("write");
    assert!(
        load_gold_corpus(&empty).is_err(),
        "empty corpora never load"
    );
}
