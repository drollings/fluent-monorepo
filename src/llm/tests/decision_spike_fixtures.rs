//! Shared hermetic fixtures for decision decoder goldens.
//!
//! Redacted live-response shapes captured from the M0 spike: a Choice
//! response with `B` top plus a leading-space variant, and a Noul response
//! with `Yes` top. Structural data only — never model output quality.
//! Included via `#[path]` into decoder tests; not a test target itself.

/// Choice fixture: single-token `B` answer with variant split.
#[must_use]
pub fn choice_b_response() -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-decision-fixture",
        "object": "chat.completion",
        "created": 0,
        "model": "fixture",
        "choices": [{
            "index": 0,
            "finish_reason": "stop",
            "message": {"role": "assistant", "content": "B"},
            "logprobs": {
                "content": [{
                    "token": "B",
                    "logprob": -0.022,
                    "top_logprobs": [
                        {"token": "B", "logprob": -0.022},
                        {"token": " B", "logprob": -3.5},
                        {"token": "A", "logprob": -4.2},
                        {"token": "C", "logprob": -5.1}
                    ]
                }]
            }
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 1, "total_tokens": 11}
    })
}

/// Noul fixture: single-token `Yes` answer with case/space variants.
#[must_use]
pub fn noul_yes_response() -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-decision-fixture",
        "object": "chat.completion",
        "created": 0,
        "model": "fixture",
        "choices": [{
            "index": 0,
            "finish_reason": "stop",
            "message": {"role": "assistant", "content": "Yes"},
            "logprobs": {
                "content": [{
                    "token": "Yes",
                    "logprob": -0.24,
                    "top_logprobs": [
                        {"token": "Yes", "logprob": -0.24},
                        {"token": " Yes", "logprob": -2.8},
                        {"token": "yes", "logprob": -3.1},
                        {"token": "No", "logprob": -4.5}
                    ]
                }]
            }
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 1, "total_tokens": 11}
    })
}
