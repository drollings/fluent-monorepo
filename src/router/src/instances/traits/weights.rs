//! The `LlmWeights` adapter for a managed llama-server (spawn/kill, residency,

//! eviction, resume expiry), plus the single pool construction helper.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fluent_llm::runtime::{
    EvictionPolicy, LlmContext, LlmResidencyRow, LlmRuntimeError, LlmWeights,
};

use crate::config::SidecarConfig;
use crate::instances::client::InstanceInfo;
use crate::instances::manager::{
    instance_name_from_server_id, resume_snapshot_name, InstanceManager,
};
use crate::instances::traits::common::{info_to_row, map_instance_err, map_server_err};
use crate::instances::traits::context::LlamaContext;
use crate::instances::InstancePool;
use crate::supervisor::{LlamaServerSupervisor, ManagedServer};

/// The `LlmWeights` adapter for one managed llama-server: spawn/kill through
/// the supervisor's `ManagedServer`, the residency view through the manager's
/// `list_with_fallback` (synthesized plain-model footprint included), and the
/// `FootprintColdness` eviction ordering (the llama sidecar's).
pub struct LlamaWeights {
    model_key: String,
    server: Arc<ManagedServer>,
    manager: Arc<InstanceManager>,
    /// The supervisor's `llama-server` binary (needed for `ensure_running`).
    bin: PathBuf,
    /// Sidecar residency policy (drives the `expire_resume` hook).
    policy: SidecarConfig,
    /// The latest envelope snapshot (instance name → info) so the sync
    /// `context()` lookup resolves without a network call.
    last_rows: Mutex<HashMap<String, InstanceInfo>>,
}

impl LlamaWeights {
    /// Build the adapter over an existing manager + server. `bin` is the
    /// supervisor's resolved `llama-server` path.
    pub fn new(
        manager: Arc<InstanceManager>,
        server: Arc<ManagedServer>,
        bin: PathBuf,
        policy: SidecarConfig,
    ) -> Self {
        Self {
            model_key: manager.model_key().to_string(),
            server,
            manager,
            bin,
            policy,
            last_rows: Mutex::new(HashMap::new()),
        }
    }

    /// The configured `max_ctx` cap for a named instance profile, if any.
    fn profile_max_ctx(&self, name: &str) -> Option<u64> {
        self.manager
            .profiles()
            .iter()
            .find(|p| p.name.as_deref() == Some(name))
            .and_then(|p| p.max_ctx)
    }
}

#[async_trait::async_trait]
impl LlmWeights for LlamaWeights {
    fn model_key(&self) -> &str {
        &self.model_key
    }

    fn weights_bytes(&self) -> u64 {
        self.manager.weights_bytes()
    }

    fn pinned(&self) -> bool {
        self.manager.profiles().iter().any(|p| p.pinned)
    }

    fn refuse_unload(&self) -> bool {
        false // the supervisor may always unload a llama-server
    }

    fn is_loaded(&self) -> bool {
        self.server.is_running()
    }

    fn sleep_idle_seconds(&self) -> Option<i32> {
        None // llama weights are never router-side idle-released (the fork owns their sleep)
    }

    async fn ensure_loaded(&self) -> Result<(), LlmRuntimeError> {
        self.server
            .ensure_running(&self.bin)
            .await
            .map_err(|e| map_server_err(&self.model_key, &e))?;
        // A load is a use: stamp recency so the residency pass right after an
        // on-demand load finds a fresh stamp and never unloads a model that
        // has not served its first request yet.
        self.manager.touch();
        Ok(())
    }

    async fn unload(&self) -> Result<(), LlmRuntimeError> {
        self.server.unload().await;
        Ok(())
    }

    fn touch(&self) {
        self.manager.touch();
    }

    fn last_used(&self) -> i64 {
        self.manager.last_used()
    }

    fn in_flight(&self) -> usize {
        self.manager.in_flight()
    }

    async fn residency_rows(&self) -> Vec<LlmResidencyRow> {
        let Some((envelope, _plain)) = self.manager.list_with_fallback().await else {
            return Vec::new(); // server down / never loaded → no rows
        };
        let mut cache = match self.last_rows.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        cache.clear();
        let mut rows = Vec::with_capacity(envelope.instances.len());
        for info in envelope.instances {
            let name = instance_name_from_server_id(&info.id).to_string();
            cache.insert(name, info.clone());
            rows.push(info_to_row(&self.model_key, &info));
        }
        rows
    }

