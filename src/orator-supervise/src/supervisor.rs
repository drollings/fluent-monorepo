//! Supervised local inference endpoint: one `llama-server` child per
//! served model, owned by the daemon.
//!
//! The shape mirrors the router's fleet supervisor at a single-server
//! scale: resolve the binary from the environment or `$PATH` with a
//! fail-fast actionable error, bind a free localhost port, pass
//! `--slot-save-path` only when its directory resolves, and wait for
//! the readiness probe before serving. Restarts are bounded and explicit — a server
//! that keeps dying is contained, never silently respawned forever.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use thiserror::Error;

pub use orator::paths::{find_all_on_path, find_on_path};

/// The `llama-server` binary name resolved from `$PATH`.
pub const LLAMA_SERVER_BIN: &str = "llama-server";

/// Env var that overrides the resolved `llama-server` binary path.
pub const LLAMA_SERVER_ENV: &str = "LLAMA_SERVER";

/// How often a running server must answer `/health` before it counts.
const HEALTH_POLL_STEP: Duration = Duration::from_millis(250);

/// Per-probe HTTP bound. Short: probes run in a poll loop.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Supervisor failures. Every variant names what to do next.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
/// Supervisor failures. Every variant names what to do next.
pub enum SupervisorError {
    /// No binary to spawn. Install it or point at it.
    #[error("{0}")]
    NoBinary(String),
    /// The child could not start.
    #[error("llama-server spawn failed: {0}")]
    Spawn(String),
    /// The child never answered `/health` in time.
    #[error("llama-server unhealthy at {0}: {1}")]
    Unhealthy(String, String),
    /// The server died past its restart budget. Contained, not retried.
    #[error("llama-server contained after {0} restarts: {1}")]
    Contained(u32, String),
    /// Caller passed a flag the server never takes. Fix the config.
    #[error("refusing unknown server flag: {0}")]
    Usage(String),
}

/// Marker for a spawn refused with ETXTBSY: the binary was mid-write
/// (concurrent upgrade, lingering writer, container-FS settle). Always
/// transient — the boot loop retries the same port instead of failing.
pub(crate) fn spawn_busy_marker(error: &std::io::Error) -> &'static str {
    if error.kind() == std::io::ErrorKind::ExecutableFileBusy || error.raw_os_error() == Some(26) {
        "text-busy "
    } else {
        ""
    }
}

/// Flags the supervisor passes through verbatim. Anything else in
/// `extra_args` fails as usage before a child ever spawns — the
/// server argv stays a closed set, never an open shell.
const ALLOWED_EXTRA_ARGS: &[&str] = &[
    "--n-gpu-layers",
    "--verbose",
    "--log-format",
    "--log-verbosity",
    "--cache-type-k",
    "--cache-type-v",
    "--threads",
    "--ctx-size",
    "--batch-size",
];

/// Validate `extra_args` against the allowlist. Unknown flags fail as
/// usage with the flag named; values ride along untouched.
pub fn validate_extra_args(extra: &[String]) -> Result<(), SupervisorError> {
    let mut iter = extra.iter().peekable();
    while let Some(arg) = iter.next() {
        if !arg.starts_with('-') {
            continue;
        }
        let flag = arg.split('=').next().unwrap_or(arg);
        if !ALLOWED_EXTRA_ARGS.contains(&flag) {
            return Err(SupervisorError::Usage(format!(
                "refusing unknown server flag '{arg}' (allowed: {})",
                ALLOWED_EXTRA_ARGS.join(", ")
            )));
        }
        // Flags taking a separate value (`--n-gpu-layers 999`) consume
        // the next token so values never validate as flags.
        if !arg.contains('=') {
            if let Some(next) = iter.peek() {
                if !next.starts_with('-') {
                    iter.next();
                }
            }
        }
    }
    Ok(())
}

