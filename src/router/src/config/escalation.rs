//! Escalation-ladder configuration
//!
//! Roles carry the routing member lists; an [`EscalationLadderConfig`] may be
//! attached to a role (`roles.<role>.escalation`). The ladder types are plain
//! data; the runtime that consumes them lives in
//! `crate::dispatch::escalation`.

use serde::{Deserialize, Serialize};

use crate::frontier::modes::EscalationMode;

/// One member of a role `models` list: either a literal model key or an
/// availability-ordering sentinel. `Last` expands to the role's
/// most-recently-successful key (when still a member); `Any` orders the
/// currently-loaded members first, in config order. Both only *order*
/// candidates — the intelligence climb underneath is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleMember {
    /// A literal model key (possibly qualified, e.g. `qwen3.5-9b:quick`).
    Key(String),
    /// The role's most-recently-successful member.
    Last,
    /// Any loaded member first, then the rest in config order.
    Any,
}

impl RoleMember {
    /// Parse one raw role member. Only the bare, lowercase `last`/`any`
    /// spellings are sentinels (case-sensitive); every other spelling —
    /// including qualified forms like `quick:last` — stays a literal key and
    /// keeps today's fail-closed lookup when unknown.
    pub fn parse(raw: &str) -> Self {
        match raw {
            "last" => RoleMember::Last,
            "any" => RoleMember::Any,
            _ => RoleMember::Key(raw.to_string()),
        }
    }

    /// The raw member spelling this parsed from (identity round trip).
    pub fn raw(&self) -> &str {
        match self {
            RoleMember::Key(key) => key,
            RoleMember::Last => "last",
            RoleMember::Any => "any",
        }
    }
}

/// The frontier endpoint a ladder dispatches to, plus the model name it asks
/// that endpoint to use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrontierConfig {
    /// OpenAI-compatible chat-completions base URL. Reuses the canonical
    /// `dispatch/backend.rs` transport — no third HTTP path.
    pub endpoint: String,
    /// Name of the environment variable holding the API key. Optional for
    /// local OpenAI-compatible endpoints; when present the key is injected as
    /// a `Bearer` token on the frontier request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// The model name sent to the frontier endpoint.
    pub model: String,
}

/// The full escalation ladder for one role (`roles.<role>.escalation`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationLadderConfig {
    /// Ordered escalation modes; tried filter → question → team → turnover.
    /// Empty/absent → no frontier escalation for this group.
    #[serde(default)]
    pub modes: Vec<EscalationMode>,
    #[serde(default)]
    pub frontier: Option<FrontierConfig>,
    /// Local model keys (referencing `models`) for the per-mode roles.
    /// Missing roles disable the modes that require them.
    #[serde(default)]
    pub decomposer_model: Option<String>,
    #[serde(default)]
    pub assembler_model: Option<String>,
    #[serde(default)]
    pub classifier_model: Option<String>,
    /// Number of parallel classifier slots in team mode.
    #[serde(default = "default_classifier_parallel")]
    pub classifier_parallel: usize,
    #[serde(default)]
    pub draft_model: Option<String>,
    #[serde(default)]
    pub judge_model: Option<String>,
}

impl Default for EscalationLadderConfig {
    fn default() -> Self {
        Self {
            modes: Vec::new(),
            frontier: None,
            decomposer_model: None,
            assembler_model: None,
            classifier_model: None,
            classifier_parallel: default_classifier_parallel(),
            draft_model: None,
            judge_model: None,
        }
    }
}

const fn default_classifier_parallel() -> usize {
    3
}
#[cfg(test)]
#[path = "../../tests/config_escalation.rs"]
mod tests;
