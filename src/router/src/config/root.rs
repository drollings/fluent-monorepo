//! Router configuration root types - thin facade ownership (M3).
//! This module owns RouterConfig and its directly-associated helpers.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use fluent_types::model_key::split_model_key;
use fluent_wvr::{Describable, FieldAccess};

use super::fleet::{default_onnx_limiter_cap, default_onnx_threads, RoleParams};
use super::models::ModelEntry;
use super::{
    default_classifier_failure_policy, materialize_effective_pool, ChartsConfig,
    ClassifierFailurePolicy, FrameResolutionConfig, LedgerConfig, OverlayConfig, PostProcessConfig,
    ReviewConfig, RigorConfig, ServerConfig, SessionConfig, SidecarConfig,
};
use crate::config::builder::PipelineParams;
use crate::config::classification::ClassificationTree;
use crate::config::filters::MockConfig;
use crate::config::normalize::{
    normalize_fleet_role_params, normalize_model_defaults, warn_on_non_object_params,
};
use crate::config::routing::{models_serving_role, role_head_key, RoleEntry, RouteRef};
use crate::logging::LoggingConfig;

#[derive(Debug, Clone, Serialize, Deserialize, FieldAccess, Describable)]
#[serde(deny_unknown_fields)]
pub struct RouterConfig {
    /// Named pipeline stage tables (keyed by pipeline name). This is stage
    /// configuration — not flat route authoring — so it stays past M3c: the
    /// tree carries routes/roles/descriptions, never stage knobs
    /// (deterministic prefilter, blacklist, nlp flags, thresholds).
    #[field(skip)]
    #[serde(default)]
    pub pipelines: HashMap<String, PipelineParams>,
    #[field(skip)]
    #[serde(default)]
    pub models: HashMap<String, ModelEntry>,
    /// Routing-vocabulary table: role name → candidate model keys + the
    /// inference point each candidate serves. A role's `models` list is the
    /// dispatch candidate set for every route naming the role (empty lists
    /// ride the `last` sentinel — see `RoleEntry::effective_models`); roles
    /// resolve to concrete `base:qualifier` targets per request at dispatch
    /// time, never at boot, so lazy models load correctly.
    #[field(skip)]
    #[serde(default)]
    pub roles: HashMap<String, RoleEntry>,
    #[field(desc = "safety threshold", min = 0.0, max = 1.0)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety_threshold: Option<f64>,
    #[field(desc = "default route")]
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
    /// a `ContentNodeStore` (with a real `Summarizer` backend targeting
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
    /// Async overlay composition section. `Some`
    /// opts the boot path into the entity-link overlay worker — the async
    /// plane that writes `EntityLink` candidates to `overlay_candidates` (never
    /// a doc-id write). `None` (the default) leaves today's behavior — no
    /// overlay worker at boot.
    #[field(skip)]
    #[serde(default)]
    pub overlay: Option<OverlayConfig>,
    /// Frame-resolution composition section. `Some` opts the boot path into
    /// the `FrameResolutionWorker` (wave-batched ambiguity resolution with
    /// validated-only sense promotion) behind its runtime gate. `None` (the
    /// default) leaves today's behavior — no frame worker at boot.
    #[field(skip)]
    #[serde(default)]
    pub frame_resolution: Option<FrameResolutionConfig>,
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
    ///
    /// It is also the prefix for every relative `models.<model>.weights` and
    /// `template` path: a relative declaration resolves under this directory
    /// (absolute declarations pass through unchanged) wherever the setup
    /// logic consumes the file — spawn argv, presence gate, weights identity,
    /// snapshot identity, and CLI resolution all share
    /// [`RouterConfig::join_config_dir`] (via [`RouterConfig::resolve_weights`]
    /// and [`RouterConfig::resolve_template`]).
    #[field(skip)]
    #[serde(default)]
    pub gguf_dir: Option<String>,
    /// ONNX model directory: the prefix for every relative ONNX file path
    /// (`model_path`, `tokenizer_path`, `label_source`, `policy_labels` in
    /// the `onnx` fleet and the top-level role sections merged into it).
    /// Absolute declarations pass through unchanged. Applied once in the
    /// ort registry build through [`RouterConfig::resolve_onnx_path`], so
    /// every session loader sees a fully resolved path.
    #[field(skip)]
    #[serde(default)]
    pub onnx_dir: Option<String>,
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
            frame_resolution: None,
            session: None,
            onnx_limiter_cap: common_core::constants::DEFAULT_ONNX_LIMITER_CAP,
            onnx_threads: common_core::constants::DEFAULT_ONNX_THREADS,
            gguf_dir: None,
            onnx_dir: None,
            onnx: None,
            colbert: None,
            encoder: None,
            pii: None,
            router: None,
            inference_registry: None,
        }
    }
}

