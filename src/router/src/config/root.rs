//! Router configuration root types - thin facade ownership (M3).
//! This module owns RouterConfig and its directly-associated helpers.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use fluent_wvr::{Describable, FieldAccess};

use crate::logging::LoggingConfig;
use crate::config::builder::PipelineParams;
use crate::config::classification::ClassificationTree;
use crate::config::escalation::ModelGroup;
use crate::config::filters::MockConfig;
use crate::config::rounds::{BoundedRounds, EscalationConfidence, SeverityThreshold};
use crate::config::routing::{RoleEntry, RouteRef};

/// Decompose a possibly-qualified model key (`base:qualifier`) into the base
/// `models` config key and the optional qualifier. A bare key (or a malformed
/// `:` split) passes through as `(key, None)`. The qualifier selects a named
/// instance/group of the base model; `latest` is normalized to the entry's
/// default dispatch point by the callers that honor it.
///
/// The typed form is `crate::pipeline::QualifiedModelId` (router-local newtype,
/// `pipeline.rs:12`) — this function is the low-level `&str` parser used by
/// callers that need a zero-alloc split. Prefer `QualifiedModelId::parse` for
/// owned values. Also re-exported to the `coral-router` composition root,
/// which resolves qualified instance keys for the embedding duty chain.
pub fn split_model_key(key: &str) -> (&str, Option<&str>) {
    match key.split_once(':') {
        Some((base, qualifier)) if !base.is_empty() && !qualifier.is_empty() => {
            (base, Some(qualifier))
        }
        _ => (key, None),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, FieldAccess, Describable)]
#[serde(deny_unknown_fields)]
pub struct RouterConfig {
    /// Named pipeline stage tables (keyed by pipeline name). This is stage
    /// configuration — not flat route authoring — so it stays past M3c: the
    /// tree carries routes/groups/descriptions, never stage knobs
    /// (deterministic prefilter, blacklist, nlp flags, thresholds).
    #[field(skip)]
    #[serde(default)]
    pub pipelines: HashMap<String, PipelineParams>,
    #[field(skip)]
    #[serde(default)]
    pub models: HashMap<String, ModelEntry>,
    #[field(skip)]
    #[serde(default)]
    pub model_groups: HashMap<String, ModelGroup>,
    /// Routing-vocabulary table: role name → candidate model keys + the
    /// inference point each candidate serves. Absent (the default) leaves
    /// today's key-based routing untouched; roles resolve to concrete
    /// `base:qualifier` targets per request once consumed.
    #[field(skip)]
    #[serde(default)]
    pub roles: HashMap<String, RoleEntry>,
    #[field(desc="safety threshold", min=0.0, max=1.0)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety_threshold: Option<f64>,
    #[field(desc="default route")]
    #[serde(default = "default_route")]
    pub default_route: String,
    /// What the classifier stage does when its LLM call fails or its response
    /// cannot be parsed. Safe default: reject rather than route on fabricated
    /// scores.
    #[field(skip)]
    #[serde(default = "default_classifier_failure_policy")]
    pub classifier_failure_policy: ClassifierFailurePolicy,
    #[field(skip)]
    #[serde(default = "ServerConfig::default")]
    pub server: ServerConfig,
    #[field(skip)]
    #[serde(default)]
    pub logging: LoggingConfig,
    /// Chart-embedding model: a role name first (the `embedding` role
    /// mapping), else a literal `models` key. `None` falls back to
    /// `charts.selector_model`, then the classifier role head.
    #[field(skip)]
    #[serde(default)]
    pub embedding_model: Option<String>,
    /// Chart-candidate reranker model key. Selects an entry
    /// from `models`. `None` skips the rerank stage (Step 2 - Step 3
    /// directly).
    #[field(skip)]
    #[serde(default)]
    pub reranker_model: Option<String>,
    #[field(skip)]
    #[serde(default)]
    pub mock: Option<MockConfig>,
    /// Chart store configuration (DAG workflow library).
    #[field(skip)]
    #[serde(default)]
    pub charts: ChartsConfig,
    /// Post-processing configuration.
    #[field(skip)]
    #[serde(default)]
    pub post_process: PostProcessConfig,
    /// Nested classification tree.  `Some` switches the classifier stage
    /// into tree-driven mode; the flat pipeline sections remain for
    /// backward compatibility and are derived from the tree where the rest
    /// of the server needs flat views.
    #[field(skip)]
    #[serde(default)]
    pub classification: Option<ClassificationTree>,
    /// Rigor-route configuration. `None` (the default) leaves the route
    /// present but unconfigured - requests return an explicit `Unconfigured`
    /// error, never a crash.
    #[field(skip)]
    #[serde(default)]
    pub rigor: Option<RigorConfig>,
    /// Sidecar instance-management policy. Governs the sidecar task that
    /// reconciles the fork's shared-weight instances against the configured
    /// profiles, polls `/memory`, and evicts/allocates KV + compute only (the
    /// weights stay loaded in `llama-server`).
    #[field(skip)]
    #[serde(default)]
    pub sidecar: SidecarConfig,
    /// Ledger composition section. `Some` opts the boot path into opening
    /// a `ContentNodeLedger` (with a real `Summarizer` backend targeting
    /// `<base>:ledger`) so LOD derivation exists at runtime. `None` (the
    /// default) leaves today's behavior - no ledger at boot.
    #[field(skip)]
    #[serde(default)]
    pub ledger: Option<LedgerConfig>,
    /// Async review composition section. `Some` opts the boot path into the
    /// `ReviewWorker` (async parse review, correction reuse, parse_review
    /// ledger handoff). `None` (the default) leaves today's behavior — no
    /// review worker at boot.
    #[field(skip)]
    #[serde(default)]
    pub review: Option<ReviewConfig>,
    /// Async overlay composition section (ROADMAP_20260827_ORT §6). `Some`
    /// opts the boot path into the entity-link overlay worker — the async
    /// plane that writes `EntityLink` candidates to `overlay_candidates` (never
    /// a doc-id write). `None` (the default) leaves today's behavior — no
    /// overlay worker at boot.
    #[field(skip)]
    #[serde(default)]
    pub overlay: Option<OverlayConfig>,
    /// Session composition section. `Some` opts the boot path into a
    /// `SessionRegistry` (canonical session home) so rigor rewind and
    /// checkpoint/rewind state exist at runtime. `None` (the default) leaves
    /// today's behavior - no session registry at boot.
    #[field(skip)]
    #[serde(default)]
    pub session: Option<SessionConfig>,
    /// In-process ONNX fleet: one optional role-scoped model declaration per
    /// role (Encoder / PII / Router / Policy / ColBERT). Every role is optional
    /// and the pipeline is fully functional (pure-deterministic) with none of
    /// them loaded. The config vocabulary parallels the llama.cpp
    /// `ModelEntry`/`roles.<role>.params` surface (resident/pinned residency, run
    /// and idle timeouts, sampling `params`), but the models run in-process via
    /// `ort` — never a spawned `llama-server`. Absent → fully fail-open.
    #[field(skip)]
    #[serde(default)]
    pub onnx: Option<fluent_llm::onnx_config::OnnxFleetConfig>,
    /// Top-level ONNX role keys: an alternative to the nested `onnx` section.
    /// When `onnx` is absent but any of these are present, they are merged
    /// into `onnx` during `apply_defaults()`. This supports the simplified
    /// config format where roles are declared at the root level.
    #[field(skip)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colbert: Option<fluent_llm::onnx_config::OnnxRoleConfig>,
    #[field(skip)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoder: Option<fluent_llm::onnx_config::OnnxRoleConfig>,
    #[field(skip)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pii: Option<fluent_llm::onnx_config::OnnxRoleConfig>,
    #[field(skip)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router: Option<fluent_llm::onnx_config::OnnxRoleConfig>,
    /// ONNX CPU decode concurrency cap (M10): the single `Limiter` budget that
    /// bounds concurrent ONNX decodes (CPU-bound, never blocks the async executor).
    /// Defaults to `DEFAULT_ONNX_LIMITER_CAP` (2).
    #[field(desc = "ONNX CPU decode concurrency cap", min = 1.0, max = 16.0)]
    #[serde(default = "default_onnx_limiter_cap")]
    pub onnx_limiter_cap: usize,
    /// ONNX intra-op threads (M10): per-decode parallelism, forwarded to `ort`
    /// `intra_threads`. Defaults to `DEFAULT_ONNX_THREADS` (1, deterministic decode).
    #[field(desc = "ONNX intra-op threads", min = 1.0, max = 8.0)]
    #[serde(default = "default_onnx_threads")]
    pub onnx_threads: usize,
    /// GGUF model directory for the admin CLI commands (`list`, `scan`, `rm`,
    /// `show`, `pull`, and `ps` weights resolution). Overridable per-invocation
    /// with `--gguf-dir`; `None` falls back to the built-in default.
    #[field(skip)]
    #[serde(default)]
    pub gguf_dir: Option<String>,
    /// The shared inference-backend registry (llama + onnx adapters),
    /// installed by the composition root behind a lock so backends whose
    /// inputs boot later (the llama pool) can register after the first
    /// resolvers run. Not serialized. When present, `local_backend` /
    /// `local_backend_for_instance` resolve through it; when absent (unit
    /// tests, registry-less boots) the legacy construction path serves
    /// instead, byte-identically.
    #[field(skip)]
    #[serde(skip)]
    pub(crate) inference_registry:
        Option<Arc<std::sync::RwLock<fluent_llm::backend::InferenceRegistry>>>,
}

impl Default for RouterConfig {
    fn default() -> Self {
        let mut pipelines = HashMap::new();
        pipelines.insert("default".into(), PipelineParams::default());
        Self {
            pipelines,
            models: HashMap::new(),
            model_groups: HashMap::new(),
            roles: HashMap::new(),
            safety_threshold: None,
            default_route: "local".into(),
            classifier_failure_policy: ClassifierFailurePolicy::Reject,
            server: ServerConfig::default(),
            logging: LoggingConfig::default(),
            embedding_model: None,
            reranker_model: None,
            mock: None,
            charts: ChartsConfig::default(),
            post_process: PostProcessConfig::default(),
            classification: None,
            rigor: None,
            sidecar: SidecarConfig::default(),
            ledger: None,
            review: None,
            overlay: None,
            session: None,
            onnx_limiter_cap: common_core::constants::DEFAULT_ONNX_LIMITER_CAP,
            onnx_threads: common_core::constants::DEFAULT_ONNX_THREADS,
            gguf_dir: None,
            onnx: None,
            colbert: None,
            encoder: None,
            pii: None,
            router: None,
            inference_registry: None,
        }
    }
}

impl RouterConfig {
    /// Reserved `models` key: the fleet template, never a routable model.
    /// Every other `models` entry sparse-inherits from it at parse (see
    /// [`Self::from_json_value`]).
    pub const DEFAULT_MODEL_KEY: &'static str = "default";

    /// Parse a router config from its JSON text through the boot-equivalent
    /// path. Two sparse-inheritance legs run **before** typing, so entries
    /// declare only deltas:
    ///
    /// 1. `models.default` fills every absent top-level key of each sibling
    ///    model entry (its `params` object shallow-fills the sibling's
    ///    missing sampling keys); `roles.default.params` run knobs fill the
    ///    same knobs of `models.default.params` first, so one fleet block
    ///    serves both vocabularies.
    /// 2. `roles.default.params` shallow-fills every other role's `params`
    ///    (legacy nested `params`-inside-`params` objects are hoisted flat
    ///    first and never interpreted as a layer).
    ///
    /// Direct `serde_json::from_str::<RouterConfig>` skips both legs (absent
    /// keys then read as struct defaults) — composition roots must use this
    /// constructor; unit tests that need the inheritance use it too.
    pub fn from_json_str(s: &str) -> serde_json::Result<Self> {
        Self::from_json_value(serde_json::from_str(s)?)
    }

