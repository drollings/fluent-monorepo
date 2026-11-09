//! Backward-compat parity tests (tests-first).
//!
//! Pins pre-roadmap behavior byte-identical: the no-think default and
//! protected-keys merge survive decision extras; the sync `ChatBackend`
//! seam still delegates; content-only extraction still ignores
//! `reasoning_content`.

use fluent_llm::client::{ChatBackend, LlmClient};
use fluent_llm::openai::build_openai_chat_body;
use fluent_llm::protocol::{ChatMessage, LlmError};

fn messages() -> serde_json::Value {
    serde_json::json!([{"role": "user", "content": "hi"}])
}

#[test]
fn decision_extras_keep_no_think_default_and_protected_keys() {
    let extras = serde_json::json!({
        "max_tokens": 1,
        "temperature": 0,
        "logprobs": true,
        "top_logprobs": 10,
        "model": "override",
        "messages": "override",
        "stream": true,
    });
    let body = build_openai_chat_body("m", &messages(), Some(&extras), false, None);
    assert_eq!(body["model"], "m");
    assert_eq!(body["messages"], messages());
    assert_eq!(body["stream"], false);
    assert_eq!(
        body["chat_template_kwargs"],
        serde_json::json!({"enable_thinking": false})
    );
    assert_eq!(body["max_tokens"], 1);
    assert_eq!(body["logprobs"], true);
}

struct PlainBackend {
    response: String,
}

// Deliberately no `chat_complete_with_extras` override: the default must
// delegate to `chat_complete` so stubs keep working unchanged.
impl ChatBackend for PlainBackend {
    fn chat_complete(&self, _messages: &[ChatMessage]) -> Result<String, LlmError> {
        Ok(self.response.clone())
    }
}

#[test]
fn backend_default_extras_seam_delegates() {
    let backend = PlainBackend {
        response: "hi".to_string(),
    };
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: "hi".into(),
    }];
    let extras = serde_json::json!({"logprobs": true});
    assert_eq!(
        backend
            .chat_complete_with_extras(&messages, &extras)
            .expect("delegate"),
        "hi"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_extraction_ignores_reasoning_content() {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/chat/completions");
        then.status(200).json_body(serde_json::json!({
            "id": "cmpl-1", "object": "chat.completion", "created": 0, "model": "m",
            "choices": [{
                "index": 0, "finish_reason": "stop",
                "message": {"role": "assistant", "content": "hello",
                    "reasoning_content": "secret thinking"},
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        }));
    });
    let client = LlmClient::new(&server.base_url(), "m");
    let out = client
        .chat_complete(&[ChatMessage {
            role: "user".into(),
            content: "hi".into(),
        }])
        .expect("content");
    assert_eq!(out, "hello");
}

#[test]
fn decision_tree_scanned_by_boundary_guard() {
    // The `no_domain_imports` guard walks `src/llm/src` recursively, so the
    // decision tree is covered without a second scan. Pin that the tree
    // exists with real content to scan — an empty dir would pass vacuously.
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = manifest.join("src").join("decision");
    assert!(dir.is_dir());
    let count = std::fs::read_dir(&dir)
        .expect("read decision dir")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "rs"))
        .count();
    assert!(count >= 10, "decision tree has {count} modules");
}
