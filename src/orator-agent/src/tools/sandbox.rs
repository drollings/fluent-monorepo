//! Guarded execution: the process half of the sandbox.
//!
//! Shell and file tools run through here under a clean environment
//! (allowlist plus pinned `PATH`), a working-directory jail, an
//! address-space cap, a wall-clock timeout, and process-group kill on
//! timeout or cancel. Every call appends an audit row shaped like the
//! router's `AuditRecord`. File paths additionally pass the
//! check-then-open guard.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::process::Command;

use crate::tools::tool::ToolError;

/// Symlink resolutions before containment refuses (matches the 40-hop
/// shape of the reference guard).
/// Canonical home: `common_core::path::MAX_SYMLINK_HOPS` (re-exported
/// for API compat).
pub use common_core::path::MAX_SYMLINK_HOPS;

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
    /// Legacy directory-cache TTL. Retained for config compatibility;
    /// authorization never consults a cache (every open re-resolves).
    pub dir_cache_ttl: std::time::Duration,
}

/// Private jail root: `~/.local/share/orator/sandbox`. Fail-closed
/// sentinel (empty relative path, rejected by `new`) when no home
/// resolves — never `/tmp`, never the current directory.
#[must_use]
pub fn default_sandbox_root() -> PathBuf {
    orator::paths::home_dir()
        .map(|home| home.join(".local/share/orator/sandbox"))
        .unwrap_or_default()
}

