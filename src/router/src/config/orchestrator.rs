//! Ledger-agent coordinator configuration — the `ledger.orchestrator`
//! section of `RouterConfig`.

use serde::{Deserialize, Serialize};

/// The `ledger.orchestrator` section: configures the
/// `LedgerAgentCoordinator`'s restore-vs-re-prefill policy, its prompt budget,
/// and the default role recorded for agent output nodes.
///
/// Default-on since M14c (M14-CAL projects a net frontier decrease):
/// a present `ledger` section attaches the coordinator unless the operator
/// opts out with `enabled = false` or rolls back at runtime via
/// `POST /admin/coordinator/rollback` (`coral-router coordinator-rollback`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorSection {
    /// Whether to attach the coordinator at boot. `true` (the default)
    /// attaches it wherever a ledger is configured; `false` opts out and
    /// leaves the server's dispatch path on the pre-coordinator pipeline.
    #[serde(default = "default_orchestrator_enabled")]
    pub enabled: bool,
    /// The restore-vs-re-prefill decision rule for per-model KV snapshots.
    #[serde(default)]
    pub kv_policy: crate::dag_session::KvSnapshotPolicy,
    /// The worker's context-window budget (characters) for prompt assembly.
    /// Default 32768 (8192 tokens × 4 chars/token).
    #[serde(default = "default_orchestrator_prompt_budget_chars")]
    pub prompt_budget_chars: usize,
    /// Default role recorded for agent output nodes.
    #[serde(default = "default_orchestrator_role")]
    pub role: String,
    /// Optional concurrency cap for the coordinator's KV-affinity scheduler.
    /// `Some(cap)` attaches an `AffinityScheduler` bounded by `cap` concurrent
    /// agent turns: the active session's turns get a priority bonus (minimize
    /// context switches) while starved sessions age up. `None` (the default)
    /// leaves affinity bookkeeping off — existing deployments are untouched.
    #[serde(default)]
    pub affinity_cap: Option<usize>,
}

const fn default_orchestrator_enabled() -> bool {
    true
}

const fn default_orchestrator_prompt_budget_chars() -> usize {
    32768
}

fn default_orchestrator_role() -> String {
    "agent".into()
}

impl Default for OrchestratorSection {
    fn default() -> Self {
        Self {
            enabled: default_orchestrator_enabled(),
            kv_policy: crate::dag_session::KvSnapshotPolicy::RestoreIfSameModel,
            prompt_budget_chars: default_orchestrator_prompt_budget_chars(),
            role: default_orchestrator_role(),
            affinity_cap: None,
        }
    }
}
