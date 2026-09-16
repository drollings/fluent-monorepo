//! Router configuration types - deserialized from JSON via `common_core::config`.
//!
//!  This file is a thin facade (M3) - the authoritative types live in
//!  `config/root.rs`. It re-exports submodules and root types so
//!  `crate::config::RouterConfig` remains stable.

pub mod addr;
pub mod builder;
pub mod classification;
pub mod compiled_tree;
pub mod escalation;
pub mod filters;
pub mod ledger_group;
pub mod refine_policy;
pub mod root;
pub mod rounds;
pub mod routing;
pub mod runtime;
pub mod serving;
pub mod tree_reload;

pub use self::addr::{hosts_equivalent, parse_bind_addr, validate_no_self_routing};
pub use self::builder::{NlpConfig, NlpOrdering, PipelineParams, TargetMatchMode};
pub use self::classification::{ClassificationChild, ClassificationNode, ClassificationTree};
pub use self::compiled_tree::{CompileError, CompiledTree};
pub use self::escalation::{EscalationLadderConfig, FrontierConfig, GroupMember, ModelGroup};
pub use self::filters::{
    CommandConfig, ConfidenceGate, FilterAction, FilterOutcome, FilterScope, MockConfig,
    PatternEntry, RejectPatterns,
};
pub use self::refine_policy::{RouterRefineMode, RouterRefinePolicy};
#[allow(unused_imports)]
pub(crate) use self::root::materialize_effective_pool;
pub use self::root::split_model_key;
pub use self::root::{
    detect_device_vram_total, models_serving_role, overlay_params, resolve_group_head_key,
    resolve_inference_point, role_head_key, strip_declaration_params, warn_on_non_object_params,
    ArcReadyConfig, AuditLogConfig, ChartsConfig, ClassifierFailurePolicy, ClassifierOutput,
    EvictionPolicy, InstanceProfile, LedgerConfig, ModelBinding, ModelEntry, ModelRoleOverride,
    OrchestratorSection, OverlayConfig, PostProcessConfig, ReviewConfig, RigorConfig, RoleParams,
    RouterConfig, ServerConfig, SessionConfig, SidecarConfig, WorkflowExtractionMode,
    DECLARATION_PARAM_KEYS, DEFAULT_LEDGER_MAX_SUMMARY_TOKENS,
};
pub use self::rounds::{BoundedRounds, EscalationConfidence, SeverityThreshold};
pub use self::routing::{
    expanded_group_members, select_group_member, strip_model_prefix, RequestedRef, RoleConcurrency,
    RoleContext, RoleEntry, RouteRef, RoutingConfig, MODEL_PREFIX,
};
pub use common_core::constants::default_true;

/// Derived config schema inventory: assembled from the config types'
/// [`Describable`](fluent_wvr::Describable) documents — the single source of
/// truth, never a hand-kept file. The `coral-router schema` subcommand
/// prints this document; the boot gate validates through the same types, so
/// the schema cannot drift from what the loader accepts.
pub fn config_schema() -> serde_json::Value {
    use fluent_wvr::Describable;
    serde_json::json!({
        "source": "derived from config types via Describable",
        "sections": {
            "config": RouterConfig::default().describe(),
            "classifier_output": ClassifierOutput::default().describe(),
        },
    })
}