/// Readiness probe shape. Text servers answer `GET /health`; the
/// audio build serves no `/health`, so its readiness is the
/// `OPTIONS /v1/chat/completions` route itself (no inference runs).
/// Both accept 2xx only — a 405 is not success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
/// Readiness probe shape. Text servers answer `GET /health`; the
/// audio build serves no `/health`, so its readiness is the
/// `OPTIONS /v1/chat/completions` route itself (no inference runs).
/// Both accept 2xx only — a 405 is not success.
pub enum HealthProbe {
    /// `GET {base}/health` (text servers).
    #[default]
    GetHealth,
    /// `OPTIONS {base}/v1/chat/completions` (audio servers).
    OptionsCompletions,
}
/// What the endpoint supervisor nanny should do after a failed probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// What the endpoint supervisor nanny should do after a failed probe.
pub enum RecoveryAction {
    /// Kill (if spawned) and start again.
    Restart,
    /// Stop trying. Sessions stay degraded until a user restarts.
    Contain,
}

/// Bounded recovery policy: restart while budget remains, then contain.
/// Pure so the budget edge is unit-pinned without a process.
#[must_use]
/// Bounded recovery policy: restart while budget remains, then contain.
/// Pure so the budget edge is unit-pinned without a process.
pub fn recovery_action(consecutive_failures: u32, max_restarts: u32) -> RecoveryAction {
    if consecutive_failures >= max_restarts.max(1) {
        RecoveryAction::Contain
    } else {
        RecoveryAction::Restart
    }
}

/// Resolve the binary: the explicit override first, then `$PATH`.
#[must_use]
/// Resolve the binary: the explicit override first, then `$PATH`.
pub fn resolve_with_override(explicit: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path);
    }
    find_on_path(LLAMA_SERVER_BIN)
}

/// Resolve the `llama-server` binary: the `LLAMA_SERVER` env override
/// first, then a `$PATH` search. Explicit overrides require absolute
/// paths and log their fingerprint (the user accepts responsibility);
/// searched hits are validated by `resolve_binary_validated`.
/// Shared override home: `orator::paths::ExplicitBinary` — the
/// unvalidated first-hit search stays (fail-fast single resolve; the
/// gate lives in the validated wrapper and the candidates filter).
/// `None` when not found or relative.
#[must_use]
/// Resolve the `llama-server` binary: the `LLAMA_SERVER` env override
/// first, then a `$PATH` search. Explicit overrides require absolute
/// paths and log their fingerprint (the user accepts responsibility);
/// searched hits are validated by `resolve_binary_validated`.
/// Shared override home: `orator::paths::ExplicitBinary` — the
/// unvalidated first-hit search stays (fail-fast single resolve; the
/// gate lives in the validated wrapper and the candidates filter).
/// `None` when not found or relative.
pub fn resolve_binary() -> Option<PathBuf> {
    let explicit = orator::paths::ExplicitBinary::from_env(LLAMA_SERVER_ENV);
    match explicit.path {
        Some(path) if path.is_absolute() => {
            let fingerprint = orator::paths::binary_fingerprint(&path);
            if let Err(error) = orator::paths::validate_server_binary(
                &path,
                orator::paths::current_uid(),
                false,
            ) {
                eprintln!(
                    "orator: explicit llama-server {} ({fingerprint}) bypasses the gate with a warning: {error}",
                    path.display()
                );
            }
            Some(path)
        }
        Some(_) => None,
        None => resolve_with_override(None),
    }
}

/// Resolve every `llama-server` candidate: the `LLAMA_SERVER` env
/// override alone when set, else every validated `$PATH` hit in
/// directory order. A stale install earlier on `$PATH` (e.g. a
/// pre-LFM3 build shadowing the system one) fails its boot fast and
/// the next candidate serves, instead of one shadowed binary vetoing
/// inference.
/// Shared filter home: `orator::paths::filter_validated_binaries`
/// (order-preserving, canonical-deduped); the override classification
/// is `orator::paths::ExplicitBinary`.
#[must_use]
/// Resolve every `llama-server` candidate: the `LLAMA_SERVER` env
/// override alone when set, else every validated `$PATH` hit in
/// directory order. A stale install earlier on `$PATH` (e.g. a
/// pre-LFM3 build shadowing the system one) fails its boot fast and
/// the next candidate serves, instead of one shadowed binary vetoing
/// inference.
/// Shared filter home: `orator::paths::filter_validated_binaries`
/// (order-preserving, canonical-deduped); the override classification
/// is `orator::paths::ExplicitBinary`.
pub fn resolve_binary_candidates() -> Vec<PathBuf> {
    let explicit = orator::paths::ExplicitBinary::from_env(LLAMA_SERVER_ENV);
    match explicit.path {
        Some(path) if path.is_absolute() => vec![path],
        Some(_) => Vec::new(),
        None => orator::paths::filter_validated_binaries(
            orator::paths::find_all_on_path(LLAMA_SERVER_BIN),
            orator::paths::current_uid(),
        ),
    }
}