    /// [`Self::from_json_str`] over an already-parsed value.
    pub fn from_json_value(mut value: serde_json::Value) -> serde_json::Result<Self> {
        if let Err(msg) = reject_legacy_role_shapes(&value) {
            return Err(<serde_json::Error as serde::de::Error>::custom(msg));
        }
        normalize_model_defaults(&mut value);
        normalize_fleet_role_params(&mut value);
        serde_json::from_value(value)
    }

    /// Boot composition: materialize every model's effective instance pool
    /// so the rest of the crate resolves qualifiers and sampling through one
    /// code path ([`ModelEntry::effective_pool`]). Call once after config
    /// load (preferably loaded via [`Self::from_json_str`], which performs
    /// the sparse-inheritance legs at parse).
    ///
    /// The single documented composition order, every leg through
    /// [`overlay_params`] (shallow: each key wins wholesale, never
    /// deep-merged):
    /// `models.default` → model entry (parse time) → role run block →
    /// pool profile → per-model selection (the last two at
    /// [`materialize_effective_pool`]; the role side wins over the entry at
    /// dispatch via [`ModelEntry::instance_params_for`]).
    ///
    /// Only the server-launch knobs are read elsewhere — the supervisor
    /// consumes them from the fleet run block (`build_server_args` over
    /// [`Self::default_role_params`], which itself inherits
    /// `models.default` launch keys at parse).
    pub fn apply_defaults(&mut self) {
        // `params` is valid on pool profiles, role-side model bindings, and
        // model entries alike — every layer composes through `overlay_params`.
        // Non-object values would vanish silently in the merge, so boot
        // warns loudly over each one (fail-open: the merge itself is
        // unchanged). Bindings naming undeclared models warn the same way.
        warn_on_non_object_params(self);
        warn_on_unknown_model_bindings(&self.models, &self.roles);
        warn_on_unknown_role_overrides(&self.models, &self.roles);
        warn_on_unknown_param_keys(self);
        // Merge top-level ONNX role keys into the `onnx` fleet when `onnx` is
        // absent. This supports the simplified config format where roles like
        // `encoder`, `pii`, `router`, `colbert` are declared at the root level
        // instead of nested under `onnx`.
        self.normalize_onnx();
        warn_on_missing_endpoint(self);
        for (key, entry) in &mut self.models {
            entry.effective_profiles = Some(materialize_effective_pool(key, &self.roles));
        }
    }

    /// The effective top-level safety default: the configured
    /// `safety_threshold`, else the hard default. Per-classifier-node
    /// thresholds win over this wherever set (node-over-top-over-hard) —
    /// this is only the pipeline default the nodes fall back to.
    pub fn effective_safety_threshold(&self) -> f64 {
        self.safety_threshold
            .unwrap_or_else(super::classification::default_safety_threshold)
    }

    /// Boot presence gate: every model serving traffic must be usable.
    /// Deterministic: models visit in sorted key order, so the first
    /// reported failure is stable. Fail-closed for referenced models, warn
    /// for unreferenced ones:
    /// - `weights` set: the path (absolute, or joined under `gguf_dir`)
    ///   must exist. Referenced and missing stops boot; unreferenced and
    ///   missing warns and continues.
    /// - `hf_repo` set (no `weights`): remote — reaching it needs the
    ///   network, so presence is not validated here.
    /// - otherwise external: `endpoint` must be non-empty (same
    ///   referenced-fail / unreferenced-warn split).
    ///
    /// The `default` key is always the sparse-inheritance template, never
    /// a servable model — skipped. Referenced means: classifier duty keys,
    /// group literal bases (role members fanned out), or role-bound
    /// models.
    pub fn validate_model_presence(&self, gguf_dir: &std::path::Path) -> Result<(), String> {
        let mut referenced = std::collections::BTreeSet::new();
        if let Some(tree) = self.classification.as_ref() {
            referenced.extend(tree.classifier_model_keys());
        }
        for cfg in self.model_groups.values() {
            for member in cfg.effective_models() {
                if member == "last" || member == "any" {
                    continue;
                }
                let (base, _) = split_model_key(&member);
                if self.roles.contains_key(base) {
                    referenced.extend(models_serving_role(&self.roles, &self.models, base));
                } else {
                    referenced.insert(base.to_string());
                }
            }
        }
        for role in self.roles.values() {
            referenced.extend(role.models.keys().cloned());
        }
        let mut keys: Vec<&String> = self.models.keys().collect();
        keys.sort();
        for key in keys {
            if key == "default" {
                continue;
            }
            let entry = &self.models[key];
            let is_referenced = referenced.contains(key);
            if let Some(weights) = entry.weights.as_deref() {
                let raw = std::path::Path::new(weights);
                let full = if raw.is_absolute() {
                    raw.to_path_buf()
                } else {
                    gguf_dir.join(raw)
                };
                if !full.exists() {
                    if is_referenced {
                        return Err(format!(
                            "model '{key}' weights not found at '{}'",
                            full.display()
                        ));
                    }
                    tracing::warn!(
                        target: "router.config",
                        model = %key,
                        path = %full.display(),
                        "unreferenced model weights missing; boot continues",
                    );
                }
                continue;
            }
            if entry.hf_repo.is_some() {
                continue;
            }
            if entry.endpoint.is_empty() {
                if is_referenced {
                    return Err(format!(
                        "model '{key}' has no weights and no endpoint"
                    ));
                }
                tracing::warn!(
                    target: "router.config",
                    model = %key,
                    "unreferenced model has no weights or endpoint; boot continues",
                );
            }
            // Chat-template file: warn-only even when referenced — a missing
            // template degrades to the GGUF-baked template (the supervisor
            // omits `--chat-template-file`), never a boot fatal.
            if let Some(template) = entry.template.as_deref() {
                let raw = std::path::Path::new(template);
                let full = if raw.is_absolute() {
                    raw.to_path_buf()
                } else {
                    gguf_dir.join(raw)
                };
                if !full.exists() {
                    tracing::warn!(
                        target: "router.config",
                        model = %key,
                        path = %full.display(),
                        "chat-template file missing; spawning without --chat-template-file",
                    );
                }
            }
        }
        Ok(())
    }

    /// The fleet run block: `roles.default.params`, or struct defaults when
    /// no `default` role is declared. The single source for spawn defaults
    /// (supervisor). Parse-time inheritance fills its absent launch knobs
    /// from `models.default.params`, so the models template is the fleet
    /// base even when `roles.default` is an empty `{}`.
    pub fn default_role_params(&self) -> RoleParams {
        self.roles
            .get("default")
            .map(|r| r.params.clone())
            .unwrap_or_default()
    }

    /// Per-model spawn defaults for the supervisor: the fleet run block
    /// overlaid with the model's own launch knobs from its
    /// (template-inherited) `params`. A pool-less model therefore spawns
    /// with its declared `num_ctx` instead of the fleet's, while instance
    /// pools keep owning residency for bound models. Unknown models fall
    /// back to the fleet block.
    pub fn spawn_defaults_for(&self, model_key: &str) -> RoleParams {
        let base = self.default_role_params();
        let over = self.models.get(model_key).and_then(|e| e.params.as_ref());
        base.with_model_launch_overrides(over)
    }

    /// The classifier's serving key: the head candidate of the `classifier`
    /// role (sorted model-key order over the role's bound models), else
    /// `None`. The single classifier-key source.
    pub fn classifier_role_key(&self) -> Option<String> {
        models_serving_role(&self.roles, &self.models, "classifier")
            .into_iter()
            .next()
    }

    /// The ledger duty's serving key: the `ledger` section's `group`
    /// (resolved through [`resolve_group_head_key`]), else its literal
    /// `model`, else the classifier key. Group wins when set; the literal is
    /// the legacy fallback.
    pub fn ledger_head_key(&self) -> Option<String> {
        if let Some(ledger) = &self.ledger {
            if let Some(group) = ledger.group.as_deref() {
                if let Some(key) =
                    resolve_group_head_key(&self.models, &self.roles, &self.model_groups, group)
                {
                    return Some(key);
                }
            }
            if let Some(model) = ledger.model.as_deref() {
                return Some(model.to_string());
            }
        }
        self.classifier_role_key()
    }

    /// The chart-selector duty's serving key: the `charts` section's
    /// `selector_group` (resolved through [`resolve_group_head_key`]), else
    /// its literal `selector_model`. Group wins when set; the literal is the
    /// legacy fallback.
    pub fn chart_selector_key(&self) -> Option<String> {
        if let Some(group) = self.charts.selector_group.as_deref() {
            if let Some(key) =
                resolve_group_head_key(&self.models, &self.roles, &self.model_groups, group)
            {
                return Some(key);
            }
        }
        self.charts.selector_model.clone()
    }

    /// Merge top-level ONNX role keys into the `onnx` fleet. When `onnx` is
    /// `None` but top-level `encoder`/`pii`/`router`/`colbert` fields are
    /// present, they are collected into an `OnnxFleetConfig`. This supports the
    /// simplified config format where roles are declared at the root level
    /// instead of nested under `onnx`.
    fn normalize_onnx(&mut self) {
        if self.onnx.is_some() {
            // Already has a nested `onnx` section — top-level role keys are
            // redundant and ignored (nested wins).
            if self.encoder.is_some() || self.pii.is_some() || self.router.is_some() || self.colbert.is_some() {
                tracing::warn!(
                    target: "router.config",
                    "both `onnx` section and top-level role keys present; \
                     `onnx` section takes precedence, top-level roles ignored",
                );
            }
            return;
        }
        // Check for any top-level role key.
        let has_roles = self.encoder.is_some()
            || self.pii.is_some()
            || self.router.is_some()
            || self.colbert.is_some();
        if !has_roles {
            return;
        }
        let mut fleet = fluent_llm::onnx_config::OnnxFleetConfig {
            encoder: self.encoder.take(),
            pii: self.pii.take(),
            router: self.router.take(),
            colbert: self.colbert.take(),
            policy: None,
            llm: None,
        };
        // Roles like `router` and `pii` often share the encoder's tokenizer.
        // When a role is missing its tokenizer_path, inherit it from the
        // encoder role. This keeps the simplified config format concise.
        if let Some(ref tok) = fleet.encoder.as_ref().and_then(|e| e.model.tokenizer_path.clone()) {
            let tok = tok.clone();
            for role_cfg in [&mut fleet.pii, &mut fleet.router, &mut fleet.colbert, &mut fleet.policy] {
                if let Some(cfg) = role_cfg.as_mut() {
                    if cfg.model.tokenizer_path.is_none() {
                        cfg.model.tokenizer_path = Some(tok.clone());
                    }
                }
            }
        }
        tracing::info!(
            target: "router.config",
            roles = ?fleet.iter().map(|(r, _)| r.registry_key()).collect::<Vec<_>>(),
            "top-level ONNX role keys merged into fleet config",
        );
        self.onnx = Some(fleet);
    }

