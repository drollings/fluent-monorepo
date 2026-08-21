use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use common_core::sync::lock;
use fluent_llm::client::ChatBackend;
use fluent_llm::{ChatMessage, LlmError};
use serde::{Deserialize, Serialize};

use crate::config::ClassifierOutput;
use crate::needle::backend::NeedleBackend;
use crate::needle::envelope::{NeedleEnvelope, NeedleEnvelopeType};
use crate::needle::NeedleError;
use crate::pipeline::RoutingTarget;
use crate::stages::common::routing_window;
use crate::types::{RouterMessage, RouterMessageContent, RouterResponse, Usage};

pub struct TranscriptProvider {
    entries: HashMap<String, String>,
    default_response: String,
}

impl TranscriptProvider {
    pub fn new(entries: HashMap<String, String>) -> Self {
        Self {
            entries,
            default_response: Self::default_pass_response(),
        }
    }

    #[must_use]
    pub fn with_default(mut self, response: String) -> Self {
        self.default_response = response;
        self
    }

    fn default_pass_response() -> String {
        serde_json::to_string(&ClassifierOutput {
            action: "route".into(),
            response: None,
            target: Some("fast".into()),
            coherence_score: 0.95,
            safety_score: 0.9,
            complexity: None,
            intent: Some("question".into()),
            reason: "well-formed factual query".into(),
            completeness: None,
            risk: None,
        })
        .unwrap_or_default()
    }
}

impl ChatBackend for TranscriptProvider {
    fn chat_complete(&self, messages: &[ChatMessage]) -> Result<String, LlmError> {
        let user_msg = messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .and_then(|m| {
                let content = m.content.trim();
                if content.is_empty() {
                    None
                } else {
                    Some(content)
                }
            })
            .unwrap_or("");

        if user_msg.is_empty() {
            return Ok(self.default_response.clone());
        }

        Ok(self
            .entries
            .get(user_msg)
            .cloned()
            .unwrap_or_else(|| self.default_response.clone()))
    }
}

pub fn default_transcript() -> TranscriptProvider {
    TranscriptProvider::new(HashMap::new())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MockTranscriptEntry {
    pub user_message: String,
    pub classifier_response: String,
    /// Optional canned Needle envelope JSON for this probe. When present, the
    /// `NeedlePreFilter` rung (with an injected `NeedleTranscriptProvider`)
    /// decides the request from this envelope instead of the classifier. Keyed
    /// by the routing window of `user_message`. Absent → the provider returns
    /// its default (a `refuse` decline), so the classifier decides as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needle_response: Option<String>,
    #[serde(default)]
    pub expected_route: Option<String>,
    /// When set, the resolved routing target must have dispatched through this
    /// `model_groups` name (i.e. `RoutingTarget.group` equals it). This is the
    /// intent→model_group assertion hook for the config-synced integration
    /// tests: `expected_route` proves the *route* resolved, this proves the
    /// dispatch went through the *group* the route maps to in the config.
    #[serde(default)]
    pub expect_model_group: Option<String>,
    #[serde(default)]
    pub dispatch_response: Option<String>,
    #[serde(default)]
    pub rejected: bool,
    #[serde(default)]
    pub reject_reason_contains: Option<String>,
}

pub fn load_transcript_file(
    path: impl AsRef<Path>,
) -> Result<Vec<MockTranscriptEntry>, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path.as_ref())?;
    Ok(serde_json::from_str(&content)?)
}

pub fn transcript_provider_from_entries(entries: &[MockTranscriptEntry]) -> TranscriptProvider {
    let map: HashMap<String, String> = entries
        .iter()
        .map(|e| (e.user_message.clone(), e.classifier_response.clone()))
        .collect();
    TranscriptProvider::new(map)
}

