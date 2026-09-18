//! LedgerGroupConfig (M8).
use serde::{Deserialize, Serialize};

use super::OrchestratorSection;
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LedgerGroupConfig {
    #[serde(default)]
    pub ledger: Option<crate::config::LedgerConfig>,
}

/// Default cap on the ledger `Summarizer`'s summary length (tokens). Only a
/// named constant - `LedgerConfig.max_summary_tokens` defaults to it.
pub const DEFAULT_LEDGER_MAX_SUMMARY_TOKENS: u32 = 200;

/// Ledger composition section - the `ledger` block of `RouterConfig`.
///
/// `Some` opts the composition root (`main.rs`) into opening a
/// `ContentNodeStore` and attaching a `Summarizer` backend targeting the
/// named `ledger` instance. `None` (absent) keeps today's behavior - no
/// ledger at boot - so existing deployments are untouched until they opt in.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerConfig {
    /// Durable store path. `None` falls back to an in-memory ledger with a
    /// `warn!` (ephemeral, still functional for LOD derivation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Model key for the ledger `Summarizer`. `None` falls back to the
    /// classifier model key, then to no summarizer. Legacy spelling: prefer
    /// `role` (duties name roles); an explicit `model` still resolves when
    /// no role is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Role serving the ledger duty, resolved through
    /// [`crate::config::role_head_key`].
    /// Wins over `model` when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Max summary length (tokens) for LOD1-LOD4 derivation.
    #[serde(default = "default_ledger_max_summary_tokens")]
    pub max_summary_tokens: u32,
    /// Enable continuous background LOD4/LOD5 generation. `false` (the
    /// default) keeps today's lazy-on-demand behavior.
    #[serde(default)]
    pub background_tiering: bool,
    /// Model key for the tier worker's labeler/summarizer. `None` falls back
    /// to the ledger model, then the classifier model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_model: Option<String>,
    /// Max characters for LOD4 (short summary). Default 240 (§0.3).
    #[serde(default = "default_lod4_max_chars")]
    pub lod4_max_chars: usize,
    /// Max characters for LOD5 (description). Default 80 (§0.3).
    #[serde(default = "default_lod5_max_chars")]
    pub lod5_max_chars: usize,
    /// Tier worker batch size (nodes drained per poll).
    #[serde(default = "default_tier_batch_size")]
    pub tier_batch_size: usize,
    /// Tier worker poll interval (ms).
    #[serde(default = "default_tier_poll_interval_ms")]
    pub tier_poll_interval_ms: u64,
    /// Credit granted to the tier feed's producer up front: the max
    /// outstanding `NodeId`s the async (credit-gated) enqueue path may have in
    /// flight before it blocks, bounding a burst of agent turns. Default 256.
    #[serde(default = "default_tier_credit_limit")]
    pub tier_credit_limit: usize,
    /// How many processed nodes the tier worker waits for before bumping
    /// credit back to the producer. Default 8.
    #[serde(default = "default_tier_credit_more_after")]
    pub tier_credit_more_after: usize,
    /// Ledger-agent coordinator section. `enabled = true` opts the boot
    /// path into attaching a `LedgerAgentCoordinator` to the server so a
    /// request with a session + ledger runs through its synchronization loop
    /// (`restore-or-assemble → execute → record → snapshot → enqueue`).
    /// Default-absent so existing deployments are untouched.
    #[serde(default)]
    pub orchestrator: OrchestratorSection,
    /// Workflow replay loop (M14a): record solved runs as `Target` DAGs and
    /// replay near-neighbor runs without frontier calls. Default-off
    /// (`enabled = false`) so existing deployments are untouched; thresholds
    /// arrive pre-set from calibration.
    #[serde(default)]
    pub replay: crate::ledger::workflow_replay::ReplayConfig,
}

const fn default_ledger_max_summary_tokens() -> u32 {
    DEFAULT_LEDGER_MAX_SUMMARY_TOKENS
}

const fn default_lod4_max_chars() -> usize {
    240
}

const fn default_lod5_max_chars() -> usize {
    80
}

const fn default_tier_batch_size() -> usize {
    8
}

const fn default_tier_poll_interval_ms() -> u64 {
    100
}

const fn default_tier_credit_limit() -> usize {
    256
}

const fn default_tier_credit_more_after() -> usize {
    8
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            path: None,
            model: None,
            role: None,
            max_summary_tokens: DEFAULT_LEDGER_MAX_SUMMARY_TOKENS,
            background_tiering: false,
            tier_model: None,
            lod4_max_chars: default_lod4_max_chars(),
            lod5_max_chars: default_lod5_max_chars(),
            tier_batch_size: default_tier_batch_size(),
            tier_poll_interval_ms: default_tier_poll_interval_ms(),
            tier_credit_limit: default_tier_credit_limit(),
            tier_credit_more_after: default_tier_credit_more_after(),
            orchestrator: OrchestratorSection::default(),
            replay: crate::ledger::workflow_replay::ReplayConfig::default(),
        }
    }
}