    /// Startup schema verification: the loaded config conforms to the derived
    /// [`crate::config::config_schema`] document it will be served under.
    /// Fail-closed (`Err` stops boot at the composition root) over three
    /// structural legs — everything open-vocabulary underneath stays
    /// warn-only (see [`warn_on_unknown_param_keys`]):
    ///
    /// 1. the `classification` tree is present (the schema's load-bearing
    ///    section; flat-only configs are rejected);
    /// 2. the `default` pipeline exists and `default_route` is non-empty
    ///    (every fallback path assumes both);
    /// 3. the config round-trips: serialize → [`Self::from_json_value`] —
    ///    so the sparse-inheritance legs are idempotent and no loaded state
    ///    is unrepresentable in the schema's vocabulary.
    pub fn verify_schema_conformance(&self) -> Result<(), String> {
        if self.classification.is_none() {
            return Err("schema: missing required section `classification`".into());
        }
        if !self.pipelines.contains_key("default") {
            return Err("schema: missing required pipeline `default`".into());
        }
        if self.default_route.is_empty() {
            return Err("schema: `default_route` must not be empty".into());
        }
        let value = serde_json::to_value(self)
            .map_err(|e| format!("schema: config does not serialize: {e}"))?;
        Self::from_json_value(value)
            .map_err(|e| format!("schema: config round-trip failed: {e}"))?;
        Ok(())
    }
    pub fn validate_flat_tree_coherence(&self) -> Result<(), String> {
        // M3c: configs without a `classification` tree are rejected fail-fast —
        // flat-only JSON no longer loads. (Name retained for call-site
        // stability; the flat arms are gone with the flat fields. New code
        // prefers `verify_schema_conformance`, which covers this leg.)
        if self.classification.is_none() {
            return Err("flat config removed, set classification.tree".into());
        }
        Ok(())
    }

