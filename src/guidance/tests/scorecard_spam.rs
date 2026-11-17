//! Spam and answer-floor tiers: per-task spam contracts plus
//! control abstention over the shipping path.
use super::*;

#[test]
fn spam_tier_reports_three_contracts() {
    let measured = common_core::runtime::block_on_shared(measure_explain_spam());
    let mut names: Vec<String> = measured.iter().map(|metric| metric.name.clone()).collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "explain_spam.citation_ok".to_string(),
            "explain_spam.fixture_free".to_string(),
            "explain_spam.span_ok".to_string(),
        ]
    );
    for metric in &measured {
        assert!(
            metric.higher_is_better,
            "{} should be higher-is-better",
            metric.name
        );
        assert!(
            (0.0..=1.0).contains(&metric.value),
            "{} share in range: {}",
            metric.name,
            metric.value
        );
    }
}

#[test]
fn answer_floor_tier_reports_control_abstention() {
    let measured = common_core::runtime::block_on_shared(measure_explain_answer_floor());
    assert_eq!(measured.len(), 1);
    assert_eq!(
        measured[0].name,
        "explain_answer_floor.control_abstain".to_string()
    );
    assert!(measured[0].higher_is_better);
}

#[test]
fn read_answers_carry_spam_counts() {
    let (staged, db) = index_read_fixture();
    let engine = crate::read::engine::ReadEngine::new();
    let answer = answer_read_question(&engine, &staged, &db, "callers of gen_scoped");
    assert!(answer.max_span <= crate::eval::SPAM_MAX_SPAN_LINES);
    assert_eq!(answer.fixture_count, 0);
    assert!(
        crate::eval::rendered_citation_count(&answer.rendered) <= crate::eval::SPAM_MAX_CITATIONS,
        "rendered list respects the payload ceiling"
    );
    let _ = staged;
}
