//! Chat role vocabulary — the closed set of OpenAI-compatible message roles.
//!
//! The wire type stays `String` on `ChatMessage` so unknown roles pass through
//! the protocol boundary unchanged. Decision points parse via
//! `ChatRole :: try_from` / `FromStr` and refuse unknown input instead of
//! silently accepting it. The parse result measures correctness of the
//! input for the task, never producer confidence.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Closed vocabulary of chat message roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

/// Refusal for reserved role input outside the closed vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown chat role: {0}")]
pub struct RoleParseError(pub String);

impl ChatRole {
    /// Canonical wire string for the role.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

impl std::str::FromStr for ChatRole {
    type Err = RoleParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "system" => Ok(Self::System),
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            "tool" => Ok(Self::Tool),
            _ => Err(RoleParseError(s.to_string())),
        }
    }
}

impl TryFrom<&str> for ChatRole {
    type Error = RoleParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl std::fmt::Display for ChatRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[path = "../tests/roles.rs"]
mod tests;
