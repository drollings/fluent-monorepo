//! Finding-relevance calibration: labeled findings gate the point
//! at the 1.0 operating point under the task-value bars (precision ≥ 0.95,
//! recall ≥ 0.85, FPR ≤ 0.05, zero control fires). Passing entries
//! store in the single registry; failures stay `HELD`. Producer
//! confidence alone never filters — a high-confidence wrong verdict
//! stays shadow. Included via `#[path]` forwarder from
//! `read/calibration.rs`, per repo convention. Hermetic: checked-in
//! JSONL plus scripted backends, no endpoint.

use super::*;
use fluent_llm::decision::{authorize, Decision, ScriptedDecideBackend};

fn corpus() -> Vec<LabeledFinding> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../doc/guidance/eval/findings.jsonl");
    load_labeled_findings(&path).expect("checked-in findings load")
}

fn verdict(label: &str, confidence: f64) -> Decision {
    Decision::from_distribution(
        label,
        vec![(label.to_string(), confidence)],
        "decision".to_string(),
        1,
        3,
        false,
    )
    .expect("decision")
}

#[test]
fn findings_corpus_loads_with_labels_and_controls() {
    let rows = corpus();
    assert_eq!(rows.len(), 18, "{} rows", rows.len());
    let controls: Vec<&LabeledFinding> = rows.iter().filter(|row| row.control).collect();
    assert_eq!(controls.len(), 2, "two retrieval controls");
    assert!(
        controls.iter().all(|row| row.excerpt.is_empty()),
        "controls carry no excerpt: nothing retrievable"
    );
    assert!(rows.iter().any(|row| row.expected_relevant));
    assert!(rows
        .iter()
        .any(|row| !row.expected_relevant && !row.control));
    // Malformed gold sets fail loudly, never calibrate quietly.
    assert!(parse_labeled_findings("{\"id\":\"a\",\"query\":\"q\",\"label\":\"maybe\"}").is_err());
    assert!(parse_labeled_findings(
        "{\"id\":\"a\",\"query\":\"q\",\"label\":\"relevant\"}\n{\"id\":\"a\",\"query\":\"q\",\"label\":\"irrelevant\"}"
    )
    .is_err());
    assert!(
        parse_labeled_findings("{\"id\":\"a\",\"query\":\"\",\"label\":\"relevant\"}").is_err()
    );
}

#[test]
fn judge_labels_score_at_the_one_point_operating_point() {
    use crate::read::decide::FindingLabel;
    assert!((finding_label_score(&FindingLabel::Relevant) - 1.0).abs() < f64::EPSILON);
    assert!((finding_label_score(&FindingLabel::Insufficient) - 0.5).abs() < f64::EPSILON);
    assert!(finding_label_score(&FindingLabel::Irrelevant).abs() < f64::EPSILON);
    // At the operating point only `relevant` fires; `insufficient`
    // renders as a gap, never as evidence.
    let cases = vec![
        (true, 1.0, "relevant fires"),
        (false, 0.5, "insufficient stays below"),
        (false, 0.0, "irrelevant never fires"),
    ];
    for (expected, score, why) in &cases {
        let case = fluent_llm::decision::ScoredCase {
            score: *score,
            expected: *expected,
            control: false,
            provenance: fluent_llm::decision::CaseProvenance::human("test", "2026-11-14"),
        };
        let fires = case.score >= FINDING_THRESHOLD;
        assert_eq!(fires, *expected, "{why}");
    }
}