    /// The `routes` view the server consumes (model - pipeline mapping).
    ///
    /// Derived solely from the classification tree's `terminal` nodes: each
    /// terminal synthesizes a `RouteRef` routed through its own `group` (or
    /// the route name when no group is given) carrying the terminal's
    /// `always_route` and `role`, so `RoutingConfig::resolve_route` and
    /// `resolve_pipeline` work with no structural change to the server.
    pub fn routes_view(&self) -> HashMap<String, RouteRef> {
        let mut routes = HashMap::new();
        if let Some(tree) = &self.classification {
            for (route, group, description) in tree.terminal_views() {
                routes.insert(
                    route.clone(),
                    RouteRef {
                        group: group.unwrap_or_else(|| route.clone()),
                        role: tree.terminal_role(&route),
                        pipelines: vec!["default".into()],
                        description,
                        always_route: tree.terminal_always_route(&route),
                    },
                );
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
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    #[serde(default)]
    pub name: Option<String>,
    /// OpenAI-compatible chat-completions endpoint. Optional for managed
    /// (weights/hf_repo/instances) models — Coral Router rewrites it to the
    /// spawned `llama-server`'s address at boot. Required for external OpenAI
    /// endpoints (a model with no `weights`/`hf_repo`/`instances`).
    #[serde(default)]
    pub endpoint: String,
    /// Name of an environment variable holding the `Authorization: Bearer`
    /// token for an external OpenAI endpoint. When set and the variable
    /// resolves, dispatch sends it as the Bearer token; a managed model (one
    /// Coral Router spawns) ignores this. `None` sends no auth header.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Capability rating (0–10): the target-matching ladder routes prompts
    /// assessed above a member's `intelligence` to the next member.
    #[serde(default = "default_intelligence")]
    pub intelligence: u8,
    #[serde(default)]
    pub cost_input: f64,
    #[serde(default)]
    pub cost_output: f64,
    #[serde(default)]
    pub cost_cached_read: f64,
    /// Observed mean throughput (tokens/s) for this model on this host.
    /// Sparse-inherited from `models.default`; metadata for capacity
    /// planning (the ladder orders by cost, never by speed).
    #[serde(default = "default_tok_s")]
    pub tok_s: f64,
    #[serde(default = "default_total_timeout_ms")]
    pub total_timeout_ms: u64,
    #[serde(default = "default_idle_timeout_ms")]
    pub idle_timeout_ms: u64,
    #[serde(default = "default_true")]
    pub stream: bool,
    #[serde(default)]
    pub filter_thinking: bool,
    /// Explicit thinking level for this model (`off` | `on` | a
    /// model-specific level such as `low`/`high`/`max`). Additive with
    /// [`Self::filter_thinking`]: absent behaves exactly as today; present
    /// resolves through [`Self::resolve_thinking`] (model → role → override)
    /// and an explicit level wins over `filter_thinking: true` with a loud
    /// warn (`filter_thinking: true` alone still means `off`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default)]
    pub retry_count: u32,
    #[serde(default = "default_retry_interval")]
    pub retry_base_interval_s: u64,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    /// Embedding-model reference for this model: a `models` key, a role
    /// name, or a weights path. Sparse-inherited from `models.default` and
    /// overridable per instance profile and per role-side model binding
    /// ([`ModelEntry::embedding_for`]); the chart embedder resolves it as a
    /// duty key and falls through fail-open when it names no servable model.
    #[serde(default)]
    pub embedding: Option<String>,
    /// Boot-materialized effective pool: every profile the qualifier and
    /// params paths resolve against, with the role-base ← pool ← selection
    /// sampling chain already composed into each profile's `params`.
    /// `None` before boot composition runs. Not serialized (derived).
    #[serde(skip)]
    pub effective_profiles: Option<Vec<InstanceProfile>>,
    /// Local GGUF weights file path. When set (or when `hf_repo` is set),
    /// Coral Router is the process owner: it spawns and supervises a
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
    /// Local chat-template file passed to the spawned `llama-server` as
    /// `--chat-template-file`. Takes precedence over the template baked into
    /// the GGUF metadata (and over the `template.txt` companion convention
    /// the `preset` CLI surface auto-detects — explicit wins). Sparse-inherited
    /// from `models.default` like every other top-level model key. A path that
    /// does not exist warns loudly at boot and the flag is omitted (fail-open:
    /// the server still serves with its baked-in template).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Sparse per-role sampling override for this model, keyed by role name
    /// (serde name `roles`). Params-only: it carries a sampling update for
    /// the named role and never confers membership — a model listed here but
    /// in no role's `models` map is still served only through the `default`
    /// role's pool. `None` (the default) contributes nothing.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "roles")]
    pub role_params: Option<HashMap<String, ModelRoleOverride>>,
}

impl ModelEntry {
    /// Whether Coral Router manages this model's lifecycle itself: a dedicated
    /// `llama-server` process per weights file or HuggingFace repo. Roles
    /// never confer management — a role-side binding for an endpoint-only
    /// model routes to its declared `endpoint`; only weights-backed models
    /// are spawned. In-process ONNX models are NOT `ModelEntry`s — they are
    /// declared in the top-level `onnx` role section and managed by the ort
    /// registry (see `RouterConfig::onnx`).
    pub fn is_managed(&self) -> bool {
        self.weights.is_some() || self.hf_repo.is_some()
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

/// One role-side model binding: the value type of `RoleEntry.models`, keyed
/// by model key. Roles and models stay separate tables; the binding is where
/// they compose — a role names the models that serve it, never the reverse.
///
/// `select` names a profile in the role's own `instances` pool, and that
/// profile IS the model's pool contribution for the role (a binding narrows;
/// a bound model with no `select` uses the pool as authored). Absent selects
/// the pool's `default: true` profile (else the single profile, else
/// nothing). `params` is a sparse sampling update composed over the bound
/// profile's params at boot; absent contributes nothing. A wholly-absent
/// binding (every field `None`) is "pool as authored" — no narrowing.
///
/// Unknown fields are rejected: a legacy pool profile deserializes here only
/// by accident, and silently dropping its keys would misroute — fail loud.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ModelBinding {
    /// Pool profile name selected for the model; see above for the absent rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<String>,
    /// Sparse sampling update over the selected profile's params.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    /// Sparse embedding-model override for the selected profile, composed
    /// into it at boot (this binding wins over the profile's own).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<String>,
}

/// One model-side per-role override: the value type of
/// `ModelEntry.role_params` (serde name `roles`), keyed by role name.
/// Params-only by construction — the struct carries a single sparse sampling
/// update and has no membership fields, so a model can tune its behavior for
/// a task without joining (or redefining) the role's serving set.
///
/// Unknown fields are rejected: `models`/`select` belong on the role-side
/// binding (`roles.<role>.models.<model>`), and silently dropping them here
/// would look like a per-task tune while actually being ignored — fail loud
/// with a pointer at the role side instead.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ModelRoleOverride {
    /// Sparse sampling update for the named role, composed last in the
    /// params chain (absent contributes nothing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// One config-declared instance profile. The map key on
/// `roles.<role>.instances` provides the default instance name; `count > 1`
/// expands into sibling instances named `<key>-0` .. `<key>-{count-1}`
/// sharing the profile's group. Sampling `params` are merged into the request
/// body for dispatches through these instances; declaration-only keys
/// (`num_ctx`/`parallel`/ `sleep_idle_seconds`) are stripped before dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Sparse embedding-model override for this profile: wins over the
    /// owning model's entry (which itself inherits `models.default`).
    #[serde(default)]
    pub embedding: Option<String>,
    /// Per-profile context-size cap (tokens). `None` = inherit (the role's
    /// `max_ctx`, else no cap). A cap below this profile's
    /// `num_ctx` clamps the context window at materialization
    /// (`ModelEntry::instance_profiles`).
    #[serde(default)]
    pub max_ctx: Option<u64>,
    /// Multi-step instance: holds snapshottable state across requests (the
    /// resume/snapshot path applies). Absent (`false`, the default) means the
    /// profile is one-shot — never snapshotted, and snapshot-scoped request
    /// fields do not apply to its contexts. Declared intent, not a
    /// measurement: the author states the instance carries multi-step state.
    #[serde(default)]
    pub session: bool,
}

fn default_instance_count() -> u32 {
    1
}

impl ModelEntry {
    /// The boot-materialized effective pool: `effective_profiles` when boot
    /// composition ran, else empty (pre-boot entries resolve bare). The
    /// single pool every qualifier and params path reads — never a fork.
    pub fn effective_pool(&self) -> &[InstanceProfile] {
        self.effective_profiles.as_deref().unwrap_or_default()
    }
}

/// Expand a raw profile map into the flat sibling list: `count` expansion
/// (naming siblings `<key>-0` .. `<key>-{count-1}`), name/group default
/// resolution, the `max_ctx` clamp, and the one-shot `resume` rule. Shared by
/// the boot materializer below — the expansion lives here once.
fn expand_instance_map(merged: &HashMap<String, InstanceProfile>) -> Vec<InstanceProfile> {
    // Exactly one profile may carry `default: true`: a second flag is a
    // declaration collision, warned loudly and resolved first-wins in
    // deterministic map order (the same order the expansion below and
    // every `find(|p| p.default)` consumer observe).
    if merged.values().filter(|p| p.default).count() > 1 {
        let mut keys: Vec<&str> = merged
            .iter()
            .filter(|(_, p)| p.default)
            .map(|(k, _)| k.as_str())
            .collect();
        keys.sort_unstable();
        tracing::warn!(
            target: "router.config",
            keys = ?keys,
            "multiple `default: true` instance profiles merged; \
             the first in map order wins",
        );
    }
    let mut keys: Vec<&String> = merged.keys().collect();
    keys.sort();
    let mut out = Vec::new();
    for key in keys {
        let profile = &merged[key];
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
            // A profile whose `max_ctx` cap sits below its `num_ctx` is
            // clamped at materialization; absent cap (the default) is a
            // no-op — byte-identical to today's profiles.
            if let Some(cap) = p.max_ctx {
                if cap < p.num_ctx {
                    p.num_ctx = cap;
                }
            }
            // One-shot profiles (the default) never carry multi-step
            // state: a `resume: true` on them is inapplicable, forced
            // false fail-open with a loud warn. Session profiles keep
            // their declared `resume` value unchanged.
            if !p.session && p.resume {
                tracing::warn!(
                    target: "router.config",
                    profile = %key,
                    "resume:true on a one-shot instance profile is inapplicable \
                     (multi-step state needs session:true); forcing resume:false",
                );
                p.resume = false;
            }
            p.name = Some(name);
            p.group = Some(group.clone());
            out.push(p);
        }
    }
    out
}

/// Compose one model's effective instance pool at boot: the single
/// role → per-model-instance params chain, deserialized and composed here so
/// every qualifier, sampling, backend, and supervision path reads composed
/// values through `ModelEntry::effective_pool`.
///
/// Contributing roles (deterministic sorted order): every role whose
/// `models` binding names this model. A model listed in no role's `models`
/// map inherits the `default` role's pool (the fleet-inherit fallback). A
/// `select` naming an unknown pool profile warns loudly and resolves to the
/// pool as authored.
///
/// Per contributing role, the sampling chain composes role-base ← pool
/// profile ← per-model binding into each profile's `params` (each sparser
/// layer wins; the role side is the final layer over the model entry
/// top-level at dispatch — see [`ModelEntry::instance_params_for`]).
/// A binding's `embedding` override composes into the chosen profile the
/// same way (binding wins over the profile's own). Profiles collide
/// first-role-wins with a loud warn; a non-empty binding narrows the
/// role's contribution to the resolved target (the selected instance IS the
/// model's pool for that role) and designates it the dispatch point
/// (`default: true`, displacing a pool default with a loud warn).
#[allow(clippy::implicit_hasher)]
pub(crate) fn materialize_effective_pool(
    model_key: &str,
    roles: &HashMap<String, RoleEntry>,
) -> Vec<InstanceProfile> {
    let mut contributing: Vec<&str> = roles
        .iter()
        .filter(|(_, role)| role.models.contains_key(model_key))
        .map(|(name, _)| name.as_str())
        .collect();
    contributing.sort_unstable();
    if contributing.is_empty() && roles.contains_key("default") {
        contributing.push("default");
    }
    let mut merged: HashMap<String, InstanceProfile> = HashMap::new();
    for role_name in &contributing {
        let role = &roles[*role_name];
        for (profile_name, profile) in &role.instances {
            if merged.contains_key(profile_name) {
                tracing::warn!(
                    target: "router.config",
                    model = %model_key,
                    profile = %profile_name,
                    role = %role_name,
                    "instance profile collides across contributing roles; \
                     first role wins",
                );
                continue;
            }
            let mut composed = profile.clone();
            composed.params = Some(overlay_params(
                role.params.sampling_value().as_ref(),
                composed.params.as_ref(),
            ));
            merged.insert(profile_name.clone(), composed);
        }
    }
    for role_name in &contributing {
        // Fleet-inherited models carry no binding on the role: pool as
        // authored, nothing to narrow or designate.
        let Some(binding) = roles[*role_name].models.get(model_key) else {
            continue;
        };
        // A wholly-absent binding ("pool as authored") contributes
        // nothing beyond the merge above.
        if binding.select.is_none() && binding.params.is_none() && binding.embedding.is_none() {
            continue;
        }
        let target = match &binding.select {
            Some(name) => {
                if merged.contains_key(name) {
                    Some(name.clone())
                } else {
                    tracing::warn!(
                        target: "router.config",
                        model = %model_key,
                        role = %role_name,
                        select = %name,
                        "binding selects an unknown pool profile; \
                         pool used as authored",
                    );
                    None
                }
            }
            None => {
                // Pool as authored: the pool default, else the single
                // profile, else nothing to designate.
                merged
                    .iter()
                    .find(|(_, p)| p.default)
                    .map(|(k, _)| k.clone())
                    .or_else(|| {
                        if merged.len() == 1 {
                            merged.keys().next().cloned()
                        } else {
                            None
                        }
                    })
            }
        };
        let Some(target) = target else { continue };
        // The binding narrows the role's contribution to the resolved
        // target: the selected instance IS the model's pool for the role.
        // Only profiles this role contributed are removed — another
        // role's same-named profile (first-wins above) stays.
        let role_names: Vec<String> = roles[*role_name]
            .instances
            .keys()
            .cloned()
            .collect();
        merged.retain(|name, _| *name == target || !role_names.contains(name));
        // The binding designates the role's dispatch point, displacing
        // a pool default with a loud warn.
        for (name, profile) in &mut merged {
            if *name != target && profile.default {
                tracing::warn!(
                    target: "router.config",
                    model = %model_key,
                    role = %role_name,
                    profile = %name,
                    "explicit binding displaces pool default profile",
                );
                profile.default = false;
            }
        }
        if let Some(chosen) = merged.get_mut(&target) {
            chosen.default = true;
            if binding.params.is_some() {
                chosen.params = Some(overlay_params(
                    chosen.params.as_ref(),
                    binding.params.as_ref(),
                ));
            }
            if let Some(embedding) = binding.embedding.clone() {
                chosen.embedding = Some(embedding);
            }
        }
    }
    expand_instance_map(&merged)
}

/// The open `params` vocabulary: every key a `params` map (model entry,
/// pool profile, role-side binding, model-side override) or a flattened role
/// run block may carry with a defined meaning. Anything else is forwarded to
/// the request body verbatim — so a typo'd key (`reasoning-effort`,
/// `temprature`) would sail silently to the server. The boot audit below
/// reports unknown keys loudly (fail-open: forwarding is unchanged).
pub const KNOWN_PARAM_KEYS: [&str; 48] = [
    // Launch knobs (spawn + residency; stripped at the body boundary).
    "num_ctx",
    "ctx_size",
    "max_ctx",
    "batch_size",
    "ubatch_size",
    "cache_type_k",
    "cache_type_v",
    "flash_attn",
    "n_gpu_layers",
    "n_cpu_moe",
    "sleep_idle_seconds",
    "parallel",
    "stream",
    "filter_thinking",
    "rope_freq_base",
    // Sampling knobs (forwarded into dispatch bodies).
    "temperature",
    "top_p",
    "top_k",
    "min_p",
    "typical_p",
    "repeat_penalty",
    "presence_penalty",
    "frequency_penalty",
    "mirostat",
    "mirostat_tau",
    "mirostat_eta",
    "seed",
    "max_tokens",
    "n_predict",
    "stop",
    "cache_prompt",
    "slot_id",
    // Thinking vocabulary (resolved by `ModelEntry::resolve_thinking` /
    // `filter_thinking_for`; `preserve_thinking` is router-local and stripped
    // at the body boundary, the rest forward to the server).
    "thinking",
    "enable_thinking",
    "reasoning_effort",
    "reasoning_budget_tokens",
    "preserve_thinking",
    "chat_template_kwargs",
    // Structured server knobs (nested objects, replaced wholesale per layer).
    "samplers",
    "logit_bias",
    "response_format",
    "tools",
    "tool_choice",
    "grammar",
    "json_schema",
    "chat_template",
    "mmproj",
    "model_alias",
];

/// Warn loudly over `params` keys outside [`KNOWN_PARAM_KEYS`]: the params
/// maps are open-vocabulary (forwarded verbatim), so an unknown key is
/// almost always a typo that would otherwise reach the server silently.
/// Fail-open — forwarding is unchanged, boot continues. Runs in
/// [`RouterConfig::apply_defaults`] beside the other boot audits, over every
/// params-bearing position (model entries, model-side overrides, role run
/// blocks, pool profiles, role-side bindings). Deterministic: positions
/// visit in sorted order.
pub fn warn_on_unknown_param_keys(cfg: &RouterConfig) {
    fn unknown_in(value: Option<&serde_json::Value>) -> Vec<String> {
        let mut out: Vec<String> = value
            .and_then(|v| v.as_object())
            .map(|obj| {
                obj.keys()
                    .filter(|k| !KNOWN_PARAM_KEYS.contains(&k.as_str()))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        out.sort_unstable();
        out
    }
    fn report(path: &str, keys: &[String]) {
        if !keys.is_empty() {
            tracing::warn!(
                target: "router.config",
                path = %path,
                keys = ?keys,
                "unknown params keys have no defined meaning and forward verbatim — likely typos, see KNOWN_PARAM_KEYS",
            );
        }
    }
    let mut model_keys: Vec<&String> = cfg.models.keys().collect();
    model_keys.sort();
    for model_key in model_keys {
        let entry = &cfg.models[model_key];
        report(
            &format!("models.{model_key}.params"),
            &unknown_in(entry.params.as_ref()),
        );
        if let Some(overrides) = entry.role_params.as_ref() {
            let mut roles: Vec<&String> = overrides.keys().collect();
            roles.sort();
            for role in roles {
                report(
                    &format!("models.{model_key}.roles.{role}.params"),
                    &unknown_in(overrides[role].params.as_ref()),
                );
            }
        }
    }
    let mut role_names: Vec<&String> = cfg.roles.keys().collect();
    role_names.sort();
    for role_name in role_names {
        let role = &cfg.roles[role_name];
        let sampling = role.params.sampling_value();
        report(
            &format!("roles.{role_name}.params"),
            &unknown_in(sampling.as_ref()),
        );
        let mut profiles: Vec<&String> = role.instances.keys().collect();
        profiles.sort();
        for profile in profiles {
            report(
                &format!("roles.{role_name}.instances.{profile}.params"),
                &unknown_in(role.instances[profile].params.as_ref()),
            );
        }
        let mut bound: Vec<&String> = role.models.keys().collect();
        bound.sort();
        for model in bound {
            report(
                &format!("roles.{role_name}.models.{model}.params"),
                &unknown_in(role.models[model].params.as_ref()),
            );
        }
    }
}

/// Warn loudly over model-side per-role overrides naming roles with no
/// `roles` entry: an override for an undeclared role can never compose into
/// a dispatch, so it is always a config bug (a renamed role, a typo).
/// Fail-open — the override is kept, boot continues. The role-side
/// counterpart is [`warn_on_unknown_model_bindings`]; the two run together
/// in [`RouterConfig::apply_defaults`].
#[allow(clippy::implicit_hasher)]
pub fn warn_on_unknown_role_overrides(
    models: &HashMap<String, ModelEntry>,
    roles: &HashMap<String, RoleEntry>,
) {
    let mut model_names: Vec<&String> = models.keys().collect();
    model_names.sort();
    for model_name in model_names {
        let Some(overrides) = models[model_name].role_params.as_ref() else {
            continue;
        };
        let mut unknown: Vec<&String> = overrides
            .keys()
            .filter(|r| !roles.contains_key(*r))
            .collect();
        unknown.sort();
        if !unknown.is_empty() {
            tracing::warn!(
                target: "router.config",
                model = %model_name,
                roles = ?unknown,
                "model overrides unknown roles; overrides kept but never compose",
            );
        }
    }
}

/// Warn loudly over role-side model bindings naming models with no `models`
/// entry: a binding for an undeclared model can never contribute to a pool,
/// so it is always a config bug (a renamed model key, a typo). Fail-open —
/// the binding is ignored, boot continues.
#[allow(clippy::implicit_hasher)]
pub fn warn_on_unknown_model_bindings(
    models: &HashMap<String, ModelEntry>,
    roles: &HashMap<String, RoleEntry>,
) {
    let mut role_names: Vec<&String> = roles.keys().collect();
    role_names.sort();
    for role_name in role_names {
        let mut unknown: Vec<&String> = roles[role_name]
            .models
            .keys()
            .filter(|m| !models.contains_key(*m))
            .collect();
        unknown.sort();
        if !unknown.is_empty() {
            tracing::warn!(
                target: "router.config",
                role = %role_name,
                models = ?unknown,
                "role binds unknown models; bindings ignored",
            );
        }
    }
}

/// The entry-default step of the inference-point precedence: the `default:
/// true` profile's group, else the single shared group across all profiles,
/// else `None` (bare `<base>`). `None` also when no pool was materialized.
/// Runs over the boot-materialized effective pool, so unbound entries inherit
/// the `default` role's pool through the same code path. Shared by
/// the single precedence function below and `RoutingTarget` construction
/// (whose entries arrive materialized) so backend model ids and dispatch wire
/// ids agree; not a second path — the rule lives here once.
pub(crate) fn default_inference_point(entry: &ModelEntry) -> Option<String> {
    let profiles = entry.effective_pool();
    if profiles.is_empty() {
        return None;
    }
    if let Some(d) = profiles.iter().find(|p| p.default) {
        return d.group.clone();
    }
    let first = profiles[0].group.clone()?;
    if profiles
        .iter()
        .all(|p| p.group.as_deref() == Some(first.as_str()))
    {
        Some(first)
    } else {
        None
    }
}

/// Resolve the inference point a role or model key serves, as the qualifier
/// of the dispatch `base:qualifier` id (`None` = bare `<base>`). The single
/// qualifier resolver — every construction path and adapter composes it, so
/// the precedence is documented once, here:
///
/// 1. Explicit qualifier — embedded (`base:point`) or parametric — wins,
///    except `latest`, which normalizes away and falls through.
/// 2. The entry default over the boot-materialized effective pool (the
///    `default: true` profile's group, else the single shared group —
///    [`default_inference_point`]).
/// 3. Bare key (`None`): no pool materialized, or no rule matched.
///
/// A role carries no qualifier of its own: it resolves from the route, which
/// selects a model from a group, and the selected model's entry default
/// supplies the point (the caller resolves *what* serves via
/// [`role_head_key`], then this function resolves *where* on it).
/// Unknown roles and keys fail closed (`None`). Entries are expected
/// boot-materialized (pools composed); pre-boot entries resolve through
/// step 3.
#[allow(clippy::implicit_hasher)]
pub fn resolve_inference_point(
    models: &HashMap<String, ModelEntry>,
    roles: &HashMap<String, RoleEntry>,
    role_or_key: &str,
    qualifier: Option<&str>,
) -> Option<String> {
    let (base, embedded) = split_model_key(role_or_key);
    if let Some(point) = embedded {
        if point != "latest" {
            return Some(point.to_string());
        }
    }
    if let Some(point) = qualifier {
        if point != "latest" {
            return Some(point.to_string());
        }
    }
    let _ = roles;
    models.get(base).and_then(default_inference_point)
}

/// Model keys serving a role, in sorted model-key order: every model named
/// in the role's `models` binding map that also declares a `models` entry.
/// Membership lives on the role side — models never name roles. Empty when
/// the role is unknown, binds nothing, or names only undeclared models (the
/// last case warns loudly at boot via [`warn_on_unknown_model_bindings`]).
/// The single membership source for every "who serves this role" question
/// (`role_head_key`, `classifier_role_key`, `role_expanded_members`).
/// Pools (`materialize_effective_pool`) deliberately resolve bindings
/// through the same map rather than this helper: params composition stays
/// on declared pools only.
#[allow(clippy::implicit_hasher)]
pub fn models_serving_role(
    roles: &HashMap<String, RoleEntry>,
    models: &HashMap<String, ModelEntry>,
    role: &str,
) -> Vec<String> {
    let mut out: Vec<String> = roles
        .get(role)
        .map(|entry| {
            entry
                .models
                .keys()
                .filter(|key| models.contains_key(key.as_str()))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    out.sort_unstable();
    out
}

/// Resolve a model group to its serving model key: the first non-sentinel
/// member, with bare role names fanned out to their head candidate.
/// Qualified members (`base:point`) and unknown literals pass through
/// unchanged. `None` for unknown groups and groups with no servable member.
/// The single group-duty resolver: classifier, ledger, and chart-selector
/// duties name groups, never models, and all three compose through this
/// function.
#[allow(clippy::implicit_hasher)]
pub fn resolve_group_head_key(
    models: &HashMap<String, ModelEntry>,
    roles: &HashMap<String, RoleEntry>,
    groups: &HashMap<String, ModelGroup>,
    group: &str,
) -> Option<String> {
    groups
        .get(group)?
        .effective_models()
        .iter()
        .filter(|m| *m != "last" && *m != "any")
        .find_map(|m| role_head_key(models, roles, m, false))
}

/// Resolve a role or model key to its serving model key: a bare role name
/// fans out to its head candidate (sorted model-key order over the models
/// binding to it — see [`models_serving_role`]), everything else passes
/// through unchanged. `None` for unknown roles, roles with no bound models,
/// and (when `require_entry` is set) keys with no `models` entry. A
/// qualified key (`base:point`, other than `base:latest`) always passes
/// through: it already names an instance of a concrete model, so a role
/// sharing the base name must never swallow its qualifier. The companion
/// to [`resolve_inference_point`]: the key identifies *what* serves, the
/// point identifies *where* on it.
#[allow(clippy::implicit_hasher)]
pub fn role_head_key(
    models: &HashMap<String, ModelEntry>,
    roles: &HashMap<String, RoleEntry>,
    role_or_key: &str,
    require_entry: bool,
) -> Option<String> {
    let (base, qualifier) = split_model_key(role_or_key);
    if let Some(point) = qualifier {
        if point != "latest" {
            if require_entry {
                models.get(base)?;
            }
            return Some(role_or_key.to_string());
        }
    }
    if roles.contains_key(base) {
        let mut serving = models_serving_role(roles, models, base);
        if serving.is_empty() {
            return None;
        }
        let head = serving.remove(0);
        if require_entry && !models.contains_key(&head) {
            return None;
        }
        return Some(head);
    }
    if require_entry {
        models.get(base)?;
    }
    Some(role_or_key.to_string())
}

/// Declaration-only request-body keys the fork ignores: the instance grammar
/// owns them (`ctx`/`parallel`/`sleep`), so they must not leak into the body.
pub const DECLARATION_PARAM_KEYS: [&str; 4] =
    ["num_ctx", "parallel", "sleep_idle_seconds", "rope_freq_base"];

/// Router-local params key selecting the response-side keep behavior for
/// thinking blocks (see [`ModelEntry::preserve_thinking`]). Never a server
/// knob — the body-boundary stripper below removes it, so it never reaches
/// the wire (llama-server knows no such field).
pub const PRESERVE_THINKING_KEY: &str = "preserve_thinking";

/// Remove declaration-only keys from a params object, keeping sampling params
/// (`temperature`, `repeat_penalty`, `chat_template_kwargs`, ...). Non-object
/// params are returned unchanged. Router-local control keys (currently only
/// [`PRESERVE_THINKING_KEY` — consumed by the router, unknown to every
/// server) are removed here too: this is the single body boundary every
/// params path funnels through, including the bare-entry fallbacks.
pub fn strip_declaration_params(params: serde_json::Value) -> serde_json::Value {
    let Some(obj) = params.as_object() else {
        return params;
    };
    let mut out = obj.clone();
    for k in DECLARATION_PARAM_KEYS {
        out.remove(k);
    }
    out.remove(PRESERVE_THINKING_KEY);
    serde_json::Value::Object(out)
}

/// Flat residency keys that belong inside a role's typed `context` block,
/// never at the role top level. A role entry carrying one is always the
/// legacy flat shape — rejected with a migration pointer, never hoisted
/// silently (a silent hoist would let a `num_ctx` typo inside `params`
/// diverge from the top-level value with no error).
const LEGACY_ROLE_CONTEXT_KEYS: [&str; 14] = [
    "num_ctx",
    "ctx_size",
    "max_ctx",
    "pinned",
    "count",
    "parallel",
    "no_sleep",
    "warm",
    "sleep_idle_seconds",
    "resume",
    "session",
    "default",
    "group",
    "name",
];

/// Membership keys that belong on the role-side binding
/// (`roles.<role>.models.<model>`), never on a model-side per-role override
/// (`models.<model>.roles.<role>`). A model-side override carrying one is
/// always a membership declaration in the wrong table — rejected with a
/// migration pointer instead of being dropped by `deny_unknown_fields`
/// with no guidance.
const LEGACY_MODEL_ROLE_MEMBERSHIP_KEYS: [&str; 3] = ["models", "select", "instance"];

/// Reject legacy role shapes on the raw JSON, before typing, with migration
/// pointers at the new locations. Runs first in
/// [`RouterConfig::from_json_value`], so the error names the intended shape
/// instead of surfacing as a bare `deny_unknown_fields` unknown-field error:
///
/// - `roles.<role>` carrying flat residency keys → `roles.<role>.context`;
/// - `roles.<role>.max_parallel` → the per-role limiter table
///   (`roles.<role>.concurrency.max_parallel`);
/// - `models.<model>.roles.<role>` carrying membership keys →
///   `roles.<role>.models`.
///
/// Deterministic: roles and models are visited in sorted key order, so the
/// first reported shape is stable across runs.
fn reject_legacy_role_shapes(value: &serde_json::Value) -> Result<(), String> {
    let mut role_names: Vec<&str> = value
        .get("roles")
        .and_then(|roles| roles.as_object())
        .map(|roles| roles.keys().map(String::as_str).collect())
        .unwrap_or_default();
    role_names.sort_unstable();
    for role in role_names {
        let entry = &value["roles"][role];
        let Some(obj) = entry.as_object() else {
            continue;
        };
        if obj.contains_key("max_parallel") {
            return Err(format!(
                "roles.{role}.max_parallel is not a role field; declare admission caps as roles.{role}.concurrency.max_parallel"
            ));
        }
        let mut flat: Vec<&str> = LEGACY_ROLE_CONTEXT_KEYS
            .iter()
            .filter(|k| obj.contains_key(**k))
            .copied()
            .collect();
        flat.sort_unstable();
        if !flat.is_empty() {
            return Err(format!(
                "roles.{role} carries flat key(s) [{}]; move residency keys into roles.{role}.context (num_ctx/max_ctx/pinned/count)",
                flat.join(", ")
            ));
        }
    }
    let mut model_names: Vec<&str> = value
        .get("models")
        .and_then(|models| models.as_object())
        .map(|models| models.keys().map(String::as_str).collect())
        .unwrap_or_default();
    model_names.sort_unstable();
    for model in model_names {
        let mut override_roles: Vec<&str> = value["models"][model]["roles"]
            .as_object()
            .map(|overrides| overrides.keys().map(String::as_str).collect())
            .unwrap_or_default();
        override_roles.sort_unstable();
        for role in override_roles {
            let entry = &value["models"][model]["roles"][role];
            let Some(obj) = entry.as_object() else {
                continue;
            };
            let mut membership: Vec<&str> = LEGACY_MODEL_ROLE_MEMBERSHIP_KEYS
                .iter()
                .filter(|k| obj.contains_key(**k))
                .copied()
                .collect();
            membership.sort_unstable();
            if !membership.is_empty() {
                return Err(format!(
                    "models.{model}.roles.{role} carries membership key(s) [{}]; membership lives on the role side in roles.{role}.models (models.{model}.roles.{role} is a params-only override)",
                    membership.join(", ")
                ));
            }
        }
    }
    Ok(())
}

/// Sparse-inherit the `models.default` template over every sibling model
/// entry on the raw JSON, before typing. Models declare only deltas: an
/// absent top-level key inherits the template value, a declared key wins;
/// when both template and sibling declare a `params` object, the template's
/// missing sampling keys fill the sibling's (shallowly — nested objects are
/// never recursed into). Runs before [`normalize_fleet_role_params`] so the
/// fleet run block below observes the template-fed `roles.default.params`.
/// No `models.default` object leaves the document untouched.
fn normalize_model_defaults(value: &mut serde_json::Value) {
    let Some(template) = value
        .get("models")
        .and_then(|models| models.get(RouterConfig::DEFAULT_MODEL_KEY))
        .and_then(|default| default.as_object())
        .cloned()
    else {
        return;
    };
    let Some(models) = value
        .as_object_mut()
        .and_then(|root| root.get_mut("models"))
        .and_then(|models| models.as_object_mut())
    else {
        return;
    };
    for (name, entry) in models.iter_mut() {
        if name == RouterConfig::DEFAULT_MODEL_KEY {
            continue;
        }
        let Some(entry_obj) = entry.as_object_mut() else {
            continue;
        };
        for (key, template_value) in &template {
            match entry_obj.get(key) {
                None => {
                    entry_obj.insert(key.clone(), template_value.clone());
                }
                Some(entry_value) if key == "params" => {
                    // Both sides declare sampling maps: the template fills
                    // only the sibling's missing keys, shallowly.
                    if let (Some(base), Some(over)) =
                        (template_value.as_object(), entry_value.as_object())
                    {
                        let mut merged = base.clone();
                        for (k, v) in over {
                            merged.insert(k.clone(), v.clone());
                        }
                        entry_obj.insert(key.clone(), serde_json::Value::Object(merged));
                    }
                }
                Some(_) => {}
            }
        }
    }
}

/// Hoist a legacy nested `params`-inside-`params` sampling object one level
/// up, in place. The nested form (`roles.<role>.params.params: {...}`) is no
/// longer a composition layer: its keys move into the enclosing run block
/// (enclosing keys win on collision) and the nested object is dropped. A
/// non-object `params` value is left for the boot-time
/// [`warn_on_non_object_params`] pass to report.
fn hoist_nested_role_params(params: &mut serde_json::Value) {
    let Some(obj) = params.as_object_mut() else {
        return;
    };
    if let Some(nested) = obj.remove("params") {
        if let Some(nested_obj) = nested.as_object() {
            for (key, val) in nested_obj {
                obj.entry(key.clone()).or_insert(val.clone());
            }
        } else {
            obj.insert("params".to_string(), nested);
        }
    }
}

/// Sparse-merge the fleet run block (`roles.default.params`) over every
/// other role's `params` on the raw JSON, before typing — then fill the
/// fleet block's own absent launch knobs from `models.default.params`, so
/// the models template is the fleet base even when `roles.default` is `{}`.
/// Roles declare only deltas: absent keys inherit the fleet value, declared
/// keys win, shallowly (nested objects are replaced wholesale, never
/// deep-merged). A role without a `params` block inherits the fleet block
/// whole. No `default` role, or no `params` on it, leaves sibling roles
/// untouched (the models-template leg above still applies).
fn normalize_fleet_role_params(value: &mut serde_json::Value) {
    // The models template feeds the fleet block's absent launch knobs first.
    if let Some(template_params) = value
        .get("models")
        .and_then(|models| models.get(RouterConfig::DEFAULT_MODEL_KEY))
        .and_then(|default| default.get("params"))
        .and_then(|params| params.as_object())
        .cloned()
    {
        if let Some(default_role) = value
            .as_object_mut()
            .and_then(|root| root.get_mut("roles"))
            .and_then(|roles| roles.as_object_mut())
            .and_then(|roles| roles.get_mut("default"))
            .and_then(|default| default.as_object_mut())
        {
            match default_role.get_mut("params") {
                Some(params) => {
                    // Declared run block: the template fills only absent keys.
                    if let Some(fleet_params) = params.as_object_mut() {
                        for (key, val) in &template_params {
                            fleet_params.entry(key.clone()).or_insert(val.clone());
                        }
                    }
                }
                // No run block at all (`"default": {}`): the template's run
                // block IS the fleet block.
                None => {
                    default_role.insert(
                        "params".to_string(),
                        serde_json::Value::Object(template_params),
                    );
                }
            }
        }
    }
    let Some(roles) = value
        .as_object_mut()
        .and_then(|root| root.get_mut("roles"))
        .and_then(|roles| roles.as_object_mut())
    else {
        return;
    };
    // Legacy nesting hoists before any merge, per role: a role's own nested
    // keys land in its own top level first (its own top-level keys win ties),
    // so the fleet merge below cannot bury a declared nested key under a
    // fleet value.
    for role in roles.values_mut() {
        if let Some(params) = role.get_mut("params") {
            hoist_nested_role_params(params);
        }
    }
    let Some(fleet) = roles
        .get("default")
        .and_then(|default| default.get("params"))
        .cloned()
    else {
        return;
    };
    for (name, role) in roles.iter_mut() {
        if name == "default" {
            continue;
        }
        match role.get("params") {
            None => {
                if let Some(obj) = role.as_object_mut() {
                    obj.insert("params".to_string(), fleet.clone());
                }
            }
            Some(role_params) => {
                let merged = overlay_params(Some(&fleet), Some(role_params));
                role["params"] = merged;
            }
        }
    }
}

/// Warn loudly over models the supervisor cannot assign an endpoint: no
/// `endpoint` declared and no `weights`/`hf_repo` for the supervisor to
/// spawn from. Managed models (spawned `llama-server`s) have their endpoint
/// rewritten at boot, so an empty endpoint is only a bug on external models.
/// The reserved [`RouterConfig::DEFAULT_MODEL_KEY`] template is skipped: it
/// is never supervised, only inherited from.
fn warn_on_missing_endpoint(cfg: &RouterConfig) {
    let mut keys: Vec<&String> = cfg.models.keys().collect();
    keys.sort();
    for key in keys {
        if *key == RouterConfig::DEFAULT_MODEL_KEY {
            continue;
        }
        let entry = &cfg.models[key];
        if !entry.endpoint.is_empty() {
            continue;
        }
        if entry.weights.is_some() || entry.hf_repo.is_some() {
            continue;
        }
        tracing::warn!(
            target: "router.config",
            model = %key,
            "model has no endpoint and no weights/hf_repo — the supervisor \
             cannot assign one; external models must declare `endpoint`",
        );
    }
}

/// Warn loudly over every `params` map in the config that is not a JSON
/// object. `params` is valid on pool profiles, role-side model bindings, and
/// model entries — role run blocks carry no nested `params` object (their
/// sampling keys are flattened into `roles.<role>.params` itself) — and
/// every layer composes through [`overlay_params`], which silently drops
/// non-object sides. A string/number/array in any of these positions is
/// therefore always a config bug that would otherwise vanish without a
/// trace; the merge itself is unchanged (fail-open, non-objects contribute
/// nothing).
pub fn warn_on_non_object_params(cfg: &RouterConfig) {
    fn check(value: Option<&serde_json::Value>, path: &str) {
        if let Some(v) = value {
            if !v.is_object() {
                tracing::warn!(
                    target: "router.config",
                    path = %path,
                    "params must be a JSON object; non-object value ignored \
                     (role params are supplemented or overwritten per key, \
                     never replaced wholesale)",
                );
            }
        }
    }
    let mut role_names: Vec<&String> = cfg.roles.keys().collect();
    role_names.sort();
    for role_name in role_names {
        let role = &cfg.roles[role_name];
        let mut profile_names: Vec<&String> = role.instances.keys().collect();
        profile_names.sort();
        for profile_name in profile_names {
            check(
                role.instances[profile_name].params.as_ref(),
                &format!("roles.{role_name}.instances.{profile_name}.params"),
            );
        }
        let mut bound_models: Vec<&String> = role.models.keys().collect();
        bound_models.sort();
        for model_key in bound_models {
            check(
                role.models[model_key].params.as_ref(),
                &format!("roles.{role_name}.models.{model_key}.params"),
            );
        }
    }
    let mut model_keys: Vec<&String> = cfg.models.keys().collect();
    model_keys.sort();
    for model_key in model_keys {
        let entry = &cfg.models[model_key];
        check(entry.params.as_ref(), &format!("models.{model_key}.params"));
        if let Some(overrides) = entry.role_params.as_ref() {
            let mut override_roles: Vec<&String> = overrides.keys().collect();
            override_roles.sort();
            for role in override_roles {
                check(
                    overrides[role].params.as_ref(),
                    &format!("models.{model_key}.roles.{role}.params"),
                );
            }
        }
    }
}

/// Overlay one sampling `params` object over a base (the overlay wins),
/// returning the merged object. Objects merge per key at the top level
/// only — a nested object on both sides is replaced wholesale, never
/// deep-merged (there is no `params`-inside-`params` layer anywhere in the
/// config); non-object sides degrade to an empty object (nothing to merge —
/// the `warn_on_non_object_params` boot pass reports those loudly). This is
/// the single canonical params merge: every layer of the
/// `models.default` → model → role → pool profile → binding chain composes
/// through it. The matching profile is looked up by name-or-group so both
/// the exact-instance and group dispatch paths reach the same value.
pub fn overlay_params(
    base: Option<&serde_json::Value>,
    over: Option<&serde_json::Value>,
) -> serde_json::Value {
    use serde_json::Value;
    fn as_object_or_empty(v: &Value) -> Value {
        v.as_object()
            .map_or_else(empty_params_object, |m| Value::Object(m.clone()))
    }
    match (base, over) {
        (Some(b), Some(o)) => match (b.as_object(), o.as_object()) {
            (Some(base_map), Some(over_map)) => {
                let mut merged = base_map.clone();
                for (key, over_value) in over_map {
                    merged.insert(key.clone(), over_value.clone());
                }
                Value::Object(merged)
            }
            // A non-object side contributes nothing (reported loudly by
            // `warn_on_non_object_params`); the object side survives whole.
            (Some(base_map), None) => Value::Object(base_map.clone()),
            (None, Some(over_map)) => Value::Object(over_map.clone()),
            (None, None) => empty_params_object(),
        },
        (Some(b), None) => as_object_or_empty(b),
        (None, Some(o)) => as_object_or_empty(o),
        (None, None) => empty_params_object(),
    }
}

fn empty_params_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

impl ModelEntry {
    /// Resolve the sampling params to send when dispatching to `qualifier`
    /// (an instance name or group of this model's boot-materialized pool):
    /// the qualifier-only shorthand for [`Self::answer_params_for`] with no
    /// role, no role sampling, and no classification duty. `None` when no
    /// profile matches `qualifier` — callers fall back to the entry's bare
    /// params. The composition rule lives in `answer_params_for` once; this
    /// stays so qualifier-only callers do not restate the chain.
    pub fn instance_params_for(&self, qualifier: &str) -> Option<serde_json::Value> {
        self.answer_params_for(Some(qualifier), None, None, None)
    }

    /// The single five-leg answer-params chain, every leg through
    /// [`overlay_params`] (shallow: each key wins wholesale, never
    /// deep-merged) with the role applied exactly once:
    ///
    /// 1. classification-duty params (the classifier serving key's params;
    ///    `None` on paths without a classifier duty),
    /// 2. the group-selected model's entry (`models.default`
    ///    sparse-inheritance already applied at parse),
    /// 3. `model.params`,
    /// 4. `role.params` (the role run block's sampling, passed in as
    ///    `role_sampling`),
    /// 5. `models.<model>.roles.<role>.params` (the model-specific override
    ///    for the task; wins when present, absent contributes nothing).
    ///
    /// The instance profile (`qualifier`) resolves *where* on the model and
    /// reads through the boot-materialized [`Self::effective_pool`] — it is
    /// the qualifier step underneath the chain, not a sixth leg. The
    /// resolved thinking level (see [`Self::resolve_thinking`]) is written
    /// into the composed object as `thinking`; declaration-only keys are
    /// stripped at the body boundary. `None` when a named `qualifier`
    /// matches no profile — callers fall back to the entry's bare params
    /// (the `instance_params_for` contract, preserved).
    pub fn answer_params_for(
        &self,
        qualifier: Option<&str>,
        role: Option<&str>,
        role_sampling: Option<&serde_json::Value>,
        classification_params: Option<&serde_json::Value>,
    ) -> Option<serde_json::Value> {
        let profile = match qualifier {
            Some(q) => self.effective_pool().iter().find(|p| {
                p.name.as_deref() == Some(q) || p.group.as_deref() == Some(q)
            }),
            None => None,
        };
        if qualifier.is_some() && profile.is_none() {
            return None;
        }
        let override_params: Option<&serde_json::Value> = role
            .and_then(|r| self.role_params.as_ref()?.get(r))
            .and_then(|o| o.params.as_ref());
        let mut merged = overlay_params(classification_params, self.params.as_ref());
        merged = overlay_params(Some(&merged), role_sampling);
        if let Some(p) = profile {
            merged = overlay_params(Some(&merged), p.params.as_ref());
        }
        merged = overlay_params(Some(&merged), override_params);
        if let Some(level) = self.resolve_thinking(role_sampling, override_params) {
            if let Some(obj) = merged.as_object_mut() {
                obj.insert(
                    "thinking".to_string(),
                    serde_json::Value::String(level),
                );
            }
        }
        Some(strip_declaration_params(merged))
    }

    /// Resolve the thinking level for a dispatch through `role`: the
    /// model-side override wins, then the role sampling, then the entry's
    /// explicit [`Self::thinking`]. Within one params layer the precedence is
    /// `thinking` (explicit level string), then `enable_thinking` (bool →
    /// `on`/`off`, the llama.cpp chat-template switch), then
    /// `reasoning_effort` (a model-specific effort string such as
    /// `low`/`medium`/`high`/`max`, passed through verbatim). `None` means no
    /// level was declared anywhere (a `filter_thinking: true` entry with no
    /// level still filters via [`Self::filter_thinking_for`] — the bool
    /// alias). An explicit level wins over `filter_thinking: true` with a
    /// loud warn; `off` always filters. Contradictory layers
    /// (`enable_thinking: false` beside a `reasoning_effort`, `preserve`
    /// beside `off`) warn loudly with the winner named.
    pub fn resolve_thinking(
        &self,
        role_sampling: Option<&serde_json::Value>,
        override_params: Option<&serde_json::Value>,
    ) -> Option<String> {
        fn level(params: Option<&serde_json::Value>) -> Option<String> {
            let obj = params?.as_object()?;
            if let Some(t) = obj.get("thinking").and_then(|t| t.as_str()) {
                return Some(t.to_string());
            }
            if let Some(enabled) = obj.get("enable_thinking").and_then(serde_json::Value::as_bool) {
                if !enabled && obj.contains_key("reasoning_effort") {
                    tracing::warn!(
                        target: "router.config",
                        "enable_thinking:false beside reasoning_effort — effort ignored, level resolves off",
                    );
                }
                return Some(if enabled { "on".to_string() } else { "off".to_string() });
            }
            obj.get("reasoning_effort")
                .and_then(|e| e.as_str())
                .map(str::to_string)
        }
        let explicit = level(override_params)
            .or_else(|| level(role_sampling))
            .or_else(|| self.thinking.clone());
        if self.filter_thinking {
            if let Some(ref set) = explicit {
                if set != "off" {
                    tracing::warn!(
                        target: "router.config",
                        thinking = %set,
                        "explicit thinking level wins over filter_thinking:true",
                    );
                }
            }
        }
        explicit
    }

    /// Whether `preserve_thinking` (see [`PRESERVE_THINKING_KEY`]) keeps
    /// thinking blocks for a dispatch: the model-side override wins, then the
    /// role sampling. Absent everywhere contributes nothing (`None`).
    pub fn preserve_thinking(
        role_sampling: Option<&serde_json::Value>,
        override_params: Option<&serde_json::Value>,
    ) -> Option<bool> {
        fn keep(params: Option<&serde_json::Value>) -> Option<bool> {
            params
                ?.as_object()?
                .get(PRESERVE_THINKING_KEY)?
                .as_bool()
        }
        keep(override_params).or_else(|| keep(role_sampling))
    }

    /// Whether thinking blocks are stripped for a dispatch whose resolved
    /// level is `resolved_thinking`: an explicit `off` always filters;
    /// `preserve_thinking: true` (model-side override, then role sampling,
    /// then the entry's own params) keeps the blocks for any live level,
    /// beating the entry bool; otherwise the entry bool decides. A preserve
    /// beside `off` warns loudly — there is nothing generated to keep, so
    /// the filter wins. Instance-profile params carry pools, never thinking
    /// policy — a profile-level `preserve_thinking` is not consulted.
    pub fn filter_thinking_for(
        &self,
        resolved_thinking: Option<&str>,
        role_sampling: Option<&serde_json::Value>,
        override_params: Option<&serde_json::Value>,
    ) -> bool {
        let preserve = Self::preserve_thinking(role_sampling, override_params)
            .or_else(|| Self::preserve_thinking(None, self.params.as_ref()));
        if matches!(resolved_thinking, Some("off")) {
            if preserve == Some(true) {
                tracing::warn!(
                    target: "router.config",
                    "preserve_thinking:true beside thinking:off — nothing generated to keep, filter wins",
                );
            }
            return true;
        }
        if preserve == Some(true) {
            return false;
        }
        self.filter_thinking
    }

    /// Resolve the embedding-model reference serving `qualifier` (an
    /// instance name or group of this model's boot-materialized pool): the
    /// matching profile's override, else the entry's own (which inherits
    /// `models.default`). `None` selects no embedding duty from this model.
    /// `qualifier = None` resolves the entry default (the bare-model path).
    pub fn embedding_for(&self, qualifier: Option<&str>) -> Option<String> {
        if let Some(q) = qualifier {
            if let Some(profile) = self.effective_pool().iter().find(|p| {
                p.name.as_deref() == Some(q) || p.group.as_deref() == Some(q)
            }) {
                if let Some(embed) = profile.embedding.clone() {
                    return Some(embed);
                }
            }
        }
        self.embedding.clone()
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
    PathBuf::from("logs/audit")
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

/// The classifier's parsed LLM output. `FieldAccess` + the `#[field(...)]`
/// coercions make the struct the single source of truth for the boundary
/// decode path (`fluent_wvr::boundary::decode_boundary`): the `coerce`/`parse`
/// modes shape the raw model value strings exactly as the repair walker does,
/// so both decode paths share one vocabulary.
#[derive(Debug, Clone, Serialize, Deserialize, Default, FieldAccess, Describable)]
pub struct ClassifierOutput {
    #[field(desc = "classifier action", coerce = "strip_quotes,trim")]
    pub action: String,
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
    #[field(desc = "complexity", min = 0.0, max = 10.0, coerce = "strip_quotes,trim", parse = "number")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complexity: Option<u8>,
    #[field(desc = "intent", coerce = "strip_quotes,trim,normalize_literal")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
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
    /// Legacy spelling: prefer `selector_group` (duties name groups); an
    /// explicit `selector_model` still resolves when no group is set.
    #[serde(default)]
    pub selector_model: Option<String>,
    /// Model group serving the chart-selector duty, resolved through
    /// [`resolve_group_head_key`]. Wins over `selector_model` when set.
    #[serde(default)]
    pub selector_group: Option<String>,
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
            selector_group: None,
            max_candidates: default_charts_max_candidates(),
            min_score: default_charts_min_score(),
            entity_context: default_charts_entity_context(),
        }
    }
}

const fn default_charts_max_candidates() -> usize {
    5
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
    /// classifier model key, then to no summarizer. Legacy spelling: prefer
    /// `group` (duties name groups); an explicit `model` still resolves when
    /// no group is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Model group serving the ledger duty, resolved through
    /// [`resolve_group_head_key`]. Wins over `model` when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
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
            group: None,
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

/// Async review configuration (ROADMAP §12.7, C4). `None` (the default)
/// disables the review worker and its endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReviewConfig {
    /// Model key for the review model — an **independent, more capable**
    /// tier so review is not self-review (§12.7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_model: Option<String>,
    /// Bounded review-queue capacity (the `WorkerPool` queue cap).
    #[serde(default = "default_review_queue_capacity")]
    pub queue_capacity: usize,
    /// Credit granted up front to the review feed (backpressure, §12.6).
    #[serde(default = "default_review_credit_limit")]
    pub credit_limit: usize,
    /// Opt-in PII auto-enqueue (ROADMAP_20260827_ORT §3.4): after a parse is
    /// recorded, PII-shaped spans detected on the request text enqueue a
    /// review candidate, bounded by the existing credit gate. `false` (the
    /// default) keeps today's behavior — review jobs only come from the
    /// explicit `POST /v1/sessions/{id}/review-parse` endpoint.
    #[serde(default)]
    pub auto_enqueue: bool,
    /// Policy-Linter flag threshold (0..1): a text token whose score against a
    /// policy rule clears this is flagged. Default 0.5. Only meaningful when a
    /// `ZeroShotTokenMatching` linter model is registered.
    #[serde(default = "default_review_pii_threshold")]
    pub pii_threshold: f64,
}

fn default_review_queue_capacity() -> usize {
    32
}

fn default_review_credit_limit() -> usize {
    16
}

fn default_review_pii_threshold() -> f64 {
    0.5
}

/// Async overlay configuration (ROADMAP_20260827_ORT §6). `None` (the default)
/// disables the overlay worker and its request-path hook.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OverlayConfig {
    /// Opt-in entity-link overlay (M6.2): when `true`, the request path submits
    /// `EntityLink` residuals (unresolved PROPN spans) to a credit-gated
    /// worker that scores them against boot-cached concept-label embeddings and
    /// writes candidates to `overlay_candidates` (never a doc-id write).
    #[serde(default)]
    pub entity_link_enabled: bool,
    /// Minimum cosine similarity for an entity-link candidate to be accepted.
    #[serde(default = "default_entity_link_threshold")]
    pub entity_link_threshold: f64,
    /// Bounded overlay-queue capacity (the `WorkerPool` queue cap).
    #[serde(default = "default_overlay_queue_capacity")]
    pub queue_capacity: usize,
    /// Credit granted up front to the overlay feed (backpressure).
    #[serde(default = "default_overlay_credit_limit")]
    pub credit_limit: usize,
    /// The opt-in `arc_ready` annotation-overlay sub-config (OVERLAYS §8). `None`
    /// (the default) leaves the three arc_ready overlays off — byte-identical to
    /// a deployment with no `overlay.arc_ready` block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arc_ready: Option<ArcReadyConfig>,
}

/// The `arc_ready` annotation-overlay configuration (OVERLAYS §8). Opt-in and
/// additive: every field defaults to off/absent so a config with no
/// `overlay.arc_ready` block is byte-identical to a deployment that never
/// mentions it. The numeric knobs mirror the overlay worker's `OverlayWorkerConfig`
/// defaults (the `CreditedFeedWorker` load-bearing constants).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ArcReadyConfig {
    /// Master switch. `false` (the default) leaves the overlays off.
    #[serde(default)]
    pub enabled: bool,
    /// Whether the spacy pipeline overlay is requested. `false` (the default)
    /// means the `NlpPipeline` seam is not wired → the spacy overlay is
    /// fail-open `Ok(None)`.
    #[serde(default)]
    pub nlp: bool,
    /// Name of the `models` key whose `ChatBackend` drives the LLM enrichment
    /// overlay. `None` (the default) → the LLM overlay is off.
    #[serde(default)]
    pub llm_model: Option<String>,
    /// Name of the `models` key whose `EmbeddingProvider` drives the embedding
    /// overlay. `None` (the default) → the embedding overlay is off.
    #[serde(default)]
    pub embedding_model: Option<String>,
    /// Bounded pending-node feed capacity (the `CreditedFeedWorker` `mpsc` bound).
    #[serde(default = "default_arc_ready_queue_capacity")]
    pub queue_capacity: usize,
    /// Credit granted up front to the feed's producer (backpressure).
    #[serde(default = "default_arc_ready_credit_limit")]
    pub credit_limit: usize,
    /// How many processed nodes the consumer waits for before bumping credit.
    #[serde(default = "default_arc_ready_credit_more_after")]
    pub credit_more_after: usize,
    /// Max concurrent node-derivations (the `Limiter` cap).
    #[serde(default = "default_arc_ready_max_concurrent")]
    pub max_concurrent: usize,
    /// Whether to boot-backfill nodes already missing an overlay. `false` (the
    /// default) leaves boot behavior unchanged (nodes enqueue on create only).
    #[serde(default)]
    pub backfill: bool,
}

const fn default_arc_ready_queue_capacity() -> usize {
    1024
}

const fn default_arc_ready_credit_limit() -> usize {
    256
}

const fn default_arc_ready_credit_more_after() -> usize {
    8
}

const fn default_arc_ready_max_concurrent() -> usize {
    8
}

fn default_entity_link_threshold() -> f64 {
    0.6
}

fn default_overlay_queue_capacity() -> usize {
    32
}

fn default_overlay_credit_limit() -> usize {
    16
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
/// A role's run parameters — the `params` block on a `roles` entry.
///
/// Supplies the "how a model is run" configuration for everything serving
/// the role: the `llama-server` launch knobs (`--batch-size`,
/// `--ubatch-size`, `--cache-type-k/v`, `--flash-attn`, `--n-gpu-layers`,
/// `--n-cpu-moe`, `--sleep-idle-seconds`, `--ctx-size`) and flat sampling
/// keys (e.g. `temperature`) every selection for the role composes over
/// (role-base, then pool profile, then per-model selection — each sparser
/// layer wins; the role side wins over the model entry top-level at
/// dispatch). There is no nested `params` object inside a run block.
/// The fleet-wide block lives as `roles.default.params` (itself fed by
/// `models.default` at parse); the supervisor reads per-model spawn
/// defaults through [`RouterConfig::spawn_defaults_for`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleParams {
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
    /// Flat open sampling extras merged into dispatch bodies (e.g.
    /// `temperature`, `repeat_penalty`). Flattened into the run block itself —
    /// there is deliberately no nested `params` object inside a role's
    /// `params`: every sampling key lives at this level and composes
    /// per key, shallowly, like every other `params` map in the config.
    #[serde(flatten)]
    pub sampling: HashMap<String, serde_json::Value>,
    /// Global context-size cap (tokens) applied to every managed model that
    /// does not declare its own `max_ctx`. `None` = no cap (existing behavior —
    /// a model's `num_ctx`/`ctx_size` is the sole bound).
    #[serde(default)]
    pub max_ctx: Option<u64>,
}

impl Default for RoleParams {
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
            sampling: HashMap::new(),
            max_ctx: None,
        }
    }
}

impl RoleParams {
    /// The sampling extras as a params object (`None` when the role declares
    /// no sampling keys): the single conversion from the flattened run block
    /// to the `params`-map vocabulary every composition path speaks.
    pub fn sampling_value(&self) -> Option<serde_json::Value> {
        if self.sampling.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(
                self.sampling.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            ))
        }
    }

