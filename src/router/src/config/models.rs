//! Model-table entry types: `models` entries, role bindings, instance
//! profiles, and the per-model params/thinking composition.
//! Re-exported through `crate::config` so `crate::config::X` paths
//! keep resolving; the parse-time template/fleet legs live in
//! `super::normalize`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use common_core::constants::default_true;

use super::{overlay_params, strip_declaration_params};
use crate::config::routing::RoleEntry;

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
    /// Whether thinking blocks are stripped for dispatches through this
    /// model. Composes with [`Self::enable_thinking`] /
    /// [`Self::preserve_thinking`] (see [`Self::filter_thinking_for`]).
    #[serde(default)]
    pub filter_thinking: bool,
    /// Whether the model's chat template emits thinking blocks
    /// (`on`/`off` in the composed `thinking` level).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_thinking: Option<bool>,
    /// Keep generated thinking blocks in the response (router-local; never
    /// forwarded to the server).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserve_thinking: Option<bool>,
    /// Context-size cap (tokens) for this model: bounds `num_ctx` in every
    /// composed params object and the spawned window. `None` = no cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_ctx: Option<u64>,
    /// Boot the model's server process at startup instead of lazily on
    /// first dispatch. Role-level `pinned` composes over this (either
    /// true pins the synthesized profile).
    #[serde(default)]
    pub pinned: bool,
    /// Instance slots the synthesized pool profile admits; also the
    /// per-model parallelism hint for the capacity audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_parallel: Option<u32>,
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
    /// Sparse per-role override for this model, keyed by role name (serde
    /// name `roles`). Params-only: it carries a per-task update (thinking
    /// policy, `max_ctx` cap, sampling `params`) for the named role and
    /// never confers membership — a model is served through the roles that
    /// bind it in their `models` list. `None` (the default) contributes
    /// nothing.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "roles")]
    pub role_params: Option<HashMap<String, ModelRoleOverride>>,
    /// Snapshot save mode (`snapshot: {mode: manual|auto}`), beside
    /// `weights`. `None` inherits from the serving roles, else `Auto`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotConfig>,
}

impl Default for ModelEntry {
    fn default() -> Self {
        serde_json::from_value(serde_json::json!({})).expect("the all-defaults ModelEntry parses")
    }
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

/// One model-side per-role override: the value type of
/// `ModelEntry.role_params` (serde name `roles`), keyed by role name.
/// Params-only by construction — the struct carries per-task tuning (a
/// sparse sampling update, thinking policy, and a `max_ctx` cap) and no
/// membership fields, so a model can be tuned for a task without joining
/// (or redefining) the role's serving set.
///
/// Unknown fields are rejected: `models` belongs on the role side
/// (`roles.<role>.models`), and silently dropping it here would look like a
/// per-task tune while actually being ignored — fail loud with a pointer at
/// the role side instead.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ModelRoleOverride {
    /// Whether the named role's dispatches emit thinking blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_thinking: Option<bool>,
    /// Whether thinking blocks are stripped for the named role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter_thinking: Option<bool>,
    /// Keep generated thinking blocks for the named role (router-local).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserve_thinking: Option<bool>,
    /// Context-size cap for the named role; bounds `num_ctx` in the
    /// composed params object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_ctx: Option<u64>,
    /// Sparse sampling update for the named role, composed last in the
    /// params chain (absent contributes nothing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// Snapshot save mode for a model or role entry (`snapshot: {mode}`).
/// `Auto` (the default) preserves today's behavior: a resume-marked session
/// context is KV-snapshotted before eviction drops it. `Manual` destroys
/// without saving (and audits the skip) — the operator asked for no silent
/// saves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotMode {
    /// Save-before-destroy on evict (today's behavior).
    #[default]
    Auto,
    /// Destroy without saving on evict (audited).
    Manual,
}

impl SnapshotMode {
    /// Whether an evict may save the context's KV before destroying it.
    #[must_use]
    pub fn saves_on_evict(self) -> bool {
        matches!(self, Self::Auto)
    }
}

