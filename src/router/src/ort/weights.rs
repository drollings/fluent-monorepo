//! The onnx half of the shared `LlmWeights` contract: one `OnnxWeights`
//! adapter per registered onnx role, plus the role-profile resolution the
//! context-bound backends compose.

use std::sync::Arc;

#[cfg(feature = "onnx")]
use fluent_llm::onnx_error::OrtError;
#[cfg(feature = "onnx")]
use fluent_llm::onnx_session::OrtSessionRegistry;

use crate::config::RouterConfig;

use super::registry::OrtRegistry;

/// The default idle threshold (seconds) after which an onnx session may be
/// released, when the role's `sleep_idle_seconds` is absent or zero. Feeds
/// the shared residency engine; per-role overrides come from each entry's
/// `sleep_idle_seconds` (registered by `build_onnx_registry`).
pub const DEFAULT_SLEEP_IDLE_SECONDS: i32 = 30;

// ── The onnx half of the shared LlmWeights contract ─────────────
//
// `OnnxWeights` presents one in-process onnx role through the shared
// `fluent_llm::runtime::LlmWeights` surface (the same contract the llama
// adapters in `instances/traits.rs` implement). It wraps the role's generative
// `OnnxContextPool` (built **lazily** — never forces a session load) and the
// `OrtSessionRegistry` entry. The residency engine (M5), the unified
// `/instances` + `/v1/models` facade, `ps`, and `POST /models/unload` drive
// the onnx fleet through it with the `LruLargest` eviction ordering.

/// Map an onnx registry error onto the runtime-agnostic error type. A release
/// refusal (`Always`/pinned) surfaces as [`LlmRuntimeError::UnloadRefused`].
#[cfg(feature = "onnx")]
/// ── The onnx half of the shared LlmWeights contract ─────────────
/// 
/// `OnnxWeights` presents one in-process onnx role through the shared
/// `fluent_llm::runtime::LlmWeights` surface (the same contract the llama
/// adapters in `instances/traits.rs` implement). It wraps the role's generative
/// `OnnxContextPool` (built **lazily** — never forces a session load) and the
/// `OrtSessionRegistry` entry. The residency engine (M5), the unified
/// `/instances` + `/v1/models` facade, `ps`, and `POST /models/unload` drive
/// the onnx fleet through it with the `LruLargest` eviction ordering.
/// Map an onnx registry error onto the runtime-agnostic error type. A release
/// refusal (`Always`/pinned) surfaces as [`LlmRuntimeError::UnloadRefused`].
fn map_ort_err(e: &OrtError) -> fluent_llm::runtime::LlmRuntimeError {
    let s = e.to_string();
    if s.contains("refuses unload") {
        fluent_llm::runtime::LlmRuntimeError::UnloadRefused(s)
    } else {
        fluent_llm::runtime::LlmRuntimeError::Other(s)
    }
}

/// The `LlmWeights` implementor for one onnx role. Wraps the
/// role's `OnnxContextPool` (generative roles) + the registry entry. A loaded
/// generative role with no materialized contexts synthesizes the single
/// `default` context row — the onnx analogue of a plain llama model's
/// footprint. Non-generative roles render no context rows (today's
/// invisibility — their weights still count toward the RAM working set).
#[cfg(feature = "onnx")]
/// The `LlmWeights` implementor for one onnx role. Wraps the
/// role's `OnnxContextPool` (generative roles) + the registry entry. A loaded
/// generative role with no materialized contexts synthesizes the single
/// `default` context row — the onnx analogue of a plain llama model's
/// footprint. Non-generative roles render no context rows (today's
/// invisibility — their weights still count toward the RAM working set).
pub struct OnnxWeights {
    model_key: String,
    registry: Arc<OrtSessionRegistry>,
    role: fluent_llm::onnx_config::OnnxRoleConfig,
    pool: std::sync::Mutex<Option<Arc<fluent_onnx::OnnxContextPool>>>,
}

#[cfg(feature = "onnx")]
impl OnnxWeights {
    /// Build the adapter over a registered role's registry entry + role config
    /// (the `instances` block drives `ensure_context` profiles).
    pub fn new(
        model_key: String,
        registry: Arc<OrtSessionRegistry>,
        role: fluent_llm::onnx_config::OnnxRoleConfig,
    ) -> Self {
        Self {
            model_key,
            registry,
            role,
            pool: std::sync::Mutex::new(None),
        }
    }

    /// The role config (the `instances` block + `max_ctx` drive context
    /// profiles).
    pub(crate) fn role(&self) -> &fluent_llm::onnx_config::OnnxRoleConfig {
        &self.role
    }

    /// Whether the registry reports the session loaded.
    fn is_loaded(&self) -> bool {
        self.registry
            .residency_report()
            .iter()
            .any(|r| r.key == self.model_key && r.loaded)
    }