    fn context(&self, name: &str) -> Option<Arc<dyn LlmContext>> {
        let cache = match self.last_rows.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        let info = cache.get(name).cloned()?;
        Some(Arc::new(LlamaContext::from_info(
            Arc::clone(&self.manager),
            self.manager.client().clone(),
            &info,
            self.profile_max_ctx(name),
        )) as Arc<dyn LlmContext>)
    }

    async fn ensure_context(&self, name: &str) -> Result<Arc<dyn LlmContext>, LlmRuntimeError> {
        self.manager
            .ensure_instance(name)
            .await
            .map_err(map_instance_err)?;
        let profile = self
            .manager
            .profiles()
            .iter()
            .find(|p| p.name.as_deref() == Some(name))
            .ok_or_else(|| {
                LlmRuntimeError::NotLoaded(format!("no configured profile for instance '{name}'"))
            })?;
        Ok(Arc::new(LlamaContext::from_profile(
            Arc::clone(&self.manager),
            self.manager.client().clone(),
            profile,
        )) as Arc<dyn LlmContext>)
    }

    fn eviction_policy(&self) -> EvictionPolicy {
        EvictionPolicy::FootprintColdness
    }

    /// Whole-model eviction: every listed context is evicted
    /// (resume-marked ones snapshotted first, via `LlamaContext::evict`) and
    /// then the weights are unloaded. The sidecar's whole-model eviction
    /// order, now behind the engine's whole-weights arm.
    async fn evict_weights(&self) -> Result<(), LlmRuntimeError> {
        if let Some((envelope, _)) = self.manager.list_with_fallback().await {
            for info in envelope.instances {
                let name = instance_name_from_server_id(&info.id);
                let ctx = LlamaContext::from_info(
                    Arc::clone(&self.manager),
                    self.manager.client().clone(),
                    &info,
                    self.profile_max_ctx(name),
                );
                if let Err(e) = ctx.evict().await {
                    tracing::warn!(
                        target: "router.instances",
                        model = %self.model_key,
                        instance = %name,
                        error = %e,
                        "model-eviction context destroy failed",
                    );
                }
            }
        }
        self.server.unload().await;
        Ok(())
    }

    async fn expire_resume(&self) {
        // Snapshot retention (age + byte budget) rides the same engine seam:
        // one "llama expiry pass", two distinct concerns in two methods, so
        // the resume lifecycle below stays untouched.
        self.enforce_snapshot_retention().await;
        let Some(ttl) = self.policy.resume_ttl_s else {
            return;
        };
        let now = i64::try_from(common_core::now_secs()).unwrap_or(i64::MAX);
        let Some((envelope, _)) = self.manager.list_with_fallback().await else {
            return;
        };
        for info in envelope.instances {
            let name = instance_name_from_server_id(&info.id);
            // The resume-TTL path touches only multi-step contexts; one-shot
            // contexts hold nothing to expire (vacuous for the rest).
            if !self.manager.session_for(name) {
                continue;
            }
            if !self.manager.resume_for(name) {
                continue;
            }
            let idle = now.saturating_sub(info.last_used);
            if idle < ttl as i64 {
                continue;
            }
            self.manager.set_resume(name, false);
            let snapshot = resume_snapshot_name(name);
            match self.manager.client().delete_snapshot(name, &snapshot).await {
                Ok(()) => {
                    tracing::info!(
                        target: "router.instances",
                        model = %self.model_key,
                        instance = %name,
                        idle_secs = idle,
                        ttl_secs = ttl,
                        "resume expired - work concluded, snapshot dropped",
                    );
                    crate::audit::emit(
                        "instances",
                        serde_json::json!({
                            "action": "expire_resume",
                            "model": self.model_key,
                            "instance": name,
                            "reason": "idle_ttl",
                        }),
                    );
                }
                Err(e) => tracing::warn!(
                    target: "router.instances",
                    model = %self.model_key,
                    instance = %name,
                    error = %e,
                    "resume snapshot delete on expiry failed",
                ),
            }
        }
    }
}

/// One fork-reported snapshot selected for retention enforcement: the owning
/// instance plus the fork's listing row (name/size/mtime).
struct RetentionCandidate {
    instance: String,
    name: String,
    size: u64,
    /// Fork `mtime` (unix seconds) when numeric and positive; `None` when
    /// unknowable — never a fabricated default.
    mtime: Option<i64>,
}