/// The `snapshot` block beside `weights` on model/role entries. Only `mode`
/// today; the block shape leaves room for retention knobs without a schema
/// break. `None` (absent) inherits — the model entry wins, else the first
/// serving role (sorted) that sets one, else `Auto`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotConfig {
    /// Save mode; default `Auto` (today's behavior).
    #[serde(default)]
    pub mode: SnapshotMode,
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

impl ModelEntry {
    /// Resolve the sampling params to send when dispatching to `qualifier`
    /// (an instance name or group of this model's boot-materialized pool):
    /// the qualifier-only shorthand for [`Self::answer_params_for`] with no
    /// role and no classification duty. `None` when no profile matches
    /// `qualifier` — callers fall back to the entry's bare params. The
    /// composition rule lives in `answer_params_for` once; this stays so
    /// qualifier-only callers do not restate the chain.
    pub fn instance_params_for(&self, qualifier: &str) -> Option<serde_json::Value> {
        self.answer_params_for(Some(qualifier), None, None)
    }

    /// The single answer-params chain, every leg through [`overlay_params`]
    /// (shallow: each key wins wholesale, never deep-merged), with the role
    /// applied exactly once:    ///
    /// 1. classification-duty params (the classifier serving key's params;
    ///    `None` on paths without a classifier duty),
    /// 2. the model's entry `params` (`models.default` sparse-inheritance
    ///    already applied at parse),
    /// 3. `role.params` (the role run block's sampling, when a role is
    ///    dispatched through),
    /// 4. the pool profile's composed params (when `qualifier` matches one),
    /// 5. `models.<model>.roles.<role>.params` (the model-specific override
    ///    for the task; wins when present, absent contributes nothing).
    ///
    /// `max_ctx`, if specified (model-side role override, then the entry),
    /// limits the size of `num_ctx` in the composed object — the cap clamps
    /// the composed window. The resolved thinking level (see
    /// [`Self::resolve_thinking`]) is written into the composed object as
    /// `thinking`; declaration-only keys are stripped at the body boundary.
    /// `None` when a named `qualifier` matches no profile — callers fall
    /// back to the entry's bare params (the `instance_params_for` contract,
    /// preserved).
    pub fn answer_params_for(
        &self,
        qualifier: Option<&str>,
        role: Option<(&str, &RoleEntry)>,
        classification_params: Option<&serde_json::Value>,
    ) -> Option<serde_json::Value> {
        let profile = match qualifier {
            Some(q) => self
                .effective_pool()
                .iter()
                .find(|p| p.name.as_deref() == Some(q) || p.group.as_deref() == Some(q)),
            None => None,
        };
        if qualifier.is_some() && profile.is_none() {
            return None;
        }
        let role_name = role.map(|(name, _)| name);
        let role_entry = role.map(|(_, entry)| entry);
        let role_sampling = role_entry.and_then(|r| r.params.sampling_value());
        let over: Option<&ModelRoleOverride> =
            role_name.and_then(|name| self.role_params.as_ref()?.get(name));
        let override_params: Option<&serde_json::Value> = over.and_then(|o| o.params.as_ref());
        let mut merged = overlay_params(classification_params, self.params.as_ref());
        if let Some(sampling) = role_sampling.as_ref() {
            merged = overlay_params(Some(&merged), Some(sampling));
        }
        if let Some(p) = profile {
            merged = overlay_params(Some(&merged), p.params.as_ref());
        }
        if let Some(op) = override_params {
            merged = overlay_params(Some(&merged), Some(op));
        }
        // The max_ctx cap bounds the composed window: the model-side role
        // override wins, then the entry cap. A composed num_ctx above the
        // cap is clamped down (never silently dropped).
        if let Some(cap) = over.and_then(|o| o.max_ctx).or(self.max_ctx) {
            if let Some(obj) = merged.as_object_mut() {
                for key in ["num_ctx", "ctx_size"] {
                    if let Some(n) = obj.get(key).and_then(serde_json::Value::as_u64) {
                        if n > cap {
                            obj.insert(key.to_string(), serde_json::Value::from(cap));
                        }
                    }
                }
            }
        }
        if let Some(level) = self.resolve_thinking(role_entry, over) {
            if let Some(obj) = merged.as_object_mut() {
                obj.insert("thinking".to_string(), serde_json::Value::String(level));
            }
        }
        Some(strip_declaration_params(merged))
    }

