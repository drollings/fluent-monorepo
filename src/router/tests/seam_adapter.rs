//! The single `ChatBackend` → fetch adapter contract: prompt forwarding,
//! error mapping, and async/sync twin agreement.

use std::sync::{Arc, Mutex};

use super::{chat_backend_to_fetch, chat_backend_to_fetch_sync};
use crate::test_stubs::StubChatBackend;

struct RecordingBackend {
    response: String,
    seen: Mutex<Option<(Vec<fluent_llm::ChatMessage>, serde_json::Value)>>,
}

impl RecordingBackend {
    fn new(response: impl Into<String>) -> Self {
        Self {
            response: response.into(),
            seen: Mutex::new(None),
        }
    }
}

impl fluent_llm::client::ChatBackend for RecordingBackend {
    fn chat_complete(
        &self,
        _messages: &[fluent_llm::ChatMessage],
    ) -> Result<String, fluent_llm::LlmError> {
        Ok(self.response.clone())
    }

    fn chat_complete_with_extras(
        &self,
        messages: &[fluent_llm::ChatMessage],
        extras: &serde_json::Value,
    ) -> Result<String, fluent_llm::LlmError> {
        *self.seen.lock().expect("seen lock") =
            Some((messages.to_vec(), extras.clone()));
        Ok(self.response.clone())
    }
}

#[test]
fn sync_twin_forwards_prompt_contract_and_response() {
    let backend = Arc::new(RecordingBackend::new("[1,2]"));
    let fetch = chat_backend_to_fetch_sync(backend.clone());
    let out = fetch(vec!["hello".to_string()]).expect("stub responds");
    assert_eq!(out, "[1,2]");
    let (messages, extras) = backend.seen.lock().expect("seen lock").clone().expect("called");
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0].content,
        spacy_rs::AnnotationRecord::prompt(&["hello".to_string()])
    );
    assert_eq!(messages[1].content, "Annotate the given tokens.");
    assert_eq!(
        extras
            .pointer("/response_format/schema")
            .expect("schema extras"),
        &spacy_rs::AnnotationRecord::contract()
    );
}

#[test]
fn backend_error_maps_to_fetch_error() {
    let backend: Arc<dyn fluent_llm::client::ChatBackend> =
        Arc::new(StubChatBackend::new(Vec::new()));
    let err = chat_backend_to_fetch_sync(Arc::clone(&backend))(vec!["x".to_string()])
        .expect_err("empty stub fails");
    assert!(
        matches!(err, spacy_rs::pipeline::AnnotateError::Fetch(_)),
        "unexpected variant: {err:?}"
    );
}

#[tokio::test]
async fn twins_agree_on_response_and_error() {
    let backend: Arc<dyn fluent_llm::client::ChatBackend> = Arc::new(StubChatBackend::new(vec![
        "ok-array".to_string(),
        "ok-array".to_string(),
    ]));
    let sync_out =
        chat_backend_to_fetch_sync(Arc::clone(&backend))(vec!["t".to_string()]);
    let async_out = chat_backend_to_fetch(Arc::clone(&backend))(vec!["t".to_string()]).await;
    assert_eq!(
        sync_out.expect("sync responds"),
        async_out.expect("async responds")
    );

    let failing: Arc<dyn fluent_llm::client::ChatBackend> =
        Arc::new(StubChatBackend::new(Vec::new()));
    let sync_err =
        chat_backend_to_fetch_sync(Arc::clone(&failing))(vec!["t".to_string()]);
    let async_err = chat_backend_to_fetch(Arc::clone(&failing))(vec!["t".to_string()]).await;
    assert!(sync_err.is_err() && async_err.is_err());
}