impl LlamaWeights {
    /// Delete non-resume KV snapshots past the configured age
    /// (`snapshot_ttl_s`) or byte budget (`snapshot_budget_bytes`). Both
    /// `None` → no-op (no limits configured). Driven by the residency engine
    /// through the `expire_resume` seam (the llama expiry pass); never fails
    /// the pass — a down server or failed delete logs and moves on.
    ///
    /// The source of truth is the fork's pool envelope (every on-disk
    /// snapshot, including namespaces whose instance is already gone — a
    /// snapshot outlives its instance fork-side). Resume snapshots
    /// (`<instance>-resume`) are excluded: the resume lifecycle owns them.
    /// Legacy flat files (no owning instance) are excluded: the router never
    /// creates them, the fork drops them on save, and there is no safe
    /// instance handle to delete one through. Entries whose age is unknowable
    /// (missing or non-numeric `mtime`) are kept by the age sweep and ordered
    /// newest by the budget sweep — deletion never fires on a fabricated age.
    async fn enforce_snapshot_retention(&self) {
        let ttl = self.policy.snapshot_ttl_s;
        let budget = self.policy.snapshot_budget_bytes;
        if ttl.is_none() && budget.is_none() {
            return;
        }
        let Some((envelope, _)) = self.manager.list_with_fallback().await else {
            return; // server down / never loaded — fail open, retried next pass
        };
        let now = i64::try_from(common_core::now_secs()).unwrap_or(i64::MAX);
        let mut candidates: Vec<RetentionCandidate> = Vec::new();
        for snap in &envelope.snapshots {
            let Some(instance) = snap.instance.as_deref() else {
                continue; // legacy flat file — see doc above
            };
            if snap.name == resume_snapshot_name(instance) {
                continue; // resume lifecycle owns it
            }
            candidates.push(RetentionCandidate {
                instance: instance.to_string(),
                name: snap.name.clone(),
                size: snap.size,
                mtime: snap.mtime.as_i64().filter(|&t| t > 0),
            });
        }
        // Age sweep first (mirrors the cold-tier `age >= ttl` evicts
        // boundary verbatim — unifying it would be a behavior change).
        let mut survivors: Vec<RetentionCandidate> = Vec::new();
        for cand in candidates {
            let expired = match (ttl, cand.mtime) {
                (Some(ttl), Some(mtime)) => now.saturating_sub(mtime) as u64 >= ttl,
                _ => false, // no TTL configured, or age unknowable — keep
            };
            if expired {
                self.delete_retained_snapshot(&cand, "age_ttl").await;
            } else {
                survivors.push(cand);
            }
        }
        // Budget sweep: oldest-first until under budget (unknown mtime sorts
        // newest — kept unless everything dated is already gone).
        if let Some(budget) = budget {
            let mut used: u64 = survivors.iter().map(|c| c.size).sum();
            survivors.sort_by(|a, b| match (a.mtime, b.mtime) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            });
            for cand in survivors {
                if used <= budget {
                    break;
                }
                self.delete_retained_snapshot(&cand, "byte_budget").await;
                used = used.saturating_sub(cand.size);
            }
        }
    }

    /// Delete one retention-expired snapshot from the fork (best-effort) and
    /// audit the outcome. Never fails — a failed delete is retried next pass.
    async fn delete_retained_snapshot(&self, cand: &RetentionCandidate, reason: &str) {
        match self
            .manager
            .client()
            .delete_snapshot(&cand.instance, &cand.name)
            .await
        {
            Ok(()) => {
                tracing::info!(
                    target: "router.instances",
                    model = %self.model_key,
                    instance = %cand.instance,
                    snapshot = %cand.name,
                    reason = %reason,
                    "kv snapshot deleted by retention sweep",
                );
                crate::audit::emit(
                    "instances",
                    serde_json::json!({
                        "action": "snapshot_retention",
                        "model": self.model_key,
                        "instance": cand.instance,
                        "snapshot": cand.name,
                        "reason": reason,
                    }),
                );
            }
            Err(e) => tracing::warn!(
                target: "router.instances",
                model = %self.model_key,
                instance = %cand.instance,
                snapshot = %cand.name,
                error = %e,
                "retention snapshot delete failed - retried next pass",
            ),
        }
    }
}

/// One `LlamaWeights` adapter per managed model with a live supervisor
/// server. The single construction site — the fleet build and the pool's
/// engine admission share it, so the engine always sees the same adapters.
pub(crate) fn llama_weights_for_pool(
    pool: &InstancePool,
    supervisor: &LlamaServerSupervisor,
    policy: &SidecarConfig,
) -> Vec<Arc<dyn LlmWeights>> {
    let bin = supervisor.bin().to_path_buf();
    let mut weights: Vec<Arc<dyn LlmWeights>> = Vec::new();
    for manager in pool.managers_iter() {
        if let Some(server) = supervisor.server_for(manager.model_key()) {
            weights.push(Arc::new(LlamaWeights::new(
                manager.clone(),
                server,
                bin.clone(),
                policy.clone(),
            )));
        }
    }
    weights
}
