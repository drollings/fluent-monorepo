//! Rigor-route configuration — the `rigor` section of `RouterConfig`,
//! consumed by the rigor route builder.

use serde::{Deserialize, Serialize};

use super::{BoundedRounds, EscalationConfidence, SeverityThreshold};

/// Rigor-route configuration - the `rigor` section of `RouterConfig`.
///
/// Model keys select entries from `config.models`; backends are built **only**
/// in `coral-router`'s `build_rigor_route` (DIP, mirroring
/// `build_plan_route`/`default_adjudicator_backend`). `None` at the
/// `RouterConfig` level leaves `/v1/rigor` present but unconfigured.
#[derive(Debug, Clone, Serialize, Deserialize)]
/// Rigor-route configuration - the `rigor` section of `RouterConfig`.
/// 
/// Model keys select entries from `config.models`; backends are built **only**
/// in `coral-router`'s `build_rigor_route` (DIP, mirroring
/// `build_plan_route`/`default_adjudicator_backend`). `None` at the
/// `RouterConfig` level leaves `/v1/rigor` present but unconfigured.
pub struct RigorConfig {
    /// Model key for the blue-team candidate-answer backend.
    #[serde(default)]
    pub blue_model: Option<String>,
    /// Model key for the red-team objections backend.
    #[serde(default)]
    pub red_model: Option<String>,
    /// Model key for the judge backend.
    #[serde(default)]
    pub judge_model: Option<String>,
    /// Whether the route expects KV-cache checkpoint/rewind to be load-bearing
    /// (a `DependencySession` with a `SnapshotStore`). Rewind always resets
    /// steps; this flag only gates the KV-restore expectation.
    #[serde(default)]
    pub kv_cache_enabled: bool,
    /// Max blue/red/judge passes. Fixed round count (VISION: terminate, don't
    /// loop); a material rejection triggers **at most one** re-run of
    /// blue+judge. Default 2.
    #[serde(default)]
    pub max_passes: BoundedRounds,
    /// Objection severity at/above which a judge rejection is **material**
    /// (triggers rewind + the second blue pass). Default 0.7.
    #[serde(default)]
    pub severity_threshold: SeverityThreshold,
    /// Judge confidence below which a final rejection escalates to frontier.
    /// An explicit config value - never "red scored a point".
    /// Default 0.4.
    #[serde(default)]
    pub escalation_confidence: EscalationConfidence,
}

#[allow(clippy::derivable_impls)]
impl Default for RigorConfig {
    fn default() -> Self {
        Self {
            blue_model: None,
            red_model: None,
            judge_model: None,
            kv_cache_enabled: false,
            max_passes: BoundedRounds::default(),
            severity_threshold: SeverityThreshold::default(),
            escalation_confidence: EscalationConfidence::default(),
        }
    }
}