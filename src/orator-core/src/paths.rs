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
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Whether `candidate` stays inside `root` after resolving symlinks.
/// Missing paths and escapes both fail closed (`false`).
#[must_use]
pub fn within_root(root: &Path, candidate: &Path) -> bool {
    let (Ok(root), Ok(candidate)) = (root.canonicalize(), candidate.canonicalize()) else {
        return false;
    };
    candidate.starts_with(root)
}

/// Ownership gate for searched server binaries: the canonical path
/// must be owned by `expected_uid`, carry no group/other write bits,
/// and live outside `/tmp` in a non-world-writable directory.
/// `explicit` marks a user override, which bypasses the gate — the
/// user accepts responsibility for that path.
pub fn validate_server_binary(
    path: &Path,
    expected_uid: u32,
    explicit: bool,
) -> Result<(), String> {
    if explicit {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let canonical = std::fs::canonicalize(path).map_err(|error| {
            format!(
                "cannot resolve server binary {}: {error}",
                path.display()
            )
        })?;
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
