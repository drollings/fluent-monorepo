//! Trusted path resolution: one home lookup, one `$PATH` search, one
//! server-binary ownership gate.
//!
//! Every daemon and CLI path rooted in the user's home goes through
//! [`home_dir`]: `$HOME`, then `$XDG_DATA_HOME`, then nothing — never
//! a relative path, never `/tmp`. Server binaries found by search
//! must additionally pass [`validate_server_binary`]; an explicit
//! user override bypasses the gate and is the user's responsibility.

use std::path::{Path, PathBuf};

/// Home directory base: `$HOME`, then `$XDG_DATA_HOME`, else `None`.
/// Empty and relative values are skipped — callers must never root
/// trusted paths in one.
#[must_use]
/// Home directory base: `$HOME`, then `$XDG_DATA_HOME`, else `None`.
/// Empty and relative values are skipped — callers must never root
/// trusted paths in one.
pub fn home_dir() -> Option<PathBuf> {
    for var in ["HOME", "XDG_DATA_HOME"] {
        if let Ok(value) = std::env::var(var) {
            let path = PathBuf::from(&value);
            if !value.is_empty() && path.is_absolute() {
                return Some(path);
            }
        }
    }
    None
}

/// The process's own uid: the owner every private path must match.
/// Read from `/proc/self`, whose owner is the process's euid — no new
/// dependencies, no unsafe. Unreadable (no `/proc`) fails closed.
#[must_use]
/// The process's own uid: the owner every private path must match.
/// Read from `/proc/self`, whose owner is the process's euid — no new
/// dependencies, no unsafe. Unreadable (no `/proc`) fails closed.
pub fn current_uid() -> u32 {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata("/proc/self").map_or(u32::MAX, |metadata| metadata.uid())
    }
    #[cfg(not(target_os = "linux"))]
    {
        u32::MAX
    }
}

/// Search `$PATH` for `name`. `None` when absent.
#[must_use]
/// Search `$PATH` for `name`. `None` when absent.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    find_all_on_path(name).into_iter().next()
}

/// Every `name` hit on `$PATH` in directory order, deduplicated by
/// canonical path (the literal join is returned, so symlinked hits
/// keep their `$PATH` spelling). Boot code tries each in turn: a
/// stale install earlier on `$PATH` fails fast (measured boot
/// outcome) and the next candidate serves, instead of one shadowed
/// binary vetoing voice.
#[must_use]
/// Every `name` hit on `$PATH` in directory order, deduplicated by
/// canonical path (the literal join is returned, so symlinked hits
/// keep their `$PATH` spelling). Boot code tries each in turn: a
/// stale install earlier on `$PATH` fails fast (measured boot
/// outcome) and the next candidate serves, instead of one shadowed
/// binary vetoing voice.
pub fn find_all_on_path(name: &str) -> Vec<PathBuf> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen = Vec::new();
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if !candidate.is_file() {
            continue;
        }
        let canonical = std::fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone());
        if !seen.contains(&canonical) {
            seen.push(canonical);
            out.push(candidate);
        }
    }
    out
}

/// Whether `candidate` stays inside `root` after resolving symlinks.
/// Missing paths and escapes both fail closed (`false`). Resolution is
/// hop-capped (never an unbounded `canonicalize`): loops and deep
/// chains refuse instead of hanging or resolving outside.
///
/// Layering (intentional, not duplication): this is the fast check —
/// callers that then open the path must re-validate the opened
/// handle (`orator_agent::tools::sandbox::open_within`, fd re-validation),
/// because a check-then-open races a symlink swap. The sandbox is
/// the authoritative jail; this is the cheap gate. Both share
/// `common_core::path::MAX_SYMLINK_HOPS`.
#[must_use]
/// Whether `candidate` stays inside `root` after resolving symlinks.
/// Missing paths and escapes both fail closed (`false`). Resolution is
/// hop-capped (never an unbounded `canonicalize`): loops and deep
/// chains refuse instead of hanging or resolving outside.
/// 
/// Layering (intentional, not duplication): this is the fast check —
/// callers that then open the path must re-validate the opened
/// handle (`orator_agent::tools::sandbox::open_within`, fd re-validation),
/// because a check-then-open races a symlink swap. The sandbox is
/// the authoritative jail; this is the cheap gate. Both share
/// `common_core::path::MAX_SYMLINK_HOPS`.
pub fn within_root(root: &Path, candidate: &Path) -> bool {
    use common_core::path::MAX_SYMLINK_HOPS;
    let Some(root) = resolve_hop_capped(root, MAX_SYMLINK_HOPS) else {
        return false;
    };
    let Some(resolved) = resolve_hop_capped(candidate, MAX_SYMLINK_HOPS) else {
        return false;
    };
    resolved.starts_with(root)
}