    /// Whether the role's registered session is generative (CausalLm — the only
    /// task that serves named contexts).
    fn is_generative(&self) -> bool {
        self.registry
            .config(&self.model_key)
            .is_some_and(|c| c.task == fluent_llm::onnx_config::OnnxTask::CausalLm)
    }

    /// Build the role's context pool on first use (loads the session through
    /// `build_llm_session` — the M6 lazy-residency load point). Idempotent.
    pub(crate) fn ensure_pool(
        &self,
    ) -> Result<Arc<fluent_onnx::OnnxContextPool>, fluent_llm::runtime::LlmRuntimeError> {
        {
            let guard = common_core::sync::lock(&self.pool);
            if let Some(pool) = guard.as_ref() {
                return Ok(Arc::clone(pool));
            }
        }
        let session = fluent_onnx::build_llm_session(&self.registry, &self.model_key)
            .map_err(|e| map_ort_err(&e))?
            .ok_or_else(|| {
                fluent_llm::runtime::LlmRuntimeError::NotLoaded(format!(
                    "onnx model {} is not a CausalLm session",
                    self.model_key
                ))
            })?;
        let pool = fluent_onnx::OnnxContextPool::new(Arc::new(session), self.model_key.clone());
        *common_core::sync::lock(&self.pool) = Some(Arc::clone(&pool));
        Ok(pool)
    }

    /// The `OnnxContextProfile` for a named context: the role's declared
    /// `instances` block (name-or-key match, `max_ctx` applied), else a default
    /// profile inheriting the role's global `max_ctx` and pin.
    fn profile_for(&self, name: &str) -> fluent_onnx::OnnxContextProfile {
        onnx_role_profile_for(&self.role, name)
    }
}

/// Resolve an onnx role's named-context profile: the role's
/// `instances` block (name-or-key match, `max_ctx` applied), else a default
/// profile inheriting the role's global `max_ctx` and pin. Shared by
/// `OnnxWeights::ensure_context` and the context-bound `OnnxChatBackend`.
#[cfg(feature = "onnx")]
/// Resolve an onnx role's named-context profile: the role's
/// `instances` block (name-or-key match, `max_ctx` applied), else a default
/// profile inheriting the role's global `max_ctx` and pin. Shared by
/// `OnnxWeights::ensure_context` and the context-bound `OnnxChatBackend`.
pub(crate) fn onnx_role_profile_for(
    role: &fluent_llm::onnx_config::OnnxRoleConfig,
    name: &str,
) -> fluent_onnx::OnnxContextProfile {
    if let Some((key, p)) = role.instances.as_ref().and_then(|m| {
        m.iter()
            .find(|(k, p)| p.name.as_deref() == Some(name) || k.as_str() == name)
    }) {
        let mut p = p.clone();
        p.apply_max_ctx();
        let group = p.group.clone().unwrap_or_else(|| key.clone());
        fluent_onnx::OnnxContextProfile {
            group,
            n_ctx: p.num_ctx,
            max_ctx: p.max_ctx.or_else(|| role.model.max_ctx.map(|c| c as u64)),
            pinned: p.pinned,
            resume: p.resume,
        }
    } else {
        fluent_onnx::OnnxContextProfile {
            group: name.to_string(),
            n_ctx: 0, // the pool default
            max_ctx: role.model.max_ctx.map(|c| c as u64),
            pinned: role.pinned,
            resume: false,
        }
    }
}

#[cfg(feature = "onnx")]
#[async_trait::async_trait]
impl fluent_llm::runtime::LlmWeights for OnnxWeights {
    fn model_key(&self) -> &str {
        &self.model_key
    }

    fn weights_bytes(&self) -> u64 {
        self.registry.resident_bytes(&self.model_key).unwrap_or(0)
    }

    fn pinned(&self) -> bool {
        self.registry.is_pinned(&self.model_key)
    }

    fn refuse_unload(&self) -> bool {
        // `Always` residency OR pinned — the exact onnx residency rule
        // (`policy.is_always() || pinned` is never released/evicted). Only the
        // shared engine consumes this (the admin unload path reads the
        // registry's `refuses_unload` directly, so this does not affect it).
        self.registry.refuses_unload(&self.model_key) || self.registry.is_pinned(&self.model_key)
    }

    fn is_loaded(&self) -> bool {
        OnnxWeights::is_loaded(self)
    }

    fn sleep_idle_seconds(&self) -> Option<i32> {
        self.registry.sleep_idle_seconds(&self.model_key)
    }

    async fn ensure_loaded(&self) -> Result<(), fluent_llm::runtime::LlmRuntimeError> {
        let _ = self
            .registry
            .ensure_loaded(&self.model_key)
            .map_err(|e| map_ort_err(&e))?;
        Ok(())
    }

