//! Managed `llama-server` process supervision.
//!
//! Coral Router is the process owner of one `llama-server` per model weights
//! file (the llama.cpp router mode is never used). The [`LlamaServerSupervisor`]
//! finds `llama-server` on `$PATH`, spawns one process per managed model on a
//! free localhost port, waits for `/health`, and supervises each child
//! (logging its output, restarting it with backoff if it dies unexpectedly).
//!
//! The spawned servers bind to `127.0.0.1` only and are never exposed directly;
//! every generation and management call goes through Coral Router.

use std::path::{Path, PathBuf};

use std::process::Stdio;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use std::sync::{Arc, Mutex};

use std::time::Duration;

use common_core::retry::{PollResult, PollWithBackoff};

use tokio::io::{AsyncBufReadExt, BufReader};

use tokio::process::{Child, Command};

pub mod adopt;

pub mod health;

mod fleet;

mod spec;

use health::HealthProbe;

pub use self::fleet::LlamaServerSupervisor;

pub use self::spec::{
    build_server_args, fleet_lib_dir, free_port, prepend_library_path, resolve_llama_server,
    LlamaServerSpec, LLAMA_SERVER_BIN, LLAMA_SERVER_ENV,
};

pub(crate) use self::spec::{build_shared_client, restart_backoff, HEALTH_POLL, HEALTH_TIMEOUT};

/// One supervised server: the spawn spec, its localhost address, and the shared
/// process state guarded for the supervision task.
pub struct ManagedServer {
    spec: LlamaServerSpec,
    base_url: String,
    inner: Arc<ServerInner>,
    /// Injected async runtime for the supervision task and the child
    /// stdio log pumps (no ambient `tokio::spawn` — spawned work stays
    /// visible to the owner's shutdown and capability accounting).
    /// Abort/stop/unload semantics are unchanged: the supervision handle's
    /// `AbortHandle` accounting is identical, only the spawner differs.
    runtime: Arc<dyn fluent_wvr::Runtime>,
}

struct ServerInner {
    /// The live child while one is running (moved into the supervision task
    /// while it awaits `wait`).
    child: Mutex<Option<Child>>,
    /// OS pid of the most recently spawned child (for the persisted fleet
    /// map). Stale once the child exits; load-time reuse is guarded by
    /// `adopt::pid_still_ours`.
    child_pid: Mutex<Option<u32>>,
    /// OS pid of an adopted (non-child) server this entry manages. Set by
    /// `mark_adopted`, retained across `unload` (the orphan survives and is
    /// re-driven via HTTP only — the router can never `wait()` on or signal
    /// a process it did not spawn). Cleared by `stop` and when the watchdog
    /// replaces a dead orphan with a fresh child.
    adopted_pid: Mutex<Option<u32>>,
    /// Set by `stop()` so the supervision task never restarts.
    stopping: AtomicBool,
    /// Whether a child is currently expected to be alive (spawned, or being
    /// supervised). Cleared on stop and on unload; re-set by ensure_running.
    running: AtomicBool,
    /// Serializes spawn on the on-demand path so concurrent dispatches cannot
    /// double-spawn a lazy model.
    spawn_lock: tokio::sync::Mutex<()>,
    /// Abort handle for the supervision task.
    supervisor: Mutex<Option<tokio::task::AbortHandle>>,
    /// Consecutive failures since the server last answered `/health` (spawn
    /// failures and boot-time child exits alike). Drives restart backoff and
    /// the restart limit. Reset to 0 only on a successful `/health` probe or
    /// `wait_healthy` — never on spawn, so a process that starts but dies
    /// before becoming healthy keeps escalating.
    spawn_failures: AtomicU32,
    /// Consecutive-failure cap: past this many crashes the supervision task
    /// stops restarting and marks the server `failed` (containment). `0`
    /// disables the limit (unbounded restart with rising backoff).
    max_restarts: u32,
    /// Set when `spawn_failures` reaches `max_restarts`: the server is
    /// contained. `ensure_running`/`start` fail fast with a terminal error and
    /// the supervision task has stopped. Cleared by `stop`/`unload` (a later
    /// load attempt starts from a fresh budget) and by `wait_healthy` success.
    failed: AtomicBool,
    /// How often the supervision task probes a running server's `/health`.
    liveness_poll: Duration,
    /// Consecutive failed `/health` probes before a hung server is killed and
    /// restarted.
    liveness_failures_before_restart: u32,
}