/// 12-char fingerprint for an explicit binary path (for logs): sha256
/// of the path string, not the file contents (instant even for
/// gigabyte weights; the content hash gate lives in `verify_entry`).
#[must_use]
/// 12-char fingerprint for an explicit binary path (for logs): sha256
/// of the path string, not the file contents (instant even for
/// gigabyte weights; the content hash gate lives in `verify_entry`).
pub fn binary_fingerprint(path: &Path) -> String {
    common_core::hash::sha256_hex(path.as_os_str().as_encoded_bytes())
        .chars()
        .take(12)
        .collect()
}

/// Whether any component of `path` (or its parents up to root) is a
/// symlink. Uses `symlink_metadata` per component (never follows), so
/// a swapped parent refuses instead of serving through it.
#[must_use]
/// Whether any component of `path` (or its parents up to root) is a
/// symlink. Uses `symlink_metadata` per component (never follows), so
/// a swapped parent refuses instead of serving through it.
pub fn has_symlink_parent(path: &Path) -> bool {
    let mut cursor: Option<&Path> = Some(path);
    while let Some(current) = cursor {
        if std::fs::symlink_metadata(current).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return true;
        }
        cursor = current.parent();
    }
    false
}

/// Chase symlinks at `path` up to `hops`, normalizing `..` per join.
/// Returns `None` on loops, hop exhaustion, missing paths, or `..`
/// past the filesystem root — all fail closed for callers.
fn resolve_hop_capped(path: &Path, hops: u32) -> Option<PathBuf> {
    // Lexical normalize first so `..` cannot skip out before the walk.
    let mut current = common_core::path::normalize_std_path(path)?;
    for _ in 0..hops {
        let meta = std::fs::symlink_metadata(&current).ok()?;
        if !meta.file_type().is_symlink() {
            return Some(current);
        }
        let target = std::fs::read_link(&current).ok()?;
        let joined = if target.is_absolute() {
            target
        } else {
            current
                .parent()
                .map_or(target.clone(), |dir| dir.join(&target))
        };
        current = common_core::path::normalize_std_path(&joined)?;
    }
    // Budget exhausted: only accept when the landing is no longer a
    // symlink; loops stay `None`.
    if std::fs::symlink_metadata(&current).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return None;
    }
    Some(current)
}

/// Explicit binary override slot: the env var plus its current value.
/// The three server resolvers (`llm::resolve_binary*`,
/// `s2s::resolve_audio_server*`, `s2s::resolve_tts_binary`) share one
/// shape — absolute override wins untouched, relative refuses with no
/// search, unset falls through to the order table — which these
/// helpers capture. Per-caller order tables stay at the call sites;
/// only the classification and the gate filter move here.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Explicit binary override slot: the env var plus its current value.
/// The three server resolvers (`llm::resolve_binary*`,
/// `s2s::resolve_audio_server*`, `s2s::resolve_tts_binary`) share one
/// shape — absolute override wins untouched, relative refuses with no
/// search, unset falls through to the order table — which these
/// helpers capture. Per-caller order tables stay at the call sites;
/// only the classification and the gate filter move here.
pub struct ExplicitBinary {
    /// Env var naming the override (kept for diagnostics at call sites).
    pub env_var: &'static str,
    /// Current value of the var; `None` when unset.
    pub path: Option<PathBuf>,
}

impl ExplicitBinary {
    /// Read the override slot from the process environment.
    #[must_use]
    pub fn from_env(env_var: &'static str) -> Self {
        Self {
            env_var,
            path: std::env::var_os(env_var).map(PathBuf::from),
        }
    }
}

/// Every candidate passing [`validate_server_binary`], order
/// preserved, deduplicated by canonical path (the literal join is
/// returned, so symlinked hits keep their table spelling). Matches
/// the `find_all_on_path` dedup contract, so filtering an already
/// deduplicated list is a no-op.
#[must_use]
/// Every candidate passing [`validate_server_binary`], order
/// preserved, deduplicated by canonical path (the literal join is
/// returned, so symlinked hits keep their table spelling). Matches
/// the `find_all_on_path` dedup contract, so filtering an already
/// deduplicated list is a no-op.
pub fn filter_validated_binaries(
    candidates: impl IntoIterator<Item = PathBuf>,
    expected_uid: u32,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for candidate in candidates {
        let canonical = std::fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone());
        if seen.contains(&canonical) {
            continue;
        }
        if validate_server_binary(&candidate, expected_uid, false).is_ok() {
            seen.push(canonical);
            out.push(candidate);
        }
    }
    out
}

