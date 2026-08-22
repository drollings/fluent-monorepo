//! Router configuration types - deserialized from JSON via `common_core::config`.
//!
//! TODO(config.rs split, OPTIONAL): this file is large (the biggest
//! production file in the workspace) but it is *cohesive config data*, not a
//! logic god-file: ~42 serde structs, no duplicated logic. A future split is
//! optional, NOT required by this roadmap. If split, group by concern
//! (e.g. `RouterConfig`/`ModelEntry`/`SidecarConfig`/`PipelineParams`), but do
//! not undertake it while the structs remain serde-heavy and cohesive.

pub mod addr;
pub mod builder;
pub mod classification;
pub mod escalation;
pub mod filters;
pub mod routing;

pub use self::addr::{hosts_equivalent, parse_bind_addr, validate_no_self_routing};
pub use self::builder::{PipelineParams, TargetMatchMode};
pub use self::classification::{
    ClassificationChild, ClassificationNode, ClassificationTree, ClassifierBackend,
};
pub use self::escalation::{EscalationLadderConfig, FrontierConfig, ModelGroup};
pub use self::filters::{
    CommandConfig, ConfidenceGate, FilterAction, FilterOutcome, FilterScope, MockConfig,
    PatternEntry, RejectPatterns,
};
pub use self::routing::{RouteRef, RoutingConfig};

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use fluent_wvr::{Describable, FieldAccess};

use crate::logging::LoggingConfig;
use crate::score_matrix::ScoreMatrix;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterConfig {
    #[serde(default)]
    pub pipelines: HashMap<String, PipelineParams>,
    #[serde(default)]
    pub models: HashMap<String, ModelEntry>,
    #[serde(default)]
    pub model_groups: HashMap<String, ModelGroup>,
    #[serde(default)]
    pub routes: HashMap<String, RouteRef>,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub safety_threshold: f64,
    #[serde(default = "default_route")]
    pub default_route: String,
    /// What the classifier stage does when its LLM call fails or its response
    /// cannot be parsed. Safe default: reject rather than route on fabricated
    /// scores.
    #[serde(default = "default_classifier_failure_policy")]
    pub classifier_failure_policy: ClassifierFailurePolicy,
    #[serde(default = "ServerConfig::default")]
    pub server: ServerConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub classifier_model: Option<String>,
    /// Chart-embedding model key (HNSW index). Selects an entry from
    /// `models`. `None` falls back to `charts.selector_model`, then
    /// `classifier_model`.
    #[serde(default)]
    pub embedding_model: Option<String>,
    /// Chart-candidate reranker model key. Selects an entry
    /// from `models`. `None` skips the rerank stage (Step 2 - Step 3
    /// directly).
    #[serde(default)]
    pub reranker_model: Option<String>,
    #[serde(default)]
    pub mock: Option<MockConfig>,
    #[serde(default)]
    pub score_matrix: Option<ScoreMatrix>,
    /// Chart store configuration (DAG workflow library).
    #[serde(default)]
    pub charts: ChartsConfig,
    /// Post-processing configuration.
    #[serde(default)]
    pub post_process: PostProcessConfig,
    /// Nested classification tree.  `Some` switches the classifier stage
    /// into tree-driven mode; the flat pipeline sections remain for
    /// backward compatibility and are derived from the tree where the rest
    /// of the server needs flat views.
    #[serde(default)]
    pub classification: Option<ClassificationTree>,
    /// Rigor-route configuration. `None` (the default) leaves the route
    /// present but unconfigured - requests return an explicit `Unconfigured`
    /// error, never a crash.
    #[serde(default)]
    pub rigor: Option<RigorConfig>,
    /// Sidecar instance-management policy. Governs the sidecar task that
    /// reconciles the fork's shared-weight instances against the configured
    /// profiles, polls `/memory`, and evicts/allocates KV + compute only (the
    /// weights stay loaded in `llama-server`).
    #[serde(default)]
    pub sidecar: SidecarConfig,
    /// Ledger composition section. `Some` opts the boot path into opening
    /// a `ContentNodeLedger` (with a real `Summarizer` backend targeting
    /// `<base>:ledger`) so LOD derivation exists at runtime. `None` (the
    /// default) leaves today's behavior - no ledger at boot.
    #[serde(default)]
    pub ledger: Option<LedgerConfig>,
    /// Session composition section. `Some` opts the boot path into a
    /// `SessionRegistry` (canonical session home) so rigor rewind and
    /// checkpoint/rewind state exist at runtime. `None` (the default) leaves
    /// today's behavior - no session registry at boot.
    #[serde(default)]
    pub session: Option<SessionConfig>,
    /// Default "how a model is run" parameters (the `default_params` block).
    /// Applied to every managed model that does not declare the key itself.
    #[serde(default)]
    pub default_params: DefaultModelParams,
    /// GGUF model directory for the admin CLI commands (`list`, `scan`, `rm`,
    /// `show`, `pull`, and `ps` weights resolution). Overridable per-invocation
    /// with `--gguf-dir`; `None` falls back to the built-in default.
    #[serde(default)]
    pub gguf_dir: Option<String>,
    /// Needle configuration - the dedicated top-level `needle` block. `Some`
    /// declares the Needle pre-filter rung (subject to `NeedleConfig.enabled`);
    /// `None` (absent) keeps today's behavior with no Needle hop in the
    /// pipeline. Needle is a separate engine path and never enters the
    /// llama-server supervisor / instances / VRAM machinery.
    #[serde(default)]
    pub needle: Option<NeedleConfig>,
    /// Read-only lookup-store wiring for tool-plan `Lookup` steps. Controls
    /// which backing stores the boot path installs `ToolLookup` resolvers over
    /// (see `server/tool_lookup/`). Stores that are already configured (the
    /// ledger, the session registry, the chart store) are always used when
    /// present; this section only adds the stores with no other config home.
    #[serde(default)]
    pub tool_lookups: ToolLookupConfig,
}

impl Default for RouterConfig {
    fn default() -> Self {
        let mut pipelines = HashMap::new();
        pipelines.insert("default".into(), PipelineParams::default());
        Self {
            pipelines,
            models: HashMap::new(),
            model_groups: HashMap::new(),
            routes: HashMap::new(),
            system_prompt: String::new(),
            safety_threshold: 0.5,
            default_route: "local".into(),
            classifier_failure_policy: ClassifierFailurePolicy::Reject,
            server: ServerConfig::default(),
            logging: LoggingConfig::default(),
            classifier_model: None,
            embedding_model: None,
            reranker_model: None,
            mock: None,
            score_matrix: None,
            charts: ChartsConfig::default(),
            post_process: PostProcessConfig::default(),
            classification: None,
            rigor: None,
            sidecar: SidecarConfig::default(),
            ledger: None,
            session: None,
            default_params: DefaultModelParams::default(),
            gguf_dir: None,
            needle: None,
            tool_lookups: ToolLookupConfig::default(),
        }
    }
}

impl RouterConfig {
    /// Merge the `default_params` sampling defaults into every model entry that
    /// does not declare its own values (per-model values win). Call once after
    /// config load so the rest of the crate sees fully-materialized params.
    ///
    /// Only the sampling `params` object is merged here — the server-launch
    /// defaults (`batch_size`, KV cache types, GPU offload, context size) are
    /// consumed directly by the supervisor (`build_server_args`).
    pub fn apply_defaults(&mut self) {
        let Some(default_params) = self.default_params.params.clone() else {
            return;
        };
        let serde_json::Value::Object(defaults) = default_params else {
            return;
        };
        for entry in self.models.values_mut() {
            let Some(serde_json::Value::Object(existing)) = entry.params.as_mut() else {
                entry.params = Some(serde_json::Value::Object(defaults.clone()));
                continue;
            };
            for (key, value) in &defaults {
                existing.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
    }

    /// The flat `routes` view the server consumes (model - pipeline mapping).
    ///
    /// Flat configs return `routes` unchanged. When a classification tree is
    /// configured, every `terminal` node whose route has no explicit entry gets
    /// a synthesized `RouteRef` (routed through the terminal's own `group`, or
    /// the route name when no group is given) so `RoutingConfig::resolve_route`
    /// and `resolve_pipeline` work with no structural change to the server.
    pub fn routes_view(&self) -> HashMap<String, RouteRef> {
        let mut routes = self.routes.clone();
        if let Some(tree) = &self.classification {
            for (route, group, description) in tree.terminal_views() {
                routes.entry(route.clone()).or_insert(RouteRef {
                    group: group.unwrap_or_else(|| route.clone()),
                    pipelines: vec!["default".into()],
                    description,
                    always_route: false,
                });
            }
        }
        routes
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default)]
    pub bind_addr: String,
    #[serde(default = "default_max_payload")]
    pub max_payload: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: String::new(),
            max_payload: default_max_payload(),
        }
    }
}

fn default_max_payload() -> usize {
    1048576
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    #[serde(default)]
    pub name: Option<String>,
    pub endpoint: String,
    pub intelligence: u8,
    pub cost_input: f64,
    pub cost_output: f64,
    pub cost_cached_read: f64,
    pub speed: u8,
    #[serde(default = "default_total_timeout_ms")]
    pub total_timeout_ms: u64,
    #[serde(default = "default_idle_timeout_ms")]
    pub idle_timeout_ms: u64,
    #[serde(default = "default_true")]
    pub stream: bool,
    #[serde(default)]
    pub filter_thinking: bool,
    #[serde(default)]
    pub retry_count: u32,
    #[serde(default = "default_retry_interval")]
    pub retry_base_interval_s: u64,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    /// Instance-pool declaration for the fork's shared-weight instances. The
    /// old `sessions` key is accepted as an alias during the transition.
    #[serde(default, alias = "sessions")]
    pub instances: Option<HashMap<String, InstanceProfile>>,
    /// Local GGUF weights file path. When set (or when `hf_repo` or `instances`
    /// is set), Coral Router is the process owner: it spawns and supervises a
    /// dedicated `llama-server` for this model on a free localhost port and
    /// rewrites `endpoint` to it at boot. Passed to the server as `--model`.
    #[serde(default)]
    pub weights: Option<String>,
    /// HuggingFace repo to load (`-hf <repo>[:quant]`), the on-demand
    /// alternative to `weights`. The repo name also becomes the server's
    /// primary model alias when `name` is unset.
    #[serde(default)]
    pub hf_repo: Option<String>,
    /// HuggingFace file within `hf_repo` (`-hff <file>`); optional, overrides
    /// the quant default.
    #[serde(default)]
    pub hf_file: Option<String>,
}

impl ModelEntry {
    /// Whether Coral Router manages a dedicated `llama-server` process for this
    /// model (the model declares a weights source or an instance pool). Managed
    /// models are spawned on a free localhost port at boot and their `endpoint`
    /// is rewritten to the spawned server.
    pub fn is_managed(&self) -> bool {
        self.weights.is_some() || self.hf_repo.is_some() || self.instances.is_some()
    }

    /// The model name handed to the spawned `llama-server` (`--alias`): the
    /// configured llama.cpp model name, else the HF repo, else the config key.
    pub fn llama_model_name(&self, model_key: &str) -> String {
        self.name
            .clone()
            .or_else(|| self.hf_repo.clone())
            .unwrap_or_else(|| model_key.to_string())
    }
}

