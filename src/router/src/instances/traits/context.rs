//! The `LlmContext` adapter for one named llama-fork instance.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use fluent_llm::runtime::{LlmContext, LlmKVCache, LlmRuntimeError};

use crate::config::InstanceProfile;
use crate::instances::client::{InstanceClient, InstanceInfo};
use crate::instances::manager::{
    instance_name_from_server_id, resume_snapshot_name, InstanceManager,
};
use crate::instances::traits::common::map_instance_err;
use crate::instances::traits::kv_cache::LlamaKVCache;

/// The `LlmContext` adapter for one named llama-fork instance: resize →
/// `client.resize`, KV → `LlamaKVCache`, resume → the manager's router-side
/// flag, eviction → resume-snapshot-then-destroy (the sidecar's exact order).
pub struct LlamaContext {
    client: InstanceClient,
    manager: Arc<InstanceManager>,
    name: String,
    group: String,
    n_ctx: AtomicU64,
    max_ctx: Option<u64>,
    pinned: bool,
    vram_bytes: u64,
    last_used: AtomicI64,
}

impl LlamaContext {
    /// Build the adapter from a live fork envelope row. Crate-visible so the
    /// eviction-order tests can drive the real adapter against a stub fork.
    pub(crate) fn from_info(
        manager: Arc<InstanceManager>,
        client: InstanceClient,
        info: &InstanceInfo,
        max_ctx: Option<u64>,
    ) -> Self {
        Self {
            client,
            manager,
            name: instance_name_from_server_id(&info.id).to_string(),
            group: info.group.clone(),
            n_ctx: AtomicU64::new(info.n_ctx),
            max_ctx,
            pinned: info.pinned,
            vram_bytes: info.vram_bytes,
            last_used: AtomicI64::new(info.last_used),
        }
    }

    /// Build the adapter from a configured (expanded) profile — the shape
    /// `ensure_context` hands back before the fork's next envelope poll.
    pub(crate) fn from_profile(
        manager: Arc<InstanceManager>,
        client: InstanceClient,
        profile: &InstanceProfile,
    ) -> Self {
        let name = profile.name.clone().unwrap_or_default();
        let group = profile.group.clone().unwrap_or_else(|| name.clone());
        let n_ctx = if profile.num_ctx == 0 {
            // The `roles.default.params` num_ctx default (16384) — the plain-model
            // context size the supervisor hands a no-instance server.
            16_384
        } else {
            profile.num_ctx
        };
        Self {
            client,
            manager,
            name,
            group,
            n_ctx: AtomicU64::new(n_ctx),
            max_ctx: profile.max_ctx,
            pinned: profile.pinned,
            vram_bytes: 0,
            last_used: AtomicI64::new(-1),
        }
    }
}

#[async_trait::async_trait]
impl LlmContext for LlamaContext {
    fn name(&self) -> &str {
        &self.name
    }

    fn group(&self) -> &str {
        &self.group
    }

    fn n_ctx(&self) -> u64 {
        self.n_ctx.load(Ordering::Relaxed)
    }

    fn max_ctx(&self) -> Option<u64> {
        self.max_ctx
    }

    async fn resize(&self, n_ctx: u64) -> Result<(), LlmRuntimeError> {
        if let Some(cap) = self.max_ctx {
            if n_ctx > cap {
                return Err(LlmRuntimeError::Other(format!(
                    "context {} resize to {n_ctx} exceeds max_ctx {cap}",
                    self.name
                )));
            }
        }
        self.client
            .resize(&self.name, n_ctx)
            .await
            .map_err(map_instance_err)?;
        self.n_ctx.store(n_ctx, Ordering::Relaxed);
        Ok(())
    }

    fn pinned(&self) -> bool {
        self.pinned
    }

    fn resume(&self) -> bool {
        self.manager.resume_for(&self.name)
    }

    fn set_resume(&self, enabled: bool) {
        self.manager.set_resume(&self.name, enabled);
    }

    fn touch(&self) {
        self.manager.touch();
    }

    fn last_used(&self) -> i64 {
        self.last_used.load(Ordering::Relaxed)
    }

    fn vram_bytes(&self) -> u64 {
        self.vram_bytes
    }

    async fn destroy(&self) -> Result<(), LlmRuntimeError> {
        self.client
            .destroy(&self.name, false)
            .await
            .map_err(map_instance_err)
    }

    fn kv_cache(&self) -> Arc<dyn LlmKVCache> {
        Arc::new(LlamaKVCache::new(
            self.client.clone(),
            self.name.clone(),
            self.manager.model_key().to_string(),
        ))
    }

    async fn evict(&self) -> Result<(), LlmRuntimeError> {
        // Resume-marked multi-step contexts are KV-snapshotted BEFORE they
        // drop (the sidecar's `evict_context` order) so a later
        // `snapshot=<name>-resume` restores them — but only in `auto`
        // snapshot mode. `manual` destroys without saving (and audits the
        // skip): the operator asked for no silent saves. A failed save logs
        // and the eviction still proceeds. One-shot contexts are
        // plain-destroyed: they hold no multi-step state, so the snapshot
        // pass is skipped.
        if self.manager.session_for(&self.name) && self.manager.resume_for(&self.name) {
            if self.manager.snapshot_mode().saves_on_evict() {
                let snapshot = resume_snapshot_name(&self.name);
                if let Err(e) = self.client.save_snapshot(&self.name, &snapshot).await {
                    tracing::warn!(
                        target: "router.instances",
                        model = %self.manager.model_key(),
                        instance = %self.name,
                        snapshot = %snapshot,
                        error = %e,
                        "resume snapshot save on evict failed - context drops unsnapshotted",
                    );
                }
            } else {
                tracing::info!(
                    target: "router.instances",
                    model = %self.manager.model_key(),
                    instance = %self.name,
                    "manual snapshot mode - context drops without saving",
                );
                crate::audit::emit(
                    "instances",
                    serde_json::json!({
                        "action": "evict_skip_snapshot",
                        "model": self.manager.model_key(),
                        "instance": self.name,
                        "reason": "snapshot_manual",
                    }),
                );
            }
        }
        self.destroy().await
    }
}
