//! Guarded execution: the process half of the sandbox.
//!
//! Shell and file tools run through here under a clean environment
//! (allowlist plus pinned `PATH`), a working-directory jail, an
//! address-space cap, a wall-clock timeout, and process-group kill on
//! timeout or cancel. Every call appends an audit row shaped like the
//! router's `AuditRecord`. File paths additionally pass the
//! check-then-open guard.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::process::Command;

use crate::tool::ToolError;

/// Symlink resolutions before containment refuses (matches the 40-hop
/// shape of the reference guard).
pub const MAX_SYMLINK_HOPS: u32 = 40;

/// In-memory audit cap: oldest rows drop past this bound.
pub const MAX_AUDIT_ROWS: usize = 256;

/// Directory-cache cap: oldest entries evict past this bound.
pub const MAX_DIR_CACHE_ENTRIES: usize = 128;

/// Default directory-cache time-to-live when the config does not set one.
pub const DEFAULT_DIR_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// Path-guard failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WithinError {
    /// The path escapes the allowed root.
    #[error("path escapes the allowed root: {0}")]
    Escape(String),
    /// Symlink chase exceeded the hop cap (likely a loop).
    #[error("symlink hop cap exceeded: {0}")]
    HopCap(String),
    /// The path does not exist.
    #[error("path missing: {0}")]
    Missing(String),
}

/// One durable audit row: a `kind` plus flat JSON detail.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolAudit {
    /// Record kind (`tool_call`).
    pub kind: &'static str,
    /// Flat detail (`tool`, `tier`, `verdict`, `ms`, …).
    pub detail: serde_json::Value,
}

/// Guarded executor configuration.
#[derive(Debug, Clone)]
pub struct GuardedConfig {
    /// Working-directory jail. Tools never run outside it.
    pub root: PathBuf,
    /// Wall-clock budget per call in ms.
    pub timeout_ms: u64,
    /// Address-space cap in KiB (`ulimit -v`). Zero disables.
    pub max_address_kb: u64,
    /// Pinned `PATH` for every child.
    pub path: String,
    /// Directory-cache time-to-live. Stale entries re-resolve.
    pub dir_cache_ttl: std::time::Duration,
}

impl Default for GuardedConfig {
    fn default() -> Self {
        Self {
            root: PathBuf::from("/tmp"),
            timeout_ms: 15_000,
            max_address_kb: 2 * 1024 * 1024,
            path: "/usr/bin:/bin".to_string(),
            dir_cache_ttl: DEFAULT_DIR_CACHE_TTL,
        }
    }
}

/// One sandboxed command run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecResult {
    /// Combined stdout (stderr appended when non-empty).
    pub output: String,
    /// Whether the budget fired (and the group was killed).
    pub timed_out: bool,
    /// Wall-clock ms.
    pub ms: u64,
}

/// The Guarded executor: jail, timeout, group kill, audit.
pub struct Guarded {
    config: GuardedConfig,
    audit: Mutex<Vec<ToolAudit>>,
    dir_cache: Mutex<HashMap<PathBuf, CachedDir>>,
}

/// One cached directory resolution with insertion time and source mtime.
#[derive(Debug, Clone)]
struct CachedDir {
    canonical: PathBuf,
    inserted: Instant,
    mtime: Option<std::time::SystemTime>,
}

impl Guarded {
    /// Executor over `config`. Creates the jail root when missing.
    pub fn new(config: GuardedConfig) -> Result<Self, WithinError> {
        std::fs::create_dir_all(&config.root)
            .map_err(|_| WithinError::Missing(config.root.display().to_string()))?;
        Ok(Self {
            config,
            audit: Mutex::new(Vec::new()),
            dir_cache: Mutex::new(HashMap::new()),
        })
    }

    /// Audit rows so far, oldest first.
    #[must_use]
    pub fn audit_rows(&self) -> Vec<ToolAudit> {
        common_core::sync::lock(&self.audit).clone()
    }

