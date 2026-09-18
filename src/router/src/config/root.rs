//! Router configuration root types - thin facade ownership (M3).
//! This module owns RouterConfig and its directly-associated helpers.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use fluent_wvr::{Describable, FieldAccess};

use super::fleet::{default_onnx_limiter_cap, default_onnx_threads, RoleParams};
use super::models::{InstanceProfile, ModelEntry};
use crate::config::builder::PipelineParams;
use crate::config::classification::ClassificationTree;
use crate::config::filters::MockConfig;
use crate::config::normalize::{
    normalize_fleet_role_params, normalize_model_defaults, warn_on_non_object_params,
};
use crate::config::rounds::{BoundedRounds, EscalationConfidence, SeverityThreshold};
use crate::config::routing::{RoleEntry, RouteRef};
use crate::logging::LoggingConfig;

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
/// role → per-model-instance params chain, synthesized here so every
/// qualifier, sampling, backend, and supervision path reads composed values
/// through `ModelEntry::effective_pool`.
///
/// The config declares no instance pools; profiles are synthesized from the
/// model entry and the roles that bind it:
///
/// - one profile per role whose `models` list binds this model (sorted
///   role order), composing role run-block sampling and the model-side
///   `roles.<role>` override over the entry params, with `pinned`
///   composing as role OR model and `max_ctx` as override-else-entry;
/// - when no role binds the model, a single `base` profile (the entry's own
///   params, `pinned`, `max_ctx`, `max_parallel`), marked `default: true` —
///   the bare-model dispatch point. Role-bound models have no `base`
///   phantom: every dispatch and backend qualifier names a real role
///   window, never a synthetic default.
///
/// A pinned profile is what boots a model at startup
/// (`supervisor::LlamaServerSpec::from_entry`); everything else loads on
/// demand. `expand_instance_map` clamps each profile's `num_ctx` to its
/// `max_ctx` cap and resolves names/groups.
#[allow(clippy::implicit_hasher)]
pub(crate) fn materialize_effective_pool(
    model_key: &str,
    entry: &ModelEntry,
    roles: &HashMap<String, RoleEntry>,
) -> Vec<InstanceProfile> {
    let entry_num_ctx = || {
        entry
            .params
            .as_ref()
            .and_then(|p| p.get("num_ctx"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_else(|| RoleParams::default().num_ctx)
    };
    let mut merged: HashMap<String, InstanceProfile> = HashMap::new();
    let mut binding_roles: Vec<&str> = roles
        .iter()
        .filter(|(_, role)| role.serves(model_key))
        .map(|(name, _)| name.as_str())
        .collect();
    binding_roles.sort_unstable();
    // No bindings: the entry stands alone, so it keeps its synthetic `base`
    // dispatch point. Bound models serve only through their role windows —
    // no `base` phantom beside them (every qualifier names a real window).
    if binding_roles.is_empty() {
        merged.insert(
            "base".to_string(),
            InstanceProfile {
                name: None,
                group: None,
                count: 1,
                num_ctx: entry_num_ctx(),
                parallel: entry.max_parallel,
                pinned: entry.pinned,
                no_sleep: false,
                sleep_idle_seconds: None,
                default: true,
                resume: false,
                params: entry.params.clone(),
                embedding: None,
                max_ctx: entry.max_ctx,
                session: false,
            },
        );
    }
    for role_name in binding_roles {
        let role = &roles[role_name];
        let over = entry.role_params.as_ref().and_then(|o| o.get(role_name));
        // role run-block sampling, then the model-side override, over the
        // entry params — the same chain `answer_params_for` composes at
        // dispatch. The role run block owns the role profile's window: its
        // `num_ctx` wins over the entry params, and the `max_ctx` cap
        // (override, then entry) clamps it.
        let mut params =
            overlay_params(entry.params.as_ref(), role.params.sampling_value().as_ref());
        if let Some(op) = over.and_then(|o| o.params.as_ref()) {
            params = overlay_params(Some(&params), Some(op));
        }
        if let Some(obj) = params.as_object_mut() {
            obj.insert(
                "num_ctx".to_string(),
                serde_json::Value::from(role.params.num_ctx),
            );
        }
        let num_ctx = role.params.num_ctx;
        // Sibling windows: `max_parallel` is the admission cap AND the
        // window count (one window per parallel slot), so the cap the
        // dispatch gate enforces always has the slots it counts. Unset
        // stays a single window. `u32::try_from` saturates absurd values
        // instead of truncating (parse already rejects 0).
        let count = role
            .max_parallel
            .map_or(1, |m| u32::try_from(m).unwrap_or(u32::MAX).max(1));
        merged.insert(
            role_name.to_string(),
            InstanceProfile {
                name: None,
                group: None,
                count,
                num_ctx,
                parallel: entry.max_parallel,
                pinned: role.pinned || entry.pinned,
                no_sleep: false,
                sleep_idle_seconds: None,
                default: false,
                resume: false,
                params: Some(params),
                embedding: None,
                max_ctx: over.and_then(|o| o.max_ctx).or(entry.max_ctx),
                session: false,
            },
        );
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

/// The entry-default step of the inference-point precedence: the `default:
/// true` profile's group (unbound models keep their synthetic `base`
/// dispatch point), else the cheapest pinned group (resident by
/// construction — smallest window first, ties broken by group name, per the
/// cheap-before-expensive principle), else the single shared group across
/// all profiles, else `None` (bare `<base>`). `None` also when no pool was
/// materialized. Runs over the boot-materialized effective pool, so unbound
/// entries inherit the `default` role's pool through the same code path.
/// Shared by the single precedence function below and `RoutingTarget`
/// construction (whose entries arrive materialized) so backend model ids and
/// dispatch wire ids agree; not a second path — the rule lives here once.
pub(crate) fn default_inference_point(entry: &ModelEntry) -> Option<String> {
    let profiles = entry.effective_pool();
    if profiles.is_empty() {
        return None;
    }
    if let Some(d) = profiles.iter().find(|p| p.default) {
        return d.group.clone();
    }
    if let Some((group, _)) = profiles
        .iter()
        .filter(|p| p.pinned)
        .filter_map(|p| p.group.as_deref().map(|g| (g, p.num_ctx)))
        .min_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)))
    {
        return Some(group.to_string());
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
///    `default: true` profile's group, else the cheapest pinned group, else
///    the single shared group — [`default_inference_point`]).
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
/// (`role_head_key`, `classifier_role_key`, `role_dispatch_members`).
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
                .iter()
                .filter(|key| models.contains_key(key.as_str()))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    out.sort_unstable();
    out.dedup();
    out
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
pub const DECLARATION_PARAM_KEYS: [&str; 4] = [
    "num_ctx",
    "parallel",
    "sleep_idle_seconds",
    "rope_freq_base",
];

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
    #[field(
        desc = "coherence score",
        min = 0.0,
        max = 1.0,
        coerce = "strip_quotes,trim",
        parse = "number"
    )]
    pub coherence_score: f64,
    #[field(
        desc = "safety score",
        min = 0.0,
        max = 1.0,
        coerce = "strip_quotes,trim",
        parse = "number"
    )]
    pub safety_score: f64,
    #[field(
        desc = "complexity",
        min = 0.0,
        max = 10.0,
        coerce = "strip_quotes,trim",
        parse = "number"
    )]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complexity: Option<u8>,
    #[field(desc = "intent", coerce = "strip_quotes,trim,normalize_literal")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[field(desc = "routing reason", coerce = "strip_quotes,trim")]
    pub reason: String,
    #[field(
        desc = "completeness",
        min = 0.0,
        max = 1.0,
        coerce = "strip_quotes,trim",
        parse = "number"
    )]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completeness: Option<f64>,
    #[field(
        desc = "risk",
        min = 0.0,
        max = 1.0,
        coerce = "strip_quotes,trim",
        parse = "number"
    )]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<f64>,
}

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
#[serde(deny_unknown_fields)]
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
    /// Legacy spelling: prefer `selector_role` (duties name roles); an
    /// explicit `selector_model` still resolves when no role is set.
    #[serde(default)]
    pub selector_model: Option<String>,
    /// Role serving the chart-selector duty, resolved through
    /// [`role_head_key`]. Wins over `selector_model` when set.
    #[serde(default)]
    pub selector_role: Option<String>,
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
            selector_role: None,
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
    /// Role serving the ledger duty, resolved through [`role_head_key`].
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

