//! Shared helpers for pipeline stages.

use serde_json::{Map, Value};

use fluent_wvr::prelude::*;

use crate::types::{RouterMessageContent, RouterRequest};

/// Ensure a floating-point field exists on a classifier/tree JSON object,
/// coercing a string-valued number back to numeric. The shared "surviving
/// normalization" used by both the flat `ClassifierStage` sanitizer and the
/// classification-tree parser.
pub(crate) fn coerce_float(obj: &mut Map<String, Value>, key: &str, default: f64) {
    match obj.get(key) {
        None => {
            if let Some(n) = serde_json::Number::from_f64(default) {
                obj.insert(key.into(), Value::Number(n));
            }
        }
        Some(Value::String(s)) => {
            if let Ok(n) = s.parse::<f64>() {
                if let Some(num) = serde_json::Number::from_f64(n) {
                    obj[key] = Value::Number(num);
                }
            }
        }
        _ => {}
    }
}

/// Ensure an unsigned-integer field exists on a classifier/tree JSON object,
/// coercing from float or string. The shared "surviving normalization" for the
/// `complexity` axis.
pub(crate) fn coerce_u8(obj: &mut Map<String, Value>, key: &str, default: u8) {
    match obj.get(key) {
        None => {
            obj.insert(key.into(), Value::Number(serde_json::Number::from(default)));
        }
        Some(Value::Number(n)) => {
            let as_u8 = n
                .as_u64()
                .map(|i| i.min(u64::from(u8::MAX)) as u8)
                .or_else(|| n.as_f64().map(|f| f.round().min(f64::from(u8::MAX)) as u8));
            if let Some(v) = as_u8 {
                obj[key] = Value::Number(serde_json::Number::from(v));
            }
        }
        Some(Value::String(s)) => {
            let as_u8 = s
                .parse::<u8>()
                .ok()
                .or_else(|| s.parse::<f64>().ok().map(|f| f.round() as u8));
            if let Some(v) = as_u8 {
                obj[key] = Value::Number(serde_json::Number::from(v));
            }
        }
        _ => {}
    }
}

/// Ensure a string field exists on a classifier/tree JSON object with a
/// default when absent.
pub(crate) fn coerce_string(obj: &mut Map<String, Value>, key: &str, default: &str) {
    if !obj.contains_key(key) {
        obj.insert(key.into(), Value::String(default.into()));
    }
}

/// Extract the last user message from the request carried in
/// `ctx.structured["request"]`.
///
/// The request is the structured canonical `RouterRequest` (a typed
/// `serde_json::Value` in the structured channel, not a JSON string), so
/// content may be either a plain string (`RouterMessageContent::Text`) or an
/// array of content parts (`RouterMessageContent::Parts`, the OpenAI
/// multi-part form used by clients like Brave Leo). Text rendering for both
/// forms lives in the single canonical helper
/// `RouterMessageContent::to_string_lossy` — this function only picks the
/// message, it never re-implements content rendering.
///
/// Selection semantics: the last `role == "user"` message whose content
/// renders to text. A `Text` message is returned verbatim (an empty string is
/// a valid — if degenerate — user message). A `Parts` message with no
/// extractable text (e.g. image-only) is skipped so an earlier text message
/// still resolves, matching the historical skip of non-string content.
pub fn extract_user_message(ctx: &WorkContext) -> Result<String, WorkError> {
    let request: RouterRequest = ctx
        .structured("request")
        .map_err(|e| WorkError::Execution(format!("missing request: {e}")))?;

    request
        .messages
        .iter()
        .rev()
        .find_map(|m| {
            if m.role != "user" {
                return None;
            }
            match &m.content {
                RouterMessageContent::Text(s) => Some(s.clone()),
                RouterMessageContent::Parts(_) => {
                    let text = m.content.to_string_lossy();
                    (!text.trim().is_empty()).then_some(text)
                }
            }
        })
        .ok_or_else(|| WorkError::Execution("no user message found".into()))
}

pub fn get_metadata_string(ctx: &WorkContext, key: &str) -> Option<String> {
    ctx.metadata.get(key).and_then(|v| match v {
        MetadataValue::String(s) => Some(s.clone()),
        _ => None,
    })
}

/// The routing window for Needle: the first sentence or first paragraph of a
/// prompt, up to [`ROUTING_WINDOW_MAX_CHARS`] characters, whichever is
/// smallest.
///
/// Needle is a cheap, non-generative tool-calling rung that should always
/// decide on the *opening* of a request — the first sentence or paragraph —
/// rather than the whole message, so a long prompt can never blow the rung's
/// gate or bury the actionable intent. The window is the earlier of:
///
/// - the end of the first sentence (a `.`/`!`/`?` followed by whitespace or
///   end-of-input), or
/// - the end of the first paragraph (the first newline),
///
/// truncated to [`ROUTING_WINDOW_MAX_CHARS`] characters. Leading/trailing
/// whitespace is trimmed. Returns an empty slice for empty/whitespace input.
pub const ROUTING_WINDOW_MAX_CHARS: usize = 200;

