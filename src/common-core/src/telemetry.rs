//! Failure taxonomy — the generic observability contract.
//!
//! The domain-free failure taxonomy; `fluent_llm::http_class` re-exports
//! it as the single `FailureClass` definition (do not duplicate).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    Network,
    Authentication,
    RateLimit,
    InputValidation,
    Storage,
    Timeout,
    Internal,
    Unknown,
}

impl FailureClass {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Authentication => "authentication",
            Self::RateLimit => "rate_limit",
            Self::InputValidation => "input_validation",
            Self::Storage => "storage",
            Self::Timeout => "timeout",
            Self::Internal => "internal",
            Self::Unknown => "unknown",
        }
    }
}
