//! The unified `LlmFleet` facade over the llama pool and the onnx registry.

use std::sync::Arc;

use serde_json::Value;

use fluent_llm::runtime::{EvictionPolicy, LlmRuntimeError, LlmWeights};

use crate::config::RouterConfig;
use crate::instances::client::{InstanceError, InstanceTotals};
use crate::instances::instance_aliases;
use crate::instances::traits::common::row_to_info;
use crate::instances::traits::weights::llama_weights_for_pool;
use crate::instances::InstancePool;
use crate::supervisor::LlamaServerSupervisor;

/// The unified weights facade the server builds from the llama `InstancePool`
/// and the onnx registry. Llama rows stay **byte-identical** (delegated to
/// [`InstancePool::aggregate`]/`list_models`); onnx rows are appended by
/// rendering `dyn LlmWeights` residency rows. Also the surface `ps` and
/// `POST /models/unload` route both fleets through.
#[derive(Clone)]
/// The unified weights facade the server builds from the llama `InstancePool`
/// and the onnx registry. Llama rows stay **byte-identical** (delegated to
/// [`InstancePool::aggregate`]/`list_models`); onnx rows are appended by
/// rendering `dyn LlmWeights` residency rows. Also the surface `ps` and
/// `POST /models/unload` route both fleets through.
pub struct LlmFleet {
    /// The llama-only pool (unchanged aggregation + dispatch path).
    pool: InstancePool,
    /// Every weights instance as a trait object (llama adapters + onnx
    /// implementors). Drives the shared residency engine (M5), the onnx rows
    /// of the facade, and unified unload.
    weights: Vec<Arc<dyn LlmWeights>>,
    /// The onnx registry (refusal checks + registry-key knowledge).
    onnx: Option<crate::ort::OrtRegistry>,
}

impl LlmFleet {
    /// Build the fleet from the llama pool, the supervisor (for llama adapters'
    /// `ensure_running` bin), and the onnx registry. Onnx implementors read
    /// their role config (instances block, defaults) from `config`.
    pub fn build(
        pool: InstancePool,
        supervisor: Option<&LlamaServerSupervisor>,
        onnx: Option<crate::ort::OrtRegistry>,
        config: &RouterConfig,
    ) -> Self {
        let mut weights: Vec<Arc<dyn LlmWeights>> = Vec::new();
        if let Some(sup) = supervisor {
            weights.extend(llama_weights_for_pool(&pool, sup, &config.sidecar));
        }
        if let Some(onnx) = &onnx {
            weights.extend(crate::ort::onnx_weights_impls(onnx, config));
        }
        Self {
            pool,
            weights,
            onnx,
        }
    }

    /// Whether the fleet is empty (no llama pool, no onnx weights).
    pub fn is_empty(&self) -> bool {
        self.pool.is_empty() && self.weights.is_empty()
    }

    /// The llama-only pool (dispatch + management ops that stay llama-scoped).
    pub fn instance_pool(&self) -> &InstancePool {
        &self.pool
    }

    /// Every weights instance as a trait object (the shared engine's input).
    pub fn weights(&self) -> &[Arc<dyn LlmWeights>] {
        &self.weights
    }

    /// The onnx registry, when present.
    pub fn onnx(&self) -> Option<&crate::ort::OrtRegistry> {
        self.onnx.as_ref()
    }

    /// Whether a model key is known to the fleet (llama manager or a weights
    /// instance).
    pub fn is_known_model(&self, key: &str) -> bool {
        self.pool.manager(key).is_some() || self.weights.iter().any(|w| w.model_key() == key)
    }

    /// `GET /instances` — the aggregate envelope across every fleet (llama rows
    /// byte-identical to `InstancePool::aggregate`, onnx rows appended).
    pub async fn aggregate(&self, model: Option<&str>) -> Result<Value, InstanceError> {
        let llama = self.pool.aggregate(model).await?;
        let mut instances = llama["instances"].as_array().cloned().unwrap_or_default();
        let snapshots = llama["snapshots"].as_array().cloned().unwrap_or_default();
        let mut total: InstanceTotals =
            serde_json::from_value(llama["total"].clone()).unwrap_or_default();

        for w in &self.weights {
            if w.eviction_policy() != EvictionPolicy::LruLargest {
                continue; // llama rows already came through `pool.aggregate`
            }
            if let Some(filter) = model {
                if filter != w.model_key() {
                    continue;
                }
            }
            let rows = w.residency_rows().await;
            if rows.is_empty() {
                continue;
            }
            // The shared weights count once per loaded onnx instance.
            let model_bytes = if w.is_loaded() { w.weights_bytes() } else { 0 };
            total.model = total.model.saturating_add(model_bytes);
            total.total = total.total.saturating_add(model_bytes);
            for row in &rows {
                let mut entry =
                    serde_json::to_value(row_to_info(w.model_key(), row)).unwrap_or_default();
                if let Value::Object(ref mut obj) = entry {
                    obj.insert("runtime".into(), Value::String("onnx".into()));
                }
                instances.push(entry);
                total.context = total.context.saturating_add(row.context_bytes);
                total.compute = total.compute.saturating_add(row.compute_bytes);
                total.total = total.total.saturating_add(row.total_bytes);
            }
        }
        Ok(serde_json::json!({
            "instances": instances,
            "snapshots": snapshots,
            "total": total,
        }))
    }