/// Resolve the `llama-server` binary with the ownership gate applied:
/// an explicit `LLAMA_SERVER` override bypasses it, searched hits
/// must be owned by `expected_uid` with no group/other write bits.
/// Refusals report as `NoBinary` with the remedy named.
/// Single-hit fail-fast shape (first hit validated, later hits NOT
/// tried) — kept deliberately: the boot paths that try every
/// candidate compose `resolve_binary_candidates` /
/// `orator::paths::resolve_validated_binary` instead. Shared
/// override home: `orator::paths::ExplicitBinary`.
pub fn resolve_binary_validated(expected_uid: u32) -> Result<PathBuf, SupervisorError> {
    let explicit = orator::paths::ExplicitBinary::from_env(LLAMA_SERVER_ENV);
    let is_explicit = explicit.path.is_some();
    let binary = match explicit.path {
        Some(path) if path.is_absolute() => path,
        // Relative explicit and unset both fall through to the single
        // first-hit search: relative resolves to nothing (hence the
        // `NoBinary` below), unset to the first `$PATH` hit for
        // first-hit-then-validate.
        _ => resolve_binary().ok_or_else(|| {
            SupervisorError::NoBinary(format!(
                "llama-server binary not found in $PATH (set the {LLAMA_SERVER_ENV} env var or install it)"
            ))
        })?,
    };
    orator::paths::validate_server_binary(&binary, expected_uid, is_explicit)
        .map_err(SupervisorError::NoBinary)?;
    Ok(binary)
}

/// Reserve a free localhost port (bind `:0`, read the port, drop the
/// listener). A hint — a race is possible but negligible at boot.
/// Single home: the s2s voice/worker paths compose this through
/// `boot_with_binary` (port `0` → hint → retry on collision); no
/// second port-hint implementation exists in the tree.
#[must_use]
/// Reserve a free localhost port (bind `:0`, read the port, drop the
/// listener). A hint — a race is possible but negligible at boot.
/// Single home: the s2s voice/worker paths compose this through
/// `boot_with_binary` (port `0` → hint → retry on collision); no
/// second port-hint implementation exists in the tree.
pub fn free_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port())
}

/// Guard the `--slot-save-path` flag: keep the directory only when it
/// resolves (created when missing). A path that cannot be created is
/// omitted — the server rejects a nonexistent path at argv-parse time
/// and would crash-loop on it.
#[must_use]
/// Guard the `--slot-save-path` flag: keep the directory only when it
/// resolves (created when missing). A path that cannot be created is
/// omitted — the server rejects a nonexistent path at argv-parse time
/// and would crash-loop on it.
pub fn slot_save_flag(dir: Option<PathBuf>) -> Option<String> {
    let dir = dir?;
    match std::fs::create_dir_all(&dir) {
        Ok(()) => Some(dir.to_string_lossy().into_owned()),
        Err(error) => {
            tracing::warn!(
                target: "orator.llm",
                path = %dir.display(),
                %error,
                "slot-save-path create failed (snapshots disabled)",
            );
            None
        }
    }
}

/// Layer count requesting full GPU offload from `llama-server`
/// (`--n-gpu-layers`): pushed into `extra_args` when the operator opts
/// into GPU. The server owns the rest loudly; without a device the CLI
/// refuses before boot.
pub const GPU_OFFLOAD_LAYERS: &str = "999";