    /// Overlay a model's own launch knobs (its template-inherited `params`
    /// object) over this run block: the per-model spawn view. Only the
    /// closed launch-knob keys are honored (`num_ctx`/`ctx_size`,
    /// `batch_size`, `ubatch_size`, `cache_type_k`/`cache_type_v`,
    /// `flash_attn`, `n_gpu_layers`, `n_cpu_moe`, `sleep_idle_seconds`,
    /// `stream`, `filter_thinking`, `max_ctx`); sampling keys in the same
    /// object compose at dispatch, never at spawn. Unparseable values keep
    /// the base loudly (fail-open, never a boot fatal).
    #[must_use]
    pub fn with_model_launch_overrides(
        &self,
        params: Option<&serde_json::Value>,
    ) -> RoleParams {
        let mut out = self.clone();
        let Some(obj) = params.and_then(|p| p.as_object()) else {
            return out;
        };
        fn set<T>(obj: &serde_json::Map<String, serde_json::Value>, key: &str, slot: &mut T)
        where
            T: serde::de::DeserializeOwned,
        {
            if let Some(value) = obj.get(key) {
                match T::deserialize(value.clone()) {
                    Ok(parsed) => *slot = parsed,
                    Err(e) => tracing::warn!(
                        target: "router.config",
                        key = %key,
                        error = %e,
                        "model params launch knob unparseable — fleet value kept",
                    ),
                }
            }
        }
        set(obj, "num_ctx", &mut out.num_ctx);
        set(obj, "ctx_size", &mut out.num_ctx);
        set(obj, "batch_size", &mut out.batch_size);
        set(obj, "ubatch_size", &mut out.ubatch_size);
        set(obj, "cache_type_k", &mut out.cache_type_k);
        set(obj, "cache_type_v", &mut out.cache_type_v);
        set(obj, "flash_attn", &mut out.flash_attn);
        set(obj, "n_gpu_layers", &mut out.n_gpu_layers);
        set(obj, "n_cpu_moe", &mut out.n_cpu_moe);
        set(obj, "sleep_idle_seconds", &mut out.sleep_idle_seconds);
        set(obj, "stream", &mut out.stream);
        set(obj, "filter_thinking", &mut out.filter_thinking);
        set(obj, "max_ctx", &mut out.max_ctx);
        out
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

fn default_onnx_limiter_cap() -> usize {
    common_core::constants::DEFAULT_ONNX_LIMITER_CAP
}

fn default_onnx_threads() -> usize {
    common_core::constants::DEFAULT_ONNX_THREADS
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
    /// (`<slot_save_path>/<model_key>/<instance>/`). Feeds snapshot-path derivation.
    #[serde(default)]
    pub slot_save_path: Option<String>,
    /// Resume snapshots older than this many seconds of context idle are
    /// dropped and their contexts' `resume` flag cleared: the router's signal
    /// that an evicted workload is done and need not be restorable. `None`
    /// keeps resume snapshots until explicitly disabled. The flag also feeds
    /// the `-resume` snapshot naming the router uses on eviction.
    #[serde(default)]
    pub resume_ttl_s: Option<u64>,
    /// Non-resume KV snapshots (per-turn `<session>-<seq>-<hash>` files, rigor
    /// blue snapshots) older than this many seconds (fork `mtime`) are deleted
    /// from the fork by the residency pass. `None` disables the age sweep —
    /// snapshots then accumulate until the byte budget (or an operator)
    /// removes them. Resume snapshots (`<instance>-resume`) are never touched
    /// by this sweep; they follow `resume_ttl_s` above.
    #[serde(default)]
    pub snapshot_ttl_s: Option<u64>,
    /// Byte budget for non-resume KV snapshots per model: when their summed
    /// fork-reported sizes exceed this, the residency pass deletes oldest
    /// (by fork `mtime`) first until under budget. `None` disables the budget
    /// sweep. Combines with `snapshot_ttl_s` (the age sweep runs first).
    /// Resume snapshots are excluded from both the sum and the sweep.
    #[serde(default)]
    pub snapshot_budget_bytes: Option<u64>,
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
    /// The onnx fleet's working-set budget (bytes): when Σ resident bytes of
    /// loaded onnx sessions exceeds it, the onnx residency loop releases the
    /// LRU-largest `Unloadable` sessions. `None` (the default) → idle-only
    /// eviction — CPU RAM is cheap and the parity target is idle unload, not
    /// a tight budget.
    #[serde(default)]
    pub onnx_working_set_budget_bytes: Option<u64>,
    /// Persisted fleet map (`{model: {port, pid}}` for every running server)
    /// the supervisor writes at boot and reads back on the next boot so an
    /// orphaned `llama-server` (router killed without graceful shutdown) is
    /// adopted instead of duplicated. `None` (the default) disables
    /// persistence — adoption falls back to the `/proc` scan alone.
    #[serde(default)]
    pub server_state_path: Option<String>,
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
            snapshot_ttl_s: None,
            snapshot_budget_bytes: None,
            api_key_env: None,
            liveness_poll_interval_s: default_sidecar_liveness_poll_s(),
            liveness_failures_before_restart: default_sidecar_liveness_failures(),
            max_restarts: default_sidecar_max_restarts(),
            onnx_working_set_budget_bytes: None,
            server_state_path: None,
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
    fluent_llm::constants::DEFAULT_TOTAL_TIMEOUT_MS
}

/// Default capability rating for a sparse model entry that declares none
/// (and no `models.default` template supplies one): the smallest tier.
const fn default_intelligence() -> u8 {
    1
}

/// Default observed throughput for a sparse model entry that declares none
/// (and no `models.default` template supplies one).
fn default_tok_s() -> f64 {
    20.0
}

fn default_idle_timeout_ms() -> u64 {
    fluent_llm::constants::DEFAULT_IDLE_TIMEOUT_MS
}

fn default_retry_interval() -> u64 {
    fluent_llm::constants::DEFAULT_RETRY_INTERVAL_S
}
#[cfg(test)]
#[path = "../../tests/config_root.rs"]
mod tests;
