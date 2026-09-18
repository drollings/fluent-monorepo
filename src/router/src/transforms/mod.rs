//! Transform control-plane interface: the [`TransformStrategy`] trait, the
//! [`TransformError`], the shared helpers ([`rewrite_text_messages`],
//! [`insert_string_map`]), and the [`TransformStrategyRef`] alias. The six
//! transform *implementations* live in the sibling modules — only the
//! interface lives here, owned once so stages share it without duplication.

pub mod codeword_anonymize;
pub mod decompose_hypothetical;
pub mod decompose_subtasks;
pub mod none;
pub mod pii_anonymize;
pub mod sanitize;
pub mod secret_mask;

#[cfg(test)]
#[path = "../../tests/transforms_tests.rs"]
mod tests;

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use fluent_types::openai_wire::{RouterMessageContent, RouterRequest};

#[derive(Debug, Error, Clone, Serialize, Deserialize)]
pub enum TransformError {
    #[error("transform failed: {0}")]
    Failed(String),
    #[error("transform not applicable: {0}")]
    NotApplicable(String),
}

pub trait TransformStrategy: Send + Sync {
    fn name(&self) -> &str;
    fn transform(&self, request: &RouterRequest) -> Result<RouterRequest, TransformError>;
}

pub type TransformStrategyRef = Arc<dyn TransformStrategy>;

/// Shared clone-messages/iterate/match boilerplate for transforms that rewrite
/// each `Text` message in place. `Parts` messages are left untouched.
///
/// Each transform becomes a thin closure: only the per-message rewrite
/// (and any side-channel it wants, e.g. a captured anonymize map) lives in the
/// caller.
pub fn rewrite_text_messages(
    request: &RouterRequest,
    mut rewrite: impl FnMut(&str) -> Result<String, TransformError>,
) -> Result<RouterRequest, TransformError> {
    let mut transformed = request.clone();
    for message in &mut transformed.messages {
        let text = match &message.content {
            RouterMessageContent::Text(s) => s.clone(),
            RouterMessageContent::Parts(_) => continue,
        };
        message.content = RouterMessageContent::Text(rewrite(&text)?);
    }
    Ok(transformed)
}

/// Insert a `HashMap<String,String>` into `request.metadata[key]` as a JSON
/// object of string values, **only when non-empty** (no spurious empty object).
pub fn insert_string_map<S>(request: &mut RouterRequest, key: &str, map: HashMap<String, String, S>)
where
    S: std::hash::BuildHasher,
{
    if map.is_empty() {
        return;
    }
    let obj: serde_json::Map<String, serde_json::Value> = map
        .into_iter()
        .map(|(k, v)| (k, serde_json::Value::String(v)))
        .collect();
    request
        .metadata
        .insert(key.into(), serde_json::Value::Object(obj));
}
