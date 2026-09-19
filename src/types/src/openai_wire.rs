//! OpenAI-compatible chat wire types — the protocol boundary shapes.
//!
//! Moved verbatim from `fluent-router`'s `types.rs` (M3.2): structs/enums +
//! serde + pure constructors only, no logic. The router re-exports these
//! (`fluent_router::types`) during migration; new code imports them from here.
//!
//! The routing extensions on [`RouterRequest`] (`session_id`, `agent_id`,
//! `adapter`, `instance`, `snapshot`, `id_slot`, `metadata`) are part of the
//! wire the owning llama-server reads — they move with the struct, not with
//! router policy. Fail-open on unknown fields (no `deny_unknown_fields`
//! anywhere): forward-compat with new provider keys.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterRequest {
    pub model: String,
    pub messages: Vec<RouterMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<RouterTool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    /// Instance or group to route to (forwarded to the owning llama-server).
    /// Overrides any instance component of `model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// KV snapshot to switch into the target slot before serving.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    /// Slot to target for snapshot switching (defaults to 0 on the server).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_slot: Option<i32>,
    /// Internal pipeline metadata (anonymize map, routing annotations, etc.)
    /// Not sent to frontier providers. Serialized as empty object when absent.
    #[serde(default)]
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterMessage {
    pub role: String,
    pub content: RouterMessageContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RouterMessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl RouterMessageContent {
    pub fn as_text(&self) -> &str {
        match self {
            Self::Text(s) => s,
            Self::Parts(_) => "",
        }
    }

    pub fn to_string_lossy(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    ContentPart::ImageUrl { .. } => None,
                })
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrl },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterTool {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<RouterChoice>,
    pub usage: Usage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterChoice {
    pub index: u32,
    pub message: RouterMessage,
    pub finish_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

impl RouterResponse {
    pub fn text(model: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: String::new(),
            object: "chat.completion".into(),
            created: common_core::now_secs(),
            model: model.into(),
            choices: vec![RouterChoice {
                index: 0,
                message: RouterMessage {
                    role: "assistant".into(),
                    content: RouterMessageContent::Text(text.into()),
                    tool_calls: None,
                    tool_call_id: None,
                },
                finish_reason: "stop".into(),
            }],
            usage: Usage::default(),
        }
    }

    pub fn error(model: impl Into<String>, err: impl AsRef<str>) -> Self {
        Self::text(model, format!("ERROR: {}", err.as_ref()))
    }

    // NOTE: there is deliberately no `fallback` constructor. Terminal
    // dispatch failure leaves the router as an honest HTTP error status,
    // never a 200 carrying an `ERROR:` completion body.

    pub fn answer_text(&self) -> Option<String> {
        self.choices
            .first()
            .map(|c| c.message.content.to_string_lossy())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request() -> RouterRequest {
        serde_json::from_value(serde_json::json!({
            "model": "fast",
            "messages": [{"role": "user", "content": "hello"}],
            "temperature": 0.7,
            "session_id": "s1",
        }))
        .expect("deserialize sample request")
    }

    #[test]
    fn request_round_trip() {
        let req = sample_request();
        assert_eq!(req.model, "fast");
        assert_eq!(req.temperature, Some(0.7));
        let back: RouterRequest =
            serde_json::from_str(&serde_json::to_string(&req).expect("serialize"))
                .expect("round trip");
        assert_eq!(back.model, "fast");
        assert_eq!(back.messages.len(), 1);
    }

    #[test]
    fn missing_optionals_default_none() {
        let req: RouterRequest = serde_json::from_value(serde_json::json!({
            "model": "m",
            "messages": [],
        }))
        .expect("deserialize");
        assert!(req.temperature.is_none());
        assert!(req.tools.is_none());
        assert!(req.instance.is_none());
        assert!(req.metadata.is_empty());
    }

    #[test]
    fn content_text_vs_parts() {
        let t = RouterMessageContent::Text("hi".into());
        assert_eq!(t.as_text(), "hi");
        assert_eq!(t.to_string_lossy(), "hi");
        let p = RouterMessageContent::Parts(vec![
            ContentPart::Text { text: "one".into() },
            ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: "http://i".into(),
                },
            },
            ContentPart::Text { text: "two".into() },
        ]);
        assert_eq!(p.as_text(), "");
        assert_eq!(p.to_string_lossy(), "one two");
    }

    #[test]
    fn tool_calls_empty_array_vs_none() {
        let empty: RouterMessage = serde_json::from_value(serde_json::json!({
            "role": "assistant", "content": "hi", "tool_calls": [],
        }))
        .expect("deserialize");
        assert!(empty.tool_calls.as_ref().is_some_and(Vec::is_empty));
        let absent: RouterMessage = serde_json::from_value(serde_json::json!({
            "role": "assistant", "content": "hi",
        }))
        .expect("deserialize");
        assert!(absent.tool_calls.is_none());
    }

    #[test]
    fn unknown_fields_tolerated() {
        let req: RouterRequest = serde_json::from_value(serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi", "future_key": 1}],
            "future_top_level": true,
        }))
        .expect("unknown fields tolerated");
        assert_eq!(req.messages.len(), 1);
    }

    #[test]
    fn response_constructors() {
        let r = RouterResponse::text("m", "hi");
        assert_eq!(r.object, "chat.completion");
        assert_eq!(r.answer_text().as_deref(), Some("hi"));
        let e = RouterResponse::error("m", "boom");
        assert!(e.answer_text().is_some_and(|t| t.starts_with("ERROR: ")));
        assert_eq!(Usage::default().total_tokens, 0);
    }
}
