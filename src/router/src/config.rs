//! Router configuration types - deserialized from JSON via `common_core::config`.
//!
//!  This file is a thin facade (M3) - the authoritative types live in the
//!  `config/` submodules (`root.rs` holds `RouterConfig` itself; each section
//!  lives next to its domain). It re-exports submodules and section types so
//!  `crate::config::RouterConfig` and every `crate::config::X` section path
//!  remain stable.

pub mod addr;
pub mod audit;
pub mod builder;
pub mod charts;
pub mod classification;
pub mod compiled_tree;
pub mod escalation;
pub mod filters;
pub mod fleet;
pub mod frame;
pub mod ledger_group;
pub mod models;
pub mod normalize;
pub mod orchestrator;
pub mod overlay;
pub mod refine_policy;
pub mod review;
pub mod rigor;
pub mod root;
pub mod rounds;
pub mod routing;
pub mod runtime;
pub mod server;
pub mod serving;
pub mod session;
pub mod sidecar;
pub mod tree_reload;

pub use self::addr::{hosts_equivalent, parse_bind_addr, validate_no_self_routing};
pub use self::audit::AuditLogConfig;
pub use self::builder::{NlpConfig, NlpOrdering, PipelineParams, TargetMatchMode};
pub use self::charts::{ChartsConfig, PostProcessConfig, WorkflowExtractionMode};
#[allow(unused_imports)]
pub(crate) use self::classification::default_classifier_failure_policy;
pub use self::classification::{
    ClassificationChild, ClassificationNode, ClassificationTree, ClassifierFailurePolicy,
    ClassifierOutput,
};
pub use self::compiled_tree::{CompileError, CompiledTree};
pub use self::escalation::{EscalationLadderConfig, FrontierConfig, RoleMember};
pub use self::filters::{
    CommandConfig, ConfidenceGate, FilterAction, FilterOutcome, FilterScope, MockConfig,
    PatternEntry, RejectPatterns,
};
pub use self::fleet::RoleParams;
#[allow(unused_imports)]
pub(crate) use self::fleet::{materialize_effective_pool, role_window_templates};
pub use self::frame::FrameResolutionConfig;
pub use self::ledger_group::{LedgerConfig, DEFAULT_LEDGER_MAX_SUMMARY_TOKENS};
pub use self::models::{
    InstanceProfile, ModelEntry, ModelRoleOverride, SnapshotConfig, SnapshotMode,
};
pub use self::normalize::{
    strip_declaration_params, warn_on_non_object_params, DECLARATION_PARAM_KEYS,
};
pub use self::orchestrator::OrchestratorSection;
pub use self::overlay::{overlay_params, ArcReadyConfig, OverlayConfig};
pub use self::refine_policy::{RouterRefineMode, RouterRefinePolicy};
pub use self::review::ReviewConfig;
pub use self::rigor::RigorConfig;
pub use self::root::RouterConfig;
pub use self::rounds::{BoundedRounds, EscalationConfidence, SeverityThreshold};
#[allow(unused_imports)]
pub(crate) use self::routing::default_inference_point;
pub use self::routing::{
    expanded_role_members, fleet_default_member, models_serving_role, resolve_inference_point,
    role_head_key, select_role_member, select_viable_member, strip_model_prefix, RequestedRef,
    RoleEntry, RouteRef, RoutingConfig, MODEL_PREFIX,
};
pub use self::server::ServerConfig;
pub use self::session::SessionConfig;
pub use self::sidecar::{detect_device_vram_total, EvictionPolicy, SidecarConfig};
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