impl Default for GuardedConfig {
    fn default() -> Self {
        Self {
            root: default_sandbox_root(),
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
}

impl Guarded {
    /// Executor over `config`. Creates the jail root when missing with
    /// owner-only permissions. Rejects relative roots, the bare `/tmp`
    /// root, and the no-home sentinel — all fail closed before any tool
    /// runs.
    pub fn new(config: GuardedConfig) -> Result<Self, WithinError> {
        if !config.root.is_absolute() {
            return Err(WithinError::Missing(config.root.display().to_string()));
        }
        if config.root == std::path::Path::new("/tmp") {
            return Err(WithinError::Escape(config.root.display().to_string()));
        }
        std::fs::create_dir_all(&config.root)
            .map_err(|_| WithinError::Missing(config.root.display().to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&config.root, std::fs::Permissions::from_mode(0o700));
        }
        Ok(Self {
            config,
            audit: Mutex::new(Vec::new()),
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
    /// their nearest existing ancestor (writes must pass too). Both the
    /// ancestor walk and the tail re-attachment are hop-capped with a
    /// containment check per hop and `..` normalized per join, so
    /// `a/../../x` and chained `b->c->outside` both refuse.
    ///
    /// Layering (intentional, not duplication): this is the
    /// authoritative jail — authorization always goes through `within`
    /// plus `open_within` (fd re-validation). `orator::paths`
    /// `within_root` is the fast check for paths that are never
    /// opened. Both share `common_core::path::MAX_SYMLINK_HOPS`.
    pub fn within(&self, path: &Path) -> Result<PathBuf, WithinError> {
        let joined = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.config.root.join(path)
        };
        let normal = common_core::path::normalize_std_path(&joined)
            .ok_or_else(|| WithinError::Escape(path.display().to_string()))?;
        if !normal.starts_with(&self.config.root) {
            return Err(WithinError::Escape(path.display().to_string()));
        }
        // Nearest existing ancestor anchors the symlink walk; the
        // remainder re-attaches one component at a time below.
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
        let mut current =
            resolve_symlink_chain(&ancestor, &self.config.root, path, MAX_SYMLINK_HOPS)?;
        let mut remaining = MAX_SYMLINK_HOPS;
        for part in tail.into_iter().rev() {
            let next = current.join(&part);
            current = resolve_symlink_chain(&next, &self.config.root, path, remaining)?;
            // `resolve_symlink_chain` on a non-symlink normalizes the
            // join (so `..` in a link target cannot skip out) and checks
            // containment; a hop consumed above shrinks the budget.
            remaining = remaining.saturating_sub(1);
            if remaining == 0 && is_symlink(&current) {
                return Err(WithinError::HopCap(path.display().to_string()));
            }
        }
        if !current.starts_with(&self.config.root) {
            return Err(WithinError::Escape(path.display().to_string()));
        }
        Ok(current)
    }

    /// Display-only directory resolution: re-resolves on every call and
    /// never caches. Authorization always goes through `within` plus
    /// `open_within` (fd re-validation), so a replaced directory can
    /// never serve a stale jail path.
    #[must_use]
    pub fn real_dir(&self, dir: &Path) -> Option<PathBuf> {
        let canonical = std::fs::canonicalize(dir).ok()?;
        if !canonical.starts_with(&self.config.root) {
            return None;
        }
        Some(canonical)
    }

    /// Open `path` inside the jail and validate the opened handle.
    /// Check-then-open alone races a symlink swap; this opens first and
    /// then re-validates the file the descriptor points at, refusing
    /// anything that now resolves outside the jail. An unreadable
    /// descriptor path (masked `/proc`) refuses instead of falling back
    /// to the pre-open candidate.
    pub fn open_within(
        &self,
        path: &Path,
        options: &std::fs::OpenOptions,
    ) -> Result<std::fs::File, WithinError> {
        let candidate = self.within(path)?;
        let file = options
            .open(&candidate)
            .map_err(|_| WithinError::Missing(path.display().to_string()))?;
        let real = real_path_of(&file).map_err(|error| error.to_string());
        let _ = check_fd_containment(real, &candidate, &self.config.root)?;
        Ok(file)
    }

    /// Create the parent directories of an already-validated `candidate`
    /// without following a swapped symlink outside the jail. Each prefix
    /// is checked for symlink-ness before its child is created; any
    /// escape removes the directories this call created and refuses.
    pub fn mkdir_parents_inside(&self, candidate: &Path) -> Result<(), WithinError> {
        let Some(parent) = candidate.parent() else {
            return Ok(());
        };
        if parent.starts_with(&self.config.root) && parent.exists() {
            // Fast path still refuses a symlinked parent.
            if is_symlink(parent) {
                return Err(WithinError::Escape(candidate.display().to_string()));
            }
            return Ok(());
        }
        // Collect the missing chain from the root down.
        let mut missing: Vec<PathBuf> = Vec::new();
        let mut cursor = parent.to_path_buf();
        while !cursor.exists() {
            missing.push(cursor.clone());
            let Some(up) = cursor.parent().map(Path::to_path_buf) else {
                break;
            };
            if !up.starts_with(&self.config.root) {
                return Err(WithinError::Escape(candidate.display().to_string()));
            }
            cursor = up;
        }
        // The existing anchor must itself be inside and not a symlink;
        // otherwise creating below it would land outside.
        if is_symlink(&cursor) || !cursor.starts_with(&self.config.root) {
            return Err(WithinError::Escape(candidate.display().to_string()));
        }
        let mut created: Vec<PathBuf> = Vec::new();
        for dir in missing.into_iter().rev() {
            // Re-check the parent before each level: a concurrent swap
            // to a symlink refuses instead of following outside.
            if let Some(up) = dir.parent() {
                if is_symlink(up) {
                    for done in created.into_iter().rev() {
                        let _ = std::fs::remove_dir(&done);
                    }
                    return Err(WithinError::Escape(candidate.display().to_string()));
                }
            }
            match std::fs::create_dir(&dir) {
                Ok(()) => created.push(dir),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => {
                    // Lost a race with another creator: re-validate the
                    // full candidate before proceeding.
                    let _ = self.within(
                        candidate
                            .strip_prefix(&self.config.root)
                            .unwrap_or(candidate),
                    )?;
                }
            }
        }
        // Final re-validation: the whole candidate must still resolve
        // inside after the mkdirs.
        let _ = self.within(
            candidate
                .strip_prefix(&self.config.root)
                .unwrap_or(candidate),
        )?;
        Ok(())
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
                Err(ToolError::Failed(
                    tool.to_string(),
                    format!("wait: {error}"),
                ))
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
/// `/proc/self/fd`. Errors (masked `/proc`, closed fd) fail closed in
/// `open_within` — never a fallback to the pre-open candidate.
fn real_path_of(file: &std::fs::File) -> Result<PathBuf, std::io::Error> {
    use std::os::unix::io::AsRawFd as _;
    let fd = file.as_raw_fd();
    let link = std::fs::read_link(format!("/proc/self/fd/{fd}"))?;
    let text = link.to_string_lossy();
    let trimmed = text.strip_suffix(" (deleted)").unwrap_or(&text).to_string();
    Ok(PathBuf::from(trimmed))
}

/// Fail-closed fd decision: an unresolvable descriptor refuses, an
/// outside target refuses even when the pre-open candidate was inside.
/// Split out so the masked-`/proc` case is pinned without masking the
/// host's `/proc`.
pub fn check_fd_containment(
    real: Result<PathBuf, String>,
    candidate: &Path,
    root: &Path,
) -> Result<PathBuf, WithinError> {
    let path = real.map_err(|_| WithinError::Escape(candidate.display().to_string()))?;
    if path.starts_with(root) {
        Ok(path)
    } else {
        Err(WithinError::Escape(candidate.display().to_string()))
    }
}

/// Whether `path` is itself a symlink (never follows).
fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// Chase symlinks at `start` up to `hops`, normalizing `..` per join
/// and checking containment per hop. Non-symlinks normalize the join
/// and check once. Exhausting the budget on a symlink is `HopCap`;
/// leaving the jail is `Escape`.
fn resolve_symlink_chain(
    start: &Path,
    root: &Path,
    original: &Path,
    hops: u32,
) -> Result<PathBuf, WithinError> {
    // Normalize the join itself so `..` in a link target cannot skip
    // out before the containment check runs.
    let mut current = common_core::path::normalize_std_path(start)
        .ok_or_else(|| WithinError::Escape(original.display().to_string()))?;
    if !current.starts_with(root) {
        return Err(WithinError::Escape(original.display().to_string()));
    }
    for _ in 0..hops {
        if !is_symlink(&current) {
            return Ok(current);
        }
        let target = std::fs::read_link(&current)
            .map_err(|_| WithinError::Missing(original.display().to_string()))?;
        let joined = if target.is_absolute() {
            target
        } else {
            match current.parent() {
                Some(dir) => dir.join(&target),
                None => target,
            }
        };
        current = common_core::path::normalize_std_path(&joined)
            .ok_or_else(|| WithinError::Escape(original.display().to_string()))?;
        if !current.starts_with(root) {
            return Err(WithinError::Escape(original.display().to_string()));
        }
    }
    if is_symlink(&current) {
        return Err(WithinError::HopCap(original.display().to_string()));
    }
    if !current.starts_with(root) {
        return Err(WithinError::Escape(original.display().to_string()));
    }
    Ok(current)
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
#[path = "../../tests/tools_sandbox.rs"]
mod tests;
