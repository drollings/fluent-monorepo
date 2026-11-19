//! Finding-relevance judge: batch alignment, label mapping,
//! malformed/transport fail-closed, shadow-only no-filter no-reorder.
//! Included via `#[path]` forwarder from `read/decide.rs`, per repo
//! convention. Hermetic: scripted `DecideBackend`, no endpoint.

use super::{FindingJudge, FindingLabel, FindingVerdict};
use fluent_llm::decision::{Decision, DecisionError, ScriptedDecideBackend};

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
fn judge_maps_labels_and_aligns_batches() {
    let backend = ScriptedDecideBackend::new(vec![
        Ok(verdict("relevant", 0.9)),
        Ok(verdict("irrelevant", 0.8)),
        Ok(verdict("insufficient", 0.6)),
    ]);
    let judge = FindingJudge::new(&backend, "decision".to_string(), std::time::Duration::from_secs(2));
    let verdicts = judge.judge(
        "how does sync decide what to reprocess",
        &[
            ("f1".to_string(), "Sync converges the mirror.".to_string()),
            ("f2".to_string(), "Baking instructions.".to_string()),
            ("f3".to_string(), "Maybe related.".to_string()),
        ],
    );
    assert_eq!(
        verdicts.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
        vec!["f1", "f2", "f3"],
        "verdicts align with requests in order"
    );
    assert_eq!(
        verdicts.iter().map(|v| v.label).collect::<Vec<_>>(),
        vec![
            FindingLabel::Relevant,
            FindingLabel::Irrelevant,
            FindingLabel::Insufficient
        ]
    );
    // One batched multi-question POST worth of requests, not N round
    // trips: the backend records one request per finding in order.
    assert_eq!(backend.requests().len(), 3);
    for request in backend.requests() {
        assert_eq!(request.kind, fluent_llm::decision::DecisionKind::Choice);
        assert_eq!(request.options.len(), 3);
        assert!(!request.state.is_empty(), "state carries query + findings");
    }
}

#[test]
fn malformed_and_transport_outcomes_fail_closed() {
    let backend = ScriptedDecideBackend::new(vec![
        Ok(verdict("maybe", 0.5)),
        Err(DecisionError::Unavailable("transport down".to_string())),
    ]);
    let judge = FindingJudge::new(&backend, "decision".to_string(), std::time::Duration::from_secs(2));
    let verdicts = judge.judge(
        "q",
        &[
            ("f1".to_string(), "weird label".to_string()),
            ("f2".to_string(), "offline".to_string()),
        ],
    );
    assert_eq!(verdicts.len(), 2);
    assert!(verdicts
        .iter()
        .all(|v| v.label == FindingLabel::Insufficient));
    let transport: Vec<&FindingVerdict> = verdicts.iter().filter(|v| v.id == "f2").collect();
    assert!(transport[0].confidence.abs() < f64::EPSILON);
}

#[test]
fn shadow_records_log_without_filtering_or_reordering() {
    use crate::query::shadow::finding_judge_shadow;
    // Three findings in deterministic rank order; every verdict logs
    // exactly once, in order, however the labels read.
    let ordered = ["f1", "f2", "f3"];
    let labels = [
        FindingLabel::Irrelevant,
        FindingLabel::Relevant,
        FindingLabel::Insufficient,
    ];
    let records: Vec<_> = ordered
        .iter()
        .zip(labels.iter())
        .enumerate()
        .map(|(rank, (id, label))| finding_judge_shadow(id, label.as_str(), 0.8, rank + 1))
        .collect();
    assert_eq!(records.len(), 3, "shadow logs; it never filters");
    for (index, record) in records.iter().enumerate() {
        assert_eq!(record.point, "finding");
        assert_eq!(
            record.authoritative,
            format!("finding:{} rank:{}", ordered[index], index + 1)
        );
    }
    assert_eq!(records[0].label, "irrelevant");
    assert_eq!(records[1].label, "relevant");
    let verdicts: Vec<FindingVerdict> = ordered
        .iter()
        .zip(labels.iter())
        .map(|(id, label)| FindingVerdict {
            id: id.to_string(),
            label: *label,
            confidence: 0.8,
        })
        .collect();
    assert_eq!(
        verdicts.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
        ordered,
        "input order is the output order"
    );
}

#[test]
fn empty_findings_make_no_calls() {
    let backend = ScriptedDecideBackend::new(vec![]);
    let judge = FindingJudge::new(&backend, "decision".to_string(), std::time::Duration::from_secs(2));
    assert!(judge.judge("q", &[]).is_empty());
    assert!(backend.requests().is_empty());
}