impl ManagedServer {
    fn with_liveness(
        spec: LlamaServerSpec,
        liveness_poll: Duration,
        liveness_failures_before_restart: u32,
        max_restarts: u32,
    ) -> Self {
        let base_url = format!("http://127.0.0.1:{}", spec.port);
        Self {
            spec,
            base_url,
            runtime: fluent_concurrency::tokio_runtime(),
            inner: Arc::new(ServerInner {
                child: Mutex::new(None),
                child_pid: Mutex::new(None),
                adopted_pid: Mutex::new(None),
                stopping: AtomicBool::new(false),
                running: AtomicBool::new(false),
                spawn_lock: tokio::sync::Mutex::new(()),
                supervisor: Mutex::new(None),
                spawn_failures: AtomicU32::new(0),
                max_restarts,
                failed: AtomicBool::new(false),
                liveness_poll,
                liveness_failures_before_restart,
            }),
        }
    }

    /// Route supervision/log-pump tasks through `runtime` (tests substitute
    /// a recording runtime to prove the routing).
    #[must_use]
    pub fn with_runtime(mut self, runtime: Arc<dyn fluent_wvr::Runtime>) -> Self {
        self.runtime = runtime;
        self
    }

    pub fn spec(&self) -> &LlamaServerSpec {
        &self.spec
    }

    /// The server's management base URL (`http://127.0.0.1:<port>`).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Whether the process was intentionally stopped.
    pub fn stopping(&self) -> bool {
        self.inner.stopping.load(Ordering::Relaxed)
    }

