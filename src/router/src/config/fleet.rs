//! Fleet run-block types: the `roles.<role>.params` vocabulary the
//! supervisor and dispatch compose over.
//! Re-exported through `crate::config` so `crate::config::X` paths
//! keep resolving.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{overlay_params, InstanceProfile, ModelEntry, RoleEntry};
use common_core::constants::default_true;

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
/// defaults through [`super::root::RouterConfig::spawn_defaults_for`].
#[derive(Debug, Clone, Serialize, Deserialize)]
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
/// defaults through [`super::root::RouterConfig::spawn_defaults_for`].
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
                self.sampling
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
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
    pub fn with_model_launch_overrides(&self, params: Option<&serde_json::Value>) -> RoleParams {
        let mut out = self.clone();
        if let Some(obj) = params.and_then(|p| p.as_object()) {
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
        }
        // The max_ctx cap limits the size of num_ctx: a model whose cap
        // sits below its (possibly overridden) window clamps at spawn.
        if let Some(cap) = out.max_ctx {
            if out.num_ctx > cap {
                out.num_ctx = cap;
            }
        }
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

pub(crate) fn default_onnx_limiter_cap() -> usize {
    common_core::constants::DEFAULT_ONNX_LIMITER_CAP
}

pub(crate) fn default_onnx_threads() -> usize {
    common_core::constants::DEFAULT_ONNX_THREADS
}

const fn default_n_gpu_layers() -> i32 {
    999
}

const fn default_sleep_idle_seconds() -> i32 {
    15
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

/// On-demand role-window templates for one model: every role maps to the
/// window a dispatch through that role opens when the model has no
/// materialized profile for it (`<model>:<role>` from the role's params).
/// Same composition as a bound profile — the role run block's `num_ctx`
/// clamped to the model-side cap (role override, then the entry), the
/// entry's `max_parallel`, the entry's `max_ctx` — except `pinned: false`
/// always: on-demand windows are demand-driven and idle-reaped, never
/// resident anchors (a pinned role's window exists only where the role
/// binds the model and boot materializes it). `name` is filled at creation;
/// `session: false` (one-shot: never snapshotted, like every undeclared
/// window).
#[allow(clippy::implicit_hasher)]
/// On-demand role-window templates for one model: every role maps to the
/// window a dispatch through that role opens when the model has no
/// materialized profile for it (`<model>:<role>` from the role's params).
/// Same composition as a bound profile — the role run block's `num_ctx`
/// clamped to the model-side cap (role override, then the entry), the
/// entry's `max_parallel`, the entry's `max_ctx` — except `pinned: false`
/// always: on-demand windows are demand-driven and idle-reaped, never
/// resident anchors (a pinned role's window exists only where the role
/// binds the model and boot materializes it). `name` is filled at creation;
/// `session: false` (one-shot: never snapshotted, like every undeclared
/// window).
pub(crate) fn role_window_templates(
    entry: &ModelEntry,
    roles: &HashMap<String, RoleEntry>,
) -> HashMap<String, InstanceProfile> {
    let mut names: Vec<&String> = roles.keys().collect();
    names.sort();
    let mut out = HashMap::new();
    for name in names {
        let role = &roles[name];
        let over = entry.role_params.as_ref().and_then(|o| o.get(name));
        let cap = over.and_then(|o| o.max_ctx).or(entry.max_ctx);
        let mut num_ctx = role.params.num_ctx;
        if let Some(c) = cap {
            if c < num_ctx {
                num_ctx = c;
            }
        }
        out.insert(
            name.clone(),
            InstanceProfile {
                name: None,
                group: Some(name.clone()),
                count: 1,
                num_ctx,
                parallel: entry.max_parallel,
                pinned: false,
                no_sleep: false,
                sleep_idle_seconds: None,
                default: false,
                resume: false,
                params: None,
                embedding: None,
                max_ctx: entry.max_ctx,
                session: false,
            },
        );
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
/// - when no role binds the model, no profile at all — there is no
///   synthetic `base` dispatch point. Every dispatch names a real role
///   window (`<model>:<role>`), opened on demand from the role's params
///   when the model is selected; unlisted models stay fully lazy until
///   then.
///
/// A pinned profile is what boots a model at startup
/// (`supervisor::LlamaServerSpec::from_entry`); everything else loads on
/// demand. `expand_instance_map` clamps each profile's `num_ctx` to its
/// `max_ctx` cap and resolves names/groups.
#[allow(clippy::implicit_hasher)]
/// Compose one model's effective instance pool at boot: the single
/// role → per-model-instance params chain, synthesized here so every
/// qualifier, sampling, backend, and supervision path reads composed values
/// through `ModelEntry::effective_pool`.
/// 
/// The config declares no instance pools; profiles are synthesized from the
/// model entry and the roles that bind it:
/// 
/// - one profile per role whose `models` list binds this model (sorted
/// role order), composing role run-block sampling and the model-side
/// `roles.<role>` override over the entry params, with `pinned`
/// composing as role OR model and `max_ctx` as override-else-entry;
/// - when no role binds the model, no profile at all — there is no
/// synthetic `base` dispatch point. Every dispatch names a real role
/// window (`<model>:<role>`), opened on demand from the role's params
/// when the model is selected; unlisted models stay fully lazy until
/// then.
/// 
/// A pinned profile is what boots a model at startup
/// (`supervisor::LlamaServerSpec::from_entry`); everything else loads on
/// demand. `expand_instance_map` clamps each profile's `num_ctx` to its
/// `max_ctx` cap and resolves names/groups.
pub(crate) fn materialize_effective_pool(
    model_key: &str,
    entry: &ModelEntry,
    roles: &HashMap<String, RoleEntry>,
) -> Vec<InstanceProfile> {
    let mut merged: HashMap<String, InstanceProfile> = HashMap::new();
    let mut binding_roles: Vec<&str> = roles
        .iter()
        .filter(|(_, role)| role.serves(model_key))
        .map(|(name, _)| name.as_str())
        .collect();
    binding_roles.sort_unstable();
    // No bindings: no profile. The model serves only through role windows
    // opened on demand (`<model>:<role>` from the role's params) — there is
    // no synthetic `base` dispatch point beside them, so an unlisted model
    // stays fully lazy (not even a pre-created window) until selected.
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