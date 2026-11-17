//! M7.1 characterization for the flow primitives (`semantic_if`,
//! `match_arm`, `rerank_items`, `descend_tree`): fail-closed
//! defaults, empty/single options, threshold edges, error paths —
//! no call-site specifics. Hermetic: `ScriptedDecideBackend`, no
//! endpoint.

use std::sync::Arc;

use fluent_llm::client::{block_on, LlmClient};
use fluent_llm::decision::{
    descend_tree, match_arm, rerank_items, semantic_if, Decision, DecisionError, FlowAsk,
    RerankItem, ScriptedDecideBackend, TreeLevel,
};

fn choice(label: &str, confidence: f64) -> Decision {
    let (top, top_p, rest_p) = if confidence >= 0.5 {
        (label.to_string(), confidence, 1.0 - confidence)
    } else {
        ("other".to_string(), 1.0 - confidence, confidence)
    };
    let other = if top == label { "other" } else { label };
    Decision::from_distribution(
        label,
        vec![(top, top_p), (other.to_string(), rest_p)],
        "m".into(),
        1,
        5,
        false,
    )
    .expect("canned")
}

fn score(expected_top: &str, top_p: f64) -> Decision {
    let rungs = ["1", "2", "3", "4", "5"];
    let rest = (1.0 - top_p) / (rungs.len() - 1) as f64;
    Decision::from_distribution(
        expected_top,
        rungs
            .iter()
            .map(|r| (r.to_string(), if *r == expected_top { top_p } else { rest }))
            .collect(),
        "m".into(),
        1,
        5,
        false,
    )
    .expect("canned")
}

fn client(script: Vec<Result<Decision, DecisionError>>) -> LlmClient {
    let backend = Arc::new(ScriptedDecideBackend::new(script));
    let mut client = LlmClient::new("http://127.0.0.1:9", "stub");
    client.decide_backend = Some(backend);
    client
}

#[test]
fn semantic_if_acts_on_gated_yes() {
    let ask = FlowAsk::ask(0.5);
    assert!(block_on(semantic_if(
        &client(vec![Ok(choice("yes", 0.9))]),
        "st",
        "q?",
        &ask,
        false
    )));
    assert!(!block_on(semantic_if(
        &client(vec![Ok(choice("no", 0.9))]),
        "st",
        "q?",
        &ask,
        false
    )));
}

#[test]
fn semantic_if_below_gate_is_not_yes() {
    let ask = FlowAsk::ask(0.5);
    // Below-gate "yes": top-1 mass sits under the act bar, so the
    // label alone must not act. (Built directly: the `choice`
    // helper always makes its label top-1.)
    let weak_yes = Decision::from_distribution(
        "yes",
        vec![
            ("yes".to_string(), 0.3),
            ("no".to_string(), 0.35),
            ("other".to_string(), 0.35),
        ],
        "m".into(),
        1,
        0,
        false,
    )
    .expect("canned");
    assert!(!block_on(semantic_if(
        &client(vec![Ok(weak_yes)]),
        "st",
        "q?",
        &ask,
        false
    )));
}

#[test]
fn semantic_if_fails_closed_to_default() {
    let ask = FlowAsk::ask(0.5);
    let err = Err(DecisionError::Unavailable("down".to_string()));
    assert!(block_on(semantic_if(
        &client(vec![err]),
        "st",
        "q?",
        &ask,
        true
    )));
    let err = Err(DecisionError::Unavailable("down".to_string()));
    assert!(!block_on(semantic_if(
        &client(vec![err]),
        "st",
        "q?",
        &ask,
        false
    )));
}

#[test]
fn match_arm_returns_winner_index() {
    let ask = FlowAsk::ask(0.5);
    let arms = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let (index, decision) = block_on(match_arm(
        &client(vec![Ok(choice("b", 0.8))]),
        "st",
        "q?",
        &arms,
        &ask,
        0,
    ));
    assert_eq!(index, 1);
    assert!(decision.is_some());
}

#[test]
fn match_arm_fails_closed() {
    let ask = FlowAsk::ask(0.5);
    let arms = vec!["a".to_string(), "b".to_string()];
    let (index, decision) = block_on(match_arm(
        &client(vec![Ok(choice("zzz", 0.9))]),
        "st",
        "q?",
        &arms,
        &ask,
        1,
    ));
    assert_eq!(index, 1);
    assert!(decision.is_some());
    let err = Err(DecisionError::Unavailable("down".to_string()));
    let (index, decision) = block_on(match_arm(&client(vec![err]), "st", "q?", &arms, &ask, 1));
    assert_eq!(index, 1);
    assert!(decision.is_none());
    let (index, decision) = block_on(match_arm(
        &client(vec![]),
        "st",
        "q?",
        &["only".to_string()],
        &ask,
        0,
    ));
    assert_eq!(index, 0);
    assert!(decision.is_none());
}

#[test]
fn rerank_items_orders_by_score() {
    let ask = FlowAsk::ask(0.5);
    let items = vec![
        RerankItem {
            id: "low".to_string(),
            text: "cold".to_string(),
        },
        RerankItem {
            id: "high".to_string(),
            text: "hot".to_string(),
        },
    ];
    let order = block_on(rerank_items(
        &client(vec![Ok(score("2", 0.7)), Ok(score("5", 0.8))]),
        "st",
        &items,
        &ask,
    ));
    assert_eq!(order, vec![1, 0]);
    assert!(block_on(rerank_items(&client(vec![]), "st", &[], &ask)).is_empty());
    let err = Err(DecisionError::Unavailable("down".to_string()));
    let order = block_on(rerank_items(&client(vec![err]), "st", &items, &ask));
    assert_eq!(order, vec![0, 1]);
}

#[test]
fn descend_tree_settles_at_first_clearing_gate() {
    let ask = FlowAsk::ask(0.5);
    let levels = vec![
        TreeLevel {
            state: "s0".to_string(),
            question: "q0?".to_string(),
            options: vec!["x".to_string(), "y".to_string()],
            act: 0.9,
        },
        TreeLevel {
            state: "s1".to_string(),
            question: "q1?".to_string(),
            options: vec!["p".to_string(), "q".to_string()],
            act: 0.5,
        },
    ];
    let outcome = block_on(descend_tree(
        &client(vec![Ok(choice("x", 0.6)), Ok(choice("q", 0.8))]),
        &ask,
        &levels,
    ));
    assert!(outcome.settled);
    assert_eq!(
        outcome.path,
        vec![(0, "x".to_string()), (1, "q".to_string())]
    );
    assert_eq!(outcome.decision.map(|d| d.label), Some("q".to_string()));
}

#[test]
fn descend_tree_unsettled_when_no_gate_clears() {
    let ask = FlowAsk::ask(0.5);
    let levels = vec![TreeLevel {
        state: "s".to_string(),
        question: "q?".to_string(),
        options: vec!["x".to_string(), "y".to_string()],
        act: 0.99,
    }];
    let outcome = block_on(descend_tree(
        &client(vec![Ok(choice("x", 0.6))]),
        &ask,
        &levels,
    ));
    assert!(!outcome.settled);
    assert!(outcome.decision.is_none());
    assert_eq!(outcome.path, vec![(0, "x".to_string())]);
    assert!(block_on(descend_tree(&client(vec![]), &ask, &[]))
        .path
        .is_empty());
}
