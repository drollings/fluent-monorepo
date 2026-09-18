//! The llama inference backend: a thin `InferenceBackend` adapter over the

//! `InstancePool` + supervisor lookup.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use fluent_llm::backend::{BackendCaps, InferenceBackend, Readiness};
use fluent_llm::client::ChatBackend;
use fluent_llm::runtime::LlmWeights;
use fluent_wvr::prelude::*;

use crate::config::builder::{llama_chat_backend_for_instance, llama_chat_backend_for_key};
use crate::config::{ModelEntry, SidecarConfig};
use crate::instances::traits::weights::LlamaWeights;
use crate::instances::InstancePool;

/// The llama inference backend: a thin [`InferenceBackend`] adapter over the
/// `InstancePool` + supervisor lookup. Chat construction delegates to the
/// single shared `llama_chat_backend_for_*` constructors (the same functions
/// `RouterConfig::local_backend` falls back to), and `weights()` builds the
/// existing `LlamaWeights` adapter exactly as `LlmFleet::build` does — no
/// second construction site for either.
///
/// Onnx precedence: a key whose base names a configured onnx role resolves
/// `None` here even when a `models` entry exists, preserving today's
/// resolver-first ordering (the onnx branch wins on collision).
pub struct LlamaBackend {
    pool: InstancePool,
    models: HashMap<String, ModelEntry>,
    roles: HashMap<String, crate::config::RoleEntry>,
    onnx_keys: BTreeSet<String>,
    sidecar: SidecarConfig,
}

impl LlamaBackend {
    /// Build the adapter over the managed pool, the `models` map (for chat
    /// construction — entries arrive boot-materialized, so no fleet-default
    /// map is carried), the `roles` table (role-key resolution through the
    /// single inference-point precedence), the configured onnx role keys
    /// (precedence), and the sidecar policy (for `LlamaWeights` construction).
    pub fn new(
        pool: InstancePool,
        models: HashMap<String, ModelEntry>,
        roles: HashMap<String, crate::config::RoleEntry>,
        onnx_keys: BTreeSet<String>,
        sidecar: SidecarConfig,
    ) -> Self {
        Self {
            pool,
            models,
            roles,
            onnx_keys,
            sidecar,
        }
    }
}

impl FieldAccess for LlamaBackend {
    fn set_field(&mut self, name: &str, _value: &str) -> Result<(), FieldError> {
        Err(FieldError::NotFound(name.into()))
    }
    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        Err(FieldError::NotFound(name.into()))
    }
    fn field_names(&self) -> &'static [&'static str] {
        &[]
    }
}

impl Describable for LlamaBackend {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "description": "llama-server fleet inference backend",
        })
    }
}

impl WorkUnit for LlamaBackend {
    fn name(&self) -> &str {
        "llama"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        Ok(WorkOutput::ok("llama backend adapter"))
    }
}

impl_component!(LlamaBackend);

impl InferenceBackend for LlamaBackend {
    fn backend_id(&self) -> &'static str {
        "llama"
    }
    fn model_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .models
            .keys()
            .filter(|k| {
                let (base, _) = fluent_types::model_key::split_model_key(k);
                !self.onnx_keys.contains(base)
            })
            .cloned()
            .collect();
        keys.sort();
        keys
    }
    fn weights(&self, key: &str) -> Option<Arc<dyn LlmWeights>> {
        let manager = self.pool.manager(key)?;
        let supervisor = self.pool.supervisor()?;
        let server = supervisor.server_for(key)?;
        let bin = supervisor.bin().to_path_buf();
        Some(Arc::new(LlamaWeights::new(
            manager,
            server,
            bin,
            self.sidecar.clone(),
        )))
    }
    fn chat_backend(&self, key: &str, instance: Option<&str>) -> Option<Arc<dyn ChatBackend>> {
        let (base, _) = fluent_types::model_key::split_model_key(key);
        if self.onnx_keys.contains(base) {
            return None;
        }
        // Both halves resolve role keys through the single inference-point
        // precedence (a role serves its head candidate at the role's point).
        match instance {
            // Entries arrive boot-materialized (effective pools composed),
            // so the shared constructors resolve through one code path.
            Some(name) => llama_chat_backend_for_instance(&self.models, &self.roles, key, name),
            None => llama_chat_backend_for_key(&self.models, &self.roles, key),
        }
    }
    fn capabilities(&self) -> BackendCaps {
        BackendCaps::chat_with_contexts()
    }
    fn readiness(&self, key: &str) -> Readiness {
        let Some(manager) = self.pool.manager(key) else {
            return Readiness::Unloaded;
        };
        let running = self
            .pool
            .supervisor()
            .and_then(|s| s.server_for(manager.model_key()))
            .is_some_and(|s| s.is_running());
        if running {
            Readiness::Loaded
        } else {
            Readiness::Unloaded
        }
    }
}