/// One config-declared instance profile. The map key on `ModelEntry.instances`
/// provides the default instance name; `count > 1` expands into sibling
/// instances named `<key>-0` .. `<key>-{count-1}` sharing the profile's group.
/// Sampling `params` are merged into the request body for dispatches through
/// these instances; declaration-only keys (`num_ctx`/`parallel`/
/// `sleep_idle_seconds`) are stripped before dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct InstanceProfile {
    /// Instance name; default = the map key (expanded `<name><i>` for count > 1).
    #[serde(default)]
    pub name: Option<String>,
    /// Group; default = instance name. count > 1 instances share this group.
    #[serde(default)]
    pub group: Option<String>,
    /// Number of sibling instances this profile expands to (1 = single instance).
    #[serde(default = "default_instance_count")]
    pub count: u32,
    /// Context size in tokens.
    pub num_ctx: u64,
    /// Slots per instance; default = inherit server global.
    #[serde(default)]
    pub parallel: Option<u32>,
    /// Exempt from auto-sleep and in-process eviction; implies no_sleep.
    #[serde(default)]
    pub pinned: bool,
    /// Never auto-sleep (stays warm); the fork grammar's sleep=0. `warm` is a
    /// friendly serde alias for the same flag.
    #[serde(default, alias = "warm")]
    pub no_sleep: bool,
    /// >0 = per-instance idle timeout seconds; -1 = inherit global; None = inherit.
    #[serde(default)]
    pub sleep_idle_seconds: Option<i32>,
    /// Target of a bare `<base>` request.
    #[serde(default)]
    pub default: bool,
    /// Preserve this context across eviction: when the router must free VRAM,
    /// the context's KV cache is snapshotted (and its session transcript is
    /// already durable in the ledger) before it is dropped, so a later request
    /// can resume it with `snapshot=<name>-resume`. `pinned` contexts are never
    /// evicted, so `resume` is moot on them. Cleared at runtime (explicitly via
    /// `POST /instances/:name/no-resume`, or automatically after
    /// `sidecar.resume_ttl_s` of idle) when Coral Router concludes the work is
    /// done - the snapshot is then deleted.
    #[serde(default)]
    pub resume: bool,
    /// Sampling params merged into the request body for dispatches through this
    /// instance.
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

fn default_instance_count() -> u32 {
    1
}

impl ModelEntry {
    /// The expanded flat list of `InstanceProfile`s for this model: applies
    /// `count` expansion (naming each sibling `<key>-0` .. `<key>-{count-1}`)
    /// and resolves the name/group defaults (name = map key, group = name when
    /// absent). Empty when no instances are configured.
    pub fn instance_profiles(&self) -> Vec<InstanceProfile> {
        let Some(instances) = &self.instances else {
            return Vec::new();
        };
        let mut keys: Vec<&String> = instances.keys().collect();
        keys.sort();
        let mut out = Vec::new();
        for key in keys {
            let profile = &instances[key];
            let base_name = profile.name.clone().unwrap_or_else(|| key.clone());
            let count = profile.count.max(1);
            // All siblings share the profile's group (default = base name).
            let group = profile.group.clone().unwrap_or_else(|| base_name.clone());
            for i in 0..count {
                let name = if count > 1 {
                    format!("{base_name}-{i}")
                } else {
                    base_name.clone()
                };
                let mut p = profile.clone();
                p.name = Some(name);
                p.group = Some(group.clone());
                out.push(p);
            }
        }
        out
    }

    /// The dispatch qualifier for the model's default inference point: the
    /// `default: true` profile's group, else the single shared group across all
    /// profiles, else `None` (bare `<base>`). `None` also when no instances are
    /// configured. Encoded as `model = "<base>:<qualifier>"`.
    pub fn default_dispatch_qualifier(&self) -> Option<String> {
        let profiles = self.instance_profiles();
        if profiles.is_empty() {
            return None;
        }
        if let Some(d) = profiles.iter().find(|p| p.default) {
            return d.group.clone();
        }
        let first = profiles[0].group.clone()?;
        if profiles.iter().all(|p| p.group.as_deref() == Some(first.as_str())) {
            Some(first)
        } else {
            None
        }
    }

    /// The dispatch qualifier for the router's *internal work group* (the
    /// "pool"): the classifier, chart selector/adjudicator/reranker,
    /// target-matching ladder, and rigor role backends spread across the
    /// instance pool rather than pinning to the client-facing default instance.
    /// This is a distinct intent from `default_dispatch_qualifier`, which
    /// resolves the fork's *default instance* for client-facing bare-`<base>`
    /// dispatch. Resolution order (D1), deterministic:
    ///
    /// 1. The group of the `default: false` profile with the largest `count`
    ///    (the "work pool"; for the reference config this is `swarm`).
    /// 2. Else the `default: true` profile's group.
    /// 3. Else the single group shared by all profiles.
    /// 4. Else `None` (bare `<base>`, upstream models unchanged).
    pub fn pool_qualifier(&self) -> Option<String> {
        let profiles = self.instance_profiles();
        if profiles.is_empty() {
            return None;
        }
        // 1. The non-default profile with the largest sibling count (ties
        //    resolve to the first encountered in deterministic map order).
        let mut best: Option<&InstanceProfile> = None;
        let mut best_count: u32 = 0;
        for p in profiles.iter().filter(|p| !p.default) {
            let c = p.count.max(1);
            if best.is_none() || c > best_count {
                best = Some(p);
                best_count = c;
            }
        }
        if let Some(b) = best {
            return b.group.clone();
        }
        // 2. The default profile's group.
        if let Some(d) = profiles.iter().find(|p| p.default) {
            return d.group.clone();
        }
        // 3. The single group shared by all profiles.
        let first = profiles[0].group.clone()?;
        if profiles.iter().all(|p| p.group.as_deref() == Some(first.as_str())) {
            Some(first)
        } else {
            None
        }
    }
}

/// Declaration-only request-body keys the fork ignores: the instance grammar
/// owns them (`ctx`/`parallel`/`sleep`), so they must not leak into the body.
pub const DECLARATION_PARAM_KEYS: [&str; 4] =
    ["num_ctx", "parallel", "sleep_idle_seconds", "rope_freq_base"];

/// Remove declaration-only keys from a params object, keeping sampling params
/// (`temperature`, `repeat_penalty`, `chat_template_kwargs`, ...). Non-object
/// params are returned unchanged.
pub fn strip_declaration_params(params: serde_json::Value) -> serde_json::Value {
    let Some(obj) = params.as_object() else {
        return params;
    };
    let mut out = obj.clone();
    for k in DECLARATION_PARAM_KEYS {
        out.remove(k);
    }
    serde_json::Value::Object(out)
}

/// Merge a model entry's top-level sampling `params` with a specific
/// instance profile's `params` (profile wins), returning the merged object.
/// Non-object params degrade to an empty object (nothing to merge). This is
/// the single canonical merge for per-instance sampling knobs; the profile is
/// looked up by name-or-group so both the exact-instance and group dispatch
/// paths reach the same value.
pub(crate) fn merge_sampling_params(
    entry: Option<&serde_json::Value>,
    profile: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut merged = serde_json::Map::new();
    if let Some(v) = entry.and_then(serde_json::Value::as_object) {
        merged.extend(v.clone());
    }
    if let Some(v) = profile.and_then(serde_json::Value::as_object) {
        merged.extend(v.clone());
    }
    serde_json::Value::Object(merged)
}

impl ModelEntry {
    /// Resolve the sampling params to send when dispatching to `qualifier`
    /// (an instance name or group of this model's pool): the matching
    /// profile's `params` overlaid onto the entry's top-level `params`
    /// (profile wins), declaration-only keys stripped. `None` when no profile
    /// matches `qualifier` — callers fall back to the entry's bare params.
    pub fn instance_params_for(&self, qualifier: &str) -> Option<serde_json::Value> {
        let profile = self.instance_profiles().into_iter().find(|p| {
            p.name.as_deref() == Some(qualifier) || p.group.as_deref() == Some(qualifier)
        })?;
        let merged =
            merge_sampling_params(self.params.as_ref(), profile.params.as_ref());
        Some(strip_declaration_params(merged))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum EvictionPolicy {
    #[default]
    Lru,
    Ttl,
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLogConfig {
    #[serde(default = "default_audit_log_dir")]
    pub log_dir: PathBuf,
    #[serde(default = "default_audit_file_size_mb")]
    pub max_file_size_mb: u64,
    #[serde(default = "default_audit_age_days")]
    pub max_age_days: u64,
    #[serde(default = "default_audit_max_files")]
    pub max_files: usize,
    #[serde(default)]
    pub json_format: bool,
    #[serde(default)]
    pub console_output: bool,
}

fn default_audit_log_dir() -> PathBuf {
    PathBuf::from("/tmp/coral-router-audit-logs")
}

const fn default_audit_file_size_mb() -> u64 {
    50
}

const fn default_audit_age_days() -> u64 {
    90
}

const fn default_audit_max_files() -> usize {
    20
}

impl Default for AuditLogConfig {
    fn default() -> Self {
        Self {
            log_dir: default_audit_log_dir(),
            max_file_size_mb: default_audit_file_size_mb(),
            max_age_days: default_audit_age_days(),
            max_files: default_audit_max_files(),
            json_format: true,
            console_output: false,
        }
    }
}

/// The classifier's parsed LLM output — the unified confident-offload envelope.
///
/// The model never chooses whether to respond. It emits a `domain` route key
/// and a self-assessed `confidence` (0.0–1.0, the same calibrated self-assessment
/// semantic Needle's `confidence` carries); the router derives `respond` vs
/// `route` from `domain + confidence + always_route` (see `routing_policy`).
/// `coherence_score`/`safety_score` remain the hard gating checks that protect
/// downstream models from garbage or harmful input.
///
/// `FieldAccess` + the `#[field(...)]` coercions make the struct the single
/// source of truth for the boundary decode path
/// (`fluent_wvr::boundary::decode_boundary`): the `coerce`/`parse` modes shape
/// the raw model value strings exactly as the repair walker does, so both decode
/// paths share one vocabulary.
#[derive(Debug, Clone, Serialize, Deserialize, Default, FieldAccess, Describable)]
pub struct ClassifierOutput {
    /// The route key this decision belongs to (1:1 with a route in
    /// `env/coral-router.json` — DD-4). The route table is the single source of
    /// truth; an unknown `domain` resolves to `default_route` with a warning.
    #[field(desc = "domain route key", coerce = "strip_quotes,trim")]
    pub domain: String,
    #[field(desc = "direct response text", coerce = "strip_quotes,trim")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    #[field(desc = "routing target", coerce = "strip_quotes,trim")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[field(desc = "coherence score", min = 0.0, max = 1.0, coerce = "strip_quotes,trim", parse = "number")]
    pub coherence_score: f64,
    #[field(desc = "safety score", min = 0.0, max = 1.0, coerce = "strip_quotes,trim", parse = "number")]
    pub safety_score: f64,
    /// The classifier's self-assessed confidence in its decision (0.0–1.0).
    /// Read verbatim into `routing_policy::derive_action`; never nulled.
    #[field(desc = "confidence", min = 0.0, max = 1.0, coerce = "strip_quotes,trim", parse = "number")]
    pub confidence: f64,
    #[field(desc = "routing reason", coerce = "strip_quotes,trim")]
    pub reason: String,
    #[field(desc = "completeness", min = 0.0, max = 1.0, coerce = "strip_quotes,trim", parse = "number")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completeness: Option<f64>,
    #[field(desc = "risk", min = 0.0, max = 1.0, coerce = "strip_quotes,trim", parse = "number")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<f64>,
}

use common_core::constants::default_true;

/// What the classifier stage does when its LLM call fails or its response
/// cannot be parsed. The safe default is `Reject`: the router
/// must never convert a classifier outage into a maximum-confidence dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassifierFailurePolicy {
    /// Return `StageVerdict::Rejected` with a truthful reason (no fabricated
    /// scores).
    Reject,
    /// Route to the configured default route, but with scores that reflect the
    /// failure (coherence/safety = 0.0) and a `reason` stating the error.
    RouteToDefaultTruthful,
    /// Fall back to route with fabricated 1.0 scores.
    /// Deprecated; exists only for backward compatibility in code (never
    /// deserializable from config).
    #[serde(skip)]
    LegacyFailOpen,
}

/// Safe default for `RouterConfig.classifier_failure_policy`: reject on
/// classifier failure rather than route on fabricated scores.
fn default_classifier_failure_policy() -> ClassifierFailurePolicy {
    ClassifierFailurePolicy::Reject
}

/// The default route when a config omits `default_route`: `local`, matching
/// the shipped `env/coral-router.json` (no `fast` model exists in-tree).
fn default_route() -> String {
    "local".into()
}

// -- Charts (DAG workflow library) configuration --------------------------

/// Chart store configuration - the `charts` section of `RouterConfig`.
///
/// The store is owned by `fluent-router` (see `coral-router`/`charts/`): a
/// directory of human-authored chart JSON files, a router-side
/// `workflow_library` HNSW/SQLite path for retrieval, and the model key
/// used by chart-selection LLM adjudication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChartsConfig {
    /// Directory of `*.json` chart files loaded at boot. `None` - empty
    /// store (a missing directory is tolerated with a `warn!`).
    #[serde(default)]
    pub dir: Option<String>,
    /// `workflow_library` HNSW/SQLite file path. The index is built lazily
    /// at boot only when this is set.
    #[serde(default)]
    pub index_path: Option<String>,
    /// Chart-selection classifier model key (LLM adjudication step).
    #[serde(default)]
    pub selector_model: Option<String>,
    /// Max candidates surfaced to the selector's LLM adjudication.
    #[serde(default = "default_charts_max_candidates")]
    pub max_candidates: usize,
    /// Embedding-similarity threshold below which a chart is not a candidate.
    #[serde(default = "default_charts_min_score")]
    pub min_score: f64,
    /// Whether bound context entities are exposed to chart templates.
    #[serde(default = "default_charts_entity_context")]
    pub entity_context: bool,
}

impl Default for ChartsConfig {
    fn default() -> Self {
        Self {
            dir: None,
            index_path: None,
            selector_model: None,
            max_candidates: default_charts_max_candidates(),
            min_score: default_charts_min_score(),
            entity_context: default_charts_entity_context(),
        }
    }
}

const fn default_charts_max_candidates() -> usize {
    5
}

// -- Needle (cheapest structured rung) configuration -------------------

/// Needle route-to-tool schema — the enriched per-route schema. The engine
/// only ever sees the **plain OpenAI tool format** (name, description,
/// parameters) rendered from it; `examples`/`intents` are retrieval context
/// for the HNSW shortlister, never engine grammar (measured routing regression
/// otherwise — see `needle::schema::render_tool_schema`).
///
/// This is the single typed home for a route's tool description. It is derived
/// from the `routes.<key>` entry (description) and **overridden** by
/// `NeedleConfig.schema_overrides` when a route declares one (kills the drift a
/// parallel hand-maintained schema list would create). Fields:
///
/// - `name` — the route key (the tool name Needle may call).
/// - `description` — what the tool does (drives the engine schema + retrieval).
/// - `examples` — canonical command phrasings (retrieval context only).
/// - `parameters` — the tool's argument object schema (grammar constraints).
/// - `intents` — intent labels/phrases that map onto this route (retrieval).
/// - `output_template` — when set, a `call` to this tool whose invocation is
///   complete is answered **directly** by rendering this template with the
///   bound arguments — no dispatch, no classifier, no extra inference. The
///   template is literal text with `{arg}` placeholders substituted from the
///   `arguments` object (JSON values rendered inline). A template that cannot
///   be fully rendered (a referenced arg missing, or a malformed brace) never
///   produces a direct answer — the call falls through to the normal
///   route/dispatch path. Absent on tools that must keep dispatching to a
///   model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NeedleRouteSchema {
    /// The route key — the tool name.
    pub name: String,
    /// What the route does.
    #[serde(default)]
    pub description: String,
    /// Canonical command phrasings that should trigger this route.
    #[serde(default)]
    pub examples: Vec<String>,
    /// Argument object schema (`{"type": "object", "properties": {...}}`).
    /// Defaults to an empty object schema.
    #[serde(default = "default_needle_empty_object")]
    pub parameters: serde_json::Value,
    /// Intent labels/phrases that map onto this route.
    #[serde(default)]
    pub intents: Vec<String>,
    /// When set, enables a direct (template) tool response for complete
    /// invocations of this tool. See the struct doc for syntax.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_template: Option<String>,
    /// When true, a Needle `call` to this route is **not** a routing decision:
    /// the general category falls through to the classifier LLM, which
    /// classifies the whole prompt as-is (roadmap Milestone 3 — "primary
    /// router; general-category → classifier fallback"). Non-general route
    /// tools keep the authoritative `Rerouted` short-circuit. Marking a route
    /// `general` is the operator's way to say "Needle should never answer
    /// this on its own"; a template-bearing general route still answers
    /// directly when the template renders (a direct answer beats a fallback).
    #[serde(default)]
    pub general: bool,
}

