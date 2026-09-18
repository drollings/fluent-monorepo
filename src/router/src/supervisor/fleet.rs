//! The supervisor process: one managed server per managed model.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use common_core::registry::ConcurrentRegistry;

use crate::config::RouterConfig;

use super::adopt;
use super::spec::{
    build_shared_client, free_port, resolve_llama_server, LlamaServerSpec, LLAMA_SERVER_ENV,
};
use super::ManagedServer;

/// The supervisor: one [`ManagedServer`] per managed model.
pub struct LlamaServerSupervisor {
    bin: PathBuf,
    /// Widened to `pub(crate)` so `supervisor::adopt` (a sibling module of
    /// this one, no longer a descendant of the struct's defining module)
    /// keeps its pre-split access to the registry.
    pub(crate) servers: ConcurrentRegistry<String, ManagedServer>,
    /// Per-boot adoption records (see `adopt.rs`). Keyed by model key;
    /// verified against the entry's live adopted pid on read (a dead orphan
    /// replaced by a fresh child self-heals to `None`).
    /// Widened to `pub(crate)` for the same reason as `servers`.
    pub(crate) adoptions: std::sync::Mutex<std::collections::HashMap<String, adopt::AdoptionInfo>>,
    http_client: Arc<reqwest::Client>,
}

impl LlamaServerSupervisor {
    /// Look up a managed server by key. A key gathered from `servers.keys()`
    /// can vanish under a concurrent eviction, so the miss is a typed boot
    /// error the caller propagates — never a panic on the serving path.
    pub(crate) fn live_server(&self, key: &str) -> Result<Arc<ManagedServer>, String> {
        self.servers
            .get(&key.to_string())
            .ok_or_else(|| format!("model '{key}' evicted during boot"))
    }

    /// Resolve the binary and build one server per managed model (`weights` /
    /// `hf_repo` / `instances` declared), each on its own free localhost port.
    /// Fails fast when `llama-server` is not found on `$PATH`.
    pub fn build(config: &RouterConfig) -> Result<Self, String> {
        let bin = resolve_llama_server().ok_or_else(|| {
            format!(
                "llama-server binary not found in $PATH (set the {LLAMA_SERVER_ENV} env var or install it)"
            )
        })?;
        let api_key = config
            .sidecar
            .api_key_env
            .as_deref()
            .map(std::env::var)
            .and_then(Result::ok)
            .filter(|k| !k.is_empty());
        // `--slot-save-path` must name an existing directory: llama-server
        // rejects a nonexistent path at argv-parse time and exits, which would
        // crash-loop every managed server (and with it the whole boot). Resolve
        // the directory once here; if it cannot be created (e.g. an unwritable
        // mount), snapshots are disabled for every managed server — the flag is
        // omitted entirely rather than handed to a process that will die on it.
        let slot_save_path = match &config.sidecar.slot_save_path {
            Some(dir) => match fluent_wvr::capability::capability_aware_fs::create_dir_all(dir) {
                Ok(()) => Some(dir.clone()),
                Err(e) => {
                    tracing::warn!(
                        target: "router.supervisor",
                        slot_save_path = %dir,
                        error = %e,
                        "slot-save-path create failed (snapshots disabled)",
                    );
                    None
                }
            },
            None => None,
        };
        let servers = ConcurrentRegistry::new();
        let mut keys: Vec<&String> = config.models.keys().collect();
        keys.sort();
        let liveness_poll = Duration::from_secs(config.sidecar.liveness_poll_interval_s);
        let liveness_failures = config.sidecar.liveness_failures_before_restart;
        for key in keys {
            let entry = &config.models[key];
            // Onnx models are router-managed but NOT llama-managed: the ort
            // registry serves them, so the
            // supervisor must never spawn a llama-server for one.
            if !entry.is_managed() {
                continue;
            }
            let port = free_port().ok_or_else(|| "no free localhost port".to_string())?;
            let spec = LlamaServerSpec::from_entry(
                key,
                entry,
                port,
                slot_save_path.clone(),
                api_key.clone(),
                config.spawn_defaults_for(key),
            );
            servers.insert(
                key.clone(),
                ManagedServer::with_liveness(
                    spec,
                    liveness_poll,
                    liveness_failures,
                    config.sidecar.max_restarts,
                ),
            );
        }
        tracing::info!(
            target: "router.supervisor",
            binary = %bin.display(),
            server_count = servers.len(),
            "llama-server supervisor built",
        );
        Ok(Self {
            bin,
            servers,
            adoptions: std::sync::Mutex::new(std::collections::HashMap::new()),
            http_client: Arc::new(build_shared_client()),
        })
    }

    /// Register a non-running test server for `model_key` (no process is
    /// spawned; management traffic goes wherever the caller's client points).
    /// Test-only: lets hermetic tests drive adapter paths that need a
    /// `ManagedServer` handle without a real `llama-server` binary.
    #[cfg(test)]
    pub fn register_test_server(&self, model_key: &str) {
        let spec = LlamaServerSpec {
            model_key: model_key.to_string(),
            name: model_key.to_string(),
            weights: None,
            hf_repo: None,
            hf_file: None,
            template: None,
            port: 1,
            instances: Vec::new(),
            boot: false,
            slot_save_path: None,
            api_key: None,
            instance_wait_s: None,
            defaults: crate::config::RoleParams::default(),
            extra_args: Vec::new(),
        };
        self.servers.insert(
            model_key.to_string(),
            ManagedServer::with_liveness(spec, std::time::Duration::from_secs(30), 3, 5),
        );
    }

