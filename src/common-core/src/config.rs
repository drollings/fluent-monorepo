//! JSON config loaders: `load_json_or_default` (fallback to `T::default()`) and `load_json` (strict).

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use crate::error::IoError;

/// Load a JSON config file, falling back to `T::default()` if the file is
/// missing or cannot be read.
///
/// This is the "load-or-default" pattern: read, parse, and on any failure
/// return the type's default.  When the file **exists but deserialization
/// fails**, a warning is emitted to stderr so that silent config skew
/// (e.g. forward-looking JSON fields that don't match current Rust types)
/// does not go unnoticed.
pub fn load_json_or_default<T: DeserializeOwned + Default>(path: &Path) -> T {
    match load_json(path) {
        Ok(t) => t,
        Err(e) => {
            if path.exists() {
                eprintln!(
                    "WARNING: config file '{}' exists but failed to parse: {}. Falling back to default.",
                    path.display(),
                    e
                );
            }
            T::default()
        }
    }
}

/// Load a JSON config file strictly — errors on missing file, invalid JSON,
/// or I/O failure.
pub fn load_json<T: DeserializeOwned>(path: &Path) -> Result<T, IoError> {
    let content = crate::io::read_to_string_err(path)?;
    serde_json::from_str(&content).map_err(|e| {
        IoError(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("JSON parse error: {e}"),
        ))
    })
}

/// Return the first candidate that exists as a file, in order (project
/// beats user beats absent). Callers build the ordered list — typically
/// `[project_dir.join(rel), user_config_dir.join(rel)]` — so levels stay
/// composable and unit tests stay hermetic (no ambient directories).
///
/// # Examples
///
/// ```no_run
/// use std::path::PathBuf;
/// use common_core::config::find_hierarchical;
///
/// let found = find_hierarchical(&[
///     PathBuf::from("/repo/.guidance/guidance-config.json"),
///     PathBuf::from("/home/u/.config/guidance/guidance-config.json"),
/// ]);
/// ```
/// Walk up from `start` (a file path; need not exist) looking for
/// `filename`, checking at most `cap` directories. Past the filesystem
/// root the walk ends with `None`. Caps stay per-caller: a search bound,
/// never a quality threshold.
#[must_use]
pub fn find_ancestor_file(start: &Path, filename: &str, cap: usize) -> Option<PathBuf> {
    let mut dir = start.parent()?.to_path_buf();
    for _ in 0..cap {
        let candidate = dir.join(filename);
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
    None
}

#[must_use]
pub fn find_hierarchical(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|path| path.is_file()).cloned()
}

/// Home directory, falling back to `.` when it is unreadable — never a
/// construction failure. The single spelling for home-anchored defaults
/// (memory DB, state files).
#[must_use]
pub fn home_or_dot() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}
