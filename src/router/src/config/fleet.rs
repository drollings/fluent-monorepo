//! Fleet run-block types: the `roles.<role>.params` vocabulary the
//! supervisor and dispatch compose over.
//! Re-exported through `crate::config` so `crate::config::X` paths
//! keep resolving.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

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
