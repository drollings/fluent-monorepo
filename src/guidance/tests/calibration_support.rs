//! Claim-support calibration: hand-labelled claims gate the point
//! at the 1.0 operating point under the task-value bars (precision ≥ 0.95,
//! recall ≥ 0.90, FPR ≤ 0.05, zero control fires). Contradicted
//! claims drop (including the q3 router-decide contradiction);
//! all-dropped answers abstain to evidence-only. Before promotion
//! every sentence stays `[unverified]` and nothing filters.
//! Included via `#[path]` forwarder from `read/calibration.rs`, per
//! repo convention. Hermetic: checked-in JSONL plus scripted scores,
//! no endpoint.

use super::*;
use crate::read::claims::{bind_claims, gate_answer, Claim, Support};

fn corpus() -> Vec<ClaimCase> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../doc/guidance/eval/claims.jsonl");
    load_claim_cases(&path).expect("checked-in claims load")
}

fn verdict_score(support: Support) -> f64 {
    support_verdict_score(&support)
}

#[test]
fn claims_corpus_loads_with_contradictions_and_controls() {
    let cases = corpus();
    assert_eq!(cases.len(), 34, "{} rows", cases.len());
    assert!(cases
        .iter()
        .any(|case| case.expected == ExpectedSupport::Supported));
    assert!(cases
        .iter()
        .any(|case| case.expected == ExpectedSupport::Contradicted));
    assert!(cases
        .iter()
        .any(|case| case.expected == ExpectedSupport::Insufficient));
    // Citation-present-but-unsupported controls: the Q2 SKILL.md
    // consolidation sentence names a real path yet states no excerpt.
    assert!(
        cases
            .iter()
            .any(|case| case.id == "claim-q2-consolidation-contradicted"
                && case.expected == ExpectedSupport::Contradicted),
        "Q2 consolidation control must be measured"
    );
    let controls: Vec<&ClaimCase> = cases.iter().filter(|case| case.control).collect();
    assert_eq!(controls.len(), 6, "six nonsense controls");
    assert!(
        controls
            .iter()
            .all(|case| case.expected != ExpectedSupport::Supported),
        "controls must never verify"
    );
    // The q3 contradiction is present: the decide model never ranks
    // hits, and confidence never gates.
    assert!(
        cases
            .iter()
            .any(|case| case.id == "claim-router-decide-contradicted"
                && case.expected == ExpectedSupport::Contradicted),
        "q3 router-decide contradiction must be measured"
    );
    // Malformed gold sets fail loudly.
    assert!(parse_claim_cases(
        "{\"id\":\"a\",\"sentence\":\"x (s:1).\",\"citation\":\"s:1\",\"expected\":\"maybe\"}"
    )
    .is_err());
    assert!(parse_claim_cases(
        "{\"id\":\"a\",\"sentence\":\"x.\",\"citation\":\"\",\"expected\":\"supported\"}"
    )
    .is_err());
    assert!(parse_claim_cases(
        "{\"id\":\"a\",\"sentence\":\"x (s:1).\",\"citation\":\"t:2\",\"expected\":\"supported\"}"
    )
    .is_err());
}

#[test]
fn binding_is_measured_deterministically_without_a_gate() {
    let cases = corpus();
    // Binding is structural and model-free: every non-empty citation
    // is named by its sentence (the loader enforces it), empty
    // citations bind nothing. Contradicted rows bind exactly like
    // supported ones — binding is never the verdict. This reports a
    // rate; it never authorizes one.
    assert!((binding_accuracy(&cases) - 1.0).abs() < f64::EPSILON);
    assert!((binding_accuracy(&[]) - 1.0).abs() < f64::EPSILON);
    let bound: Vec<String> = cases
        .iter()
        .filter(|case| case.expected == ExpectedSupport::Supported)
        .map(|case| case.citation.clone())
        .collect();
    let manifest: Vec<(String, String)> = bound
        .iter()
        .enumerate()
        .map(|(index, citation)| (format!("[C{}]", index + 1), citation.clone()))
        .collect();
    let claims = bind_claims(
        &cases
            .iter()
            .filter(|case| case.expected == ExpectedSupport::Supported)
            .map(|case| case.sentence.clone())
            .collect::<Vec<_>>()
            .join(" "),
        &manifest,
    );
    assert!(claims.iter().all(|claim| claim.citation.is_some()));
}

