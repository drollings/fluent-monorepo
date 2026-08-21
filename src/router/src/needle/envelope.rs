//! Typed envelope for Needle's grammar-constrained JSON tool-call response.
//!
//! Needle is non-generative: every completion is a JSON envelope of one of
//! three kinds — a `call` (one or more tool invocations), `text` (a short
//! structured label, not free prose), or `refuse` (the model declines). The
//! shape mirrors the reference implementation
//! (`/opt/src/ai/model/needle/needle/__init__.py`), which returns the raw
//! engine envelope plus a `confidence` score. This type is the single typed
//! home for that wire shape; consumers (the `NeedleBackend` trait, the
//! `NeedlePreFilter` stage) never touch the raw JSON.

use serde::{Deserialize, Serialize};

use crate::needle::NeedleError;

/// One grammar-constrained tool invocation produced by Needle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NeedleFunctionCall {
    /// Tool name — a route key (or action/target name) from the tool schema.
    pub name: String,
    /// Bound arguments as a JSON object. `Value` keeps the shape generic —
    /// argument binding is owned by the deterministic target layer.
    pub arguments: serde_json::Value,
}

/// The discriminator on a Needle envelope.
///
/// `call` carries tool invocations (routeable); `text`/`refuse` are the
/// decline paths — the pipeline falls through to the classifier rather than
/// acting on them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedleEnvelopeType {
    /// One or more tool calls — the actionable routing verdict.
    Call,
    /// A short structured label, not free prose — treated as a decline.
    Text,
    /// The model declines to act — treated as a decline.
    Refuse,
}

/// A typed Needle completion envelope.
///
/// Every field other than `type` is optional: the engine is free to omit
/// `success`/`error`/`reasoning`/`confidence` depending on the envelope kind,
/// and the caller must tolerate their absence (a `call` without `confidence`
/// is handled by the config's `decline_on_missing_confidence` policy).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NeedleEnvelope {
    #[serde(rename = "type")]
    pub r#type: NeedleEnvelopeType,
    /// Success flag, when the engine reports one explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    /// Human-readable error string, when the envelope reports a failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Numeric error code, when the engine reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<i64>,
    /// The tool invocations for a `call` envelope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub function_calls: Vec<NeedleFunctionCall>,
    /// Optional model reasoning attached to the decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Calibrated confidence in [0, 1]. `None` for finetuned weights (the
    /// confidence head is not updated by finetuning).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// Executed tool results, when the caller ran the calls (the `run()` path
    /// in the reference implementation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results: Option<serde_json::Value>,
}

impl NeedleEnvelope {
    /// Whether this envelope is an actionable tool call (the routing case).
    pub fn is_call(&self) -> bool {
        self.r#type == NeedleEnvelopeType::Call && !self.function_calls.is_empty()
    }

    /// The single routed tool name, when exactly one call is present.
    pub fn single_tool(&self) -> Option<&str> {
        if self.function_calls.len() == 1 {
            Some(self.function_calls[0].name.as_str())
        } else {
            None
        }
    }

    /// Parse an engine envelope from its wire JSON. Malformed JSON or an
    /// unknown `type` is a [`NeedleError::MalformedEnvelope`] — the engine's
    /// output is grammar-guaranteed, so this is an engine bug, not something
    /// to repair heuristically.
    pub fn parse(raw: &str) -> Result<Self, NeedleError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(NeedleError::MalformedEnvelope {
                detail: "empty envelope".into(),
            });
        }
        let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| {
            NeedleError::MalformedEnvelope {
                detail: format!("not valid JSON: {e}"),
            }
        })?;
        // The engine always reports a `type`; a missing/unknown discriminator
        // is an engine bug.
        let ty = value.get("type").and_then(serde_json::Value::as_str);
        let envelope: Self = match ty {
            Some("call" | "text" | "refuse") => {
                serde_json::from_value(value).map_err(|e| NeedleError::MalformedEnvelope {
                    detail: e.to_string(),
                })?
            }
            _ => {
                return Err(NeedleError::MalformedEnvelope {
                    detail: format!("missing or unknown envelope type: {ty:?}"),
                })
            }
        };
        Ok(envelope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope_json() -> serde_json::Value {
        json!({
            "type": "call",
            "confidence": 0.9,
            "function_calls": [{"name": "City", "arguments": {"city": "Paris"}}],
            "reasoning": "user asks for a city",
        })
    }

    #[test]
    fn parses_call_envelope() {
        let env = NeedleEnvelope::parse(&envelope_json().to_string()).expect("parse");
        assert_eq!(env.r#type, NeedleEnvelopeType::Call);
        assert!(env.is_call());
        assert_eq!(env.single_tool(), Some("City"));
        assert_eq!(env.confidence, Some(0.9));
        assert_eq!(env.reasoning.as_deref(), Some("user asks for a city"));
        let call = &env.function_calls[0];
        assert_eq!(call.arguments, json!({"city": "Paris"}));
    }

    #[test]
    fn parses_refuse_and_text_as_declines() {
        let refuse = NeedleEnvelope::parse(&json!({"type": "refuse"}).to_string()).expect("parse");
        assert_eq!(refuse.r#type, NeedleEnvelopeType::Refuse);
        assert!(!refuse.is_call());
        let text = NeedleEnvelope::parse(&json!({"type": "text"}).to_string()).expect("parse");
        assert_eq!(text.r#type, NeedleEnvelopeType::Text);
        assert!(!text.is_call());
    }

    #[test]
    fn parses_error_envelope() {
        let env = NeedleEnvelope::parse(
            &json!({"type": "call", "success": false, "error": "boom", "error_code": 7})
                .to_string(),
        )
        .expect("parse");
        assert_eq!(env.success, Some(false));
        assert_eq!(env.error.as_deref(), Some("boom"));
        assert_eq!(env.error_code, Some(7));
    }

    #[test]
    fn missing_fields_tolerated() {
        let env = NeedleEnvelope::parse(&json!({"type": "call"}).to_string()).expect("parse");
        assert_eq!(env.confidence, None);
        assert!(env.function_calls.is_empty());
        // A call with no function calls is not actionable.
        assert!(!env.is_call());
        assert_eq!(env.single_tool(), None);
    }

    #[test]
    fn call_with_multiple_tools_has_no_single_tool() {
        let env = NeedleEnvelope::parse(
            &json!({
                "type": "call",
                "function_calls": [
                    {"name": "a", "arguments": {}},
                    {"name": "b", "arguments": {}},
                ],
            })
            .to_string(),
        )
        .expect("parse");
        assert!(env.is_call());
        assert_eq!(env.single_tool(), None);
    }

    #[test]
    fn malformed_json_is_error() {
        let err = NeedleEnvelope::parse("{not json").expect_err("malformed");
        assert!(matches!(err, NeedleError::MalformedEnvelope { .. }));
    }

    #[test]
    fn empty_envelope_is_error() {
        let err = NeedleEnvelope::parse("   ").expect_err("empty");
        assert!(matches!(err, NeedleError::MalformedEnvelope { .. }));
    }

    #[test]
    fn unknown_type_is_error() {
        let err = NeedleEnvelope::parse(&json!({"type": "chat"}).to_string()).expect_err("unknown");
        assert!(matches!(err, NeedleError::MalformedEnvelope { .. }));
    }

    #[test]
    fn serde_round_trip() {
        let env = NeedleEnvelope::parse(&envelope_json().to_string()).expect("parse");
        let back: NeedleEnvelope =
            serde_json::from_str(&serde_json::to_string(&env).expect("serialize"))
                .expect("round trip");
        assert_eq!(back, env);
    }
}