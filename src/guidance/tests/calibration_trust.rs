//! Trust gate for the excerpt budget and the answer floor: labelled
//! corpora measure both, entries store only on passing bars, and a
//! single control fire vetoes promotion.
use super::*;

fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn corpora() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpora")
}

fn span_cases() -> Vec<SpanCapCase> {
    load_span_cap_cases(&corpora().join("span_cap_cases.jsonl")).expect("span-cap corpus")
}

fn floor_cases() -> Vec<AnswerFloorCase> {
    load_answer_floor_cases(&corpora().join("answer_floor_cases.jsonl")).expect("floor corpus")
}

fn dataset(name: &str) -> String {
    let path = corpora().join(name);
    common_core::hash::blake3_hex(&std::fs::read(path).expect("corpus bytes"))
}

#[test]
fn span_corpus_covers_giants_and_legit_spans() {
    let cases = span_cases();
    assert!(cases.len() >= 10, "{} cases", cases.len());
    assert!(
        cases.iter().filter(|case| case.control).count() >= 4,
        "tail controls present"
    );
    assert!(
        cases.iter().filter(|case| case.expected_keep).count() >= 6,
        "anchored lines present"
    );
    assert!(
        cases
            .iter()
            .any(|case| !case.control && case.span_lines >= 400),
        "the measured giant span is covered"
    );
    for case in &cases {
        let (excerpt, keep) =
            span_case_excerpt(&workspace_root(), case).unwrap_or_else(|error| panic!("{error}"));
        assert!(
            excerpt.contains(&keep),
            "{}: excerpt names the labelled line",
            case.id
        );
        assert!(!keep.trim().is_empty(), "{}: keep line is text", case.id);
    }
}

#[test]
fn span_cap_gate_passes_at_the_shipped_budget() {
    let verdict = measure_span_cap(
        &span_cases(),
        &workspace_root(),
        crate::read::engine::RG_EXCERPT_CHAR_CAP,
    )
    .expect("span gate measures");
    assert!(
        verdict.passed,
        "bars held: precision={:.3} recall={:.3} fpr={:.3} control_fires={}",
        verdict.precision, verdict.recall, verdict.fpr, verdict.control_fires
    );
    assert_eq!(verdict.support, 11, "every labelled span measured");
    assert!(
        verdict.precision >= 0.99 && verdict.recall >= 0.99,
        "anchored lines survive and tails drop: precision={:.3} recall={:.3}",
        verdict.precision,
        verdict.recall
    );
    assert!(verdict.fpr <= 0.01, "fpr={:.3}", verdict.fpr);
    assert_eq!(verdict.control_fires, 0, "a tail line survived");
}

#[test]
fn answer_floor_corpus_covers_controls_and_nonsense() {
    let cases = floor_cases();
    let battery = crate::eval::load_eval_tasks(&corpora().join("benchmark_tasks.jsonl"))
        .expect("benchmark loads");
    let queries: Vec<String> = cases.iter().map(|case| case.query.clone()).collect();
    for control in battery.iter().filter(|task| task.control) {
        assert!(
            queries.contains(&control.query),
            "benchmark control `{}` is labelled",
            control.id
        );
    }
    assert!(
        cases.iter().filter(|case| case.control).count() >= 9,
        "controls present"
    );
    assert!(
        cases.iter().any(|case| case.query.is_empty()),
        "empty query control"
    );
    assert!(
        cases
            .iter()
            .any(|case| case.query.trim().is_empty() && !case.query.is_empty()),
        "whitespace query control"
    );
    assert!(
        cases.iter().any(|case| case.query.contains("fixture")),
        "fixture-named query control"
    );
    assert!(
        cases
            .iter()
            .any(|case| case.evidence.is_empty() && case.control),
        "scope-excluded evidence control"
    );
    assert!(
        cases.iter().filter(|case| case.expected_answer).count() >= 6,
        "grounded positives present"
    );
}