#[test]
fn support_gate_drops_contradictions_and_abstains() {
    // Perfect verdicts (match hand labels) clear every bar and store.
    let cases = corpus();
    let verdict = measure_support(&cases, |case| match case.expected {
        ExpectedSupport::Supported => verdict_score(Support::Verified),
        ExpectedSupport::Contradicted => verdict_score(Support::Contradicted),
        ExpectedSupport::Insufficient => verdict_score(Support::Insufficient),
    });
    assert!(verdict.passed, "{verdict:?}");
    assert_eq!(verdict.control_fires, 0);
    let dir = tempfile::tempdir().expect("tempdir");
    let registry_path = dir.path().join("calibration.json");
    let scored = score_claims(&cases, |case| match case.expected {
        ExpectedSupport::Supported => 1.0,
        _ => 0.0,
    });
    let entry = fluent_llm::decision::RegistryEntry::calibrate_with(
        SUPPORT_POINT,
        "test-fingerprint",
        SUPPORT_QUESTION_VERSION,
        &support_corpus(scored, "test-fingerprint", "hash", "claims.jsonl"),
        SUPPORT_THRESHOLD,
        Some(fluent_llm::decision::ThresholdSet {
            act: SUPPORT_THRESHOLD,
            review: SUPPORT_THRESHOLD,
        }),
    )
    .expect("entry builds");
    let outcome =
        store_entry_if_passed(&registry_path, entry, verdict.passed, "below bars").expect("store");
    assert!(
        matches!(outcome, StoreOutcome::Authorized { .. }),
        "{outcome:?}"
    );
    // A weak scorer that verifies the q3 contradiction fails the
    // precision bar: HELD, registry untouched.
    let weak = measure_support(&cases, |case| {
        if case.id == "claim-router-decide-contradicted" {
            verdict_score(Support::Verified)
        } else {
            match case.expected {
                ExpectedSupport::Supported => verdict_score(Support::Verified),
                _ => verdict_score(Support::Insufficient),
            }
        }
    });
    assert!(
        !weak.passed,
        "verifying a contradiction must hold: {weak:?}"
    );
    assert!(weak.precision < SUPPORT_BARS.min_precision);
    // Claim-level gating: contradicted and insufficient drop, the
    // answer abstains to evidence-only when nothing is supported.
    let answer = "The decide model ranks code hits by confidence (src/bin/guidance/src/search.rs:1-5). Freshness uses mtime alone.";
    let manifest = vec![(
        "[C1]".to_string(),
        "src/bin/guidance/src/search.rs:1-5".to_string(),
    )];
    let bound = bind_claims(answer, &manifest);
    assert_eq!(bound.len(), 2);
    let mut contradicted = bound;
    contradicted[0].support = Support::Contradicted;
    contradicted[1].support = Support::Insufficient;
    assert!(
        gate_answer(&contradicted).is_none(),
        "all dropped means evidence-only"
    );
}

#[test]
fn nothing_filters_before_promotion() {
    // Pre-calibration every bound sentence stays `[unverified]`:
    // binding never invents `Verified`, and gating keeps every bound
    // unverified sentence with its label. Unbound sentences drop —
    // uncited prose never renders. The evidence sections stay
    // authoritative until the support point passes.
    let cases = corpus();
    let sentences: Vec<String> = cases.iter().map(|case| case.sentence.clone()).collect();
    let citations: Vec<String> = cases
        .iter()
        .filter(|case| !case.citation.is_empty())
        .map(|case| case.citation.clone())
        .collect();
    let manifest: Vec<(String, String)> = citations
        .iter()
        .enumerate()
        .map(|(index, citation)| (format!("[C{}]", index + 1), citation.clone()))
        .collect();
    let claims = bind_claims(&sentences.join(" "), &manifest);
    assert_eq!(claims.len(), sentences.len());
    assert!(
        claims
            .iter()
            .all(|claim: &Claim| claim.support == Support::Unverified),
        "binding never verifies"
    );
    let bound_count = claims
        .iter()
        .filter(|claim| claim.citation.is_some())
        .count();
    assert!(bound_count > 0, "supported rows bind");
    assert!(
        bound_count < claims.len(),
        "insufficient rows bind nothing: {claims:?}"
    );
    let gated = gate_answer(&claims).expect("bound unverified sentences are kept");
    assert_eq!(
        gated.matches("[unverified]").count(),
        bound_count,
        "{gated}"
    );
}