/// What to serve and how to run it.
#[derive(Debug, Clone)]
/// What to serve and how to run it.
pub struct SupervisorConfig {
    /// Served model tag (the server `--alias`).
    pub model: String,
    /// Local GGUF weights (`-m`). `None` serves whatever the binary
    /// defaults to — usually nothing, which fails health fast.
    pub weights: Option<PathBuf>,
    /// Audio encoder projector (`--mmproj`) for speech models. `None`
    /// for text-only servers.
    pub mmproj: Option<PathBuf>,
    /// Extra server argv appended verbatim (audio-vocoder flags on
    /// audio-capable builds, debugging knobs). Empty by default.
    pub extra_args: Vec<String>,
    /// Context window in tokens.
    pub ctx: u16,
    /// Prefill chunk size in tokens (cancel-latency bound).
    pub n_batch: u16,
    /// Snapshot directory. Guarded at boot; omission disables snapshots.
    pub slot_save_dir: Option<PathBuf>,
    /// Localhost port. `0` reserves a free one at boot.
    pub port: u16,
    /// Readiness probe: `GetHealth` for text servers,
    /// `OptionsCompletions` for the audio build.
    pub health_probe: HealthProbe,
    /// Audio vocoder weights (`-mv`). `Some` selects the audio argv
    /// shape (no `--alias`, `-mm` projector flag); `None` is a text
    /// server.
    pub vocoder: Option<PathBuf>,
    /// Audio tokenizer (`--tts-speaker-file`), alongside `vocoder`.
    pub tts_speaker_file: Option<PathBuf>,
    /// How long a fresh child has to answer `/health`.
    pub health_timeout: Duration,
    /// Liveness poll interval for the nanny loop.
    pub liveness_poll: Duration,
    /// Consecutive dead probes before a restart, and the restart budget.
    pub liveness_failures: u32,
    /// Crash-loop containment budget.
    pub max_restarts: u32,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            model: "liquidai/lfm2.5-2.6b".to_string(),
            weights: None,
            mmproj: None,
            extra_args: Vec::new(),
            ctx: 8192,
            n_batch: 512,
            slot_save_dir: None,
            port: 0,
            health_probe: HealthProbe::GetHealth,
            vocoder: None,
            tts_speaker_file: None,
            health_timeout: Duration::from_secs(120),
            liveness_poll: Duration::from_secs(30),
            liveness_failures: 3,
            max_restarts: 3,
        }
    }
}

/// Render the exact argv for a spawn. Unit-testable, no side effects.
/// The server binds loopback only; snapshots ride along only when the
/// directory resolved. An audio config (vocoder set) renders the
/// voice-server shape — backbone, projector, vocoder, speaker file,
/// host, port — with no `--alias` (that build takes none) and no
/// context/slot flags the reference file mapping never passes.
#[must_use]
/// Render the exact argv for a spawn. Unit-testable, no side effects.
/// The server binds loopback only; snapshots ride along only when the
/// directory resolved. An audio config (vocoder set) renders the
/// voice-server shape — backbone, projector, vocoder, speaker file,
/// host, port — with no `--alias` (that build takes none) and no
/// context/slot flags the reference file mapping never passes.
pub fn build_args(config: &SupervisorConfig, slot_save: Option<&str>) -> Vec<String> {
    if config.vocoder.is_some() || config.tts_speaker_file.is_some() {
        return build_audio_args(config);
    }
    let mut args = vec![
        "--host".to_string(),
        "127.0.0.1".to_string(),
        "--port".to_string(),
        config.port.to_string(),
    ];
    if !config.model.is_empty() {
        args.push("--alias".to_string());
        args.push(config.model.clone());
    }
    if let Some(weights) = &config.weights {
        args.push("-m".to_string());
        args.push(weights.to_string_lossy().into_owned());
    }
    if let Some(mmproj) = &config.mmproj {
        args.push("--mmproj".to_string());
        args.push(mmproj.to_string_lossy().into_owned());
    }
    args.push("--ctx-size".to_string());
    args.push(config.ctx.to_string());
    args.push("--batch-size".to_string());
    args.push(config.n_batch.to_string());
    if let Some(slot) = slot_save {
        args.push("--slot-save-path".to_string());
        args.push(slot.to_string());
    }
    args.extend(config.extra_args.iter().cloned());
    args
}

