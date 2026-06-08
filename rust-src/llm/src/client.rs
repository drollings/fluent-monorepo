use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum LlmError {
    #[error("API error: {0}")]
    Api(String),
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("no response from model")]
    NoResponse,
    #[error("rate limited")]
    RateLimited,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

pub struct LlmClient {
    pub api_base: String,
    pub model: String,
}

impl LlmClient {
    pub fn new(api_base: &str, model: &str) -> Self {
        Self {
            api_base: api_base.trim_end_matches('/').to_string(),
            model: model.to_string(),
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn chat_complete(&self, messages: &[ChatMessage]) -> Result<String, LlmError> {
        let url = format!("{}/chat/completions", self.api_base);

        let body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "max_tokens": 1024u32,
            "stream": false,
        });

        let response = ureq::post(&url)
            .send(serde_json::to_string(&body).map_err(|e| LlmError::Api(e.to_string()))?)
            .map_err(|e| LlmError::Http(e.to_string()))?;

        let mut body = response.into_body();
        let body_str = body
            .read_to_string()
            .map_err(|e| LlmError::Api(e.to_string()))?;

        let parsed: serde_json::Value =
            serde_json::from_str(&body_str).map_err(|e| LlmError::Api(e.to_string()))?;

        let content = parsed
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|choices| choices.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .ok_or(LlmError::NoResponse)?
            .to_string();

        Ok(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = LlmClient::new("http://localhost:11434/v1", "llama3");
        assert_eq!(client.model(), "llama3");
    }

    #[test]
    fn test_chat_message_serde() {
        let msg = ChatMessage {
            role: "user".into(),
            content: "hello".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let msg2: ChatMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg.content, msg2.content);
    }
}
