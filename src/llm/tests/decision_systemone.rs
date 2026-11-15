//! Native `/v1/systemone` transport tests.

use crate::decision::{
    batch_body, decision_from_answer, DecideBackend, DecideRequest, DecisionError, DecisionKind,
    DecisionOption, SystemOneBackend, SystemOneSupport,
};

fn choice_request(label_a: &str, label_b: &str) -> DecideRequest {
    DecideRequest {
        prompt: "Pick one.".to_string(),
        state: "query context".to_string(),
        question: "Which label?".to_string(),
        options: vec![
            DecisionOption {
                label: label_a.to_string(),
                description: "first".to_string(),
            },
            DecisionOption {
                label: label_b.to_string(),
                description: "second".to_string(),
            },
        ],
        kind: DecisionKind::Choice,
        version: 3,
        timeout_ms: 2000,
        model: "decision".to_string(),
    }
}

#[test]
fn endpoint_accepts_host_and_v1_bases() {
    assert_eq!(
        SystemOneBackend::endpoint("http://localhost:8080"),
        "http://localhost:8080/v1/systemone"
    );
    assert_eq!(
        SystemOneBackend::endpoint("http://localhost:8080/v1/"),
        "http://localhost:8080/v1/systemone"
    );
}

#[test]
fn choice_answer_maps_distribution() {
    let request = choice_request("Yes", "No");
    let answer = serde_json::json!({
        "type": "choice",
        "choice": "No",
        "probabilities": {"Yes": 0.1, "No": 0.9},
        "confidence": 0.9
    });
    let decision = decision_from_answer(&answer, &request, 7).expect("decision");
    assert_eq!(decision.label, "No");
    assert!((decision.confidence - 0.9).abs() < 1e-9);
    assert_eq!(decision.question_version, 3);
    assert_eq!(decision.latency_ms, 7);
}

#[test]
fn score_answer_maps_legend() {
    let mut request = choice_request("0", "100");
    request.kind = DecisionKind::Score;
    let answer = serde_json::json!({
        "type": "score",
        "score": 100.0,
        "legend": {
            "0": {"score": 0, "description": "irrelevant"},
            "1": {"score": 100, "description": "relevant"}
        },
        "probabilities": {"0": 0.2, "1": 0.8},
        "confidence": 0.8
    });
    let decision = decision_from_answer(&answer, &request, 1).expect("score");
    assert_eq!(decision.label, "100");
    assert!(decision
        .distribution
        .iter()
        .any(|(label, p)| label == "100" && *p > 0.7));
}

#[test]
fn noul_answer_maps_yes_no() {
    let mut request = choice_request("Yes", "No");
    request.kind = DecisionKind::Noul;
    let answer = serde_json::json!({"type": "noul", "noul": 0.8});
    let decision = decision_from_answer(&answer, &request, 1).expect("noul");
    assert_eq!(decision.label, "No");
    assert!((decision.confidence - 0.8).abs() < 1e-9);
}

#[test]
fn malformed_answer_is_decode_error() {
    let request = choice_request("Yes", "No");
    let answer = serde_json::json!({"type": "choice", "choice": "No"});
    assert!(matches!(
        decision_from_answer(&answer, &request, 0),
        Err(DecisionError::Decode(_))
    ));
}

#[test]
fn batch_body_names_questions_in_order() {
    let body = batch_body(
        "state",
        &[choice_request("Yes", "No"), choice_request("A", "B")],
    );
    assert_eq!(body["model"], "decision");
    assert!(body["questions"]["q1"].is_object());
    assert!(body["questions"]["q2"].is_object());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_choice_and_batch_align() {
    let server = httpmock::MockServer::start();
    let mock = server.mock(|when, then| {
        when.method("POST").path("/v1/systemone");
        then.status(200).json_body(serde_json::json!({
            "model": "decision",
            "answers": {
                "q1": {"type": "choice", "choice": "Yes",
                       "probabilities": {"Yes": 0.8, "No": 0.2}, "confidence": 0.8},
                "q2": {"type": "choice", "choice": "No",
                       "probabilities": {"Yes": 0.1, "No": 0.9}, "confidence": 0.9}
            },
            "usage": {"input_tokens": 12, "output_tokens": 0}
        }));
    });
    let backend = SystemOneBackend::new(server.base_url(), Default::default());
    let outcomes = backend
        .decide_batch(vec![
            choice_request("Yes", "No"),
            choice_request("Yes", "No"),
        ])
        .await;
    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[0].as_ref().expect("q1").label, "Yes");
    assert_eq!(outcomes[1].as_ref().expect("q2").label, "No");
    mock.assert_hits(1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_501_is_rejected_not_retried() {
    let server = httpmock::MockServer::start();
    let mock = server.mock(|when, then| {
        when.method("POST").path("/v1/systemone");
        then.status(501).json_body(serde_json::json!({
            "error": {"code": 501, "message": "decision model without text generation"}
        }));
    });
    let backend = SystemOneBackend::new(server.base_url(), Default::default());
    let error = backend
        .decide(choice_request("Yes", "No"))
        .await
        .expect_err("501");
    assert!(matches!(error, DecisionError::Rejected(_)));
    mock.assert_hits(1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_maps_501_to_unsupported() {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/v1/systemone");
        then.status(501).json_body(serde_json::json!({
            "error": {"code": 501, "message": "decision model without text generation"}
        }));
    });
    let backend = SystemOneBackend::new(server.base_url(), Default::default());
    assert!(matches!(
        backend.probe("decision").await,
        SystemOneSupport::Unsupported { .. }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_maps_success_to_supported() {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/v1/systemone");
        then.status(200).json_body(serde_json::json!({
            "model": "decision",
            "answers": {"q1": {"type": "noul", "noul": 0.2}},
            "usage": {"input_tokens": 3, "output_tokens": 0}
        }));
    });
    let backend = SystemOneBackend::new(server.base_url(), Default::default());
    assert_eq!(backend.probe("decision").await, SystemOneSupport::Supported);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_maps_refused_port_to_unreachable() {
    let backend = SystemOneBackend::new("http://127.0.0.1:9", Default::default());
    assert!(matches!(
        backend.probe("decision").await,
        SystemOneSupport::Unreachable { .. }
    ));
}
