//! Llama-server spawn spec and pure spawn-argument/library helpers.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::InstanceProfile;
use crate::instances::instance_grammar_string;

/// The `llama-server` binary name resolved from `$PATH`.
pub const LLAMA_SERVER_BIN: &str = "llama-server";

/// Env var that overrides the resolved `llama-server` binary path.
pub const LLAMA_SERVER_ENV: &str = "LLAMA_SERVER";

/// How long a freshly-spawned server has to answer `/health`.
pub(crate) const HEALTH_TIMEOUT: Duration = Duration::from_secs(120);

/// Poll interval while waiting for `/health`.
pub(crate) const HEALTH_POLL: Duration = Duration::from_secs(1);

/// Resolve the `llama-server` binary path: the `LLAMA_SERVER` env override
/// first, then a `$PATH` search. `None` when not found.
pub fn resolve_llama_server() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os(LLAMA_SERVER_ENV) {
        return Some(PathBuf::from(explicit));
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(LLAMA_SERVER_BIN);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Reserve a free localhost port (bind `:0`, read the port, drop the listener).
/// The returned port is a hint - a race is possible but practically negligible
/// at boot time.
pub fn free_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
}

/// Everything needed to spawn and supervise one model's `llama-server`.
#[derive(Debug, Clone)]
/// Everything needed to spawn and supervise one model's `llama-server`.
pub struct LlamaServerSpec {
    /// Coral Router model id (public).
    pub model_key: String,
    /// llama.cpp model name (`--alias`; also the server's primary model id).
    pub name: String,
    /// Local GGUF path (`-m`). `None` uses `hf_repo`.
    pub weights: Option<String>,
    /// HuggingFace repo (`-hf`).
    pub hf_repo: Option<String>,
    /// HuggingFace file within `hf_repo` (`-hff`), optional.
    pub hf_file: Option<String>,
    /// Local chat-template file (`--chat-template-file`), from the model
    /// entry's `template` (explicit config wins over GGUF-baked metadata).
    /// `None` leaves the server default untouched.
    pub template: Option<String>,
    /// Localhost port the server binds.
    pub port: u16,
    /// The declared instance pool (`--instance` grammar flags). Only pinned
    /// profiles are declared at spawn; unpinned instances are created on
    /// demand by the sidecar.
    pub instances: Vec<InstanceProfile>,
    /// Whether the model is spawned at boot: true when it declares at least
    /// one pinned instance. Models without a pinned instance are loaded on
    /// demand by the router (see `LlamaServerSupervisor::ensure_running`).
    pub boot: bool,
    /// `--slot-save-path` for KV snapshots.
    pub slot_save_path: Option<String>,
    /// `--api-key` for the server's management + generation endpoints.
    pub api_key: Option<String>,
    /// `--instance-wait` (group wait seconds); `None` keeps the server default.
    pub instance_wait_s: Option<i64>,
    /// Per-model spawn defaults: the fleet run block overlaid with the
    /// model's own launch knobs (`RouterConfig::spawn_defaults_for`) —
    /// batch sizes, KV cache types, flash attention, GPU offload, and the
    /// plain-model context size.
    pub defaults: crate::config::RoleParams,
    /// Additional raw args passed through verbatim.
    pub extra_args: Vec<String>,
}

impl LlamaServerSpec {
    /// Build the spec for a managed model entry on `port`. `gguf_dir`
    /// prefixes relative `weights`/`template` declarations (absolute ones
    /// pass through) so the spawned argv, adoption comparison, and state
    /// file all carry the same resolved path.
    pub fn from_entry(
        model_key: &str,
        entry: &crate::config::ModelEntry,
        gguf_dir: &std::path::Path,
        port: u16,
        slot_save_path: Option<String>,
        api_key: Option<String>,
        defaults: crate::config::RoleParams,
    ) -> Self {
        let instances = entry.effective_pool().to_vec();
        let boot = instances.iter().any(|p| p.pinned);
        let resolve = |declared: Option<&String>| {
            declared.map(|d| {
                crate::config::RouterConfig::join_config_dir(gguf_dir, d)
                    .to_string_lossy()
                    .into_owned()
            })
        };
        Self {
            model_key: model_key.to_string(),
            name: entry.llama_model_name(model_key),
            weights: resolve(entry.weights.as_ref()),
            hf_repo: entry.hf_repo.clone(),
            hf_file: entry.hf_file.clone(),
            template: resolve(entry.template.as_ref()),
            port,
            instances,
            boot,
            slot_save_path,
            api_key,
            instance_wait_s: None,
            defaults,
            extra_args: Vec::new(),
        }
    }
}

/// Library directory pinned onto a spawned server's `LD_LIBRARY_PATH`: the
/// canonicalized parent of the `llama-server` binary. Canonicalization
/// matters because the binary is usually reached through a symlink chain
/// (`~/.local/bin` -> `/app/bin` -> the fork's `build-coral/bin`); the `.so`
/// files live beside the real binary, not beside the symlink. `None` when the
/// path cannot be canonicalized — the spawn then inherits the ambient
/// environment unchanged.
pub fn fleet_lib_dir(bin: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(bin)
        .ok()?
        .parent()
        .map(Path::to_path_buf)
}

/// Prepend `dir` to an `LD_LIBRARY_PATH`-style value, preserving any
/// inherited entries (ROCm, system) behind it. Pure and unit-tested.
pub fn prepend_library_path(
    dir: &Path,
    existing: Option<std::ffi::OsString>,
) -> std::ffi::OsString {
    let mut out = dir.as_os_str().to_os_string();
    if let Some(rest) = existing.filter(|s| !s.is_empty()) {
        out.push(":");
        out.push(rest);
    }
    out
}