#[test]
fn support_judge_labels_entailment_per_claim() {
    use crate::read::decide::{CandidateVerdict, SupportJudge, SupportLabel};
    use fluent_llm::decision::{Decision, DecisionError, ScriptedDecideBackend};
    fn decide(label: &str) -> Result<Decision, DecisionError> {
        Decision::from_distribution(
            label,
            vec![(label.to_string(), 0.9)],
            "d".to_string(),
            1,
            0,
            false,
        )
        .map_err(|_| DecisionError::Unavailable("bad".to_string()))
    }
    let backend = ScriptedDecideBackend::new(vec![
        decide("supported"),
        decide("contradicted"),
        decide("insufficient"),
        Err(DecisionError::Unavailable("down".to_string())),
    ]);
    let judge = SupportJudge::new(&backend, "d".to_string(), std::time::Duration::from_secs(2));
    let verdicts: Vec<CandidateVerdict<SupportLabel>> = judge.judge(
        "Which crate owns calibration?",
        &[
            (
                "claim-0".to_string(),
                "Calibration lives in common-core | common-core owns calibration".to_string(),
            ),
            (
                "claim-1".to_string(),
                "The decide model ranks code hits | the decide model transports bytes".to_string(),
            ),
            (
                "claim-2".to_string(),
                "Freshness uses mtime alone | nothing shown".to_string(),
            ),
            (
                "claim-3".to_string(),
                "Unjudged claim | transport down".to_string(),
            ),
        ],
    );
    assert_eq!(verdicts.len(), 4);
    assert_eq!(verdicts[0].label, SupportLabel::Supported);
    assert_eq!(verdicts[1].label, SupportLabel::Contradicted);
    assert_eq!(verdicts[2].label, SupportLabel::Insufficient);
    // Transport failure fails closed to Insufficient, never to a verdict.
    assert_eq!(verdicts[3].label, SupportLabel::Insufficient);
    assert!(verdicts[3].confidence.abs() < f64::EPSILON);
}

#[test]
fn scripted_support_verdicts_clear_and_hold_the_bars() {
    use crate::read::calibration::{measure_support, support_verdict_score, SUPPORT_BARS};
    let cases = corpus();
    // Perfect verdicts (match hand labels) clear every bar and store.
    let verdict = measure_support(&cases, |case| match case.expected {
        ExpectedSupport::Supported => support_verdict_score(&Support::Verified),
        ExpectedSupport::Contradicted => support_verdict_score(&Support::Contradicted),
        ExpectedSupport::Insufficient => support_verdict_score(&Support::Insufficient),
    });
    assert!(verdict.passed, "{verdict:?}");
    assert!(verdict.precision >= SUPPORT_BARS.min_precision);
    assert!(verdict.recall >= SUPPORT_BARS.min_recall);
    assert_eq!(verdict.control_fires, 0, "controls never verify");
    // A weak scorer that verifies a contradiction fails the precision bar.
    let weak = measure_support(&cases, |case| {
        if case.id == "claim-q2-consolidation-contradicted" {
            support_verdict_score(&Support::Verified)
        } else {
            match case.expected {
                ExpectedSupport::Supported => support_verdict_score(&Support::Verified),
                _ => support_verdict_score(&Support::Insufficient),
            }
        }
    });
    assert!(
        !weak.passed,
        "verifying a contradiction must hold: {weak:?}"
    );
}