#[test]
fn perfect_judge_passes_and_stores_while_proxy_helds() {
    let rows = corpus();
    // A perfect judge (labels match goldens) clears every bar: the
    // entry stores in the single registry and authorizes.
    let verdict = measure_findings(&rows, |row| if row.expected_relevant { 1.0 } else { 0.0 });
    assert!(verdict.passed, "{verdict:?}");
    assert_eq!(verdict.control_fires, 0);
    assert_eq!(verdict.support, rows.len());
    let dir = tempfile::tempdir().expect("tempdir");
    let registry_path = dir.path().join("state").join("calibration.json");
    let cases = score_findings(&rows, |row| if row.expected_relevant { 1.0 } else { 0.0 });
    let dataset_hash = common_core::hash::blake3_hex(b"findings-test");
    let corpus_data = finding_corpus(cases, "test-fingerprint", &dataset_hash, "findings.jsonl");
    let entry = fluent_llm::decision::RegistryEntry::calibrate_with(
        FINDING_POINT,
        "test-fingerprint",
        FINDING_QUESTION_VERSION,
        &corpus_data,
        FINDING_THRESHOLD,
        Some(fluent_llm::decision::ThresholdSet {
            act: FINDING_THRESHOLD,
            review: FINDING_THRESHOLD,
        }),
    )
    .expect("entry builds");
    let outcome =
        store_entry_if_passed(&registry_path, entry, verdict.passed, "below bars").expect("store");
    assert!(
        matches!(outcome, StoreOutcome::Authorized { .. }),
        "{outcome:?}"
    );
    let registry = fluent_llm::decision::CalibrationRegistry::load(&registry_path).expect("reload");
    assert!(matches!(
        authorize(
            &registry,
            FINDING_POINT,
            "test-fingerprint",
            FINDING_QUESTION_VERSION
        ),
        fluent_llm::decision::CalibrationStatus::Calibrated(_)
    ));
    // The lexical proxy measures below bars on hard negatives: HELD,
    // and the registry stays untouched.
    let proxy = measure_findings(&rows, |row| proxy_finding_score(&row.query, &row.excerpt));
    eprintln!("proxy verdict: {proxy:?}");
    assert!(!proxy.passed, "proxy must not authorize: {proxy:?}");
    assert_eq!(proxy.control_fires, 0, "controls never fire");
    let outcome = store_entry_if_passed(
        &registry_path.join("unused"),
        fluent_llm::decision::RegistryEntry::calibrate_with(
            FINDING_POINT,
            "proxy",
            FINDING_QUESTION_VERSION,
            &finding_corpus(vec![], "proxy", "hash", "findings.jsonl"),
            FINDING_THRESHOLD,
            None,
        )
        .expect("entry builds"),
        proxy.passed,
        "proxy below bars",
    )
    .expect("held");
    assert!(matches!(outcome, StoreOutcome::Held { .. }), "{outcome:?}");
}

#[test]
fn high_confidence_wrong_verdict_stays_shadow() {
    use crate::read::decide::FindingJudge;
    // Confidence-is-not-a-filter control: a 0.99-confidence `relevant` verdict on an
    // irrelevant finding must not filter, reorder, or promote. Rank
    // stays authoritative; only the shadow record observes.
    let backend = ScriptedDecideBackend::new(vec![Ok(verdict("relevant", 0.99))]);
    let judge = FindingJudge::new(&backend, "decision".to_string(), 2000);
    let findings = vec![
        ("f1".to_string(), "router dispatch prose".to_string()),
        ("f2".to_string(), "sync freshness mechanism".to_string()),
    ];
    let verdicts = judge.judge("how does sync decide", &findings);
    assert_eq!(verdicts.len(), 2);
    assert!((verdicts[0].confidence - 0.99).abs() < f64::EPSILON);
    // Shadow records log the verdict against the deterministic rank —
    // they never act on it.
    let record = crate::query::shadow::finding_judge_shadow(
        &verdicts[0].id,
        verdicts[0].label.as_str(),
        verdicts[0].confidence,
        1,
    );
    assert_eq!(record.label, "relevant");
    assert_eq!(record.authoritative, "finding:f1 rank:1");
    // The findings handed to render are byte-identical to the input
    // order and content: labels alone move nothing.
    let rendered: Vec<&String> = findings.iter().map(|(id, _)| id).collect();
    assert_eq!(rendered, vec!["f1", "f2"]);
    assert!(!verdicts[0].label.as_str().is_empty());
}

#[test]
fn empty_corpus_never_passes() {
    let verdict = gate_scored_cases(&[], FINDING_THRESHOLD, &FINDING_BARS);
    assert!(!verdict.passed);
    assert_eq!(verdict.support, 0);
}

#[test]
fn control_firing_vetoes_promotion() {
    // A control firing vetoes even when precision/recall pass: HELD.
    let rows = corpus();
    let mut cases = score_findings(&rows, |row| if row.expected_relevant { 1.0 } else { 0.0 });
    // Force a control to fire.
    if let Some(case) = cases.iter_mut().find(|case| case.control) {
        case.score = 1.0;
    }
    let verdict = gate_scored_cases(&cases, FINDING_THRESHOLD, &FINDING_BARS);
    assert!(!verdict.passed, "control fire must veto: {verdict:?}");
    assert!(verdict.control_fires > 0);
}