    /// Check-then-open guard: normalize `path`, resolve symlinks with a
    /// hop cap, verify containment in the jail root. Returns the
    /// canonical path for opening. Yet-uncreated paths resolve through
    /// their nearest existing ancestor (writes must pass too).
    pub fn within(&self, path: &Path) -> Result<PathBuf, WithinError> {
        let joined = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.config.root.join(path)
        };
        // Lexical normalize first: `..` past the root fails fast without
        // touching the filesystem.
        let mut normal = PathBuf::new();
        for component in joined.components() {
            match component {
                Component::ParentDir => {
                    if !normal.pop() {
                        return Err(WithinError::Escape(path.display().to_string()));
                    }
                }
                Component::CurDir => {}
                other => normal.push(other.as_os_str()),
            }
        }
        if !normal.starts_with(&self.config.root) {
            return Err(WithinError::Escape(path.display().to_string()));
        }
        // Nearest existing ancestor anchors the symlink walk; the
        // remainder re-attaches lexically afterwards.
        let mut ancestor = normal.clone();
        let mut tail: Vec<std::ffi::OsString> = Vec::new();
        while std::fs::symlink_metadata(&ancestor).is_err() {
            match ancestor.file_name() {
                Some(name) => {
                    tail.push(name.to_os_string());
                    ancestor.pop();
                }
                None => return Err(WithinError::Missing(normal.display().to_string())),
            }
        }
        let mut current = ancestor;
        for _ in 0..MAX_SYMLINK_HOPS {
            let metadata = std::fs::symlink_metadata(&current)
                .map_err(|_| WithinError::Missing(current.display().to_string()))?;
            if !metadata.file_type().is_symlink() {
                break;
            }
            let target = std::fs::read_link(&current)
                .map_err(|_| WithinError::Missing(current.display().to_string()))?;
            if target.is_absolute() {
                current = target;
            } else {
                current = match current.parent() {
                    Some(dir) => dir.join(&target),
                    None => target,
                };
            }
        }
        if std::fs::symlink_metadata(&current)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(WithinError::HopCap(path.display().to_string()));
        }
        for part in tail.into_iter().rev() {
            // A symlinked tail component re-anchors: resolve one more hop.
            let next = current.join(&part);
            if let Ok(target) = std::fs::read_link(&next) {
                current = if target.is_absolute() {
                    target
                } else {
                    current.join(target)
                };
            } else {
                current = next;
            }
        }
        if !current.starts_with(&self.config.root) {
            return Err(WithinError::Escape(path.display().to_string()));
        }
        Ok(current)
    }

    /// Memoized real directory for `dir` with a bounded time-to-live
    /// plus ancestor-change invalidation. A hit returns only when the
    /// entry is fresh and the source mtime still matches; replacement of
    /// the directory re-resolves instead of serving a stale jail path.
    #[must_use]
    pub fn real_dir(&self, dir: &Path) -> Option<PathBuf> {
        let now = Instant::now();
        let current_mtime = std::fs::symlink_metadata(dir).ok()?.modified().ok();
        if let Some(hit) = common_core::sync::lock(&self.dir_cache).get(dir).cloned() {
            if now.duration_since(hit.inserted) <= self.config.dir_cache_ttl
                && hit.mtime == current_mtime
            {
                return Some(hit.canonical);
            }
        }
        let canonical = std::fs::canonicalize(dir).ok()?;
        if !canonical.starts_with(&self.config.root) {
            return None;
        }
        let mut cache = common_core::sync::lock(&self.dir_cache);
        if cache.len() >= MAX_DIR_CACHE_ENTRIES {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, entry)| entry.inserted)
                .map(|(key, _)| key.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(
            dir.to_path_buf(),
            CachedDir {
                canonical: canonical.clone(),
                inserted: now,
                mtime: current_mtime,
            },
        );
        Some(canonical)
    }

    /// Open `path` inside the jail and validate the opened handle.
    /// Check-then-open alone races a symlink swap; this opens first and
    /// then re-validates the file the descriptor points at, refusing
    /// anything that now resolves outside the jail.
    pub fn open_within(
        &self,
        path: &Path,
        options: &std::fs::OpenOptions,
    ) -> Result<std::fs::File, WithinError> {
        let candidate = self.within(path)?;
        let file = options
            .open(&candidate)
            .map_err(|_| WithinError::Missing(path.display().to_string()))?;
        let real = real_path_of(&file).unwrap_or_else(|_| candidate.clone());
        if !real.starts_with(&self.config.root) {
            return Err(WithinError::Escape(path.display().to_string()));
        }
        Ok(file)
    }

    /// Run `argv` under the guard. `tool` names the audit row.
    pub async fn run(&self, tool: &str, argv: &[String]) -> Result<ExecResult, ToolError> {
        if argv.is_empty() {
            return Err(ToolError::Args(tool.to_string(), "empty argv".to_string()));
        }
        let started = Instant::now();
        // Address-space cap rides in a shell prefix; the tool itself is
        // `exec`ed so pids and groups stay exact.
        let (program, args): (String, Vec<String>) = if self.config.max_address_kb > 0 {
            let inner = shell_escape(argv);
            (
                "sh".to_string(),
                vec![
                    "-c".to_string(),
                    format!(
                        "ulimit -v {} 2>/dev/null; exec {inner}",
                        self.config.max_address_kb
                    ),
                ],
            )
        } else {
            (argv[0].clone(), argv[1..].to_vec())
        };
        let mut child = Command::new(&program);
        child
            .args(&args)
            .current_dir(&self.config.root)
            .env_clear()
            .env("PATH", &self.config.path)
            .env("LANG", "C.UTF-8")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        // The child leads its own process group (safe std-level API on
        // the command builder), so the timeout kill below takes
        // grandchildren with it instead of orphaning them.
        child.process_group(0);
        let child = child.spawn().map_err(|error| {
            self.audit(tool, "spawn-failed", started, None);
            ToolError::Failed(tool.to_string(), format!("spawn: {error}"))
        })?;
        let pgid = child.id();
        let timeout = Duration::from_millis(self.config.timeout_ms.max(1));
        // `wait_with_output` drains the pipes concurrently (no 64 KiB
        // deadlock); the waiter stays owned so the timeout arm below can
        // re-await it after the group kill lands.
        let mut wait = Box::pin(child.wait_with_output());
        let output = tokio::time::timeout(timeout, &mut wait).await;
        let ms = started.elapsed().as_millis() as u64;
        match output {
            Ok(Ok(output)) => {
                let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.trim().is_empty() {
                    text.push_str("\n[stderr]\n");
                    text.push_str(&stderr);
                }
                self.audit(
                    tool,
                    if output.status.success() {
                        "ok"
                    } else {
                        "nonzero-exit"
                    },
                    started,
                    Some(ms),
                );
                Ok(ExecResult {
                    output: text,
                    timed_out: false,
                    ms,
                })
            }
            Ok(Err(error)) => {
                self.audit(tool, "wait-failed", started, Some(ms));
                Err(ToolError::Failed(tool.to_string(), format!("wait: {error}")))
            }
            Err(_) => {
                // Budget fired: kill the whole group, then reap. The
                // re-await resolves once the kill lands; a short second
                // bound keeps a missing `kill` binary from hanging us
                // (the waiter drops there, and `kill_on_drop` fires).
                if let Some(pgid) = pgid {
                    let _ = Command::new(kill_program())
                        .args(["-KILL", "--", &format!("-{pgid}")])
                        .output()
                        .await;
                }
                let reaped = tokio::time::timeout(Duration::from_secs(5), &mut wait).await;
                if reaped.is_err() {
                    self.audit(tool, "timeout-unreaped", started, Some(ms));
                } else {
                    self.audit(tool, "timeout", started, Some(ms));
                }
                Err(ToolError::Timeout(tool.to_string(), self.config.timeout_ms))
            }
        }
    }

    fn audit(&self, tool: &str, verdict: &str, started: Instant, ms: Option<u64>) {
        let detail = serde_json::json!({
            "tool": tool,
            "verdict": verdict,
            "elapsed_ms": started.elapsed().as_millis() as u64,
            "ms": ms,
        });
        let mut audit = common_core::sync::lock(&self.audit);
        audit.push(ToolAudit {
            kind: "tool_call",
            detail,
        });
        if audit.len() > MAX_AUDIT_ROWS {
            let excess = audit.len() - MAX_AUDIT_ROWS;
            audit.drain(0..excess);
        }
    }
}