/// How the candidate set is shortlisted when it exceeds `candidates_per_rung`.
///
/// **BM25 is excluded by design** (roadmap design decision 4); at
/// `candidates_per_rung` or fewer candidates every one is grammar-rendered and
/// reachable (O(1), no index needed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NeedleShortlistMode {
    /// No shortlisting — pass all candidates up to `candidates_per_rung`.
    #[default]
    None,
    /// Shortlist to ≤ `candidates_per_rung` via the HNSW tool index.
    Hnsw,
}

/// The `shortlist` sub-block of the `needle` config section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeedleShortlistConfig {
    /// Shortlisting strategy: `"none"` or `"hnsw"`.
    #[serde(default)]
    pub mode: NeedleShortlistMode,
    /// Model key for the embed provider that embeds schemas + queries once.
    /// Selects an entry from `models` (the same embed seam the charts store
    /// uses).
    #[serde(default)]
    pub embedding_model: Option<String>,
    /// Persisted HNSW index path for the tool catalogue. Built lazily.
    #[serde(default)]
    pub index_path: Option<String>,
    /// Cosine-similarity threshold below which a candidate is dropped.
    #[serde(default = "default_needle_shortlist_min_score")]
    pub min_score: f64,
}

impl Default for NeedleShortlistConfig {
    fn default() -> Self {
        Self {
            mode: NeedleShortlistMode::None,
            embedding_model: None,
            index_path: None,
            min_score: default_needle_shortlist_min_score(),
        }
    }
}

/// The kind of work a tool-plan step performs.
///
/// Each variant maps to a distinct dispatch path: `Dispatch` goes through the
/// standard `ChatBackend` chain targeting a model group; `Lookup` performs a
/// read-only data-store / knowledge-graph / DAG lookup; `Compose` runs the
/// final synthesis over all prior step results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPlanStepKind {
    /// Dispatch to a model group via the standard ChatBackend chain.
    /// The step's `target_group` selects which `model_groups` entry to use.
    Dispatch,
    /// Read-only lookup: DAG graph, knowledge-graph, data-store, entity-tool,
    /// or chart store. The lookup kind is carried in the step's `lookup_kind`
    /// field; M4 supports `"dag"`, `"knowledge_graph"`, `"chart"`, and
    /// `"entity_tool"` as recognized values — anything else is a passthrough
    /// error so future kinds are additive.
    Lookup,
    /// Compose the final answer from prior step results. Must be the last
    /// step in a plan.
    Compose,
    /// Generic passthrough for extensibility — forward the step's input text
    /// to the target group and return the response verbatim.
    #[serde(other)]
    Passthrough,
}

/// A single step in a bounded tool plan.
///
/// Steps execute in list order. Each step records its result into the session
/// ledger as a typed `ContentNode` by origin, and writes audit metadata (step
/// id, target, confidence) so the dispatch is legible post-hoc (VISION:
/// "auditable by construction").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPlanStep {
    /// Step identifier for audit trail and dependency tracking.
    pub id: String,
    /// What this step does.
    pub kind: ToolPlanStepKind,
    /// `model_groups` key for `Dispatch` steps. Ignored for `Compose`.
    #[serde(default)]
    pub target_group: Option<String>,
    /// Human-readable description for audit / prompt assembly.
    #[serde(default)]
    pub description: Option<String>,
    /// Lookup kind for `Lookup` steps: `"dag"`, `"knowledge_graph"`,
    /// `"chart"`, `"entity_tool"`, etc. Ignored for non-Lookup kinds.
    #[serde(default)]
    pub lookup_kind: Option<String>,
    /// Per-step round limit. `None` defers to the plan-level max_rounds
    /// (which itself defers to `needle.max_rounds`).
    #[serde(default)]
    pub step_max_rounds: Option<usize>,
}

/// A config-declared, bounded tool plan for a specific route.
///
/// When a `Rerouted` target matches a route with a `tool_plans` entry, the
/// handler executes the plan's ordered steps instead of a single
/// `handle_dispatch` call. Each step goes through the standard `ChatBackend`
/// chain (for `Dispatch` steps) or a ledger/DAG lookup (for `Lookup` steps),
/// and the results are composed into the final answer.
///
/// Plans are **config-declared, never hardcoded** (VISION: "self-updating
/// routing config"). Exceeding `max_rounds` falls back to the route's plain
/// group dispatch rather than looping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPlan {
    /// Ordered list of steps to execute.
    pub steps: Vec<ToolPlanStep>,
    /// Plan-level round limit. Overrides `needle.max_rounds` when set.
    /// Falls back to the route's plain group dispatch on exhaustion.
    #[serde(default)]
    pub max_rounds: Option<usize>,
}

/// Needle configuration - the dedicated top-level `needle` section of
/// `RouterConfig`.
///
/// Needle is **not** a `models` entry: it is a separate engine path that never
/// touches the llama-server supervisor / instances / VRAM machinery
/// (`ModelEntry::is_managed` is left untouched). The rung runs between the
/// deterministic pre-filter and the classifier, gated by `enabled`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeedleConfig {
    /// Master switch for the Needle pre-filter rung. `false` (default) keeps
    /// today's behavior — the pipeline goes deterministic pre-filter →
    /// classifier with no Needle hop.
    #[serde(default)]
    pub enabled: bool,
    /// Explicit `libneedle` shared-library path. `None` resolves via
    /// `NEEDLE_LIB_PATH`, the package dir, then the user cache (mirrors
    /// `needle/__init__.py::_library_path`).
    #[serde(default)]
    pub engine: Option<String>,
    /// Engine version key for the cache directory. `None` uses the version the
    /// wrapper was written against (`needle::engine::ENGINE_VERSION`).
    #[serde(default)]
    pub engine_version: Option<String>,
    /// Tuned `.cact` weights blob path, loaded into the engine once (sticky for
    /// the process). `None` keeps the engine's base weights.
    #[serde(default)]
    pub weights: Option<String>,
    /// Pipeline id to run the rung on. `None` applies to the default pipeline.
    #[serde(default)]
    pub pipeline: Option<String>,
    /// Minimum command length (chars) for the gate. Shorter requests skip.
    #[serde(default = "default_needle_min_command_chars")]
    pub min_command_chars: usize,
    /// Maximum command length (chars) for the gate. Longer requests skip.
    #[serde(default = "default_needle_max_command_chars")]
    pub max_command_chars: usize,
    /// Maximum input size (tokens) for the gate. Bulk-context requests skip.
    #[serde(default = "default_needle_max_input_tokens")]
    pub max_input_tokens: usize,
    /// Calibrated-confidence floor in [0, 1]. Envelopes below it decline.
    #[serde(default = "default_needle_confidence_threshold")]
    pub confidence_threshold: f64,
    /// When `true`, a `call` envelope with no `confidence` declines (the
    /// finetuned-weights case). When `false` (default) a missing confidence is
    /// not itself a reason to decline — the envelope type is the primary
    /// signal.
    #[serde(default)]
    pub decline_on_missing_confidence: bool,
    /// Per-completion wall-clock budget (ms).
    #[serde(default = "default_needle_timeout_ms")]
    pub timeout_ms: u64,
    /// Max candidates per rung. At or below this every candidate is
    /// grammar-rendered and reachable; on overflow the `shortlist` strategy
    /// reduces to ≤ `candidates_per_rung`. Clamped to ≥ 1.
    #[serde(
        default = "default_needle_candidates_per_rung",
        deserialize_with = "deserialize_needle_candidates_per_rung"
    )]
    pub candidates_per_rung: usize,
    /// Persisted tool-index path (grammar retrieval context for the engine).
    #[serde(default)]
    pub tool_index_path: Option<String>,
    /// Fixed cap on round-after-round DAG construction (VISION: "terminate, don't
    /// loop"). Needle is consulted at each bounded choice point; the round count
    /// is never open-ended. Scaffolded here for the deferred workflow roadmap;
    /// the current single-shot rung makes at most one Needle call regardless.
    #[serde(default = "default_needle_max_rounds")]
    pub max_rounds: usize,
    /// Per-route tool-schema overrides. Keys are route keys; values override the
    /// description/examples/parameters/intents derived from `routes.<key>`.
    #[serde(default)]
    pub schema_overrides: HashMap<String, NeedleRouteSchema>,
    /// Candidate shortlisting strategy for tool catalogues larger than
    /// `candidates_per_rung`.
    #[serde(default)]
    pub shortlist: NeedleShortlistConfig,
    /// Config-declared bounded tool plans. Keys are route keys (tool names);
    /// values are ordered step sequences. When a `Rerouted` target matches a
    /// route with a plan, the handler runs the plan instead of a single
    /// `handle_dispatch`. Exceeding `max_rounds` falls back to plain dispatch.
    #[serde(default)]
    pub tool_plans: HashMap<String, ToolPlan>,
}

