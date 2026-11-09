//! Remote opt-in boundary tests (tests-first).

use std::sync::{Arc, Mutex};

use fluent_llm::client::LlmClient;
use fluent_llm::decision::{
    anonymize_hook, DecisionAudit, DecisionTransport, Question, QuestionSpec,
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

fn pii_prompt() -> String {
    "Call 192.168.1.1 or jane@example.com. Reply with only A or B.".to_string()
}

fn choice_b_body() -> serde_json::Value {
    serde_json::json!({
        "id": "cmpl-1", "object": "chat.completion", "created": 0, "model": "m",
        "choices": [{
            "index": 0, "finish_reason": "stop",
            "message": {"role": "assistant", "content": "B"},
            "logprobs": {"content": [{
                "token": "B", "logprob": -0.02,
                "top_logprobs": [
                    {"token": "B", "logprob": -0.02},
                    {"token": "A", "logprob": -4.0}
                ]
            }]}
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
}

#[test]
fn localhost_prepare_sends_unchanged_without_flag() {
    let transport = DecisionTransport::local_only();
    let prompt = pii_prompt();
    let prepared = transport
        .prepare("http://127.0.0.1:8080", "m", prompt.clone())
        .expect("localhost allowed");
    assert_eq!(prepared.text, prompt);
    assert!(!prepared.audited);
}

#[test]
fn remote_without_flag_refused() {
    let transport = DecisionTransport::local_only();
    let err = transport
        .prepare("https://llm.example.invalid", "m", pii_prompt())
        .expect_err("remote refused");
    assert!(matches!(
        err,
        fluent_llm::decision::DecisionError::Rejected(_)
    ));
    assert!(err.to_string().contains("opt-in"));
}

#[test]
fn remote_without_hook_refused() {
    let transport = DecisionTransport::local_only().with_allow_remote(true);
    let err = transport
        .prepare("https://llm.example.invalid", "m", pii_prompt())
        .expect_err("hook required");
    assert!(matches!(
        err,
        fluent_llm::decision::DecisionError::Rejected(_)
    ));
}

#[test]
fn remote_with_flag_redacts_and_audits() {
    let audits: Arc<Mutex<Vec<DecisionAudit>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_audits = Arc::clone(&audits);
    let transport = DecisionTransport::local_only()
        .with_allow_remote(true)
        .with_redact(anonymize_hook())
        .with_audit(Arc::new(move |a| {
            sink_audits.lock().expect("audit lock").push(a);
        }));
    let prepared = transport
        .prepare("https://llm.example.invalid", "m", pii_prompt())
        .expect("remote allowed");
    assert!(!prepared.text.contains("192.168.1.1"));
    assert!(!prepared.text.contains("jane@example.com"));
    assert!(prepared.text.contains("[IP_ADDRESS]"));
    assert!(prepared.audited);
    let logged = audits.lock().expect("audit lock");
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0].model, "m");
    assert_eq!(logged[0].host, "llm.example.invalid");
    assert!(logged[0].redaction_applied);
    // No raw state bytes in the audit record.
    let encoded = serde_json::to_string(&logged[0]).expect("encode audit");
    assert!(!encoded.contains("192.168.1.1"));
    assert!(!encoded.contains("jane@example.com"));
}

#[test]
fn pii_fixtures_never_leave_unredacted_control() {
    let transport = DecisionTransport::local_only()
        .with_allow_remote(true)
        .with_redact(anonymize_hook());
    for secret in [
        "server 192.168.1.1 down",
        "mail jane@example.com now",
        "call 10.0.0.7 and bob@example.org today",
    ] {
        let prepared = transport
            .prepare("https://llm.example.invalid", "m", secret.to_string())
            .expect("allowed");
        for raw in [
            "192.168.1.1",
            "jane@example.com",
            "10.0.0.7",
            "bob@example.org",
        ] {
            assert!(
                !prepared.text.contains(raw),
                "{raw} leaked into {prepared:?}"
            );
        }
    }
}

#[test]
fn dns_suffix_on_loopback_prefix_is_remote_not_local() {
    // 127.0.0.1.evil.invalid resolves via DNS (attacker-controlled), not
    // via loopback — treating it as localhost sends unredacted prompts to a
    // remote host with no audit. Must be gated as remote.
    let transport = DecisionTransport::local_only();
    let err = transport
        .prepare("http://127.0.0.1.evil.invalid/", "m", pii_prompt())
        .expect_err("dns-suffixed host must not be localhost");
    assert!(matches!(
        err,
        fluent_llm::decision::DecisionError::Rejected(_)
    ));
}

#[test]
fn bracketed_ipv6_loopback_is_local() {
    let transport = DecisionTransport::local_only();
    let prompt = pii_prompt();
    let prepared = transport
        .prepare("http://[::1]:8080/", "m", prompt.clone())
        .expect("bracketed ::1 is loopback");
    assert_eq!(prepared.text, prompt);
}

#[test]
fn userinfo_tricks_fail_closed() {
    let transport = DecisionTransport::local_only();
    for url in [
        "http://127.0.0.1@evil.invalid/",
        "http://user@127.0.0.1:8080/",
        "http://127.0.0.1%09@evil.invalid/",
    ] {
        assert!(
            transport
                .prepare(url, "m", pii_prompt())
                .expect_err("userinfo must fail closed")
                .to_string()
                .contains("refused"),
            "{url}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decide_refuses_remote_without_dialing() {
    let client = LlmClient::new("https://llm.example.invalid", "m");
    let s = spec();
    let q = Question {
        spec: &s,
        state: "st",
        question: "q?",
    };
    let err = client.decide_async(&q).await.expect_err("refused");
    assert!(matches!(
        err,
        fluent_llm::decision::DecisionError::Rejected(_)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loopback_with_hook_sends_redacted() {
    let server = httpmock::MockServer::start();
    let mock = server.mock(|when, then| {
        when.method("POST")
            .path("/chat/completions")
            .body_contains("[IP_ADDRESS]");
        then.status(200).json_body(choice_b_body());
    });
    let audits: Arc<Mutex<Vec<DecisionAudit>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_audits = Arc::clone(&audits);
    let mut client = LlmClient::new(&server.base_url(), "m");
    client.decision_transport = DecisionTransport::local_only()
        .with_allow_remote(true)
        .with_redact(anonymize_hook())
        .with_audit(Arc::new(move |a| {
            sink_audits.lock().expect("audit lock").push(a);
        }));
    let s = spec();
    let q = Question {
        spec: &s,
        state: "server 192.168.1.1 down",
        question: "A or B?",
    };
    let decision = client.decide_async(&q).await.expect("redacted send");
    assert_eq!(decision.label, "B");
    mock.assert_hits(1);
    // Localhost sends are not audited (no log spam on the trusted path).
    assert!(audits.lock().expect("audit lock").is_empty());
}