pub fn routing_window(input: &str) -> &str {
    let input = input.trim();
    if input.is_empty() {
        return input;
    }
    let mut boundary = input.len();
    // First paragraph boundary: the first newline.
    if let Some(idx) = input.find(['\n', '\r']) {
        boundary = boundary.min(idx);
    }
    // First sentence boundary: a terminator followed by whitespace or end.
    for (i, b) in input.as_bytes().iter().enumerate() {
        if matches!(b, b'.' | b'!' | b'?') {
            let after = i + 1;
            let followed_by_ws = input[after..].chars().next().is_none_or(char::is_whitespace);
            if followed_by_ws {
                boundary = boundary.min(after);
                break;
            }
        }
    }
    let window = &input[..boundary.min(input.len())];
    // Truncate to ROUTING_WINDOW_MAX_CHARS characters (char-boundary safe).
    let count = window.chars().count();
    if count <= ROUTING_WINDOW_MAX_CHARS {
        window
    } else {
        let mut end = 0;
        for (idx, c) in window.char_indices() {
            if idx >= ROUTING_WINDOW_MAX_CHARS {
                break;
            }
            end = idx + c.len_utf8();
        }
        &window[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContentPart, ImageUrl, RouterMessage, RouterMessageContent};

    fn make_ctx_with_messages(messages: Vec<RouterMessage>) -> WorkContext {
        let request = RouterRequest {
            model: "test".into(),
            messages,
            temperature: None,
            max_tokens: None,
            stream: None,
            tools: None,
            tool_choice: None,
            session_id: None,
            agent_id: None,
            adapter: None,
            instance: None,
            snapshot: None,
            id_slot: None,
            metadata: Default::default(),
        };
        let mut ctx = WorkContext::default();
        ctx.set_structured("request", &request);
        ctx
    }

    #[test]
    fn extracts_last_text_message() {
        let ctx = make_ctx_with_messages(vec![
            RouterMessage {
                role: "user".into(),
                content: RouterMessageContent::Text("earlier".into()),
                tool_calls: None,
                tool_call_id: None,
            },
            RouterMessage {
                role: "assistant".into(),
                content: RouterMessageContent::Text("response".into()),
                tool_calls: None,
                tool_call_id: None,
            },
            RouterMessage {
                role: "user".into(),
                content: RouterMessageContent::Text("latest".into()),
                tool_calls: None,
                tool_call_id: None,
            },
        ]);
        assert_eq!(extract_user_message(&ctx).unwrap(), "latest");
    }

    #[test]
    fn extracts_text_from_content_parts() {
        let ctx = make_ctx_with_messages(vec![RouterMessage {
            role: "user".into(),
            content: RouterMessageContent::Parts(vec![
                ContentPart::Text {
                    text: "About this user:".into(),
                },
                ContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: "https://example.test/x.png".into(),
                    },
                },
                ContentPart::Text {
                    text: "Daniel".into(),
                },
            ]),
            tool_calls: None,
            tool_call_id: None,
        }]);
        assert_eq!(
            extract_user_message(&ctx).unwrap(),
            "About this user: Daniel"
        );
    }

    #[test]
    fn errors_when_no_user_message() {
        let ctx = make_ctx_with_messages(vec![RouterMessage {
            role: "system".into(),
            content: RouterMessageContent::Text("sys".into()),
            tool_calls: None,
            tool_call_id: None,
        }]);
        let err = extract_user_message(&ctx).unwrap_err();
        assert!(err.to_string().contains("no user message found"));
    }

    #[test]
    fn errors_when_request_missing() {
        let ctx = WorkContext::default();
        let err = extract_user_message(&ctx).unwrap_err();
        assert!(err.to_string().contains("missing request"));
    }

    #[test]
    fn routing_window_takes_first_sentence() {
        assert_eq!(
            routing_window("Write a Rust function to sort a vec. Then explain it."),
            "Write a Rust function to sort a vec."
        );
        assert_eq!(routing_window("What is 2+2?"), "What is 2+2?");
    }

    #[test]
    fn routing_window_prefers_paragraph_break_before_sentence() {
        // A paragraph break ends the window even if the first sentence would
        // have run longer.
        assert_eq!(
            routing_window("Line one\nLine two continues here. Still line two."),
            "Line one"
        );
    }

    #[test]
    fn routing_window_truncates_to_char_cap() {
        let long = format!("{} and then some trailing words.", "word ".repeat(100));
        let w = routing_window(&long);
        assert!(w.chars().count() <= ROUTING_WINDOW_MAX_CHARS);
        assert!(w.chars().count() > 0);
    }

    #[test]
    fn routing_window_empty_and_whitespace() {
        assert_eq!(routing_window(""), "");
        assert_eq!(routing_window("   "), "");
    }

    #[test]
    fn routing_window_trims_leading_whitespace() {
        assert_eq!(routing_window("  Dim the lights. Then relax."), "Dim the lights.");
    }
}