    async fn unload(&self) -> Result<(), fluent_llm::runtime::LlmRuntimeError> {
        self.registry
            .release(&self.model_key)
            .map_err(|e| map_ort_err(&e))?;
        // The session is gone: drop the pool's contexts (their KV dies with it).
        let pool = common_core::sync::lock(&self.pool).take();
        if let Some(pool) = pool {
            for key in pool.context_keys() {
                pool.destroy(&key);
            }
        }
        Ok(())
    }

    fn touch(&self) {
        self.registry.touch(&self.model_key);
    }

    fn last_used(&self) -> i64 {
        self.registry.last_used_of(&self.model_key).unwrap_or(0)
    }

    async fn residency_rows(&self) -> Vec<fluent_llm::runtime::LlmResidencyRow> {
        // A cold/unloaded role is invisible (today's rule); only generative
        // roles serve context rows.
        if !self.is_loaded() || !self.is_generative() {
            return Vec::new();
        }
        let pool = common_core::sync::lock(&self.pool).clone();
        if let Some(pool) = pool {
            let rows = pool.residency_rows();
            if !rows.is_empty() {
                return rows;
            }
        }
        // A loaded generative role with no materialized contexts synthesizes
        // the single `default` row (the onnx analogue of a plain llama model).
        let resident = self.weights_bytes();
        vec![fluent_llm::runtime::LlmResidencyRow {
            context_key: format!("{}:default", self.model_key),
            group: "default".into(),
            n_ctx: fluent_onnx::DEFAULT_ONNX_CONTEXT_TOKENS,
            parallel: 1,
            pinned: self.registry.is_pinned(&self.model_key),
            resume: false,
            state: "loaded".into(),
            runtime: fluent_llm::runtime::LlmRuntime::Onnx,
            model_bytes: resident,
            context_bytes: 0,
            compute_bytes: 0,
            total_bytes: 0,
            vram_bytes: 0,
            last_used: self.registry.last_used_of(&self.model_key).unwrap_or(0),
        }]
    }

    fn context(&self, name: &str) -> Option<Arc<dyn fluent_llm::runtime::LlmContext>> {
        let pool = common_core::sync::lock(&self.pool).clone()?;
        let ctx = pool.context(name)?;
        Some(ctx as Arc<dyn fluent_llm::runtime::LlmContext>)
    }

    async fn ensure_context(
        &self,
        name: &str,
    ) -> Result<Arc<dyn fluent_llm::runtime::LlmContext>, fluent_llm::runtime::LlmRuntimeError>
    {
        let pool = self.ensure_pool()?;
        let profile = self.profile_for(name);
        let ctx = pool.ensure_context(name, profile);
        Ok(ctx as Arc<dyn fluent_llm::runtime::LlmContext>)
    }

    fn eviction_policy(&self) -> fluent_llm::runtime::EvictionPolicy {
        fluent_llm::runtime::EvictionPolicy::LruLargest
    }
}

/// Build one `OnnxWeights` per registered onnx role. The role's
/// `instances` block (from `config.onnx`) drives `ensure_context` profiles;
/// the registry supplies the lifecycle + residency data. Onnx rows appear on
/// the unified `/instances`/`/v1/models` facades only when the onnx fleet is
/// configured.
#[cfg(feature = "onnx")]
/// Build one `OnnxWeights` per registered onnx role. The role's
/// `instances` block (from `config.onnx`) drives `ensure_context` profiles;
/// the registry supplies the lifecycle + residency data. Onnx rows appear on
/// the unified `/instances`/`/v1/models` facades only when the onnx fleet is
/// configured.
/// No-ort build: no onnx weights can exist — the fleet degrades to nothing
/// (fail-open, byte-identical to today's onnx invisibility).
pub fn onnx_weights_impls(
    registry: &OrtRegistry,
    config: &RouterConfig,
) -> Vec<Arc<dyn fluent_llm::runtime::LlmWeights>> {
    let mut out = Vec::new();
    let Some(fleet) = config.onnx.as_ref() else {
        return out;
    };
    for (role, role_cfg) in fleet.iter() {
        let key = role.registry_key().to_string();
        if !registry.is_registered(&key) {
            continue;
        }
        out.push(Arc::new(OnnxWeights::new(
            key,
            Arc::clone(registry),
            role_cfg.clone(),
        )));
    }
    out
}

/// No-ort build: no onnx weights can exist — the fleet degrades to nothing
/// (fail-open, byte-identical to today's onnx invisibility).
#[cfg(not(feature = "onnx"))]
pub fn onnx_weights_impls(
    _registry: &OrtRegistry,
    _config: &RouterConfig,
) -> Vec<Arc<dyn fluent_llm::runtime::LlmWeights>> {
    Vec::new()
}