/// A `NeedleBackend` driven by a canned-envelope table — the hermetic
/// counterpart to [`TranscriptProvider`] for the Needle pre-filter rung.
///
/// Envelopes are keyed by the **routing window** of each entry's `user_message`
/// (the same window the `NeedlePreFilter` stage computes and passes as `text`
/// to `complete`), so the provider and the stage agree on the key without
/// re-deriving it. A probe without a `needle_response` falls back to the
/// default envelope (a `refuse` decline), preserving the classifier-decides
/// path for entries that don't exercise Needle.
pub struct NeedleTranscriptProvider {
    entries: HashMap<String, NeedleEnvelope>,
    default: NeedleEnvelope,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl NeedleTranscriptProvider {
    pub fn new(entries: HashMap<String, NeedleEnvelope>) -> Self {
        Self {
            entries,
            default: refuse_envelope(),
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    #[must_use]
    pub fn with_default(mut self, default: NeedleEnvelope) -> Self {
        self.default = default;
        self
    }

    /// How many `complete` calls the rung made — lets a test assert Needle was
    /// actually consulted (the feedback-loop "Needle actions are logged and
    /// tested" hook).
    pub fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl NeedleBackend for NeedleTranscriptProvider {
    fn complete(
        &self,
        text: &str,
        _tools_json: &str,
        _max_new_tokens: i32,
    ) -> Result<NeedleEnvelope, NeedleError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self
            .entries
            .get(text)
            .cloned()
            .unwrap_or_else(|| self.default.clone()))
    }

    fn is_available(&self) -> bool {
        true
    }

    fn reset(&self) {}
}

/// A canned Needle envelope that declines — the default fallback for entries
/// without a `needle_response`, so the classifier decides unchanged.
fn refuse_envelope() -> NeedleEnvelope {
    NeedleEnvelope {
        r#type: NeedleEnvelopeType::Refuse,
        success: None,
        error: None,
        error_code: None,
        function_calls: vec![],
        reasoning: None,
        confidence: None,
        results: None,
    }
}

/// Build a `NeedleTranscriptProvider` from transcript entries, keying each
/// `needle_response` envelope by the routing window of its `user_message`.
pub fn needle_provider_from_entries(entries: &[MockTranscriptEntry]) -> NeedleTranscriptProvider {
    let mut map = HashMap::new();
    for entry in entries {
        if let Some(raw) = &entry.needle_response {
            if let Ok(env) = NeedleEnvelope::parse(raw) {
                map.insert(routing_window(&entry.user_message).to_string(), env);
            }
        }
    }
    NeedleTranscriptProvider::new(map)
}

/// A needle `call` envelope naming `tool` at the given confidence with no bound
/// arguments — the routing verdict a config-synced probe needs to drive the
/// rung.
pub fn needle_call_envelope(tool: &str, confidence: f64) -> String {
    needle_call_envelope_with_args(tool, confidence, &serde_json::json!({}))
}

/// A needle `call` envelope naming `tool` at the given confidence with explicit
/// bound `arguments` — used to exercise the direct (template) tool-response
/// path, which renders the `output_template` from these arguments.
pub fn needle_call_envelope_with_args(
    tool: &str,
    confidence: f64,
    arguments: &serde_json::Value,
) -> String {
    serde_json::json!({
        "type": "call",
        "confidence": confidence,
        "function_calls": [{"name": tool, "arguments": arguments}],
    })
    .to_string()
}

pub struct MockDispatchContext {
    pub transcripts: Arc<Vec<MockTranscriptEntry>>,
    pub failures: std::sync::Mutex<Vec<String>>,
    /// Model names that should NOT be mocked — these make real LLM calls.
    pub except_models: Vec<String>,
}

impl MockDispatchContext {
    pub fn new(transcripts: Vec<MockTranscriptEntry>, except_models: Vec<String>) -> Self {
        Self {
            transcripts: Arc::new(transcripts),
            failures: std::sync::Mutex::new(Vec::new()),
            except_models,
        }
    }

    /// Reference to the underlying transcript entries.
    pub fn transcripts(&self) -> &[MockTranscriptEntry] {
        &self.transcripts
    }

    /// Returns `true` if the given model name is in the except list
    /// (i.e., should make real LLM calls instead of returning canned responses).
    pub fn is_model_excepted(&self, model: &str) -> bool {
        self.except_models.iter().any(|m| m == model)
    }

    pub fn lookup(&self, user_message: &str) -> Option<&MockTranscriptEntry> {
        let result = self
            .transcripts
            .iter()
            .find(|t| t.user_message == user_message);
        tracing::debug!(target: "router.mock",
            user_message_len = user_message.len(),
            found = result.is_some(),
            "transcript lookup"
        );
        result
    }

    pub fn validate_route(
        &self,
        entry: &MockTranscriptEntry,
        routing_target: Option<&RoutingTarget>,
    ) {
        let result = match (&entry.expected_route, routing_target) {
            (None, None) => Ok(()),
            (Some(expected), Some(actual)) => {
                let actual_route = actual.target_name.as_deref().unwrap_or(&actual.model);
                if actual_route == expected {
                    Ok(())
                } else {
                    Err(format!(
                        "route mismatch for '{}': expected '{}', got '{}' (model={}, url={})",
                        entry.user_message, expected, actual_route, actual.model, actual.url
                    ))
                }
            }
            (Some(expected), None) => Err(format!(
                "route mismatch for '{}': expected '{}', but no routing target was set",
                entry.user_message, expected
            )),
            (None, Some(actual)) => {
                let actual_route = actual.target_name.as_deref().unwrap_or(&actual.model);
                Err(format!(
                    "route mismatch for '{}': expected no route, but got '{}' (model={})",
                    entry.user_message, actual_route, actual.model
                ))
            }
        };

        if let Err(msg) = result {
            lock(&self.failures).push(msg);
        }

        // Independent model-group check: when the entry declares the expected
        // `model_groups` name, verify the resolved target was dispatched
        // through exactly that group (intent → model_group, config-derived).
        if let (Some(expected_group), Some(actual)) = (&entry.expect_model_group, routing_target) {
            let actual_group = actual.group.as_deref().unwrap_or("");
            if actual_group != expected_group {
                lock(&self.failures).push(format!(
                    "group mismatch for '{}': expected model_group '{}', got '{}' (route={}, model={})",
                    entry.user_message,
                    expected_group,
                    actual_group,
                    actual.target_name.as_deref().unwrap_or(""),
                    actual.model,
                ));
            }
        }
    }

    pub fn validate_rejection(&self, entry: &MockTranscriptEntry, reject_reason: &str) {
        if let Some(expected_substr) = &entry.reject_reason_contains {
            if !reject_reason.contains(expected_substr.as_str()) {
                lock(&self.failures).push(format!(
                    "rejection reason mismatch for '{}': expected reason to contain '{}', got '{}'",
                    entry.user_message, expected_substr, reject_reason
                ));
            }
        }
    }

    pub fn dispatch_response(
        &self,
        entry: &MockTranscriptEntry,
        model_name: &str,
    ) -> RouterResponse {
        RouterResponse {
            id: "mock-resp".into(),
            object: "chat.completion".into(),
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            model: model_name.to_string(),
            choices: vec![crate::types::RouterChoice {
                index: 0,
                message: RouterMessage {
                    role: "assistant".into(),
                    content: RouterMessageContent::Text(
                        entry.dispatch_response.clone().unwrap_or_default(),
                    ),
                    tool_calls: None,
                    tool_call_id: None,
                },
                finish_reason: "stop".into(),
            }],
            usage: Usage::default(),
        }
    }

    pub fn take_failures(&self) -> Vec<String> {
        std::mem::take(&mut lock(&self.failures))
    }
}