/// Audio argv: the reference file mapping, verbatim. Extra args ride
/// along last for debugging knobs.
fn build_audio_args(config: &SupervisorConfig) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(weights) = &config.weights {
        args.push("-m".to_string());
        args.push(weights.to_string_lossy().into_owned());
    }
    if let Some(mmproj) = &config.mmproj {
        args.push("-mm".to_string());
        args.push(mmproj.to_string_lossy().into_owned());
    }
    if let Some(vocoder) = &config.vocoder {
        args.push("-mv".to_string());
        args.push(vocoder.to_string_lossy().into_owned());
    }
    if let Some(speaker) = &config.tts_speaker_file {
        args.push("--tts-speaker-file".to_string());
        args.push(speaker.to_string_lossy().into_owned());
    }
    args.push("--host".to_string());
    args.push("127.0.0.1".to_string());
    args.push("--port".to_string());
    args.push(config.port.to_string());
    args.extend(config.extra_args.iter().cloned());
    args
}

/// One supervised endpoint: an optional child plus its base URL.
/// `attach` (no child) is the test/adopted shape: health and management
/// go through HTTP exactly as for a spawned server.
pub struct Supervisor {
    config: SupervisorConfig,
    base_url: String,
    binary: Option<PathBuf>,
    child: Mutex<Option<tokio::process::Child>>,
    healthy: AtomicBool,
    stopped: AtomicBool,
    restarts: AtomicU32,
}

impl Supervisor {
    /// Resolve the binary and boot one server. Every validated
    /// candidate is tried in order until one answers its readiness
    /// probe: a stale `$PATH` shadow fails fast and the next serves.
    /// Searched hits pass the ownership gate for the current uid; an
    /// explicit `LLAMA_SERVER` override bypasses it. Fails fast with an
    /// actionable message when nothing trusted is installed, or with
    /// the last boot error when every candidate failed.
    pub async fn boot(config: SupervisorConfig) -> Result<Self, SupervisorError> {
        let candidates = resolve_binary_candidates();
        if candidates.is_empty() {
            return Err(SupervisorError::NoBinary(format!(
                "llama-server binary not found in $PATH (set the {LLAMA_SERVER_ENV} env var or install it)"
            )));
        }
        match fluent_wvr::ladder::first_accept_in_order(
            candidates,
            |binary: PathBuf| {
                let config = config.clone();
                async move {
                    match boot_with_binary(config, &binary).await {
                        Ok(server) => Ok(Some(server)),
                        Err(error) => {
                            tracing::warn!(
                                target: "orator.llm",
                                binary = %binary.display(),
                                %error,
                                "server candidate failed; trying the next",
                            );
                            Err(error)
                        }
                    }
                }
            },
            |_: &SupervisorError| false,
        )
        .await
        {
            Ok(Some(server)) => Ok(server),
            Ok(None) => Err(SupervisorError::NoBinary(format!(
                "llama-server binary not found in $PATH (set the {LLAMA_SERVER_ENV} env var or install it)"
            ))),
            Err(error) => Err(error),
        }
    }

    /// Manage an already-running endpoint at `base_url`. No process is
    /// spawned; health goes through HTTP. Test and adoption shape.
    #[must_use]
    pub fn attach(base_url: String) -> Self {
        Self::attach_with_probe(base_url, HealthProbe::GetHealth)
    }

    /// Manage an already-running endpoint with an explicit probe.
    /// Test and adoption shape for the audio build.
    #[must_use]
    pub fn attach_with_probe(base_url: String, probe: HealthProbe) -> Self {
        Self {
            config: SupervisorConfig {
                health_probe: probe,
                ..SupervisorConfig::default()
            },
            base_url,
            binary: None,
            child: Mutex::new(None),
            healthy: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            restarts: AtomicU32::new(0),
        }
    }