#[test]
fn answer_floor_gate_passes_at_the_shipped_coverage() {
    let verdict = measure_answer_floor(
        &floor_cases(),
        crate::read::vocabulary::ANSWER_FLOOR_COVERAGE,
    );
    assert!(
        verdict.passed,
        "bars held: precision={:.3} recall={:.3} fpr={:.3} control_fires={}",
        verdict.precision, verdict.recall, verdict.fpr, verdict.control_fires
    );
    assert_eq!(verdict.support, 16, "every labelled floor case measured");
    assert!(
        verdict.precision >= 0.99 && verdict.recall >= 0.99,
        "grounded answers and refusals separate: precision={:.3} recall={:.3}",
        verdict.precision,
        verdict.recall
    );
    assert!(verdict.fpr <= 0.01, "fpr={:.3}", verdict.fpr);
    assert_eq!(verdict.control_fires, 0, "a control answered");
    assert!(answer_floor_score("", &["anything".to_string()]) < f64::EPSILON);
    assert!(answer_floor_score("   ", &["anything".to_string()]) < f64::EPSILON);
}

#[test]
fn control_fire_vetoes_promotion() {
    let echo = AnswerFloorCase {
        id: "control-echo".to_string(),
        query: "What is the quantum flux capacitor turbulator?".to_string(),
        evidence: vec!["quantum flux capacitor turbulator".to_string()],
        expected_answer: false,
        control: true,
    };
    let verdict = measure_answer_floor(&[echo], crate::read::vocabulary::ANSWER_FLOOR_COVERAGE);
    assert_eq!(verdict.control_fires, 1, "the control fires");
    assert!(
        !verdict.passed,
        "a firing control vetoes promotion even with clean counts"
    );
}

#[test]
fn entries_store_only_on_pass() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry_path = dir.path().join("registry.json");
    let cap = crate::read::engine::RG_EXCERPT_CHAR_CAP;
    let span = span_cap_entry(
        &span_cases(),
        &workspace_root(),
        cap,
        &dataset("span_cap_cases.jsonl"),
    )
    .expect("span entry");
    let floor = answer_floor_entry(
        &floor_cases(),
        crate::read::vocabulary::ANSWER_FLOOR_COVERAGE,
        &dataset("answer_floor_cases.jsonl"),
    );
    assert!(span.gate.passed && floor.gate.passed, "both gates pass");

    let span_passed = span.gate.passed;
    let stored =
        store_entry_if_passed(&registry_path, span, span_passed, "span gate").expect("span stores");
    assert!(matches!(stored, StoreOutcome::Authorized { .. }));
    let floor_passed = floor.gate.passed;
    let stored = store_entry_if_passed(&registry_path, floor, floor_passed, "floor gate")
        .expect("floor stores");
    assert!(matches!(stored, StoreOutcome::Authorized { .. }));

    let registry =
        fluent_llm::decision::CalibrationRegistry::load(&registry_path).expect("registry loads");
    assert!(
        registry
            .lookup_pinned(SPAN_CAP_POINT, "unprobed", 1, &dataset("span_cap_cases.jsonl"))
            .is_some()
    );
    let entry = registry
        .lookup_pinned(ANSWER_FLOOR_POINT, "unprobed", 1, &dataset("answer_floor_cases.jsonl"))
        .expect("floor entry");
    assert!(
        (entry.gate.threshold - crate::read::vocabulary::ANSWER_FLOOR_COVERAGE).abs()
            <= f64::EPSILON
    );
    let entries = registry.entries.len();

    let held = AnswerFloorCase {
        id: "held-control".to_string(),
        query: "What is the quantum flux capacitor turbulator?".to_string(),
        evidence: vec!["quantum flux capacitor turbulator".to_string()],
        expected_answer: false,
        control: true,
    };
    let failing = answer_floor_entry(
        &[held],
        crate::read::vocabulary::ANSWER_FLOOR_COVERAGE,
        "held-corpus",
    );
    assert!(!failing.gate.passed);
    let failing_passed = failing.gate.passed;
    let outcome = store_entry_if_passed(&registry_path, failing, failing_passed, "control fired")
        .expect("hold is not an error");
    assert!(matches!(outcome, StoreOutcome::Held { .. }));
    let registry =
        fluent_llm::decision::CalibrationRegistry::load(&registry_path).expect("registry loads");
    assert_eq!(registry.entries.len(), entries, "held entries never store");
}
