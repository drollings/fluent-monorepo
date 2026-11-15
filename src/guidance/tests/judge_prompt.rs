//! Judge prompt goldens: the generic candidate judge preserves bytes.
use super::{CandidateJudge, DocLabel, FindingLabel};
use fluent_llm::decision::{Decision, DecisionKind, ScriptedDecideBackend};

fn verdict(label: &str) -> Decision {
    Decision::from_distribution(
        label,
        vec![(label.to_string(), 0.9)],
        "decision".to_string(),
        1,
        1,
        false,
    )
    .expect("decision")
}

#[test]
fn doc_prompt_bytes_preserved() {
    let backend = ScriptedDecideBackend::new(vec![Ok(verdict("strong"))]);
    let judge: CandidateJudge<DocLabel> =
        CandidateJudge::new(&backend, "decision".to_string(), 2000);
    let out = judge.judge(
        "how does sync work",
        &[("doc/a.md#one".to_string(), "Sync converges.".to_string())],
    );
    assert_eq!(out.len(), 1);
    let reqs = backend.requests();
    assert_eq!(reqs.len(), 1);
    let req = &reqs[0];
    assert_eq!(req.prompt, "Does candidate 1 answer the query?");
    assert_eq!(req.question, "Judge candidate 1 [doc/a.md#one]");
    assert_eq!(
        req.state,
        "Query: how does sync work\nCandidate 1 [doc/a.md#one]: Sync converges.\n"
    );
    assert_eq!(req.kind, DecisionKind::Choice);
    assert_eq!(req.version, 1);
    assert_eq!(req.options.len(), 2);
    assert_eq!(req.options[0].label, "strong");
    assert_eq!(
        req.options[0].description,
        "The excerpt directly answers the query."
    );
    assert_eq!(req.options[1].label, "considered");
    assert_eq!(
        req.options[1].description,
        "Related but not decisive, or insufficient."
    );
}

#[test]
fn finding_prompt_bytes_preserved() {
    let backend = ScriptedDecideBackend::new(vec![Ok(verdict("relevant"))]);
    let judge: CandidateJudge<FindingLabel> =
        CandidateJudge::new(&backend, "decision".to_string(), 2000);
    let out = judge.judge("q", &[("f1".to_string(), "excerpt".to_string())]);
    assert_eq!(out.len(), 1);
    let reqs = backend.requests();
    assert_eq!(reqs.len(), 1);
    let req = &reqs[0];
    assert_eq!(req.prompt, "Is finding 1 relevant to the query?");
    assert_eq!(req.question, "Judge finding 1 [f1]");
    assert_eq!(req.state, "Query: q\nFinding 1 [f1]: excerpt\n");
    assert_eq!(req.kind, DecisionKind::Choice);
    assert_eq!(req.version, 1);
    assert_eq!(req.options.len(), 3);
    assert_eq!(req.options[0].label, "relevant");
    assert_eq!(
        req.options[0].description,
        "The finding directly answers the query."
    );
    assert_eq!(req.options[1].label, "irrelevant");
    assert_eq!(req.options[2].label, "insufficient");
}