/// Async review configuration. `None` (the default)
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
    /// Opt-in PII auto-enqueue: after a parse is
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

/// Async overlay configuration. `None` (the default)
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
    /// Opt-in parse-time entity promotion (M16): when `true`, accepted
    /// entity-link candidates stamp parse-time ids through the calibrated
    /// `EntityStampGate`. `false` (default) keeps the candidates-only plane —
    /// nothing is ever stamped.
    #[serde(default)]
    pub entity_promotion_enabled: bool,
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

/// Frame-resolution worker configuration. `None` (the default, i.e. no
/// `frame_resolution` block) leaves the worker unbuilt — byte-identical to a
/// deployment without it. `Some` composes the worker behind its runtime gate
/// (open at boot, closable via `POST /admin/frame/rollback`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameResolutionConfig {
    /// Model key for the resolution model (batched, grammar-shaped JSON).
    /// Falls back to the classifier key, then the onnx LLM key when absent —
    /// no key resolvable means the worker is skipped (fail-open).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Bounded frame-resolution queue capacity (the pool queue cap).
    #[serde(default = "default_frame_queue_capacity")]
    pub queue_capacity: usize,
    /// Credit granted up front to the frame feed (backpressure).
    #[serde(default = "default_frame_credit_limit")]
    pub credit_limit: usize,
}

fn default_frame_queue_capacity() -> usize {
    32
}

fn default_frame_credit_limit() -> usize {
    16
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
    /// This is the pre-admission hard gate: a cold load frees room against
    /// this before the spawn, sized with its incoming KV window.
    pub fn allocation_limit(&self) -> Option<u64> {
        let total = self.device_total_bytes()?;
        let min_remaining = self.minimum_remaining_vram.unwrap_or(0);
        Some(total.saturating_sub(min_remaining))
    }

    /// The reactive residency budget: `device_total - vram_low_watermark_bytes`.
    /// `None` when no device total is available. This is the periodic loop's
    /// floor only — it never gates pre-admission, so a load that fits the
    /// allocation budget stays quiet even past the watermark-implied usage.
    pub fn reactive_limit(&self) -> Option<u64> {
        let total = self.device_total_bytes()?;
        Some(total.saturating_sub(self.vram_low_watermark_bytes))
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

#[cfg(test)]
#[path = "../../tests/config_root.rs"]
mod tests;

#[cfg(test)]
#[path = "../../tests/config_normalize_parity.rs"]
mod config_normalize_parity_tests;