    /// The endpoint base URL (`http://127.0.0.1:<port>`).
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Whether the endpoint answered health and has not been stopped.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.healthy.load(Ordering::Relaxed) && !self.stopped.load(Ordering::Relaxed)
    }

    /// Whether `stop` was called.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }

    /// Completed restarts so far.
    #[must_use]
    pub fn restarts(&self) -> u32 {
        self.restarts.load(Ordering::Relaxed)
    }

    /// Single health check against the configured probe: 2xx only.
    pub async fn health(&self) -> Result<(), SupervisorError> {
        probe_health(&self.base_url, self.config.health_probe)
            .await
            .map_err(|detail| SupervisorError::Unhealthy(self.base_url.clone(), detail))
    }

    /// Wait for the configured probe up to `timeout`, polling every
    /// 250 ms. A spawned child that exits first fails fast instead of
    /// burning the whole timeout on refused connections.
    pub async fn wait_healthy_timeout(&self, timeout: Duration) -> Result<(), SupervisorError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match probe_health(&self.base_url, self.config.health_probe).await {
                Ok(()) => {
                    self.healthy.store(true, Ordering::Relaxed);
                    return Ok(());
                }
                Err(detail) => {
                    if self.child_exited() {
                        self.healthy.store(false, Ordering::Relaxed);
                        return Err(SupervisorError::Spawn(format!(
                            "llama-server exited before healthy at {}",
                            self.base_url
                        )));
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Err(SupervisorError::Unhealthy(self.base_url.clone(), detail));
                    }
                    tokio::time::sleep(HEALTH_POLL_STEP).await;
                }
            }
        }
    }

    /// Whether the spawned child already exited (false when attached).
    fn child_exited(&self) -> bool {
        if self.binary.is_none() {
            return false;
        }
        let mut guard = common_core::sync::lock(&self.child);
        match guard.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(Some(_))),
            None => false,
        }
    }

    /// Nanny loop: poll liveness, restart boundedly, contain past budget.
    /// Returns when stopped, or with `Contained` when the budget burns.
    pub async fn supervise(&self) -> Result<(), SupervisorError> {
        let mut failures = 0u32;
        loop {
            if self.is_stopped() {
                return Ok(());
            }
            tokio::time::sleep(self.config.liveness_poll).await;
            if self.is_stopped() {
                return Ok(());
            }
            match probe_health(&self.base_url, self.config.health_probe).await {
                Ok(()) => {
                    failures = 0;
                    self.healthy.store(true, Ordering::Relaxed);
                }
                Err(detail) => {
                    failures += 1;
                    self.healthy.store(false, Ordering::Relaxed);
                    match recovery_action(failures, self.config.max_restarts) {
                        RecoveryAction::Restart => {
                            self.stop_child();
                            if self.binary.is_none() {
                                return Err(SupervisorError::Contained(
                                    self.restarts(),
                                    format!("endpoint down at {}: {detail}", self.base_url),
                                ));
                            }
                            self.spawn_child()?;
                            self.restarts.fetch_add(1, Ordering::Relaxed);
                            // A restart that never becomes healthy burns
                            // another failure below instead of hanging here.
                            let _ = self.wait_healthy_timeout(self.config.health_timeout).await;
                        }
                        RecoveryAction::Contain => {
                            self.stop();
                            return Err(SupervisorError::Contained(
                                self.restarts(),
                                format!("endpoint down at {}: {detail}", self.base_url),
                            ));
                        }
                    }
                }
            }
        }
    }

    /// Stop the child (if spawned) and mark the endpoint down. Idempotent.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        self.healthy.store(false, Ordering::Relaxed);
        self.stop_child();
    }

    fn spawn_child(&self) -> Result<(), SupervisorError> {
        validate_extra_args(&self.config.extra_args)?;
        let binary = self.binary.as_ref().ok_or_else(|| {
            SupervisorError::NoBinary("no binary configured for spawn".to_string())
        })?;
        let slot = slot_save_flag(self.config.slot_save_dir.clone());
        let args = build_args(&self.config, slot.as_deref());
        // M6-S6 audit: no shared `supervised_cmd` builder. The sandbox
        // prefix below (env_clear + pinned PATH/LANG + null stdio +
        // kill_on_drop + process group) matches the worker's spawn, but
        // the sites diverge load-bearingly — here stderr is discarded
        // (health decides), there it is piped and drained for error
        // tails; here cwd inherits, there it is a private jail; here the
        // child is stored for the nanny, there it is scoped by a
        // timeout + whole-group kill. Extracting the common prefix
        // would hide those divergences, so it stays duplicated.
        let child = tokio::process::Command::new(binary)
            .args(&args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .process_group(0)
            .spawn()
            .map_err(|error| {
                let marker = if error.kind() == std::io::ErrorKind::AddrInUse {
                    "addr-in-use "
                } else {
                    spawn_busy_marker(&error)
                };
                SupervisorError::Spawn(format!(
                    "{marker}{} ({}): {error}",
                    binary.display(),
                    args.join(" ")
                ))
            })?;
        // Reap promptly on drop instead of leaking a zombie supervisor row.
        let _ = child.id();
        *common_core::sync::lock(&self.child) = Some(child);
        Ok(())
    }

    fn stop_child(&self) {
        let child = common_core::sync::lock(&self.child).take();
        let Some(mut child) = child else {
            return;
        };
        let _ = child.start_kill();
        // Bounded reap so no zombie survives the call; dropping
        // still kills as a backstop. The lock is released before the
        // wait so concurrent boots and health checks never block on
        // the reap.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Boot `binary` explicitly: the env-independent seam behind
/// [`Supervisor::boot`], so tests drive failure paths without touching
/// the process environment.
pub async fn boot_with_binary(
    config: SupervisorConfig,
    binary: &Path,
) -> Result<Supervisor, SupervisorError> {
    validate_extra_args(&config.extra_args)?;
    let auto_port = config.port == 0;
    let mut config = config;
    if auto_port {
        config.port = free_port()
            .ok_or_else(|| SupervisorError::Spawn("no free localhost port".to_string()))?;
    }
    // Hint-then-reuse races a parallel boot; retry on collision instead
    // of failing the whole turn on one taken port. Spawn collisions
    // retry on the spawn error; post-spawn collisions (the child exits
    // because the hint was taken) retry on fast unhealthy when the port
    // was automatic.
    let mut attempts = 0;
    loop {
        let base_url = format!("http://127.0.0.1:{}", config.port);
        let this = Supervisor {
            config: config.clone(),
            base_url,
            binary: Some(binary.to_path_buf()),
            child: Mutex::new(None),
            healthy: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            restarts: AtomicU32::new(0),
        };
        match this.spawn_child() {
            Ok(()) => {}
            Err(error) => {
                let text = error.to_string();
                let collision = text.contains("addr-in-use")
                    || text.contains("Address already in use")
                    || text.contains("EADDRINUSE")
                    || text.contains("98");
                attempts += 1;
                if text.contains("text-busy") && attempts < 3 {
                    // Transient mid-write refusal: same port, brief
                    // settle, bounded retries. Never a requeue or a new
                    // port — the binary did not move.
                    tokio::time::sleep(Duration::from_millis(50 * attempts as u64)).await;
                    continue;
                }
                if collision && attempts < 3 {
                    if let Some(port) = free_port() {
                        config.port = port;
                        continue;
                    }
                }
                return Err(error);
            }
        }
        if let Err(error) = this.wait_healthy_timeout(this.config.health_timeout).await {
            // Retry automatic ports on fast spawn exits (the hint was
            // likely taken); explicit ports, timeouts, and usage errors
            // never retry.
            attempts += 1;
            if auto_port && attempts < 3 && matches!(error, SupervisorError::Spawn(_)) {
                if let Some(port) = free_port() {
                    config.port = port;
                    this.stop();
                    continue;
                }
            }
            this.stop();
            return Err(error);
        }
        return Ok(this);
    }
}
/// Single readiness attempt shared by the supervisor loop and the
/// daemon's liveness check: one request through the caller's client,
/// 2xx only. Pass the process-wide pooled client at the call site;
/// the per-probe bound rides on the request.
pub async fn probe_voice_ready(
    client: &reqwest::Client,
    base: &str,
    probe: HealthProbe,
) -> Result<(), String> {
    let (method, path) = match probe {
        HealthProbe::GetHealth => (reqwest::Method::GET, "/health"),
        HealthProbe::OptionsCompletions => (reqwest::Method::OPTIONS, "/v1/chat/completions"),
    };
    let response = client
        .request(method, format!("{base}{path}"))
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("health status {}", response.status()))
    }
}

/// Readiness probe primitive over the shared transport: `GET
/// {base}/health` for text servers, `OPTIONS
/// {base}/v1/chat/completions` for the audio build. 2xx only.
async fn probe_health(base: &str, probe: HealthProbe) -> Result<(), String> {
    probe_voice_ready(common_core::http::shared_http_client(), base, probe).await
}

#[cfg(test)]
#[path = "../tests/supervise_supervisor.rs"]
mod tests;