    /// Whether a spawned child is currently expected to be alive. `false` for
    /// a lazy model that has never been loaded (or was unloaded for VRAM).
    /// Adopted orphans report `true` once marked (see `mark_adopted`).
    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::Relaxed)
    }

    /// Take over a live non-child `llama-server` on `spec.port` (adopt-before-
    /// spawn). Called only pre-`start_all`, when no supervision task or child
    /// exists for the entry. Marks the entry running: health and management
    /// go through HTTP exactly as for a spawned server.
    pub fn mark_adopted(&self, pid: u32) {
        if let Ok(mut guard) = self.inner.adopted_pid.lock() {
            *guard = Some(pid);
        }
        self.inner.running.store(true, Ordering::Relaxed);
    }

    /// The adopted non-child pid, if this entry manages an orphan.
    pub fn adopted_pid(&self) -> Option<u32> {
        self.inner.adopted_pid.lock().ok().and_then(|g| *g)
    }

    /// Whether this entry manages an adopted (non-child) process.
    pub fn is_adopted(&self) -> bool {
        self.adopted_pid().is_some()
    }

    /// Forget an adoption (the orphan died and a fresh child takes over, or
    /// the entry is stopped). The process itself is never signalled — the
    /// router owns no handle on it.
    pub(super) fn clear_adoption(&self) {
        if let Ok(mut guard) = self.inner.adopted_pid.lock() {
            *guard = None;
        }
    }

    /// OS pid of the most recently spawned child, if any.
    pub fn child_pid(&self) -> Option<u32> {
        self.inner.child_pid.lock().ok().and_then(|g| *g)
    }

    /// The liveness poll interval (adopted-watchdog parity with spawned).
    pub fn liveness_poll(&self) -> Duration {
        self.inner.liveness_poll
    }

    /// Consecutive failed probes before a hung server is restarted.
    pub fn liveness_failures_before_restart(&self) -> u32 {
        self.inner.liveness_failures_before_restart
    }

    /// The consecutive-crash containment budget.
    pub fn max_restarts(&self) -> u32 {
        self.inner.max_restarts
    }

    /// Spawn the child and wait for it to answer `/health`. Called at boot for
    /// boot models; the supervision task handles later exits and restarts.
    pub async fn start(self: &Arc<Self>, bin: &Path) -> Result<(), String> {
        if self.contained() {
            return Err(self.terminal_failure());
        }
        self.spawn_child(bin);
        self.wait_healthy().await
    }

    /// Bring the server up on demand: spawn (if not already running) and wait
    /// for `/health`. Idempotent and safe under concurrent dispatch — the
    /// spawn lock prevents a lazy model from being double-spawned. Called by
    /// the router when a dispatch targets a managed model that is not loaded.
    pub async fn ensure_running(self: &Arc<Self>, bin: &Path) -> Result<(), String> {
        self.ensure_running_with_client(bin, &build_shared_client())
            .await
    }

    pub async fn ensure_running_with_client(
        self: &Arc<Self>,
        bin: &Path,
        client: &reqwest::Client,
    ) -> Result<(), String> {
        if self.contained() {
            return Err(self.terminal_failure());
        }
        if self.inner.running.load(Ordering::Relaxed) {
            return Ok(());
        }
        let _guard = self.inner.spawn_lock.lock().await;
        // Re-check under the lock: a concurrent caller may have spawned.
        if self.inner.running.load(Ordering::Relaxed) {
            return Ok(());
        }
        if self.inner.stopping.load(Ordering::Relaxed) {
            return Err(format!("model '{}' is stopped", self.spec.model_key));
        }
        if self.adopted_pid().is_some() {
            // An adopted orphan survives `unload` (the router owns no handle
            // on it, so there is nothing to respawn): re-driving it is just
            // health + supervision again on the same port.
            self.inner.running.store(true, Ordering::Relaxed);
            tracing::info!(
                target: "router.supervisor",
                model = %self.spec.model_key,
                base_url = %self.base_url,
                "adopted llama-server re-driven on demand",
            );
        } else {
            self.spawn_child(bin);
            self.inner.running.store(true, Ordering::Relaxed);
            tracing::info!(
                target: "router.supervisor",
                model = %self.spec.model_key,
                base_url = %self.base_url,
                "llama-server loaded on demand",
            );
        }
        // Start the supervision task if none is running (e.g. after an unload).
        {
            let mut guard = match self.inner.supervisor.lock() {
                Ok(g) => g,
                Err(e) => e.into_inner(),
            };
            if guard.is_none() {
                let me = Arc::clone(self);
                let bin = bin.to_path_buf();
                let client = Arc::new(client.clone());
                let handle = self.runtime.spawn(Box::pin(async move {
                    me.supervise(bin, client).await;
                }));
                *guard = Some(handle.abort_handle());
            }
        }
        self.wait_healthy_with_client(client).await
    }

    /// Unload the server (on-demand teardown): kill the child and stop the
    /// supervision task so it does not restart. The spec stays registered, so
    /// a later [`Self::ensure_running`] re-spawns the model. Used by the
    /// sidecar when a model's weights must be freed for VRAM. Also clears any
    /// containment state: a later load attempt starts from a fresh (bounded)
    /// crash budget, so an operator's fix to the weights/config takes effect
    /// on the next on-demand dispatch.
    ///
    /// An adopted orphan has no child to kill (and the router must never
    /// signal a process it did not spawn): `unload` only marks it not-running
    /// and stops supervision. Its KV/instances are freed fork-side by the
    /// pool's destroy calls (delete-last unloads the weights); the next
    /// `ensure_running` re-drives the same live process with no respawn.
    pub async fn unload(self: &Arc<Self>) {
        self.inner.failed.store(false, Ordering::Relaxed);
        self.inner.spawn_failures.store(0, Ordering::Relaxed);
        self.inner.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.inner.supervisor.lock().ok().and_then(|mut g| g.take()) {
            handle.abort();
        }
        if self.is_adopted() {
            tracing::info!(
                target: "router.supervisor",
                model = %self.spec.model_key,
                "adopted llama-server released (process survives for re-adoption)",
            );
            return;
        }
        let child = {
            let mut guard = match self.inner.child.lock() {
                Ok(g) => g,
                Err(e) => e.into_inner(),
            };
            guard.take()
        };
        if let Some(mut child) = child {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        tracing::info!(
            target: "router.supervisor",
            model = %self.spec.model_key,
            "llama-server unloaded (on-demand eviction)",
        );
    }

    /// Wait for `/health` to answer 2xx, up to `HEALTH_TIMEOUT`.
    pub async fn wait_healthy(&self) -> Result<(), String> {
        self.wait_healthy_with_client(&build_shared_client()).await
    }

    pub async fn wait_healthy_with_client(&self, client: &reqwest::Client) -> Result<(), String> {
        let probe = HealthProbe::for_wait(client.clone(), self.base_url.clone());
        // Use HealthProbe's poll but log on success
        let result = probe
            .poll
            .run(|| async {
                let healthy = probe.probe_once().await;
                if healthy {
                    tracing::info!(
                        target: "router.supervisor",
                        model = %self.spec.model_key,
                        base_url = %self.base_url,
                        "llama-server healthy",
                    );
                }
                healthy
            })
            .await;
        match result {
            PollResult::Ready => {
                self.inner.spawn_failures.store(0, Ordering::Relaxed);
                self.inner.failed.store(false, Ordering::Relaxed);
                Ok(())
            }
            PollResult::Exhausted { .. } => Err(format!(
                "llama-server for model '{}' did not become healthy on {} within {}s",
                self.spec.model_key,
                self.base_url,
                HEALTH_TIMEOUT.as_secs(),
            )),
        }
    }

    /// Whether the server is contained: `spawn_failures` has reached
    /// `max_restarts` (or a limit is configured and already met).
    fn contained(&self) -> bool {
        let limit = self.inner.max_restarts;
        limit > 0 && self.inner.spawn_failures.load(Ordering::Relaxed) >= limit
    }

    /// Mark the server contained: `failed` (load paths fail fast) and no
    /// longer `running`. Called by the supervision task exactly once when the
    /// crash budget is exhausted.
    fn mark_contained(&self) {
        self.inner.failed.store(true, Ordering::Relaxed);
        self.inner.running.store(false, Ordering::Relaxed);
    }

    /// The terminal error a contained server returns from every load path,
    /// naming the model and what the operator should do. This is the single
    /// containment message — never duplicated at call sites.
    fn terminal_failure(&self) -> String {
        format!(
            "model '{}' failed to start after {} consecutive crash-restarts (supervisor containment); \
             fix its weights/endpoint config or restart the router",
            self.spec.model_key,
            self.inner.spawn_failures.load(Ordering::Relaxed),
        )
    }

    #[allow(dead_code)]
    async fn probe_health(&self) -> bool {
        self.probe_health_with_client(&build_shared_client()).await
    }

    async fn probe_health_with_client(&self, client: &reqwest::Client) -> bool {
        HealthProbe::new(
            client.clone(),
            self.base_url.clone(),
            PollWithBackoff::new(HEALTH_POLL, 1),
            self.inner.liveness_failures_before_restart,
            HEALTH_POLL,
        )
        .probe_once()
        .await
    }

    /// Spawn the child process (no health wait). Failure is logged and counted
    /// so the supervision loop backs off and retries.
    ///
    /// `Command::new(bin)` here is the **granting** boundary, not a gated
    /// effect: Coral Router is the process owner of the local inference fleet
    /// by design, and `spawn_child` is the single process-spawn point in the
    /// supervisor. It is intentionally authorized to launch `llama-server`
    /// rather than token-gated.
    ///
    /// The child's `LD_LIBRARY_PATH` is pinned to the binary's own directory
    /// (see [`fleet_lib_dir`]): the fork ships its `libllama-*.so` beside the
    /// binary, and without the pin the loader resolves whatever stale system
    /// copy sits in `/usr/local/lib` — silently running old fork code after a
    /// rebuild.
    fn spawn_child(self: &Arc<Self>, bin: &Path) {
        if self.inner.stopping.load(Ordering::Relaxed) {
            return;
        }
        let args = build_server_args(&self.spec);
        let mut cmd = Command::new(bin);
        if let Some(lib_dir) = fleet_lib_dir(bin) {
            cmd.env(
                "LD_LIBRARY_PATH",
                prepend_library_path(&lib_dir, std::env::var_os("LD_LIBRARY_PATH")),
            );
        }
        match cmd
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(mut child) => {
                let pid = child.id();
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                {
                    let mut guard = match self.inner.child.lock() {
                        Ok(g) => g,
                        Err(e) => e.into_inner(),
                    };
                    *guard = Some(child);
                }
                if let Ok(mut guard) = self.inner.child_pid.lock() {
                    *guard = pid;
                }
                // NOTE: `spawn_failures` is deliberately NOT reset here. An
                // OS-level spawn that produces a process which dies before
                // answering `/health` is a failure, not a success; the count
                // only resets on a successful health check (see
                // `wait_healthy`/`supervise`). Resetting here caused the
                // crash-loop bug: every restart looked like the first.
                // Log the server's output (best-effort, detached).
                if let Some(out) = stdout {
                    let tag = self.spec.model_key.clone();
                    self.runtime.spawn(Box::pin(async move {
                        let mut reader = BufReader::new(out);
                        let mut line = String::new();
                        while reader.read_line(&mut line).await.is_ok() && !line.is_empty() {
                            tracing::debug!(
                                target: "router.supervisor.child",
                                model = %tag,
                                line = line.trim_end(),
                                "server stdout",
                            );
                            line.clear();
                        }
                    }));
                }
                if let Some(err) = stderr {
                    let tag = self.spec.model_key.clone();
                    self.runtime.spawn(Box::pin(async move {
                        let mut reader = BufReader::new(err);
                        let mut line = String::new();
                        while reader.read_line(&mut line).await.is_ok() && !line.is_empty() {
                            tracing::info!(
                                target: "router.supervisor.child",
                                model = %tag,
                                line = line.trim_end(),
                                "server stderr",
                            );
                            line.clear();
                        }
                    }));
                }
                tracing::info!(
                    target: "router.supervisor",
                    model = %self.spec.model_key,
                    base_url = %self.base_url,
                    args = ?args,
                    "llama-server spawned",
                );
            }
            Err(e) => {
                let failures = self.inner.spawn_failures.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::error!(
                    target: "router.supervisor",
                    model = %self.spec.model_key,
                    error = %e,
                    failures = failures,
                    "llama-server spawn failed",
                );
            }
        }
    }

    /// Watch an adopted orphan by health probe (it has no child handle to
    /// `wait()` on). Returns when the entry is stopping or the orphan has
    /// failed `liveness_failures_before_restart` consecutive probes — in the
    /// latter case the adoption is cleared so the caller falls through to a
    /// fresh spawn on the same port. A healthy probe resets the crash budget,
    /// exactly like the spawned liveness path.
    async fn watch_adopted(
        self: &Arc<Self>,
        client: &reqwest::Client,
        liveness_poll: Duration,
        liveness_threshold: u32,
    ) {
        let mut failures: u32 = 0;
        loop {
            if self.inner.stopping.load(Ordering::Relaxed) {
                return;
            }
            if self.adopted_pid().is_none() {
                return;
            }
            tokio::time::sleep(liveness_poll).await;
            if self.probe_health_with_client(client).await {
                failures = 0;
                self.inner.spawn_failures.store(0, Ordering::Relaxed);
            } else {
                failures += 1;
                tracing::warn!(
                    target: "router.supervisor",
                    model = %self.spec.model_key,
                    consecutive_failures = failures,
                    threshold = liveness_threshold,
                    "adopted llama-server /health probe failed",
                );
                if failures >= liveness_threshold {
                    break;
                }
            }
        }
        tracing::error!(
            target: "router.supervisor",
            model = %self.spec.model_key,
            base_url = %self.base_url,
            failures = failures,
            "adopted llama-server hung (stopped answering /health) - spawning a fresh child on the same port",
        );
        let failures = self.inner.spawn_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if self.contained() {
            self.mark_contained();
            tracing::error!(
                target: "router.supervisor",
                model = %self.spec.model_key,
                base_url = %self.base_url,
                failures = failures,
                limit = self.inner.max_restarts,
                "adopted llama-server failed too many times - giving up (contained); fix the model's weights/config or restart the router",
            );
            return;
        }
        self.clear_adoption();
    }

    /// The supervision loop: watch the child for unexpected exit AND for
    /// post-boot liveness. A server that stays alive but stops answering
    /// `/health` for `liveness_failures_before_restart` consecutive probes is
    /// killed so the exit-restart path (with backoff) takes over. Runs as a
    /// spawned task for the life of the server. Guarded by `stopping` so a
    /// shutdown never triggers a restart.
    async fn supervise(self: Arc<Self>, bin: PathBuf, client: Arc<reqwest::Client>) {
        let liveness_poll = self.inner.liveness_poll;
        let liveness_threshold = self.inner.liveness_failures_before_restart;
        // An adopted orphan has no child handle: watch it by probe until it
        // dies (adoption cleared, fresh spawn below takes over) or we stop.
        // A contained orphan stays contained — `watch_adopted` returns without
        // clearing, and the loop below finds no child and exits via the
        // containment branch.
        if self.adopted_pid().is_some() {
            self.watch_adopted(&client, liveness_poll, liveness_threshold)
                .await;
            if self.inner.stopping.load(Ordering::Relaxed) {
                return;
            }
        }
        loop {
            let child = {
                let mut guard = match self.inner.child.lock() {
                    Ok(g) => g,
                    Err(e) => e.into_inner(),
                };
                guard.take()
            };
            let Some(mut child) = child else {
                // No child to watch (spawn failed); retry with backoff unless
                // the crash budget is exhausted.
                if self.inner.stopping.load(Ordering::Relaxed) {
                    return;
                }
                let failures = self.inner.spawn_failures.load(Ordering::Relaxed);
                if self.contained() {
                    self.mark_contained();
                    tracing::error!(
                        target: "router.supervisor",
                        model = %self.spec.model_key,
                        failures = failures,
                        limit = self.inner.max_restarts,
                        "llama-server spawn failed too many times - giving up (contained)",
                    );
                    return;
                }
                tokio::time::sleep(restart_backoff(failures.max(1))).await;
                self.spawn_child(&bin);
                continue;
            };

            // Wait for the child to exit naturally OR to trip the liveness
            // threshold. `child.wait()` borrows `child` mutably only in its own
            // select arm; the liveness arm probes via the shared client and
            // never touches `child`, so the two never conflict.
            let mut liveness_failures: u32 = 0;
            let mut liveness_kill = false;
            let mut exit_status: Option<std::io::Result<std::process::ExitStatus>> = None;
            loop {
                let signal = tokio::select! {
                    status = child.wait() => Some(status),
                    () = tokio::time::sleep(liveness_poll) => None,
                };
                if let Some(status) = signal {
                    exit_status = Some(status);
                    break;
                }
                if self.inner.stopping.load(Ordering::Relaxed) {
                    break;
                }
                if self.probe_health_with_client(&client).await {
                    liveness_failures = 0;
                    // The server answered `/health`: it earned a fresh crash
                    // budget. The reset must only happen here (and in
                    // `wait_healthy`) — a process that dies before becoming
                    // healthy keeps its rising failure count.
                    self.inner.spawn_failures.store(0, Ordering::Relaxed);
                } else {
                    liveness_failures += 1;
                    tracing::warn!(
                        target: "router.supervisor",
                        model = %self.spec.model_key,
                        consecutive_failures = liveness_failures,
                        threshold = liveness_threshold,
                        "llama-server /health probe failed",
                    );
                    if liveness_failures >= liveness_threshold {
                        liveness_kill = true;
                        break;
                    }
                }
            }

            if liveness_kill {
                tracing::error!(
                    target: "router.supervisor",
                    model = %self.spec.model_key,
                    base_url = %self.base_url,
                    failures = liveness_failures,
                    "llama-server hung (stopped answering /health) - killing so the restart path takes over",
                );
                let _ = child.kill().await;
                let _ = child.wait().await;
            }

            if self.inner.stopping.load(Ordering::Relaxed) {
                tracing::info!(
                    target: "router.supervisor",
                    model = %self.spec.model_key,
                    "llama-server stopped",
                );
                return;
            }
            let failures = self.inner.spawn_failures.fetch_add(1, Ordering::Relaxed) + 1;
            if liveness_kill {
                tracing::error!(
                    target: "router.supervisor",
                    model = %self.spec.model_key,
                    failures = failures,
                    "llama-server unresponsive - restarting with backoff",
                );
            } else {
                tracing::error!(
                    target: "router.supervisor",
                    model = %self.spec.model_key,
                    status = ?exit_status,
                    failures = failures,
                    "llama-server exited unexpectedly - restarting with backoff",
                );
            }
            if self.contained() {
                self.mark_contained();
                tracing::error!(
                    target: "router.supervisor",
                    model = %self.spec.model_key,
                    base_url = %self.base_url,
                    failures = failures,
                    limit = self.inner.max_restarts,
                    "llama-server failed to start after {failures} consecutive attempts - giving up (contained); fix the model's weights/config or restart the router",
                );
                return;
            }
            tokio::time::sleep(restart_backoff(failures)).await;
            self.spawn_child(&bin);
        }
    }

    /// Stop the server: mark stopped, kill the child (the supervision task
    /// sees `stopping` and exits without restarting). Also clears any
    /// containment state — a router restart (`make router-start`) is the
    /// documented recovery path for a contained model.
    ///
    /// An adopted orphan is never killed (the router owns no handle on it and
    /// must not signal a process it did not spawn): it is released to survive
    /// shutdown and be re-adopted by the next boot, so restarts never churn
    /// VRAM or drop live KV state.
    pub async fn stop(self: &Arc<Self>) {
        self.inner.failed.store(false, Ordering::Relaxed);
        self.inner.spawn_failures.store(0, Ordering::Relaxed);
        self.inner.stopping.store(true, Ordering::Relaxed);
        self.inner.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.inner.supervisor.lock().ok().and_then(|mut g| g.take()) {
            handle.abort();
        }
        if self.is_adopted() {
            self.clear_adoption();
            tracing::info!(
                target: "router.supervisor",
                model = %self.spec.model_key,
                "adopted llama-server released on shutdown (process survives for re-adoption)",
            );
            return;
        }
        let child = {
            let mut guard = match self.inner.child.lock() {
                Ok(g) => g,
                Err(e) => e.into_inner(),
            };
            guard.take()
        };
        if let Some(mut child) = child {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        tracing::info!(
            target: "router.supervisor",
            model = %self.spec.model_key,
            "llama-server stopped by router",
        );
    }
}

#[cfg(test)]
#[path = "../tests/supervisor.rs"]
mod tests;
