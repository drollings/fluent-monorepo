//! Backend substitution seam tests (tests-first).

use std::sync::Arc;

use fluent_llm::client::LlmClient;
use fluent_llm::decision::{
    DecideBackend, DecideRequest, Decision, DecisionError, Question, QuestionSpec,
    ScriptedDecideBackend,
};

fn spec() -> QuestionSpec {
    QuestionSpec::new()
        .id("q".to_string())
        .version(1)
        .kind("choice".to_string())
        .prompt_template("Pick.".to_string())
        .options_csv("A,B".to_string())
        .act_threshold(0.9)
        .review_threshold(0.7)
        .timeout_ms(5000)
        .cacheable(false)
        .build()
}

fn canned(label: &str) -> Decision {
    Decision::from_distribution(
        label,
        vec![(label.to_string(), 1.0)],
        "stub".to_string(),
        1,
        0,
        false,
    )
    .expect("canned")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_none_uses_logprobs_path() {
    let server = httpmock::MockServer::start();
    let mock = server.mock(|when, then| {
        when.method("POST").path("/chat/completions");
        then.status(200).json_body(serde_json::json!({
            "id": "c", "object": "chat.completion", "created": 0, "model": "m",
            "choices": [{"index": 0, "finish_reason": "stop",
                "message": {"role": "assistant", "content": "B"},
                "logprobs": {"content": [{"token": "B", "logprob": -0.02,
                    "top_logprobs": [{"token": "B", "logprob": -0.02},
                                     {"token": "A", "logprob": -4.0}]}]}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        }));
    });
    let client = LlmClient::new(&server.base_url(), "m");
    assert!(client.decide_backend.is_none());
    let s = spec();
    let q = Question {
        spec: &s,
        state: "st",
        question: "q?",
    };
    assert_eq!(client.decide_async(&q).await.expect("logprobs").label, "B");
    mock.assert_hits(1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scripted_backend_substituted_without_http() {
    let server = httpmock::MockServer::start();
    let mock = server.mock(|when, then| {
        when.method("POST").path("/chat/completions");
        then.status(500)
            .json_body(serde_json::json!({"error": "must not be hit"}));
    });
    let backend = Arc::new(ScriptedDecideBackend::canned(canned("SCRIPTED")));
    let mut client = LlmClient::new(&server.base_url(), "m");
    client.decide_backend = Some(backend.clone());
    let s = spec();
    let q = Question {
        spec: &s,
        state: "st",
        question: "which?",
    };
    let d = client.decide_async(&q).await.expect("scripted");
    assert_eq!(d.label, "SCRIPTED");
    mock.assert_hits(0);
    assert_eq!(backend.prompts().len(), 1);
    assert!(backend.prompts()[0].contains("which?"));
    // The request carries structured state plus labelled options.
    let seen = backend.requests();
    assert_eq!(seen[0].state, "st");
    assert_eq!(seen[0].options.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_default_fans_per_question_in_order() {
    let backend = Arc::new(ScriptedDecideBackend::new(vec![
        Ok(canned("ONE")),
        Ok(canned("TWO")),
        Err(DecisionError::Unavailable("tired".to_string())),
    ]));
    let requests: Vec<DecideRequest> = ["a", "b", "c"]
        .iter()
        .map(|q| DecideRequest {
            prompt: q.to_string(),
            state: String::new(),
            question: q.to_string(),
            options: Vec::new(),
            kind: fluent_llm::decision::DecisionKind::Choice,
            version: 1,
            timeout_ms: 1000,
            model: "stub".to_string(),
        })
        .collect();
    let out = backend.decide_batch(requests).await;
    assert_eq!(out.len(), 3);
    assert_eq!(out[0].as_ref().expect("one").label, "ONE");
    assert_eq!(out[1].as_ref().expect("two").label, "TWO");
    assert!(out[2].is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backend_sees_redacted_prompt() {
    use fluent_llm::decision::{anonymize_hook, DecisionTransport};
    let backend = Arc::new(ScriptedDecideBackend::canned(canned("X")));
    let mut client = LlmClient::new("http://127.0.0.1:9", "m");
    client.decide_backend = Some(backend.clone());
    client.decision_transport = DecisionTransport::local_only().with_redact(anonymize_hook());
    let s = spec();
    let q = Question {
        spec: &s,
        state: "server 192.168.1.1 down",
        question: "A or B?",
    };
    client.decide_async(&q).await.expect("redacted");
    assert_eq!(backend.prompts().len(), 1);
    assert!(!backend.prompts()[0].contains("192.168.1.1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_refusal_precedes_backend() {
    let backend = Arc::new(ScriptedDecideBackend::canned(canned("X")));
    let mut client = LlmClient::new("https://llm.example.invalid", "m");
    client.decide_backend = Some(backend.clone());
    let s = spec();
    let q = Question {
        spec: &s,
        state: "st",
        question: "q?",
    };
    let err = client.decide_async(&q).await.expect_err("refused");
    assert!(matches!(err, DecisionError::Rejected(_)));
    assert!(backend.prompts().is_empty());
}