impl Default for NeedleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            engine: None,
            engine_version: None,
            weights: None,
            pipeline: None,
            min_command_chars: default_needle_min_command_chars(),
            max_command_chars: default_needle_max_command_chars(),
            max_input_tokens: default_needle_max_input_tokens(),
            confidence_threshold: default_needle_confidence_threshold(),
            decline_on_missing_confidence: false,
            timeout_ms: default_needle_timeout_ms(),
            candidates_per_rung: default_needle_candidates_per_rung(),
            tool_index_path: None,
            max_rounds: default_needle_max_rounds(),
            schema_overrides: HashMap::new(),
            shortlist: NeedleShortlistConfig::default(),
            tool_plans: HashMap::new(),
        }
    }
}

fn default_needle_empty_object() -> serde_json::Value {
    serde_json::json!({})
}

/// Lookup-store wiring for tool-plan `Lookup` steps.
///
/// The `dag`/`knowledge_graph`/`chart`/`entity_tool` kinds are backed by the
/// stores the deployment already configures (the session registry, the
/// ledger, and the chart store) — this section only carries the stores with no
/// other config home. A kind whose backing store is absent is simply not
/// installed: a plan needing it is declined to plain group dispatch, never
/// executed with a placeholder (see `server/tool_lookup/`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolLookupConfig {
    /// SQLite path for the `data_store` lookup kind. `None` (the default)
    /// leaves the kind uninstalled — a plan needing `data_store` is declined to
    /// plain dispatch. The store is read-only and capability-gated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_store_path: Option<String>,
}

const fn default_needle_min_command_chars() -> usize {
    4
}

const fn default_needle_max_command_chars() -> usize {
    512
}

const fn default_needle_max_input_tokens() -> usize {
    1024
}

fn default_needle_confidence_threshold() -> f64 {
    0.6
}

const fn default_needle_timeout_ms() -> u64 {
    2000
}

const fn default_needle_candidates_per_rung() -> usize {
    5
}

const fn default_needle_max_rounds() -> usize {
    3
}

fn default_needle_shortlist_min_score() -> f64 {
    0.6
}

/// Deserialize `candidates_per_rung`, clamping to ≥ 1 so a rung always has at
/// least one reachable candidate (a cap of 0 would make the rung unreachable
/// by construction).
fn deserialize_needle_candidates_per_rung<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = usize::deserialize(deserializer)?;
    Ok(value.max(1))
}

// -- Rigor configuration ----------------------------------------------

/// Rigor-route configuration - the `rigor` section of `RouterConfig`.
///
/// Model keys select entries from `config.models`; backends are built **only**
/// in `coral-router`'s `build_rigor_route` (DIP, mirroring
/// `build_plan_route`/`default_adjudicator_backend`). `None` at the
/// `RouterConfig` level leaves `/v1/rigor` present but unconfigured.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    #[serde(default = "default_rigor_max_passes")]
    pub max_passes: usize,
    /// Objection severity at/above which a judge rejection is **material**
    /// (triggers rewind + the second blue pass). Default 0.7.
    #[serde(default = "default_rigor_severity_threshold")]
    pub severity_threshold: f64,
    /// Judge confidence below which a final rejection escalates to frontier.
    /// An explicit config value - never "red scored a point".
    /// Default 0.4.
    #[serde(default = "default_rigor_escalation_confidence")]
    pub escalation_confidence: f64,
}

impl Default for RigorConfig {
    fn default() -> Self {
        Self {
            blue_model: None,
            red_model: None,
            judge_model: None,
            kv_cache_enabled: false,
            max_passes: default_rigor_max_passes(),
            severity_threshold: default_rigor_severity_threshold(),
            escalation_confidence: default_rigor_escalation_confidence(),
        }
    }
}

const fn default_rigor_max_passes() -> usize {
    2
}

const fn default_rigor_severity_threshold() -> f64 {
    0.7
}

const fn default_rigor_escalation_confidence() -> f64 {
    0.4
}

/// Default cap on the ledger `Summarizer`'s summary length (tokens). Only a
/// named constant - `LedgerConfig.max_summary_tokens` defaults to it.
pub const DEFAULT_LEDGER_MAX_SUMMARY_TOKENS: u32 = 200;

/// Ledger composition section - the `ledger` block of `RouterConfig`.
///
/// `Some` opts the composition root (`main.rs`) into opening a
/// `ContentNodeLedger` and attaching a `Summarizer` backend targeting the
/// named `ledger` instance. `None` (absent) keeps today's behavior - no
/// ledger at boot - so existing deployments are untouched until they opt in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerConfig {
    /// Durable store path. `None` falls back to an in-memory ledger with a
    /// `warn!` (ephemeral, still functional for LOD derivation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Model key for the ledger `Summarizer`. `None` falls back to the
    /// classifier model key, then to no summarizer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
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
        }
    }
}

/// The `ledger.orchestrator` section: configures the
/// `LedgerAgentCoordinator`'s restore-vs-re-prefill policy, its prompt budget,
/// and the default role recorded for agent output nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorSection {
    /// Whether to attach the coordinator at boot (opt-in). `false` (the
    /// default) leaves the server's dispatch path unchanged.
    #[serde(default)]
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

const fn default_orchestrator_prompt_budget_chars() -> usize {
    32768
}

fn default_orchestrator_role() -> String {
    "agent".into()
}

impl Default for OrchestratorSection {
    fn default() -> Self {
        Self {
            enabled: false,
            kv_policy: crate::dag_session::KvSnapshotPolicy::RestoreIfSameModel,
            prompt_budget_chars: default_orchestrator_prompt_budget_chars(),
            role: default_orchestrator_role(),
            affinity_cap: None,
        }
    }
}

/// Session composition section - the `session` block of `RouterConfig`.
///
/// `Some` opts the composition root into a `SessionRegistry` (canonical
/// session home) so checkpoint/rewind state and rigor rewind exist at runtime.
/// `None` (absent) keeps today's behavior - no session registry at boot.
/// Default model run parameters - the top-level `default_params` block.
///
/// Supplies the "how a model is run" defaults applied to every managed model
/// that does not declare the key itself: the `llama-server` launch knobs
/// (`--batch-size`, `--ubatch-size`, `--cache-type-k/v`, `--flash-attn`,
/// `--n-gpu-layers`, `--n-cpu-moe`, `--sleep-idle-seconds`, `--ctx-size`) and
/// the sampling `params` merged into dispatch bodies (per-model values win).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultModelParams {
    /// Default context size in tokens (`--ctx-size`; also `ctx_size` alias).
    #[serde(default = "default_num_ctx", alias = "ctx_size")]
    pub num_ctx: u64,
    /// Logical maximum batch size (`--batch-size`).
    #[serde(default = "default_batch_size")]
    pub batch_size: u64,
    /// Physical maximum batch size (`--ubatch-size`).
    #[serde(default = "default_ubatch_size")]
    pub ubatch_size: u64,
    /// KV cache data type for K (`--cache-type-k`).
    #[serde(default = "default_cache_type")]
    pub cache_type_k: String,
    /// KV cache data type for V (`--cache-type-v`).
    #[serde(default = "default_cache_type")]
    pub cache_type_v: String,
    /// Flash attention mode (`--flash-attn on|off|auto`); `None` keeps the
    /// fork default.
    #[serde(default)]
    pub flash_attn: Option<String>,
    /// Max layers stored in VRAM (`--n-gpu-layers`).
    #[serde(default = "default_n_gpu_layers")]
    pub n_gpu_layers: i32,
    /// MoE expert layers kept in CPU RAM (`--n-cpu-moe`).
    #[serde(default)]
    pub n_cpu_moe: i32,
    /// Idle timeout after which the fork sleeps an instance
    /// (`--sleep-idle-seconds`). Only emitted for plain (no-instance) models;
    /// instance pools own residency through the sidecar.
    #[serde(default = "default_sleep_idle_seconds")]
    pub sleep_idle_seconds: i32,
    /// Whether dispatches through models without an explicit `stream` stream.
    #[serde(default = "default_true")]
    pub stream: bool,
    /// Whether dispatches through models without an explicit `filter_thinking`
    /// strip thinking blocks.
    #[serde(default)]
    pub filter_thinking: bool,
    /// Default sampling params merged into dispatch bodies (per-model wins).
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

impl Default for DefaultModelParams {
    fn default() -> Self {
        Self {
            num_ctx: default_num_ctx(),
            batch_size: default_batch_size(),
            ubatch_size: default_ubatch_size(),
            cache_type_k: default_cache_type(),
            cache_type_v: default_cache_type(),
            flash_attn: None,
            n_gpu_layers: default_n_gpu_layers(),
            n_cpu_moe: 0,
            sleep_idle_seconds: default_sleep_idle_seconds(),
            stream: default_true(),
            filter_thinking: false,
            params: None,
        }
    }
}

const fn default_num_ctx() -> u64 {
    16384
}

const fn default_batch_size() -> u64 {
    4096
}

const fn default_ubatch_size() -> u64 {
    1024
}

fn default_cache_type() -> String {
    "q8_0".into()
}

const fn default_n_gpu_layers() -> i32 {
    999
}

const fn default_sleep_idle_seconds() -> i32 {
    15
}

/// Session composition section - the `session` block of `RouterConfig`.
///
/// `Some` opts the composition root into a `SessionRegistry` (canonical
/// session home) so checkpoint/rewind state and rigor rewind exist at runtime.
/// `None` (absent) keeps today's behavior - no session registry at boot.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionConfig {
    /// Cold-tier mountpoint for KV cache snapshots, mapped to
    /// `SessionRegistry::new`'s `kv_root`. `None` uses a process-local temp
    /// directory (durable across requests, ephemeral across restarts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

