//! Native doc judge: choice labels, batch alignment, fail-closed
//! transport behavior. Included via `#[path]` forwarder from
//! `read/decide.rs`, per repo convention. Hermetic: scripted
//! `DecideBackend`, no endpoint.

use super::*;
use fluent_llm::decision::{Decision, DecisionError, ScriptedDecideBackend};

fn strong(label: &str) -> Decision {
    Decision::from_distribution(
        label,
        vec![("strong".to_string(), 0.9), ("considered".to_string(), 0.1)],
        "decision".to_string(),
        1,
        3,
        false,
    )
    .expect("decision")
}

#[test]
fn judge_maps_labels_and_confidence() {
    let backend = ScriptedDecideBackend::new(vec![Ok(strong("strong")), Ok(strong("considered"))]);
    let judge = DocJudge::new(&backend, "decision".to_string(), 2000);
    let verdicts = judge.judge(
        "how does sync work",
        &[
            (
                "doc/a.md#one".to_string(),
                "Sync converges mirrors.".to_string(),
            ),
            ("doc/b.md#two".to_string(), "Unrelated baking.".to_string()),
        ],
    );
    assert_eq!(verdicts.len(), 2, "{verdicts:?}");
    assert_eq!(verdicts[0].label, DocLabel::Strong);
    assert_eq!(verdicts[1].label, DocLabel::Considered);
    assert!(verdicts[0].confidence > 0.0);
    // One batched multi-question POST, not N round trips: sequential
    // per-question calls would serialize latency per candidate.
    assert_eq!(
        backend.requests().len(),
        2,
        "one request per candidate: {:?}",
        backend.requests().len()
    );
    for request in backend.requests() {
        assert_eq!(request.kind, fluent_llm::decision::DecisionKind::Choice);
        assert!(!request.state.is_empty(), "state carries query + excerpts");
        assert_eq!(request.options.len(), 2);
    }
}

#[test]
fn transport_failure_fails_closed_to_considered() {
    let backend = ScriptedDecideBackend::new(vec![Err(DecisionError::Unavailable(
        "transport down".to_string(),
    ))]);
    let judge = DocJudge::new(&backend, "decision".to_string(), 2000);
    let verdicts = judge.judge(
        "how does sync work",
        &[(
            "doc/a.md#one".to_string(),
            "Sync converges mirrors.".to_string(),
        )],
    );
    assert_eq!(verdicts.len(), 1);
    assert_eq!(verdicts[0].label, DocLabel::Considered);
    assert!(verdicts[0].confidence.abs() < f64::EPSILON);
}

#[test]
fn empty_candidates_make_no_calls() {
    let backend = ScriptedDecideBackend::new(vec![]);
    let judge = DocJudge::new(&backend, "decision".to_string(), 2000);
    assert!(judge.judge("q", &[]).is_empty());
    assert!(backend.requests().is_empty());
}

/// M4.1 characterization: closed-enum label parsing (C10) — case
/// folding, surrounding whitespace, unknown → fail-closed default.
/// Pinned before migrating `from_label` bodies to `match_label`.
#[test]
fn from_label_edge_table() {
    use super::{CandidateLabel, DocLabel, FindingLabel, SupportLabel};
    assert_eq!(DocLabel::from_label("STRONG"), DocLabel::Strong);
    assert_eq!(DocLabel::from_label(" strong "), DocLabel::Strong);
    assert_eq!(DocLabel::from_label("weak"), DocLabel::Considered);
    assert_eq!(DocLabel::from_label(""), DocLabel::Considered);
    assert_eq!(FindingLabel::from_label("RELEVANT"), FindingLabel::Relevant);
    assert_eq!(
        FindingLabel::from_label("  irrelevant\n"),
        FindingLabel::Irrelevant
    );
    assert_eq!(
        FindingLabel::from_label("maybe"),
        FindingLabel::Insufficient
    );
    assert_eq!(
        SupportLabel::from_label("SUPPORTED"),
        SupportLabel::Supported
    );
    assert_eq!(
        SupportLabel::from_label("contradicted"),
        SupportLabel::Contradicted
    );
    assert_eq!(SupportLabel::from_label("nope"), SupportLabel::Insufficient);
}