    #[cfg(test)]
    pub fn with_client_for_test(bin: PathBuf, client: reqwest::Client) -> Self {
        Self {
            bin,
            servers: ConcurrentRegistry::new(),
            adoptions: std::sync::Mutex::new(std::collections::HashMap::new()),
            http_client: Arc::new(client),
        }
    }

    /// The resolved binary path.
    pub fn bin(&self) -> &Path {
        &self.bin
    }

    /// The server for a model id.
    pub fn server_for(&self, model_key: &str) -> Option<Arc<ManagedServer>> {
        self.servers.get(&model_key.to_string())
    }

    /// The management base URL for a model id.
    pub fn base_url_for(&self, model_key: &str) -> Option<String> {
        self.server_for(model_key).map(|s| s.base_url().to_string())
    }

    /// Every managed model key.
    pub fn model_keys(&self) -> Vec<String> {
        self.servers.keys()
    }

    /// Spawn every **boot** managed server and wait for each to become healthy.
    /// Only models declaring at least one pinned instance are booted; the rest
    /// (lazy models) are loaded on demand via [`Self::ensure_running`]. Spawns
    /// happen first (processes load weights concurrently), then health is
    /// awaited in parallel, so boot time is bounded by the slowest boot model
    /// rather than the sum. Returns the first health failure so boot aborts
    /// loudly (a boot model that cannot load its weights must not be silently
    /// skipped).
    ///
    /// Entries adopted by [`Self::adopt_orphans`] (run before this) skip the
    /// spawn — the live process is already on the entry's port — but still get
    /// a supervision task and a health gate, so a dead-on-arrival orphan fails
    /// boot loudly instead of lingering.
    pub async fn start_all(self: &Arc<Self>) -> Result<(), String> {
        let bin = self.bin.clone();
        let mut keys: Vec<String> = self.servers.keys();
        keys.sort();
        let mut boot_keys = Vec::new();
        for key in &keys {
            let server = self.live_server(key)?;
            if !server.spec.boot {
                tracing::info!(
                    target: "router.supervisor",
                    model = %key,
                    "model deferred - no pinned instance, loaded on demand",
                );
                continue;
            }
            boot_keys.push(key.clone());
            if server.is_adopted() {
                // The orphan is already listening on the entry's port: no
                // spawn, just supervision + the health gate below.
                server.inner.running.store(true, Ordering::Relaxed);
            } else {
                server.spawn_child(&bin);
                server.inner.running.store(true, Ordering::Relaxed);
            }
            let client = Arc::clone(&self.http_client);
            let handle = {
                let me = Arc::clone(&server);
                let bin = bin.clone();
                server.runtime.spawn(Box::pin(async move {
                    me.supervise(bin, client).await;
                }))
            };
            let supervisor_guard = server.inner.supervisor.lock();
            if let Ok(mut guard) = supervisor_guard {
                *guard = Some(handle.abort_handle());
            }
        }
        // Await every boot server's /health concurrently; the first failure
        // aborts (lazy models are not health-checked at boot).
        let client = Arc::clone(&self.http_client);
        let mut health = Vec::new();
        for key in &boot_keys {
            let server = self.live_server(key)?;
            let client = Arc::clone(&client);
            health
                .push(async move { (key.clone(), server.wait_healthy_with_client(&client).await) });
        }
        for fut in health {
            let (key, result) = fut.await;
            if let Err(e) = result {
                return Err(format!("model '{key}': {e}"));
            }
        }
        Ok(())
    }

    /// Load a model on demand: spawn its `llama-server` (if not already
    /// running) and wait for `/health`. Returns `None` when the model is not
    /// managed. Used by the dispatch path before targeting a lazy model.
    pub async fn ensure_running(&self, model_key: &str) -> Result<(), String> {
        let server = self
            .servers
            .get(&model_key.to_string())
            .ok_or_else(|| format!("model '{model_key}' is not managed by the supervisor"))?;
        server
            .ensure_running_with_client(&self.bin, &self.http_client)
            .await
    }

    /// Whether a managed model's server is currently running. `None` when the
    /// model is not managed.
    pub fn is_running(&self, model_key: &str) -> Option<bool> {
        self.servers
            .get(&model_key.to_string())
            .map(|s| s.is_running())
    }

    /// Whether a managed model's entry currently drives an adopted (non-child)
    /// process. `false` for spawned children and unknown models.
    pub fn is_adopted(&self, model_key: &str) -> bool {
        self.servers
            .get(&model_key.to_string())
            .is_some_and(|s| s.is_adopted())
    }

    /// Unload a model's server on demand (frees its VRAM). The spec stays
    /// registered so [`Self::ensure_running`] can re-load it later.
    pub async fn unload(&self, model_key: &str) {
        if let Some(server) = self.servers.get(&model_key.to_string()) {
            server.unload().await;
        }
    }

    /// Stop every managed server (used on shutdown).
    pub async fn shutdown(&self) {
        let mut keys: Vec<String> = self.servers.keys();
        keys.sort();
        for key in &keys {
            if let Some(server) = self.servers.get(key) {
                server.stop().await;
            }
        }
    }
}

impl Drop for LlamaServerSupervisor {
    fn drop(&mut self) {
        // Best-effort process cleanup when the supervisor is dropped outside an
        // async context (each child has kill_on_drop(true), so dropping the
        // supervision task's child handle kills the process).
        for key in self.servers.keys() {
            if let Some(server) = self.servers.get(&key) {
                server.inner.stopping.store(true, Ordering::Relaxed);
                if let Some(handle) = server
                    .inner
                    .supervisor
                    .lock()
                    .ok()
                    .and_then(|mut g| g.take())
                {
                    handle.abort();
                }
            }
        }
    }
}