    /// Resolve the thinking level for a dispatch through `role`: the
    /// model-side override wins, then the role entry, then this entry's own
    /// declaration. Within one layer the precedence is `enable_thinking`
    /// (bool → `on`/`off`, the llama.cpp chat-template switch) then
    /// `reasoning_effort` (a model-specific effort string such as
    /// `low`/`high`, read from the role's or override's sampling `params`).
    /// `None` means no level was declared anywhere (a `filter_thinking:
    /// true` entry with no level still filters via
    /// [`Self::filter_thinking_for`] — the bool alias). An explicit live
    /// level over `filter_thinking: true` warns loudly; `off` always
    /// filters.
    pub fn resolve_thinking(
        &self,
        role: Option<&RoleEntry>,
        over: Option<&ModelRoleOverride>,
    ) -> Option<String> {
        fn params_level(params: Option<&serde_json::Value>) -> Option<String> {
            params
                .and_then(|p| p.as_object())
                .and_then(|obj| obj.get("reasoning_effort"))
                .and_then(|e| e.as_str())
                .map(str::to_string)
        }
        fn bool_level(b: bool) -> String {
            if b { "on" } else { "off" }.to_string()
        }
        // Override layer: the typed field wins, then the override's params.
        let mut level = over
            .and_then(|o| o.enable_thinking)
            .map(bool_level)
            .or_else(|| over.and_then(|o| params_level(o.params.as_ref())));
        // Role layer.
        if level.is_none() {
            if let Some(role) = role {
                level = role
                    .enable_thinking
                    .map(bool_level)
                    .or_else(|| params_level(role.params.sampling_value().as_ref()));
            }
        }
        // Entry layer.
        if level.is_none() {
            level = self.enable_thinking.map(bool_level);
        }
        if self.filter_thinking {
            if let Some(ref set) = level {
                if set != "off" {
                    tracing::warn!(
                        target: "router.config",
                        thinking = %set,
                        "explicit thinking level wins over filter_thinking:true",
                    );
                }
            }
        }
        level
    }

    /// Whether thinking blocks are stripped for a dispatch whose resolved
    /// level is `resolved_thinking`: an explicit `off` always filters;
    /// `preserve_thinking: true` (model-side override, then the role entry,
    /// then this entry) keeps the blocks for any live level, beating the
    /// entry bool; otherwise the entry bool decides. A preserve beside
    /// `off` warns loudly — there is nothing generated to keep, so the
    /// filter wins. Pool profiles carry windows, never thinking policy — a
    /// profile-level `preserve_thinking` is not consulted.
    pub fn filter_thinking_for(
        &self,
        resolved_thinking: Option<&str>,
        role: Option<&RoleEntry>,
        over: Option<&ModelRoleOverride>,
    ) -> bool {
        let preserve = over
            .and_then(|o| o.preserve_thinking)
            .or_else(|| role.and_then(|r| r.preserve_thinking))
            .or(self.preserve_thinking);
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
        // The model-side role override's filter flag wins over the entry
        // bool; the entry bool decides otherwise.
        over.and_then(|o| o.filter_thinking)
            .unwrap_or(self.filter_thinking)
    }

    /// Resolve the embedding-model reference serving `qualifier` (an
    /// instance name or group of this model's boot-materialized pool): the
    /// matching profile's override, else the entry's own (which inherits
    /// `models.default`). `None` selects no embedding duty from this model.
    /// `qualifier = None` resolves the entry default (the bare-model path).
    pub fn embedding_for(&self, qualifier: Option<&str>) -> Option<String> {
        if let Some(q) = qualifier {
            if let Some(profile) = self
                .effective_pool()
                .iter()
                .find(|p| p.name.as_deref() == Some(q) || p.group.as_deref() == Some(q))
            {
                if let Some(embed) = profile.embedding.clone() {
                    return Some(embed);
                }
            }
        }
        self.embedding.clone()
    }
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