/// Resolve the filesystem path a live descriptor points at via
/// `/proc/self/fd`. Falls back to an error when the platform link is
/// unreadable so callers keep their pre-open candidate.
fn real_path_of(file: &std::fs::File) -> Result<PathBuf, std::io::Error> {
    use std::os::unix::io::AsRawFd as _;
    let fd = file.as_raw_fd();
    let link = std::fs::read_link(format!("/proc/self/fd/{fd}"))?;
    let text = link.to_string_lossy();
    let trimmed = text
        .strip_suffix(" (deleted)")
        .unwrap_or(&text)
        .to_string();
    Ok(PathBuf::from(trimmed))
}

/// Timeout-kill binary: absolute path when present, `PATH` lookup
/// otherwise. The group kill below never depends on shell resolution.
#[must_use]
pub fn kill_program() -> String {
    for candidate in ["/bin/kill", "/usr/bin/kill"] {
        if std::path::Path::new(candidate).is_file() {
            return candidate.to_string();
        }
    }
    "kill".to_string()
}

/// Shell-quote `argv` for the `ulimit` prefix: single quotes with
/// embedded quotes escaped. Never interpolated — always quoted.
#[must_use]
pub fn shell_escape(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| format!("'{}'", arg.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
#[path = "../tests/sandbox.rs"]
mod tests;