    /// `GET /v1/models` — llama entries byte-identical to
    /// `InstancePool::list_models`, onnx entries appended (one per context,
    /// with the llama alias vocabulary).
    pub async fn list_models(&self) -> Vec<Value> {
        let mut out = self.pool.list_models().await;
        let created = common_core::now_secs();
        for w in &self.weights {
            if w.eviction_policy() != EvictionPolicy::LruLargest {
                continue;
            }
            let rows = w.residency_rows().await;
            if rows.is_empty() {
                continue;
            }
            for row in &rows {
                let name = row
                    .context_key
                    .strip_prefix(&format!("{}:", w.model_key()))
                    .unwrap_or(&row.context_key)
                    .to_string();
                let is_default = name == "default";
                let mut entry = serde_json::json!({
                    "id": row.context_key,
                    "object": "model",
                    "created": created,
                    "owned_by": "coral-router",
                    "n_ctx": row.n_ctx,
                    "parallel": row.parallel,
                    "pinned": row.pinned,
                    "resume": row.resume,
                    "is_default": is_default,
                    "state": row.state,
                    "last_used": row.last_used,
                    "runtime": "onnx",
                });
                entry["aliases"] = Value::Array(
                    instance_aliases(w.model_key(), &row.context_key, &row.group, is_default)
                        .into_iter()
                        .map(Value::String)
                        .collect(),
                );
                out.push(entry);
            }
        }
        out
    }

    /// Unload a model through the trait surface (onnx `Unloadable` roles via
    /// their `OnnxWeights`; llama via the supervisor adapter). `Always`/pinned
    /// refusals surface as [`LlmRuntimeError::UnloadRefused`].
    pub async fn unload(&self, key: &str) -> Result<(), LlmRuntimeError> {
        if let Some(w) = self.weights.iter().find(|w| w.model_key() == key) {
            return w.unload().await;
        }
        Err(LlmRuntimeError::NotLoaded(format!("unknown model: {key}")))
    }

    /// Resize an onnx context through `LlmContext::resize`: the
    /// onnx half of `POST /instances/:name/resize`. Only onnx weights
    /// implementors (the `LruLargest` policy) serve named contexts in-process;
    /// llama contexts stay on the pool's `client.resize` path. An unknown model
    /// or a missing context is a loud error.
    pub async fn resize_context(
        &self,
        model: &str,
        name: &str,
        n_ctx: u64,
    ) -> Result<(), LlmRuntimeError> {
        let weights = self
            .weights
            .iter()
            .find(|w| w.model_key() == model && w.eviction_policy() == EvictionPolicy::LruLargest)
            .ok_or_else(|| {
                LlmRuntimeError::NotLoaded(format!("no onnx model or context: '{model}:{name}'"))
            })?;
        let ctx = weights.context(name).ok_or_else(|| {
            LlmRuntimeError::NotLoaded(format!("onnx context '{model}:{name}' not materialized"))
        })?;
        ctx.resize(n_ctx).await
    }

    /// Resolve the onnx instance id grammar `<model_id>:<name>` to `(model key,
    /// context name)`. `None` when the model is not an onnx weights key or the
    /// name breaks the instance-name grammar. Llama keys are handled by the
    /// llama pool's resolver; this is the onnx branch of the unified grammar.
    pub fn resolve_instance_id(&self, id: &str) -> Option<(String, String)> {
        let parsed = fluent_types::instance_id::InstanceId::parse(id).ok()?;
        let is_onnx = self.weights.iter().any(|w| {
            w.model_key() == &**parsed.model() && w.eviction_policy() == EvictionPolicy::LruLargest
        });
        is_onnx.then(|| {
            (
                (**parsed.model()).to_string(),
                (**parsed.name()).to_string(),
            )
        })
    }
}