/// Resolve one server binary: an absolute explicit override wins
/// untouched (bypass — the caller logs the gate warning and owns the
/// path); a relative override refuses with no search; otherwise the
/// first gate-passing candidate serves (`None` when nothing trusted).
/// Single-hit fail-fast wrappers (`resolve_binary_validated`) keep
/// their own first-hit-then-validate shape on top of
/// [`ExplicitBinary`] instead — the boot paths that try every
/// candidate compose this function.
#[must_use]
/// Resolve one server binary: an absolute explicit override wins
/// untouched (bypass — the caller logs the gate warning and owns the
/// path); a relative override refuses with no search; otherwise the
/// first gate-passing candidate serves (`None` when nothing trusted).
/// Single-hit fail-fast wrappers (`resolve_binary_validated`) keep
/// their own first-hit-then-validate shape on top of
/// [`ExplicitBinary`] instead — the boot paths that try every
/// candidate compose this function.
pub fn resolve_validated_binary(
    explicit: &ExplicitBinary,
    candidates: impl IntoIterator<Item = PathBuf>,
    expected_uid: u32,
) -> Option<PathBuf> {
    match &explicit.path {
        Some(path) if path.is_absolute() => Some(path.clone()),
        Some(_) => None,
        None => filter_validated_binaries(candidates, expected_uid)
            .into_iter()
            .next(),
    }
}

#[cfg(test)]
#[path = "../tests/paths.rs"]
mod tests;

/// Ownership gate for searched server binaries: the canonical path
/// must be absolute, owned by `expected_uid`, carry no group/other
/// write bits, and live outside `/tmp` in a non-world-writable
/// directory. `explicit` marks a user override, which bypasses the
/// gate — the user accepts responsibility for that path. Resolution is
/// hop-capped, never an unbounded `canonicalize`.
pub fn validate_server_binary(
    path: &Path,
    expected_uid: u32,
    explicit: bool,
) -> Result<(), String> {
    if explicit {
        return Ok(());
    }
    if !path.is_absolute() {
        return Err(format!(
            "refusing relative server binary {}; set the explicit binary override to accept responsibility for this path",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let canonical =
            resolve_hop_capped(path, common_core::path::MAX_SYMLINK_HOPS).ok_or_else(|| {
                format!(
                    "cannot resolve server binary {}: hop cap or missing",
                    path.display()
                )
            })?;
        // Component-prefix `/tmp` check (`Path::starts_with` is
        // component-wise, so `/tmpfoo` never matches).
        if canonical.starts_with("/tmp") {
            return Err(format!(
                "refusing server binary {} in world-writable /tmp; set the explicit binary override to accept responsibility for this path",
                path.display()
            ));
        }
        let metadata = std::fs::metadata(&canonical)
            .map_err(|error| format!("cannot stat server binary {}: {error}", path.display()))?;
        let owner = metadata.uid();
        if owner != expected_uid {
            return Err(format!(
                "refusing server binary {} owned by uid {owner}, expected uid {expected_uid}; set the explicit binary override to accept responsibility for this path",
                path.display()
            ));
        }
        let mode = metadata.permissions().mode();
        if mode & 0o022 != 0 {
            return Err(format!(
                "refusing group/other-writable server binary {} (mode {:o}); set the explicit binary override to accept responsibility for this path",
                path.display(),
                mode & 0o777
            ));
        }
        let Some(parent) = canonical.parent() else {
            return Err(format!(
                "refusing server binary {} with no parent directory",
                path.display()
            ));
        };
        // Unreadable parent metadata fails closed as world-writable.
        let dir_mode = std::fs::metadata(parent)
            .map_or(0o777, |metadata| metadata.permissions().mode() & 0o777);
        if dir_mode & 0o002 != 0 {
            return Err(format!(
                "refusing server binary {} in world-writable directory {}; set the explicit binary override to accept responsibility for this path",
                path.display(),
                parent.display()
            ));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, expected_uid);
        Ok(())
    }
}