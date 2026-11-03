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
/// hop-capped (40) with no unbounded `canonicalize`: loops and deep
/// chains refuse instead of hanging or resolving outside.
#[must_use]
pub fn within_root(root: &Path, candidate: &Path) -> bool {
    let Some(root) = resolve_hop_capped(root, 40) else {
        return false;
    };
    let Some(resolved) = resolve_hop_capped(candidate, 40) else {
        return false;
    };
    resolved.starts_with(root)
}

/// 12-char fingerprint for an explicit binary path (for logs): sha256
/// of the path string, not the file contents (instant even for
/// gigabyte weights; the content hash gate lives in `verify_entry`).
#[must_use]
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
    use std::path::Component;
    // Lexical normalize first so `..` cannot skip out before the walk.
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !current.pop() {
                    return None;
                }
            }
            Component::CurDir => {}
            other => current.push(other.as_os_str()),
        }
    }
    for _ in 0..hops {
        let meta = std::fs::symlink_metadata(&current).ok()?;
        if !meta.file_type().is_symlink() {
            return Some(current);
        }
        let target = std::fs::read_link(&current).ok()?;
        let joined = if target.is_absolute() {
            target
        } else {
            current.parent().map_or(target.clone(), |dir| dir.join(&target))
        };
        let mut normal = PathBuf::new();
        for component in joined.components() {
            match component {
                Component::ParentDir => {
                    if !normal.pop() {
                        return None;
                    }
                }
                Component::CurDir => {}
                other => normal.push(other.as_os_str()),
            }
        }
        current = normal;
    }
    // Budget exhausted: only accept when the landing is no longer a
    // symlink; loops stay `None`.
    if std::fs::symlink_metadata(&current)
        .is_ok_and(|meta| meta.file_type().is_symlink())
    {
        return None;
    }
    Some(current)
}

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
        let canonical = resolve_hop_capped(path, 40).ok_or_else(|| {
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
        let metadata = std::fs::metadata(&canonical).map_err(|error| {
            format!("cannot stat server binary {}: {error}", path.display())
        })?;
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
        let dir_mode = std::fs::metadata(parent).map_or(0o777, |metadata| {
            metadata.permissions().mode() & 0o777
        });
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
