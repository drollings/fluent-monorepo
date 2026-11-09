//! Async decide seam tests over `httpmock` (tests-first).

use fluent_llm::client::LlmClient;
use fluent_llm::decision::{Question, QuestionSpec, ThresholdSet, Verdict};
use fluent_wvr::{WorkContext, WorkUnit};

fn spec() -> QuestionSpec {
    QuestionSpec::new()
        .id("intent".to_string())
        .version(1)
        .kind("choice".to_string())
        .prompt_template("Pick the intent.".to_string())
        .options_csv("A,B,C".to_string())
        .act_threshold(0.9)
        .review_threshold(0.7)
        .timeout_ms(2000)
        .cacheable(true)
        .build()
}

fn choice_b_body() -> serde_json::Value {
    serde_json::json!({
        "id": "cmpl-1", "object": "chat.completion", "created": 0, "model": "m",
        "choices": [{
            "index": 0, "finish_reason": "stop",
            "message": {"role": "assistant", "content": "B"},
            "logprobs": {"content": [{
                "token": "B", "logprob": -0.022,
                "top_logprobs": [
                    {"token": "B", "logprob": -0.022},
                    {"token": " B", "logprob": -3.5},
                    {"token": "A", "logprob": -4.2},
                    {"token": "C", "logprob": -5.1}
                ]
            }]}
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decide_merges_extras_without_clobbering_model() {
    let server = httpmock::MockServer::start();
    let mock = server.mock(|when, then| {
        when.method("POST")
            .path("/chat/completions")
            .body_contains("\"model\":\"m\"")
            .body_contains("\"logprobs\":true")
            .body_contains("\"max_tokens\":1");
        then.status(200).json_body(choice_b_body());
    });
    let client = LlmClient::new(&server.base_url(), "m");
    let s = spec();
    let q = Question {
        spec: &s,
        state: "the sky is blue",
        question: "which letter?",
    };
    let d = client.decide_async(&q).await.expect("decide");
    assert_eq!(d.label, "B");
    mock.assert_hits(1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decide_timeout_maps_to_unavailable() {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/chat/completions");
        then.status(200)
            .delay(std::time::Duration::from_millis(300))
            .json_body(choice_b_body());
    });
    let mut s = spec();
    s.timeout_ms = 50;
    let client = LlmClient::new(&server.base_url(), "m");
    let q = Question {
        spec: &s,
        state: "st",
        question: "q?",
    };
    let err = client.decide_async(&q).await.expect_err("timeout");
    assert!(err.is_unavailable(), "got {err:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decide_retryable_statuses_retry_then_unavailable() {
    for status in [429u16, 502, 503, 504] {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method("POST").path("/chat/completions");
            then.status(status)
                .json_body(serde_json::json!({"error": "busy"}));
        });
        let client = LlmClient::new(&server.base_url(), "m");
        let s = spec();
        let q = Question {
            spec: &s,
            state: "st",
            question: "q?",
        };
        let err = client.decide_async(&q).await.expect_err("must fail");
        assert!(err.is_unavailable(), "status {status}: got {err:?}");
        mock.assert_hits(3);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decide_hard_reject_does_not_retry() {
    let server = httpmock::MockServer::start();
    let mock = server.mock(|when, then| {
        when.method("POST").path("/chat/completions");
        then.status(400).body("bad request");
    });
    let client = LlmClient::new(&server.base_url(), "m");
    let s = spec();
    let q = Question {
        spec: &s,
        state: "st",
        question: "q?",
    };
    let err = client.decide_async(&q).await.expect_err("400");
    assert!(!err.is_unavailable(), "got {err:?}");
    assert!(matches!(
        err,
        fluent_llm::decision::DecisionError::Rejected(_)
    ));
    mock.assert_hits(1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decide_reasoning_only_never_an_answer() {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/chat/completions");
        then.status(200).json_body(serde_json::json!({
            "id": "cmpl-1", "object": "chat.completion", "created": 0, "model": "m",
            "choices": [{
                "index": 0, "finish_reason": "stop",
                "message": {"role": "assistant", "content": "",
                    "reasoning_content": "The user wants B"},
                "logprobs": {"content": [{
                    "token": "The", "logprob": -0.1,
                    "top_logprobs": [{"token": "The", "logprob": -0.1}]
                }]}
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        }));
    });
    let client = LlmClient::new(&server.base_url(), "m");
    let s = spec();
    let q = Question {
        spec: &s,
        state: "st",
        question: "q?",
    };
    let err = client.decide_async(&q).await.expect_err("reasoning-only");
    assert!(matches!(
        err,
        fluent_llm::decision::DecisionError::Decode(_)
    ));
}

#[test]
fn wvr_units_are_pure_sync() {
    // No tokio runtime here: every unit below runs `execute` directly,
    // proving the sync purity contract (no I/O in `execute`).
    let s = spec();
    let render = fluent_llm::decision::RenderQuestionUnit::new(s.clone());
    let mut ctx = WorkContext::default();
    ctx.set("state".to_string(), "STATE".to_string());
    ctx.set("question".to_string(), "Q?".to_string());
    let out = render.execute(&ctx).expect("render");
    let text: String = out.data_as().expect("typed prompt");
    assert!(text.starts_with("STATE"));
    assert!(text.ends_with("Q?"));

    let decode = fluent_llm::decision::DecodeDecisionUnit::new(
        vec!["A".to_string(), "B".to_string()],
        "m".to_string(),
        1,
    );
    let mut ctx = WorkContext::default();
    ctx.set(
        "raw_response".to_string(),
        serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "B"},
                "logprobs": {"content": [{
                    "token": "B", "logprob": -0.02,
                    "top_logprobs": [
                        {"token": "B", "logprob": -0.02},
                        {"token": "A", "logprob": -4.0}
                    ]
                }]}
            }]
        }),
    );
    let out = decode.execute(&ctx).expect("decode");
    let d: fluent_llm::decision::Decision = out.data_as().expect("typed decision");
    assert_eq!(d.label, "B");

    let policy = fluent_llm::decision::PolicyVerdictUnit::new(ThresholdSet {
        act: 0.9,
        review: 0.7,
    });
    let mut ctx = WorkContext::default();
    ctx.set("decision".to_string(), d);
    let out = policy.execute(&ctx).expect("policy");
    let v: Verdict = out.data_as().expect("typed verdict");
    assert_eq!(v, Verdict::Act);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_sees_logprobs_when_endpoint_honors_them() {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/chat/completions");
        then.status(200).json_body(serde_json::json!({
            "id": "cmpl-1", "object": "chat.completion", "created": 0, "model": "m",
            "choices": [{
                "index": 0, "finish_reason": "stop",
                "message": {"role": "assistant", "content": "A"},
                "logprobs": {"content": [{
                    "token": "A", "logprob": -0.02,
                    "top_logprobs": [
                        {"token": "A", "logprob": -0.02},
                        {"token": "B", "logprob": -4.0}
                    ]
                }]}
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        }));
    });
    let client = LlmClient::new(&server.base_url(), "m");
    let probe = client.probe_decision_support().await.expect("probe");
    assert!(probe.logprobs);
    assert_eq!(probe.top_entries, 2);
    assert!(!probe.reasoning_hijack);
    assert!(!probe.degrades_to_text());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_reports_silent_logprobs_drop() {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/chat/completions");
        then.status(200).json_body(serde_json::json!({
            "id": "cmpl-1", "object": "chat.completion", "created": 0, "model": "m",
            "choices": [{
                "index": 0, "finish_reason": "stop",
                "message": {"role": "assistant", "content": "A"},
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        }));
    });
    let client = LlmClient::new(&server.base_url(), "m");
    let probe = client.probe_decision_support().await.expect("probe");
    assert!(!probe.logprobs);
    assert!(probe.degrades_to_text());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_reports_reasoning_hijack() {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/chat/completions");
        then.status(200).json_body(serde_json::json!({
            "id": "cmpl-1", "object": "chat.completion", "created": 0, "model": "m",
            "choices": [{
                "index": 0, "finish_reason": "stop",
                "message": {"role": "assistant", "content": "",
                    "reasoning_content": "thinking"},
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        }));
    });
    let client = LlmClient::new(&server.base_url(), "m");
    let probe = client.probe_decision_support().await.expect("probe");
    assert!(probe.reasoning_hijack);
    assert!(probe.degrades_to_text());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redirect_to_remote_is_an_error_not_a_follow() {
    // The shared client never follows redirects: a loopback 302 pointing
    // remote must surface as a terminal rejection, never a dial-out.
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/chat/completions");
        then.status(302).header(
            "Location",
            "https://llm.example.invalid/v1/chat/completions",
        );
    });
    let client = LlmClient::new(&server.base_url(), "m");
    let err = client
        .probe_decision_support()
        .await
        .expect_err("redirect is an error");
    assert!(matches!(
        err,
        fluent_llm::decision::DecisionError::Rejected(_)
    ));
}