/// Whether a serde_json error is an unknown-field rejection (as opposed to
/// a syntax error, type mismatch, or custom semantic failure). Matched on
/// the canonical message prefix — the only discriminator serde_json offers.
fn is_unknown_field_error(e: &serde_json::Error) -> bool {
    e.to_string().starts_with("unknown field")
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
    ///
    /// Location-rich errors: the normalized `from_value` leg below reports
    /// `0:0` (spans do not survive the `Value` round-trip), so a structural
    /// pre-pass parses the raw text first. Only an unknown-field failure is
    /// promoted from it — the normalizers solely *add* keys, so an unknown
    /// field in the raw text is still unknown after normalization, while any
    /// other pre-pass failure (e.g. a non-object `params` the merge rescues)
    /// is discarded in favor of the normalized verdict. Boot therefore names
    /// the retired field *and* its true line/column.
    pub fn from_json_str(s: &str) -> serde_json::Result<Self> {
        let structural: Result<Self, serde_json::Error> = serde_json::from_str(s);
        match Self::from_json_value(serde_json::from_str(s)?) {
            Ok(config) => Ok(config),
            Err(normalized_err) => match structural {
                Err(raw_err) if is_unknown_field_error(&raw_err) => Err(raw_err),
                _ => Err(normalized_err),
            },
        }
    }

    /// [`Self::from_json_str`] over an already-parsed value.
    pub fn from_json_value(mut value: serde_json::Value) -> serde_json::Result<Self> {
        normalize_model_defaults(&mut value);
        normalize_fleet_role_params(&mut value);
        let config: Self = serde_json::from_value(value)?;
        // A zero admission cap would admit nothing and wedge every dispatch
        // through the role — fail closed at parse.
        for (name, role) in &config.roles {
            if role.max_parallel == Some(0) {
                return Err(<serde_json::Error as serde::de::Error>::custom(format!(
                    "roles.{name}.max_parallel must be at least 1 (0 would admit nothing)"
                )));
            }
        }
        Ok(config)
    }

    /// Boot composition: materialize every model's effective instance pool
    /// so the rest of the crate resolves qualifiers and sampling through one
    /// code path ([`ModelEntry::effective_pool`]). Call once after config
    /// load (preferably loaded via [`Self::from_json_str`], which performs
    /// the sparse-inheritance legs at parse).
    ///
    /// The single documented composition order, every leg through
    /// [`crate::config::overlay_params`] (shallow: each key wins wholesale, never
    /// deep-merged):
    /// `models.default` → model entry (parse time) → role run block →
    /// pool profile → per-model selection (the last two at
    /// `materialize_effective_pool`; the role side wins over the entry at
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
        let entries: Vec<(String, ModelEntry)> = self
            .models
            .iter()
            .map(|(key, entry)| (key.clone(), entry.clone()))
            .collect();
        for (key, entry) in &entries {
            if let Some(slot) = self.models.get_mut(key) {
                slot.effective_profiles = Some(materialize_effective_pool(key, entry, &self.roles));
            }
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

    /// Join a config-declared file path under `dir` when relative; absolute
    /// paths pass through unchanged. The single home for the dir-prefix rule
    /// (`gguf_dir` for GGUF files, `onnx_dir` for ONNX files): every setup
    /// seam resolves through this rather than reimplementing the join.
    pub fn join_config_dir(dir: &std::path::Path, declared: &str) -> std::path::PathBuf {
        let raw = std::path::Path::new(declared);
        if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            dir.join(raw)
        }
    }

    /// The configured GGUF directory (`"."` when unset — the boot default
    /// the composition root passes to [`Self::validate_model_presence`]).
    pub fn gguf_dir_path(&self) -> std::path::PathBuf {
        self.gguf_dir
            .as_deref()
            .map_or_else(|| std::path::PathBuf::from("."), std::path::PathBuf::from)
    }

    /// The configured ONNX directory (`"."` when unset, mirroring
    /// [`Self::gguf_dir_path`]).
    pub fn onnx_dir_path(&self) -> std::path::PathBuf {
        self.onnx_dir
            .as_deref()
            .map_or_else(|| std::path::PathBuf::from("."), std::path::PathBuf::from)
    }

    /// Resolve a `models.<model>.weights` declaration under `gguf_dir`
    /// (`None` for `hf_repo`/endpoint-only entries). Absolute declarations
    /// pass through unchanged.
    pub fn resolve_weights(&self, entry: &ModelEntry) -> Option<std::path::PathBuf> {
        entry
            .weights
            .as_deref()
            .map(|w| Self::join_config_dir(&self.gguf_dir_path(), w))
    }

    /// Resolve a `models.<model>.template` declaration under `gguf_dir`
    /// (`None` when undeclared). Absolute declarations pass through
    /// unchanged.
    pub fn resolve_template(&self, entry: &ModelEntry) -> Option<std::path::PathBuf> {
        entry
            .template
            .as_deref()
            .map(|t| Self::join_config_dir(&self.gguf_dir_path(), t))
    }

    /// Resolve an ONNX file declaration (`model_path`, `tokenizer_path`,
    /// `label_source`, `policy_labels`) under `onnx_dir`. Absolute
    /// declarations pass through unchanged.
    pub fn resolve_onnx_path(&self, declared: &std::path::Path) -> std::path::PathBuf {
        let dir = self.onnx_dir_path();
        if declared.is_absolute() {
            declared.to_path_buf()
        } else {
            dir.join(declared)
        }
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
    /// role member bases (role-naming members fanned out), or role-bound
    /// models.
    pub fn validate_model_presence(&self, gguf_dir: &std::path::Path) -> Result<(), String> {
        let mut referenced = std::collections::BTreeSet::new();
        if let Some(tree) = self.classification.as_ref() {
            referenced.extend(tree.classifier_model_keys());
        }
        for role_entry in self.roles.values() {
            for member in role_entry.effective_models() {
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
        let mut keys: Vec<&String> = self.models.keys().collect();
        keys.sort();
        for key in keys {
            if key == "default" {
                continue;
            }
            let entry = &self.models[key];
            let is_referenced = referenced.contains(key);
            if let Some(weights) = entry.weights.as_deref() {
                let full = Self::join_config_dir(gguf_dir, weights);
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
                    return Err(format!("model '{key}' has no weights and no endpoint"));
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
                let full = Self::join_config_dir(gguf_dir, template);
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

    /// The ledger duty's serving key: the `ledger` section's `role`
    /// (resolved through [`role_head_key`]), else its literal `model`,
    /// else the classifier key. Role wins when set; the literal is the
    /// legacy fallback.
    pub fn ledger_head_key(&self) -> Option<String> {
        if let Some(ledger) = &self.ledger {
            if let Some(role) = ledger.role.as_deref() {
                if let Some(key) = role_head_key(&self.models, &self.roles, role, false) {
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
    /// `selector_role` (resolved through [`role_head_key`]), else its
    /// literal `selector_model`. Role wins when set; the literal is the
    /// legacy fallback.
    pub fn chart_selector_key(&self) -> Option<String> {
        if let Some(role) = self.charts.selector_role.as_deref() {
            if let Some(key) = role_head_key(&self.models, &self.roles, role, false) {
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
            if self.encoder.is_some()
                || self.pii.is_some()
                || self.router.is_some()
                || self.colbert.is_some()
            {
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
        if let Some(ref tok) = fleet
            .encoder
            .as_ref()
            .and_then(|e| e.model.tokenizer_path.clone())
        {
            let tok = tok.clone();
            for role_cfg in [
                &mut fleet.pii,
                &mut fleet.router,
                &mut fleet.colbert,
                &mut fleet.policy,
            ] {
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
    /// terminal synthesizes a `RouteRef` through its `role` carrying the
    /// terminal's `always_route`, so `RoutingConfig::resolve_route` and
    /// `resolve_pipeline` work with no structural change to the server.
    /// Terminals without a `role` fall back to the route name (resolved as
    /// a bare role at dispatch, which fails closed for unknown names).
    pub fn routes_view(&self) -> HashMap<String, RouteRef> {
        let mut routes = HashMap::new();
        if let Some(tree) = &self.classification {
            for (route, description) in tree.terminal_views() {
                let role = tree.terminal_role(&route).unwrap_or_else(|| route.clone());
                routes.insert(
                    route.clone(),
                    RouteRef {
                        role,
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

// Pool-materialization vocabulary lives in `super::fleet`; the open
// `params` vocabulary below is the boot-audit surface.

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
    }
}

/// Warn loudly over model-side per-role overrides naming roles with no
/// `roles` entry: an override for an undeclared role can never compose into
/// a dispatch, so it is always a config bug (a renamed role, a typo).
/// Fail-open — the override is kept, boot continues. The role-side
/// counterpart is [`warn_on_unknown_model_bindings`]; the two run together
/// in [`RouterConfig::apply_defaults`].
#[allow(clippy::implicit_hasher)]
/// Warn loudly over model-side per-role overrides naming roles with no
/// `roles` entry: an override for an undeclared role can never compose into
/// a dispatch, so it is always a config bug (a renamed role, a typo).
/// Fail-open — the override is kept, boot continues. The role-side
/// counterpart is [`warn_on_unknown_model_bindings`]; the two run together
/// in [`RouterConfig::apply_defaults`].
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
/// Warn loudly over role-side model bindings naming models with no `models`
/// entry: a binding for an undeclared model can never contribute to a pool,
/// so it is always a config bug (a renamed model key, a typo). Fail-open —
/// the binding is ignored, boot continues.
pub fn warn_on_unknown_model_bindings(
    models: &HashMap<String, ModelEntry>,
    roles: &HashMap<String, RoleEntry>,
) {
    let mut role_names: Vec<&String> = roles.keys().collect();
    role_names.sort();
    for role_name in role_names {
        let mut unknown: Vec<&String> = roles[role_name]
            .models
            .iter()
            .filter(|m| !models.contains_key(*m))
            .collect();
        unknown.sort();
        unknown.dedup();
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

// Key → serving-model and model → inference-point resolution lives in
// `super::routing`; the declaration vocabulary below is the body boundary.

// Declaration-only keys and the body-boundary stripper live in
// `super::normalize`.

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

// The canonical params merge lives in `super::overlay`; the
// residency-eviction vocabulary in `super::sidecar`.

// The audit-log section lives in `super::audit`; the classifier output
// vocabulary in `super::classification`.

/// The default route when a config omits `default_route`: `local`, matching
/// the shipped `env/coral-router.json` (no `fast` model exists in-tree).
fn default_route() -> String {
    "local".into()
}

// Workflow-library sections live in `super::charts` (`charts`,
// `post_process`), the rigor route section in `super::rigor`, and the
// ledger composition section in `super::ledger_group`.

// Worker sections live next to their domains: `super::review`,
// `super::overlay` (incl. `arc_ready`), `super::frame`,
// `super::orchestrator`, `super::session`, and `super::sidecar`.

// Chart-score defaults and the post-processing section live in
// `super::charts`.

#[cfg(test)]
#[path = "../../tests/config_root.rs"]
mod tests;