/// Sidecar instance-management policy.
///
/// The sidecar task is the external VRAM-policy owner the fork's docs
/// describe: it boot-reconciles configured instance profiles against
/// `GET /instances`, polls `/memory`, and evicts least-recently-used unpinned
/// instances when free device VRAM drops below the watermark. It only ever
/// allocates or frees KV + compute buffers - the shared weights stay loaded in
/// `llama-server`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidecarConfig {
    /// How often the residency loop polls `/memory`, in seconds.
    #[serde(default = "default_sidecar_poll_interval_s")]
    pub poll_interval_s: u64,
    /// Free-VRAM threshold (bytes) below which the residency loop evicts.
    #[serde(default = "default_sidecar_watermark")]
    pub vram_low_watermark_bytes: u64,
    /// Max unpinned instances evicted per low-VRAM pass.
    #[serde(default = "default_sidecar_evict_batch")]
    pub evict_batch: usize,
    /// Device VRAM ceiling (bytes). `None` disables residency eviction (the
    /// loop still polls and logs) because free VRAM cannot be computed.
    #[serde(default)]
    pub vram_total_bytes: Option<u64>,
    /// Free-VRAM floor (bytes) that must remain unallocated: the effective
    /// allocation limit is `device_total - minimum_remaining_vram`. When
    /// `vram_total_bytes` is `None`, the device total is detected at boot
    /// (ROCm `mem_info_vram_total`); `minimum_remaining_vram` then alone
    /// enables the residency eviction budget.
    #[serde(default)]
    pub minimum_remaining_vram: Option<u64>,
    /// Slot-save directory the fork writes KV snapshots under
    /// (`<slot_save_path>/<model_key>/`). Feeds snapshot-path derivation.
    #[serde(default)]
    pub slot_save_path: Option<String>,
    /// Resume snapshots older than this many seconds of context idle are
    /// dropped and their contexts' `resume` flag cleared: the router's signal
    /// that an evicted workload is done and need not be restorable. `None`
    /// keeps resume snapshots until explicitly disabled. The flag also feeds
    /// the `-resume` snapshot naming the router uses on eviction.
    #[serde(default)]
    pub resume_ttl_s: Option<u64>,
    /// Env var naming the management API key sent as `Authorization: Bearer`.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Post-boot liveness poll: how often the supervision task probes a running
    /// server's `/health` (seconds). A server that stays alive but stops
    /// answering `liveness_failures_before_restart` consecutive probes is
    /// killed and restarted.
    #[serde(default = "default_sidecar_liveness_poll_s")]
    pub liveness_poll_interval_s: u64,
    /// Consecutive failed `/health` probes before a hung server is killed and
    /// restarted.
    #[serde(default = "default_sidecar_liveness_failures")]
    pub liveness_failures_before_restart: u32,
    /// Consecutive crashes (spawn failures or boot-time child exits) after
    /// which the supervisor stops restarting a model's `llama-server` and
    /// marks it **failed** (containment, per the fluent-concurrency
    /// supervision contract — no endless crash loop). `ensure_running` then
    /// returns a terminal error until the router restarts or the model is
    /// unloaded, at which point a fresh (bounded) load attempt is allowed.
    /// The count resets the moment a server answers `/health`, so a crash
    /// after a healthy period is a fresh failure. `0` disables the limit
    /// (unbounded restart with rising backoff).
    #[serde(default = "default_sidecar_max_restarts")]
    pub max_restarts: u32,
}

impl Default for SidecarConfig {
    fn default() -> Self {
        Self {
            poll_interval_s: default_sidecar_poll_interval_s(),
            vram_low_watermark_bytes: default_sidecar_watermark(),
            evict_batch: default_sidecar_evict_batch(),
            vram_total_bytes: None,
            minimum_remaining_vram: None,
            slot_save_path: None,
            resume_ttl_s: None,
            api_key_env: None,
            liveness_poll_interval_s: default_sidecar_liveness_poll_s(),
            liveness_failures_before_restart: default_sidecar_liveness_failures(),
            max_restarts: default_sidecar_max_restarts(),
        }
    }
}

const fn default_sidecar_poll_interval_s() -> u64 {
    5
}

const fn default_sidecar_watermark() -> u64 {
    1073741824
}

const fn default_sidecar_evict_batch() -> usize {
    1
}

const fn default_sidecar_liveness_poll_s() -> u64 {
    30
}

const fn default_sidecar_liveness_failures() -> u32 {
    3
}

const fn default_sidecar_max_restarts() -> u32 {
    5
}

/// Detect the device VRAM total (bytes) from the ROCm sysfs interface. Returns
/// the first non-zero `mem_info_vram_total` found under `/sys/class/drm`. Used
/// when `sidecar.vram_total_bytes` is unset so a `minimum_remaining_vram`
/// budget alone can drive the residency loop. `None` when the interface is
/// absent (non-ROCm hosts).
pub fn detect_device_vram_total() -> Option<u64> {
    let entries = fluent_wvr::capability::capability_aware_fs::read_dir("/sys/class/drm").ok()?;
    for entry in entries.flatten() {
        let path = entry.path().join("device/mem_info_vram_total");
        let text = fluent_wvr::capability::capability_aware_fs::read_to_string(path).ok()?;
        let total = text.trim().parse::<u64>().ok()?;
        if total > 0 {
            return Some(total);
        }
    }
    None
}

impl SidecarConfig {
    /// The device VRAM total: the explicit `vram_total_bytes` ceiling first,
    /// else the ROCm sysfs detection. `None` when neither is available.
    pub fn device_total_bytes(&self) -> Option<u64> {
        self.vram_total_bytes.or_else(detect_device_vram_total)
    }

    /// The effective VRAM allocation budget: `device_total - minimum_remaining
    /// _vram`. `None` when no device total is available, or when neither a
    /// ceiling nor a minimum-remaining floor is configured (eviction off).
    pub fn allocation_limit(&self) -> Option<u64> {
        let total = self.device_total_bytes()?;
        let min_remaining = self.minimum_remaining_vram.unwrap_or(0);
        Some(total.saturating_sub(min_remaining))
    }
}

const fn default_charts_min_score() -> f64 {
    0.6
}

const fn default_charts_entity_context() -> bool {
    true
}

// -- Post-processing configuration ---------------------

/// Post-processing configuration - the `post_process` section of
/// `RouterConfig`.
///
/// Controls the VISION learning loop: whether a *successful* dispatch is
/// distilled into a reusable draft chart. Per VISION -"Post-processing:
/// audit + workflow extraction", extraction is opt-in and the produced chart
/// is a draft that only becomes selectable after a rubric-validated run.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PostProcessConfig {
    /// Whether successful dispatches are decomposed into draft charts
    /// automatically. Default `false` - the operator opts in.
    #[serde(default)]
    pub workflow_extraction: bool,
    /// Which successful dispatches are distilled into draft charts.
    /// Default `"frontier"` - the VISION learning loop learns from
    /// frontier-assisted (escalated/fallback) solutions, not the common
    /// local-primary path. `"all"` restores the blanket behavior by
    /// explicit opt-in.
    #[serde(default)]
    pub workflow_extraction_mode: WorkflowExtractionMode,
}

/// Extraction scope for the learning loop (see `PostProcessConfig`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkflowExtractionMode {
    /// Only frontier-assisted dispatches (an index > 0 in the primary +
    /// fallback chain) are distilled into draft charts.
    #[default]
    #[serde(rename = "frontier")]
    Frontier,
    /// Every successful dispatch is distilled.
    #[serde(rename = "all")]
    All,
}

fn default_total_timeout_ms() -> u64 {
    common_core::constants::DEFAULT_TOTAL_TIMEOUT_MS
}

fn default_idle_timeout_ms() -> u64 {
    common_core::constants::DEFAULT_IDLE_TIMEOUT_MS
}

fn default_retry_interval() -> u64 {
    common_core::constants::DEFAULT_RETRY_INTERVAL_S
}

#[cfg(test)]
mod tests {
    // Tests assert float config values against literal defaults - deliberate.
    #![allow(clippy::float_cmp)]
    use super::*;

    #[test]
    fn charts_config_defaults() {
        let cfg = ChartsConfig::default();
        assert_eq!(cfg.max_candidates, 5);
        assert_eq!(cfg.min_score, 0.6);
        assert!(cfg.entity_context);
        assert!(cfg.dir.is_none());
        assert!(cfg.index_path.is_none());
        assert!(cfg.selector_model.is_none());
    }

    // -- Rigor-route configuration -------------------------------------

    #[test]
    fn rigor_config_defaults() {
        let cfg = RigorConfig::default();
        assert_eq!(cfg.max_passes, 2);
        assert_eq!(cfg.severity_threshold, 0.7);
        assert_eq!(cfg.escalation_confidence, 0.4);
        assert!(!cfg.kv_cache_enabled);
        assert!(cfg.blue_model.is_none());
        assert!(cfg.red_model.is_none());
        assert!(cfg.judge_model.is_none());
    }

    #[test]
    fn router_config_absent_rigor_section_defaults_to_none() {
        // The shipped env/coral-router.json has no `rigor` section; the route
        // stays present-but-unconfigured (None), never a crash.
        let cfg: RouterConfig =
            serde_json::from_str(r#"{"server": {"bind_addr": "127.0.0.1:0"}}"#).unwrap();
        assert!(cfg.rigor.is_none());
    }

    #[test]
    fn rigor_config_round_trip() {
        let json = serde_json::json!({
            "rigor": {
                "blue_model": "fast",
                "red_model": "code",
                "judge_model": "code",
                "kv_cache_enabled": true,
                "max_passes": 3,
                "severity_threshold": 0.8,
                "escalation_confidence": 0.3,
            }
        });
        let cfg: RouterConfig = serde_json::from_value(json).unwrap();
        let rigor = cfg.rigor.expect("rigor section parsed");
        assert_eq!(rigor.blue_model.as_deref(), Some("fast"));
        assert_eq!(rigor.red_model.as_deref(), Some("code"));
        assert_eq!(rigor.judge_model.as_deref(), Some("code"));
        assert!(rigor.kv_cache_enabled);
        assert_eq!(rigor.max_passes, 3);
        assert_eq!(rigor.severity_threshold, 0.8);
        assert_eq!(rigor.escalation_confidence, 0.3);

        // Partial section still round-trips with defaults for the rest.
        let partial: RouterConfig = serde_json::from_value(serde_json::json!({
            "rigor": {"blue_model": "fast"}
        }))
        .unwrap();
        let partial_cfg = partial.rigor.expect("rigor parsed");
        assert_eq!(partial_cfg.blue_model.as_deref(), Some("fast"));
        assert_eq!(partial_cfg.max_passes, 2, "absent fields default");
        assert_eq!(partial_cfg.severity_threshold, 0.7);
    }

    #[test]
    fn router_config_absent_charts_section_defaults_cleanly() {
        let cfg: RouterConfig =
            serde_json::from_str(r#"{"server": {"bind_addr": "127.0.0.1:0"}}"#).unwrap();
        assert_eq!(cfg.charts.max_candidates, 5);
        assert_eq!(cfg.charts.min_score, 0.6);
        assert!(cfg.charts.entity_context);
        assert!(cfg.charts.dir.is_none());
    }

    #[test]
    fn router_config_embedding_and_reranker_models_parse() {
        let cfg: RouterConfig =
            serde_json::from_str(r#"{"embedding_model": "embed", "reranker_model": "rerank"}"#)
                .unwrap();
        assert_eq!(cfg.embedding_model.as_deref(), Some("embed"));
        assert_eq!(cfg.reranker_model.as_deref(), Some("rerank"));

        let absent: RouterConfig = serde_json::from_str(r"{}").unwrap();
        assert!(absent.embedding_model.is_none());
        assert!(absent.reranker_model.is_none());
    }

    #[test]
    fn charts_section_round_trips() {
        let json = r#"{
            "dir": "env/workflows/charts",
            "index_path": "data/workflow_library.sqlite",
            "selector_model": "qwen3.5-4b",
            "max_candidates": 5,
            "min_score": 0.6,
            "entity_context": true
        }"#;
        let cfg: ChartsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.dir.as_deref(), Some("env/workflows/charts"));
        assert_eq!(
            cfg.index_path.as_deref(),
            Some("data/workflow_library.sqlite")
        );
        assert_eq!(cfg.selector_model.as_deref(), Some("qwen3.5-4b"));
        assert_eq!(cfg.max_candidates, 5);
        assert_eq!(cfg.min_score, 0.6);
        assert!(cfg.entity_context);

        let serialized = serde_json::to_string(&cfg).unwrap();
        let back: ChartsConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(back.dir, cfg.dir);
        assert_eq!(back.max_candidates, cfg.max_candidates);
        assert_eq!(back.min_score, cfg.min_score);
    }

    #[test]
    fn partial_charts_section_defaults_missing_fields() {
        let cfg: ChartsConfig = serde_json::from_str(r#"{"dir": "env/workflows/charts"}"#).unwrap();
        assert_eq!(cfg.dir.as_deref(), Some("env/workflows/charts"));
        assert_eq!(cfg.max_candidates, 5);
        assert_eq!(cfg.min_score, 0.6);
        assert!(cfg.entity_context);
        assert!(cfg.index_path.is_none());
        assert!(cfg.selector_model.is_none());
    }

    #[test]
    fn router_config_parses_charts_section() {
        let json = r#"{
            "charts": { "dir": "env/workflows/charts", "max_candidates": 8 }
        }"#;
        let cfg: RouterConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.charts.dir.as_deref(), Some("env/workflows/charts"));
        assert_eq!(cfg.charts.max_candidates, 8);
        assert_eq!(cfg.charts.min_score, 0.6, "unset field keeps its default");
    }

