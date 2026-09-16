//! Model-table entry types: `models` entries, role bindings, instance
//! profiles, and the per-model params/thinking composition.
//! Re-exported through `crate::config` so `crate::config::X` paths
//! keep resolving; the parse-time template/fleet legs live in
//! `super::normalize`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use common_core::constants::default_true;

use super::root::{overlay_params, strip_declaration_params, PRESERVE_THINKING_KEY};

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
            Some(q) => self
                .effective_pool()
                .iter()
                .find(|p| p.name.as_deref() == Some(q) || p.group.as_deref() == Some(q)),
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
                obj.insert("thinking".to_string(), serde_json::Value::String(level));
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
            if let Some(enabled) = obj
                .get("enable_thinking")
                .and_then(serde_json::Value::as_bool)
            {
                if !enabled && obj.contains_key("reasoning_effort") {
                    tracing::warn!(
                        target: "router.config",
                        "enable_thinking:false beside reasoning_effort — effort ignored, level resolves off",
                    );
                }
                return Some(if enabled {
                    "on".to_string()
                } else {
                    "off".to_string()
                });
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
            params?.as_object()?.get(PRESERVE_THINKING_KEY)?.as_bool()
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