/// Render the exact argv for a spawned server (unit-testable, no side effects).
///
/// A model with a `weights` path loads it via `-m`; an `hf_repo` loads
/// on-demand via `-hf`/`-hff`. A `template` path loads it via
/// `--chat-template-file` (explicit config wins over GGUF-baked metadata). Per-model run defaults (the fleet block
/// overlaid with the model's own launch knobs) are always emitted so every
/// managed server runs identically; a plain model (no instance pool) also
/// gets its resolved context size and idle-sleep timeout. Only **pinned**
/// instance profiles are declared as `--instance` flags at spawn — unpinned
/// instances are created on demand by the sidecar. `--slot-save-path` and
/// `--api-key` enable snapshots and auth.
pub fn build_server_args(spec: &LlamaServerSpec) -> Vec<String> {
    let mut args = vec![
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        spec.port.to_string(),
    ];
    if !spec.name.is_empty() {
        args.push("--alias".into());
        args.push(spec.name.clone());
    }
    if let Some(weights) = &spec.weights {
        args.push("-m".into());
        args.push(weights.clone());
    }
    if let Some(repo) = &spec.hf_repo {
        args.push("-hf".into());
        args.push(repo.clone());
    }
    if let Some(file) = &spec.hf_file {
        args.push("-hff".into());
        args.push(file.clone());
    }
    if let Some(template) = &spec.template {
        args.push("--chat-template-file".into());
        args.push(template.clone());
    }
    // Role run defaults (the "how a model is run" contract).
    args.push("--batch-size".into());
    args.push(spec.defaults.batch_size.to_string());
    args.push("--ubatch-size".into());
    args.push(spec.defaults.ubatch_size.to_string());
    args.push("--cache-type-k".into());
    args.push(spec.defaults.cache_type_k.clone());
    args.push("--cache-type-v".into());
    args.push(spec.defaults.cache_type_v.clone());
    if let Some(mode) = &spec.defaults.flash_attn {
        args.push("--flash-attn".into());
        args.push(mode.clone());
    }
    args.push("--n-gpu-layers".into());
    args.push(spec.defaults.n_gpu_layers.to_string());
    if spec.defaults.n_cpu_moe > 0 {
        args.push("--n-cpu-moe".into());
        args.push(spec.defaults.n_cpu_moe.to_string());
    }
    // Only pinned instances are declared at spawn; unpinned instances are
    // created on demand by the sidecar (see InstanceManager::ensure_window).
    // Exception: a pool with NO pinned instance declares a single transient
    // placeholder (`_bootstrap:ctx=512`) so the server registers `/instances`
    // (verified: the route 404s with zero `--instance` flags, and keeps
    // serving 200 with zero instances after the last is destroyed). The
    // sidecar destroys the placeholder once healthy — before any dispatch
    // window — so it holds kilobytes only transiently and every real window
    // is demand-driven from a configured profile or role template.
    let any_pinned = spec.instances.iter().any(|p| p.pinned);
    for profile in &spec.instances {
        if !profile.pinned {
            continue;
        }
        args.push("--instance".into());
        args.push(instance_grammar_string(std::slice::from_ref(profile)));
    }
    if !any_pinned {
        let placeholder = crate::config::InstanceProfile {
            name: Some(crate::instances::BOOTSTRAP_INSTANCE.to_string()),
            group: None,
            count: 1,
            num_ctx: crate::instances::BOOTSTRAP_CTX,
            parallel: None,
            pinned: false,
            no_sleep: false,
            sleep_idle_seconds: None,
            default: false,
            resume: false,
            params: None,
            embedding: None,
            max_ctx: None,
            session: false,
        };
        args.push("--instance".into());
        args.push(instance_grammar_string(std::slice::from_ref(&placeholder)));
    }
    // A plain model (no instance pool) takes its resolved context size and
    // idle-sleep timeout from the per-model spawn defaults.
    if spec.instances.is_empty() {
        args.push("--ctx-size".into());
        args.push(spec.defaults.num_ctx.to_string());
        args.push("--sleep-idle-seconds".into());
        args.push(spec.defaults.sleep_idle_seconds.to_string());
    }
    if let Some(path) = &spec.slot_save_path {
        args.push("--slot-save-path".into());
        args.push(path.clone());
    }
    if let Some(key) = &spec.api_key {
        args.push("--api-key".into());
        args.push(key.clone());
    }
    if let Some(wait) = spec.instance_wait_s {
        args.push("--instance-wait".into());
        args.push(wait.to_string());
    }
    args.extend(spec.extra_args.iter().cloned());
    args
}

/// Backoff between restart attempts: 1s, 2s, 4s, ... capped at 64s.
///
/// M3: delegates to the shared no-jitter capped-exponential schedule
/// (`common_core::retry::capped_exp_backoff_ms` with base 1s, max shift 6).
/// The schedule is deliberately jitter-free — jitter would change restart
/// timing. Locked element-wise by the M3.1 golden test below.
pub(crate) fn restart_backoff(consecutive_failures: u32) -> Duration {
    Duration::from_millis(common_core::retry::capped_exp_backoff_ms(
        1000,
        consecutive_failures,
        6,
    ))
}

pub(crate) fn build_shared_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .pool_idle_timeout(Duration::from_secs(30))
        .tcp_keepalive(Duration::from_secs(30))
        .user_agent("coral-router")
        .build()
        .expect("reqwest client build")
}