    // -- Needle configuration ----------------------------------------

    #[test]
    fn needle_config_defaults() {
        let cfg = NeedleConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.min_command_chars, 4);
        assert_eq!(cfg.max_command_chars, 512);
        assert_eq!(cfg.max_input_tokens, 1024);
        assert_eq!(cfg.confidence_threshold, 0.6);
        assert!(!cfg.decline_on_missing_confidence);
        assert_eq!(cfg.timeout_ms, 2000);
        assert_eq!(cfg.candidates_per_rung, 5);
        assert!(cfg.engine.is_none());
        assert!(cfg.weights.is_none());
        assert!(cfg.tool_index_path.is_none());
        assert!(cfg.schema_overrides.is_empty());
        assert_eq!(cfg.shortlist.mode, NeedleShortlistMode::None);
        assert_eq!(cfg.shortlist.min_score, 0.6);
    }

    #[test]
    fn needle_shortlist_mode_serde() {
        let none: NeedleShortlistMode = serde_json::from_str(r#""none""#).expect("none");
        assert_eq!(none, NeedleShortlistMode::None);
        let hnsw: NeedleShortlistMode = serde_json::from_str(r#""hnsw""#).expect("hnsw");
        assert_eq!(hnsw, NeedleShortlistMode::Hnsw);
        assert!(serde_json::from_str::<NeedleShortlistMode>(r#""bm25""#).is_err());
    }

    #[test]
    fn needle_candidates_per_rung_is_clamped_to_at_least_one() {
        let zero: NeedleConfig = serde_json::from_str(r#"{"candidates_per_rung": 0}"#).expect("zero");
        assert_eq!(zero.candidates_per_rung, 1, "a cap of 0 would make the rung unreachable");
        let one: NeedleConfig = serde_json::from_str(r#"{"candidates_per_rung": 1}"#).expect("one");
        assert_eq!(one.candidates_per_rung, 1);
        let seven: NeedleConfig = serde_json::from_str(r#"{"candidates_per_rung": 7}"#).expect("seven");
        assert_eq!(seven.candidates_per_rung, 7, "upper values pass through unclamped");
    }

    #[test]
    fn needle_config_round_trip() {
        let json = serde_json::json!({
            "enabled": true,
            "engine": "/opt/lib/libneedle.so",
            "engine_version": "2.0.2",
            "weights": "/opt/weights/tuned.cact",
            "pipeline": "commands",
            "min_command_chars": 2,
            "max_command_chars": 256,
            "max_input_tokens": 512,
            "confidence_threshold": 0.8,
            "decline_on_missing_confidence": true,
            "timeout_ms": 1500,
            "candidates_per_rung": 4,
            "tool_index_path": "data/tool_index.sqlite",
            "schema_overrides": {
                "weather": {
                    "name": "weather",
                    "description": "get the weather",
                    "examples": ["weather in Paris", "what is the forecast"],
                    "parameters": {"type": "object", "properties": {"city": {"type": "string"}}},
                    "intents": ["forecast", "temperature"],
                }
            },
            "shortlist": {
                "mode": "hnsw",
                "embedding_model": "embed",
                "index_path": "data/tool_index.sqlite",
                "min_score": 0.55,
            }
        });
        let cfg: RouterConfig =
            serde_json::from_value(serde_json::json!({"needle": json})).unwrap();
        let needle = cfg.needle.as_ref().expect("needle section parsed");
        assert!(needle.enabled);
        assert_eq!(needle.engine.as_deref(), Some("/opt/lib/libneedle.so"));
        assert_eq!(needle.engine_version.as_deref(), Some("2.0.2"));
        assert_eq!(needle.weights.as_deref(), Some("/opt/weights/tuned.cact"));
        assert_eq!(needle.pipeline.as_deref(), Some("commands"));
        assert_eq!(needle.min_command_chars, 2);
        assert_eq!(needle.max_command_chars, 256);
        assert_eq!(needle.max_input_tokens, 512);
        assert_eq!(needle.confidence_threshold, 0.8);
        assert!(needle.decline_on_missing_confidence);
        assert_eq!(needle.timeout_ms, 1500);
        assert_eq!(needle.candidates_per_rung, 4);
        assert_eq!(needle.tool_index_path.as_deref(), Some("data/tool_index.sqlite"));

        let weather = needle.schema_overrides.get("weather").expect("override");
        assert_eq!(weather.name, "weather");
        assert_eq!(weather.description, "get the weather");
        assert_eq!(weather.examples.len(), 2);
        assert_eq!(weather.intents, vec!["forecast", "temperature"]);
        assert_eq!(
            weather.parameters,
            serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}})
        );

        assert_eq!(needle.shortlist.mode, NeedleShortlistMode::Hnsw);
        assert_eq!(needle.shortlist.embedding_model.as_deref(), Some("embed"));
        assert_eq!(needle.shortlist.index_path.as_deref(), Some("data/tool_index.sqlite"));
        assert_eq!(needle.shortlist.min_score, 0.55);

        let serialized = serde_json::to_string(&cfg).unwrap();
        let back: RouterConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(back.needle.unwrap().candidates_per_rung, 4);
    }

    #[test]
    fn partial_needle_section_defaults_missing_fields() {
        let cfg: RouterConfig = serde_json::from_str(r#"{"needle": {"enabled": true}}"#).unwrap();
        let needle = cfg.needle.expect("needle parsed");
        assert!(needle.enabled);
        assert_eq!(needle.candidates_per_rung, 5, "absent field keeps default");
        assert_eq!(needle.min_command_chars, 4);
        assert_eq!(needle.confidence_threshold, 0.6);
        assert!(needle.schema_overrides.is_empty());
        assert_eq!(needle.shortlist.mode, NeedleShortlistMode::None);
    }

    #[test]
    fn router_config_absent_needle_section_defaults_to_none() {
        // The shipped env/coral-router.json (before Milestone 8 wires a block)
        // has no `needle` section; the rung stays absent (None), never a crash.
        let cfg: RouterConfig =
            serde_json::from_str(r#"{"server": {"bind_addr": "127.0.0.1:0"}}"#).unwrap();
        assert!(cfg.needle.is_none());
    }

    #[test]
    fn needle_route_schema_parameters_defaults_to_empty_object() {
        let schema: NeedleRouteSchema =
            serde_json::from_str(r#"{"name": "r", "description": "d"}"#).unwrap();
        assert_eq!(schema.parameters, serde_json::json!({}));
        assert!(schema.examples.is_empty());
        assert!(schema.intents.is_empty());
    }

    // -- Post-process (learning loop) --------------------------------

    #[test]
    fn post_process_defaults_to_disabled() {
        let cfg = PostProcessConfig::default();
        assert!(!cfg.workflow_extraction, "extraction is opt-in");
        assert_eq!(
            cfg.workflow_extraction_mode,
            WorkflowExtractionMode::Frontier,
            "default scope is frontier-assisted only"
        );
    }

    #[test]
    fn post_process_absent_section_defaults_cleanly() {
        let cfg: RouterConfig =
            serde_json::from_str(r#"{"server": {"bind_addr": "127.0.0.1:0"}}"#).unwrap();
        assert!(
            !cfg.post_process.workflow_extraction,
            "absent post_process section defaults extraction off"
        );
        assert_eq!(
            cfg.post_process.workflow_extraction_mode,
            WorkflowExtractionMode::Frontier,
            "absent mode field defaults to frontier"
        );
    }

    #[test]
    fn post_process_round_trips() {
        let json = r#"{ "workflow_extraction": true }"#;
        let cfg: PostProcessConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.workflow_extraction);
        assert_eq!(
            cfg.workflow_extraction_mode,
            WorkflowExtractionMode::Frontier,
            "absent mode field keeps the frontier default"
        );

        let serialized = serde_json::to_string(&cfg).unwrap();
        let back: PostProcessConfig = serde_json::from_str(&serialized).unwrap();
        assert!(back.workflow_extraction);
        assert_eq!(back.workflow_extraction_mode, cfg.workflow_extraction_mode);
    }

    #[test]
    fn workflow_extraction_mode_parses_both_variants() {
        let all: WorkflowExtractionMode = serde_json::from_str(r#""all""#).expect("all parses");
        assert_eq!(all, WorkflowExtractionMode::All);

        let frontier: WorkflowExtractionMode =
            serde_json::from_str(r#""frontier""#).expect("frontier parses");
        assert_eq!(frontier, WorkflowExtractionMode::Frontier);

        assert!(serde_json::from_str::<WorkflowExtractionMode>(r#""bogus""#).is_err());
    }

    #[test]
    fn router_config_parses_post_process_section() {
        let json = r#"{
            "post_process": { "workflow_extraction": true },
            "charts": { "dir": "env/workflows/charts" }
        }"#;
        let cfg: RouterConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.post_process.workflow_extraction);
        assert_eq!(cfg.charts.dir.as_deref(), Some("env/workflows/charts"));
        assert_eq!(
            cfg.post_process.workflow_extraction_mode,
            WorkflowExtractionMode::Frontier,
            "existing configs without the new field still deserialize"
        );
    }

    #[test]
    fn router_config_parses_extraction_mode_all() {
        let json = r#"{
            "post_process": {
                "workflow_extraction": true,
                "workflow_extraction_mode": "all"
            }
        }"#;
        let cfg: RouterConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.post_process.workflow_extraction);
        assert_eq!(
            cfg.post_process.workflow_extraction_mode,
            WorkflowExtractionMode::All
        );
    }

    #[test]
    fn model_entry_serde_defaults_read_canonical_constants() {
        // The same constants `RoutingTarget` reads (divergence guard).
        let entry: ModelEntry = serde_json::from_value(serde_json::json!({
            "endpoint": "http://localhost:8080/v1/chat/completions",
            "intelligence": 2,
            "cost_input": 1e-6,
            "cost_output": 6e-6,
            "cost_cached_read": 4e-7,
            "speed": 8,
        }))
        .unwrap();
        assert_eq!(
            entry.total_timeout_ms,
            common_core::constants::DEFAULT_TOTAL_TIMEOUT_MS
        );
        assert_eq!(
            entry.idle_timeout_ms,
            common_core::constants::DEFAULT_IDLE_TIMEOUT_MS
        );
        assert_eq!(
            entry.retry_base_interval_s,
            common_core::constants::DEFAULT_RETRY_INTERVAL_S
        );
    }

    // -- Classification-tree derived flat views ------------------------

    fn tree_section() -> serde_json::Value {
        serde_json::json!({
            "classification": {
                "root": {
                    "type": "classifier",
                    "description": "router",
                    "model": "fast",
                    "children": [
                        {
                            "key": "code",
                            "description": "programming",
                            "node": { "type": "terminal", "route": "code", "group": "code" }
                        },
                        {
                            "key": "brand_new",
                            "description": "not in flat routes",
                            "node": { "type": "terminal", "route": "brand_new", "group": "question" }
                        }
                    ]
                }
            },
            "models": {
                "fast": {"endpoint": "http://upstream.test/v1/chat/completions", "name": "fast", "intelligence": 1, "cost_input": 1e-6, "cost_output": 6e-6, "cost_cached_read": 4e-7, "speed": 8}
            },
            "model_groups": {
                "fast": ["fast"],
                "code": ["fast"],
                "question": ["fast"]
            },
            "routes": {
                "code": {"group": "code", "pipelines": ["default"], "description": "code"}
            }
        })
    }

    #[test]
    fn routes_view_synthesizes_terminal_routes() {
        let cfg: RouterConfig = serde_json::from_value(tree_section()).unwrap();
        let routes = cfg.routes_view();
        // Explicit flat route is preserved.
        assert_eq!(routes["code"].group, "code");
        assert_eq!(routes["code"].pipelines, vec!["default".to_string()]);
        // Terminal route without a flat entry is synthesized from its group.
        assert_eq!(routes["brand_new"].group, "question");
        assert_eq!(routes["brand_new"].pipelines, vec!["default".to_string()]);
    }

    #[test]
    fn routes_view_flat_config_is_unchanged() {
        let cfg: RouterConfig =
            serde_json::from_str(r#"{"routes": {"a": {"group": "g"}}}"#).unwrap();
        assert_eq!(cfg.routes_view().len(), 1);
        assert!(cfg.routes_view().contains_key("a"));
    }

    #[test]
    fn routing_config_derives_system_prompt_from_tree() {
        let cfg: RouterConfig = serde_json::from_value(tree_section()).unwrap();
        let routing = cfg.routing_config();
        assert!(
            routing.system_prompt.contains("You are a router."),
            "tree-derived system prompt, got: {}",
            routing.system_prompt
        );
        assert!(
            routing.routes.contains_key("brand_new"),
            "derived routes reach the RoutingConfig so terminal resolution works"
        );
    }

    #[test]
    fn routing_config_keeps_explicit_system_prompt() {
        let mut cfg: RouterConfig = serde_json::from_value(tree_section()).unwrap();
        cfg.system_prompt = "custom preamble".into();
        let routing = cfg.routing_config();
        assert_eq!(routing.system_prompt, "custom preamble");
    }

    // -- In-group target-matching knob (PipelineParams) ----------------

    #[test]
    fn pipeline_params_target_match_defaults() {
        let defaults = builder::PipelineParams::default();
        assert_eq!(
            defaults.target_match,
            builder::TargetMatchMode::SelfAssess,
            "the self-assess ladder is the default policy (-4.6)"
        );
        assert_eq!(
            defaults.target_match_timeout_ms,
            common_core::constants::DEFAULT_TOTAL_TIMEOUT_MS,
            "per-self-assessment budget defaults to the shared total-timeout constant"
        );
    }

    #[test]
    fn pipeline_params_target_match_absent_fields_deserialize_to_defaults() {
        // A pipeline that omits both knob fields must deserialize to the same
        // defaults (mirror the `classifier_retry_max` pattern) - existing
        // configs stay byte-identical.
        let cfg: RouterConfig = serde_json::from_str(
            r#"{
                "pipelines": {"default": {"classifier": true, "classifier_model": "fast"}}
            }"#,
        )
        .expect("valid config");
        let params = &cfg.pipelines["default"];
        assert_eq!(params.target_match, builder::TargetMatchMode::SelfAssess);
        assert_eq!(
            params.target_match_timeout_ms,
            common_core::constants::DEFAULT_TOTAL_TIMEOUT_MS
        );
    }

    #[test]
    fn pipeline_params_target_match_parses_both_variants() {
        let self_assess: builder::TargetMatchMode =
            serde_json::from_str(r#""self_assess""#).expect("self_assess parses");
        assert_eq!(self_assess, builder::TargetMatchMode::SelfAssess);

        let static_mode: builder::TargetMatchMode =
            serde_json::from_str(r#""static""#).expect("static parses");
        assert_eq!(static_mode, builder::TargetMatchMode::Static);

        assert!(
            serde_json::from_str::<builder::TargetMatchMode>(r#""bogus""#).is_err(),
            "unknown policy must be rejected, not silently defaulted"
        );
    }

    #[test]
    fn pipeline_params_target_match_round_trips() {
        // Non-default values survive a serialize - deserialize cycle.
        let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
            "pipelines": {
                "default": {
                    "classifier": true,
                    "classifier_model": "fast",
                    "target_match": "static",
                    "target_match_timeout_ms": 12345
                }
            }
        }))
        .unwrap();
        assert_eq!(cfg.pipelines["default"].target_match, builder::TargetMatchMode::Static);
        assert_eq!(cfg.pipelines["default"].target_match_timeout_ms, 12345);

        let serialized = serde_json::to_string(&cfg).unwrap();
        let back: RouterConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(back.pipelines["default"].target_match, builder::TargetMatchMode::Static);
        assert_eq!(back.pipelines["default"].target_match_timeout_ms, 12345);
    }

    // -- Instance-pool declaration -------------------------------------

    fn profile_json(name: &str, count: u32, group: &str, num_ctx: u64) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "count": count,
            "group": group,
            "num_ctx": num_ctx,
        })
    }

    #[test]
    fn instances_count_expansion_names_siblings_in_shared_group() {
        let entry: ModelEntry = serde_json::from_value(serde_json::json!({
            "endpoint": "http://x/v1/chat/completions",
            "intelligence": 2,
            "cost_input": 1e-06, "cost_output": 6e-06, "cost_cached_read": 4e-07,
            "speed": 8,
            "instances": {
                "swarm": profile_json("swarm", 3, "swarm", 16384),
                "ledger": { "num_ctx": 131072, "pinned": true, "default": true }
            }
        }))
        .unwrap();

        let profiles = entry.instance_profiles();
        assert_eq!(profiles.len(), 4);
        // Profiles are emitted in sorted map-key order: ledger < swarm.
        assert_eq!(profiles[0].name.as_deref(), Some("ledger"));
        assert_eq!(profiles[0].group.as_deref(), Some("ledger"));
        assert!(profiles[0].pinned);
        assert!(profiles[0].default);
        // count: 3 -> `<key>-0` .. `<key>-2` in the shared group.
        assert_eq!(profiles[1].name.as_deref(), Some("swarm-0"));
        assert_eq!(profiles[1].group.as_deref(), Some("swarm"));
        assert_eq!(profiles[2].name.as_deref(), Some("swarm-1"));
        assert_eq!(profiles[3].name.as_deref(), Some("swarm-2"));
        assert_eq!(profiles[3].group.as_deref(), Some("swarm"));
        assert_eq!(profiles[3].num_ctx, 16384);
    }

    #[test]
    fn instances_single_profile_defaults_name_to_map_key() {
        let entry: ModelEntry = serde_json::from_value(serde_json::json!({
            "endpoint": "http://x/v1/chat/completions",
            "intelligence": 1,
            "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0,
            "speed": 1,
            "instances": { "scratch": { "num_ctx": 131072, "sleep_idle_seconds": 30 } }
        }))
        .unwrap();
        let profiles = entry.instance_profiles();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name.as_deref(), Some("scratch"));
        assert_eq!(profiles[0].group.as_deref(), Some("scratch"));
        assert_eq!(profiles[0].sleep_idle_seconds, Some(30));
        assert_eq!(profiles[0].count, 1);
    }

    #[test]
    fn old_sessions_key_still_parses_as_instances() {
        let entry: ModelEntry = serde_json::from_value(serde_json::json!({
            "endpoint": "http://x/v1/chat/completions",
            "intelligence": 1,
            "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0,
            "speed": 1,
            "sessions": { "ctx16384": { "num_ctx": 16384 } }
        }))
        .unwrap();
        let instances = entry.instances.expect("sessions alias maps into instances");
        assert_eq!(instances.len(), 1);
        assert!(instances.contains_key("ctx16384"));
    }

    #[test]
    fn no_instances_yields_empty_profile_list() {
        let entry: ModelEntry = serde_json::from_value(serde_json::json!({
            "endpoint": "http://x/v1/chat/completions",
            "intelligence": 1,
            "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0,
            "speed": 1,
        }))
        .unwrap();
        assert!(entry.instance_profiles().is_empty());
    }

    #[test]
    fn warm_alias_maps_to_no_sleep() {
        let entry: ModelEntry = serde_json::from_value(serde_json::json!({
            "endpoint": "http://x/v1/chat/completions",
            "intelligence": 1,
            "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0,
            "speed": 1,
            "instances": { "swarm": { "num_ctx": 16384, "warm": true } }
        }))
        .unwrap();
        let profiles = entry.instance_profiles();
        assert!(profiles[0].no_sleep);
    }

    // -- Pool vs default qualifier -------------------------------

    /// The reference swarm entry: a count=3 non-default `swarm` work pool, a
    /// pinned `default: true` ledger, and a non-default scratch profile.
    fn reference_swarm_entry() -> ModelEntry {
        serde_json::from_value(serde_json::json!({
            "endpoint": "http://x/v1/chat/completions",
            "name": "abiray/lfm2.5-2.6b-heretic-abliterated",
            "intelligence": 2,
            "cost_input": 1e-06, "cost_output": 6e-06, "cost_cached_read": 4e-07,
            "speed": 8,
            "instances": {
                "swarm": profile_json("swarm", 3, "swarm", 16384),
                "ledger": { "num_ctx": 131072, "pinned": true, "default": true },
                "scratch": { "num_ctx": 131072, "sleep_idle_seconds": 30 }
            }
        }))
        .expect("reference swarm entry parses")
    }

    #[test]
    fn pool_qualifier_reference_config_targets_swarm() {
        let entry = reference_swarm_entry();
        assert_eq!(
            entry.pool_qualifier().as_deref(),
            Some("swarm"),
            "the largest non-default profile (count=3) is the work pool"
        );
    }

    #[test]
    fn pool_qualifier_vs_default_qualifier_two_intents_two_answers() {
        // The two intents must diverge on the same entry: pool = swarm (the
        // work group), default = ledger (the client-facing default instance).
        let entry = reference_swarm_entry();
        assert_eq!(entry.pool_qualifier().as_deref(), Some("swarm"));
        assert_eq!(
            entry.default_dispatch_qualifier().as_deref(),
            Some("ledger")
        );
    }

    #[test]
    fn pool_qualifier_ledger_only_defaults_to_ledger() {
        let entry: ModelEntry = serde_json::from_value(serde_json::json!({
            "endpoint": "http://x/v1/chat/completions",
            "intelligence": 1,
            "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0,
            "speed": 1,
            "instances": { "ledger": { "num_ctx": 131072, "default": true } }
        }))
        .unwrap();
        assert_eq!(entry.pool_qualifier().as_deref(), Some("ledger"));
    }

    #[test]
    fn pool_qualifier_single_shared_group() {
        let entry: ModelEntry = serde_json::from_value(serde_json::json!({
            "endpoint": "http://x/v1/chat/completions",
            "intelligence": 1,
            "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0,
            "speed": 1,
            "instances": {
                "a": { "num_ctx": 8192, "group": "shared" },
                "b": { "num_ctx": 8192, "group": "shared" }
            }
        }))
        .unwrap();
        assert_eq!(entry.pool_qualifier().as_deref(), Some("shared"));
    }

    #[test]
    fn pool_qualifier_no_instances_is_none() {
        let entry: ModelEntry = serde_json::from_value(serde_json::json!({
            "endpoint": "http://x/v1/chat/completions",
            "intelligence": 1,
            "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0,
            "speed": 1,
        }))
        .unwrap();
        assert!(entry.pool_qualifier().is_none());
    }

    #[test]
    fn sidecar_absent_section_defaults_cleanly() {
        let cfg: RouterConfig =
            serde_json::from_str(r#"{"server": {"bind_addr": "127.0.0.1:0"}}"#).unwrap();
        assert_eq!(cfg.sidecar.poll_interval_s, 5);
        assert_eq!(cfg.sidecar.vram_low_watermark_bytes, 1073741824);
        assert_eq!(cfg.sidecar.evict_batch, 1);
        assert!(cfg.sidecar.vram_total_bytes.is_none());
        assert!(cfg.sidecar.minimum_remaining_vram.is_none());
        assert!(cfg.sidecar.slot_save_path.is_none());
        assert!(cfg.sidecar.api_key_env.is_none());
    }

    #[test]
    fn sidecar_section_round_trips() {
        let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
            "sidecar": {
                "poll_interval_s": 10,
                "vram_low_watermark_bytes": 536870912,
                "evict_batch": 2,
                "vram_total_bytes": 1048576,
                "minimum_remaining_vram": 2147483648u64,
                "slot_save_path": "/srv/slots",
                "api_key_env": "LLAMA_API_KEY",
            }
        }))
        .unwrap();
        assert_eq!(cfg.sidecar.poll_interval_s, 10);
        assert_eq!(cfg.sidecar.vram_low_watermark_bytes, 536870912);
        assert_eq!(cfg.sidecar.evict_batch, 2);
        assert_eq!(cfg.sidecar.vram_total_bytes, Some(1048576));
        assert_eq!(cfg.sidecar.minimum_remaining_vram, Some(2147483648));
        assert_eq!(cfg.sidecar.slot_save_path.as_deref(), Some("/srv/slots"));
        assert_eq!(cfg.sidecar.api_key_env.as_deref(), Some("LLAMA_API_KEY"));
    }

    #[test]
    fn sidecar_allocation_limit_from_minimum_remaining() {
        // With a ceiling configured, the budget is ceiling - minimum remaining.
        let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
            "sidecar": { "vram_total_bytes": 10000, "minimum_remaining_vram": 2000 }
        }))
        .unwrap();
        assert_eq!(cfg.sidecar.allocation_limit(), Some(8000));
    }

    #[test]
    fn sidecar_allocation_limit_without_ceiling_falls_back_to_detection() {
        // No explicit ceiling: the budget is computed from the detected total.
        // The host has a ROCm device (mem_info_vram_total > 0), so the limit is
        // detection - minimum_remaining; a missing floor yields the full total.
        // Detection reads `/sys/class/drm` through the capability-gated fs
        // helper, so it runs under the `FsCapability` grant the serving
        // path establishes at boot.
        let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
            "sidecar": { "minimum_remaining_vram": 2147483648u64 }
        }))
        .unwrap();
        let (detected, allocation_limit) = fluent_concurrency::scope::CURRENT_CAPS.sync_scope(
            fluent_concurrency::capability::default_capability_set(),
            || {
                (
                    super::detect_device_vram_total(),
                    cfg.sidecar.allocation_limit(),
                )
            },
        );
        assert!(
            detected.is_some(),
            "ROCm sysfs mem_info_vram_total present on this host"
        );
        assert_eq!(
            allocation_limit,
            detected.map(|t| t.saturating_sub(2147483648))
        );
    }

    #[test]
    fn default_params_absent_section_defaults_cleanly() {
        let cfg: RouterConfig =
            serde_json::from_str(r#"{"server": {"bind_addr": "127.0.0.1:0"}}"#).unwrap();
        assert_eq!(cfg.default_params.num_ctx, 16384);
        assert_eq!(cfg.default_params.batch_size, 4096);
        assert_eq!(cfg.default_params.n_gpu_layers, 999);
        assert!(cfg.default_params.params.is_none());
    }

    #[test]
    fn default_params_section_round_trips() {
        let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
            "default_params": {
                "num_ctx": 8192,
                "batch_size": 512,
                "ubatch_size": 256,
                "cache_type_k": "f16",
                "cache_type_v": "f16",
                "flash_attn": "off",
                "n_gpu_layers": 0,
                "n_cpu_moe": 4,
                "sleep_idle_seconds": 30,
                "stream": false,
                "filter_thinking": true,
                "params": { "temperature": 0.2 }
            }
        }))
        .unwrap();
        assert_eq!(cfg.default_params.num_ctx, 8192);
        assert_eq!(cfg.default_params.batch_size, 512);
        assert_eq!(cfg.default_params.ubatch_size, 256);
        assert_eq!(cfg.default_params.cache_type_k, "f16");
        assert_eq!(cfg.default_params.cache_type_v, "f16");
        assert_eq!(cfg.default_params.flash_attn.as_deref(), Some("off"));
        assert_eq!(cfg.default_params.n_gpu_layers, 0);
        assert_eq!(cfg.default_params.n_cpu_moe, 4);
        assert_eq!(cfg.default_params.sleep_idle_seconds, 30);
        assert!(!cfg.default_params.stream);
        assert!(cfg.default_params.filter_thinking);
        assert_eq!(
            cfg.default_params
                .params
                .as_ref()
                .and_then(|p| p.get("temperature")),
            Some(&serde_json::json!(0.2))
        );
    }

    #[test]
    fn default_params_ctx_size_alias_parses() {
        let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
            "default_params": { "ctx_size": 32768 }
        }))
        .unwrap();
        assert_eq!(cfg.default_params.num_ctx, 32768);
    }

    // -- Ledger + session composition sections ------------------------

    #[test]
    fn router_config_absent_ledger_and_session_sections_default_to_none() {
        let cfg: RouterConfig =
            serde_json::from_str(r#"{"server": {"bind_addr": "127.0.0.1:0"}}"#).unwrap();
        assert!(
            cfg.ledger.is_none(),
            "absent ledger section -> no ledger at boot (byte-identical behavior)"
        );
        assert!(
            cfg.session.is_none(),
            "absent session section -> no session registry at boot (byte-identical behavior)"
        );
    }

    #[test]
    fn ledger_and_session_sections_round_trip() {
        let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
            "ledger": {
                "path": "data/ledger.sqlite",
                "model": "swarm",
                "max_summary_tokens": 300,
            },
            "session": { "root": "data/sessions" },
        }))
        .unwrap();

        let ledger = cfg.ledger.as_ref().expect("ledger section parsed");
        assert_eq!(ledger.path.as_deref(), Some("data/ledger.sqlite"));
        assert_eq!(ledger.model.as_deref(), Some("swarm"));
        assert_eq!(ledger.max_summary_tokens, 300);

        let session = cfg.session.as_ref().expect("session section parsed");
        assert_eq!(session.root.as_deref(), Some("data/sessions"));

        let serialized = serde_json::to_string(&cfg).unwrap();
        let back: RouterConfig = serde_json::from_str(&serialized).unwrap();
        let back_ledger = back.ledger.expect("ledger round-trips");
        assert_eq!(back_ledger.path, ledger.path);
        assert_eq!(back_ledger.model, ledger.model);
        assert_eq!(back_ledger.max_summary_tokens, ledger.max_summary_tokens);
        assert_eq!(back.session.unwrap().root, session.root);
    }

    #[test]
    fn ledger_section_partial_defaults_max_summary_tokens() {
        // A ledger section that omits `max_summary_tokens` gets the named
        // constant default; the shipped config round-trips cleanly.
        let cfg: RouterConfig =
            serde_json::from_value(serde_json::json!({ "ledger": { "model": "swarm" } })).unwrap();
        let ledger = cfg.ledger.as_ref().expect("ledger parsed");
        assert_eq!(ledger.max_summary_tokens, DEFAULT_LEDGER_MAX_SUMMARY_TOKENS);
        assert_eq!(ledger.model.as_deref(), Some("swarm"));
        assert!(ledger.path.is_none());

        let serialized = serde_json::to_string(&cfg).unwrap();
        let back: RouterConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(
            back.ledger.unwrap().max_summary_tokens,
            DEFAULT_LEDGER_MAX_SUMMARY_TOKENS
        );
    }

    #[test]
    fn ledger_background_tiering_fields_default_absent() {
        // All background-tiering fields are default-absent so existing
        // `coral-router.json` files deserialize unchanged.
        let cfg: RouterConfig =
            serde_json::from_value(serde_json::json!({ "ledger": { "model": "swarm" } })).unwrap();
        let ledger = cfg.ledger.as_ref().unwrap();
        assert!(!ledger.background_tiering, "tiering is opt-in");
        assert!(ledger.tier_model.is_none());
        assert_eq!(ledger.lod4_max_chars, 240, "default lod4 cap");
        assert_eq!(ledger.lod5_max_chars, 80, "default lod5 cap");
        assert_eq!(ledger.tier_batch_size, 8);
        assert_eq!(ledger.tier_poll_interval_ms, 100);
    }

    #[test]
    fn ledger_background_tiering_fields_round_trip() {
        // A fully-populated ledger section round-trips knobs.
        let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
            "ledger": {
                "model": "swarm",
                "background_tiering": true,
                "tier_model": "qwen3.5-4b",
                "lod4_max_chars": 200,
                "lod5_max_chars": 60,
                "tier_batch_size": 16,
                "tier_poll_interval_ms": 250,
            }
        }))
        .unwrap();
        let ledger = cfg.ledger.as_ref().unwrap();
        assert!(ledger.background_tiering);
        assert_eq!(ledger.tier_model.as_deref(), Some("qwen3.5-4b"));
        assert_eq!(ledger.lod4_max_chars, 200);
        assert_eq!(ledger.lod5_max_chars, 60);
        assert_eq!(ledger.tier_batch_size, 16);
        assert_eq!(ledger.tier_poll_interval_ms, 250);
    }

    // -- Ledger orchestrator section --------------------------------

    #[test]
    fn orchestrator_section_default_absent() {
        // Existing ledger configs without an `orchestrator` section keep the
        // coordinator disabled (opt-in) and today's defaults.
        let cfg: RouterConfig =
            serde_json::from_value(serde_json::json!({ "ledger": { "model": "swarm" } })).unwrap();
        let orch = &cfg.ledger.as_ref().unwrap().orchestrator;
        assert!(!orch.enabled, "coordinator is opt-in");
        assert_eq!(
            orch.kv_policy,
            crate::dag_session::KvSnapshotPolicy::RestoreIfSameModel
        );
        assert_eq!(orch.prompt_budget_chars, 32768);
        assert_eq!(orch.role, "agent");
    }

    #[test]
    fn orchestrator_section_round_trip() {
        let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
            "ledger": {
                "model": "swarm",
                "orchestrator": {
                    "enabled": true,
                    "kv_policy": "never_restore",
                    "prompt_budget_chars": 16384,
                    "role": "planner"
                }
            }
        }))
        .unwrap();
        let orch = &cfg.ledger.as_ref().unwrap().orchestrator;
        assert!(orch.enabled);
        assert_eq!(orch.kv_policy, crate::dag_session::KvSnapshotPolicy::NeverRestore);
        assert_eq!(orch.prompt_budget_chars, 16384);
        assert_eq!(orch.role, "planner");
    }

    #[test]
    fn orchestrator_kv_policy_parses_all_variants() {
        use crate::dag_session::KvSnapshotPolicy as P;
        let a: P = serde_json::from_str(r#""restore_if_same_model""#).unwrap();
        let b: P = serde_json::from_str(r#""always_restore""#).unwrap();
        let c: P = serde_json::from_str(r#""never_restore""#).unwrap();
        assert_eq!(a, P::RestoreIfSameModel);
        assert_eq!(b, P::AlwaysRestore);
        assert_eq!(c, P::NeverRestore);
    }

    #[test]
    fn kv_snapshot_policy_round_trips_through_serde() {
        use crate::dag_session::KvSnapshotPolicy as P;
        for p in [P::RestoreIfSameModel, P::AlwaysRestore, P::NeverRestore] {
            let json = serde_json::to_string(&p).unwrap();
            let back: P = serde_json::from_str(&json).unwrap();
            assert_eq!(back, p, "round-trip {p:?} through {json}");
        }
    }
}
