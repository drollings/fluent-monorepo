//! P3 change tracking (port of zvec-grep `src/daemon/change-set.ts` verbatim
//! semantics): watcher events accumulate as touched files, rescan
//! directories, and deleted prefixes with a 1000-path compaction budget.
//! `.gitignore` changes escalate to a parent-directory rescan; storms
//! compact to directory scopes without forcing a full reconciliation.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use thiserror::Error;

/// Absolute-path violation at the `add` boundary (zvec throws).
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ChangeSetError {
    /// Changed paths must be absolute.
    #[error("changed paths must be absolute: {0}")]
    NotAbsolute(String),
}

/// Watcher event kind (zvec `ChangeKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// Created (rename-to or new directory tree).
    Created,
    /// Changed (content event on an existing path).
    Changed,
    /// Deleted (rename-away / unlink).
    Deleted,
}

/// Drained batch handed to the coordinator (zvec `ChangeSetSnapshot`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSetSnapshot {
    /// Exact files to re-index.
    pub touched_files: Vec<String>,
    /// Directories to rescan (storms, `.gitignore` escalation).
    pub rescan_directories: Vec<String>,
    /// Deleted path prefixes (collapse children).
    pub deleted_prefixes: Vec<String>,
    /// Full reconcile requested (overflow without root, watcher errors).
    pub force_full_reconcile: bool,
}

/// Construction options (zvec `ChangeSetOptions`).
#[derive(Debug, Clone)]
pub struct ChangeSetOptions {
    /// Workspace root (anchors budget widening; `None` ⇒ overflow forces full).
    pub root: Option<String>,
    /// Path budget before compaction (zvec default 1000).
    pub max_changed_paths: usize,
}

impl Default for ChangeSetOptions {
    fn default() -> Self {
        Self {
            root: None,
            max_changed_paths: crate::search_constants::CHANGE_SET_PATH_BUDGET,
        }
    }
}

/// Accumulating watcher-event batch (zvec `ChangeSet`).
#[derive(Debug)]
pub struct ChangeSet {
    touched_files: HashSet<String>,
    rescan_directories: HashSet<String>,
    deleted_prefixes: HashSet<String>,
    force_full_reconcile: bool,
    root: Option<String>,
    max_changed_paths: usize,
}

impl ChangeSet {
    /// Create an empty batch.
    #[must_use]
    pub fn new(options: ChangeSetOptions) -> Self {
        Self {
            touched_files: HashSet::new(),
            rescan_directories: HashSet::new(),
            deleted_prefixes: HashSet::new(),
            force_full_reconcile: false,
            root: options.root.map(|root| normalize_path(&root)),
            max_changed_paths: options.max_changed_paths.max(1),
        }
    }

    /// Record one watcher event (zvec `add`).
    pub fn add(
        &mut self,
        path: &str,
        kind: ChangeKind,
        is_directory: bool,
    ) -> Result<(), ChangeSetError> {
        if self.force_full_reconcile && self.size() > 0 {
            return Ok(());
        }
        if !is_absolute_path(path) {
            return Err(ChangeSetError::NotAbsolute(path.to_string()));
        }
        let normalized = normalize_path(path);
        if path_covered_by(&self.rescan_directories, &normalized)
            || path_covered_by(&self.deleted_prefixes, &normalized)
        {
            return Ok(());
        }
        if file_name(&normalized) == Some(".gitignore") {
            if let Some(parent) = parent_dir(&normalized) {
                self.rescan_directories.insert(parent);
            }
        } else if kind == ChangeKind::Deleted {
            self.deleted_prefixes.insert(normalized);
        } else if is_directory {
            self.rescan_directories.insert(normalized);
        } else {
            self.touched_files.insert(normalized);
        }
        if self.size() >= self.max_changed_paths {
            self.enforce_path_budget();
        }
        Ok(())
    }

    /// Demand a full reconciliation on drain (watcher errors, resume drift).
    pub fn require_full_reconcile(&mut self) {
        self.force_full_reconcile = true;
    }

    /// Merge a drained snapshot back (retry / coalescing; zvec `merge`).
    pub fn merge(&mut self, other: &ChangeSetSnapshot) {
        if self.force_full_reconcile && self.size() > 0 {
            return;
        }
        self.touched_files.extend(other.touched_files.iter().cloned());
        self.rescan_directories.extend(other.rescan_directories.iter().cloned());
        self.deleted_prefixes.extend(other.deleted_prefixes.iter().cloned());
        self.force_full_reconcile |= other.force_full_reconcile;
        if !self.force_full_reconcile && self.size() >= self.max_changed_paths {
            self.enforce_path_budget();
        }
    }

    /// Drain a sorted, collapsed snapshot (zvec `snapshot`).
    #[must_use]
    pub fn snapshot(&mut self) -> ChangeSetSnapshot {
        self.collapse_paths();
        let touched_files =
            common_core::sort::sorted_vec(self.touched_files.iter().cloned().collect());
        let rescan_directories =
            common_core::sort::sorted_vec(self.rescan_directories.iter().cloned().collect());
        let deleted_prefixes =
            common_core::sort::sorted_vec(self.deleted_prefixes.iter().cloned().collect());
        ChangeSetSnapshot {
            touched_files,
            rescan_directories,
            deleted_prefixes,
            force_full_reconcile: self.force_full_reconcile,
        }
    }

    /// Total tracked paths (zvec `size`).
    #[must_use]
    pub fn size(&self) -> usize {
        self.touched_files.len() + self.rescan_directories.len() + self.deleted_prefixes.len()
    }

    /// True when nothing is pending (zvec `empty`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.size() == 0 && !self.force_full_reconcile
    }

    fn collapse_paths(&mut self) {
        collapse_set(&mut self.rescan_directories);
        collapse_set(&mut self.deleted_prefixes);
        self.touched_files.retain(|file| {
            !has_ancestor(&self.rescan_directories, file)
                && !has_ancestor(&self.deleted_prefixes, file)
        });
        self.rescan_directories
            .retain(|directory| !has_ancestor(&self.deleted_prefixes, directory));
    }

    fn enforce_path_budget(&mut self) {
        self.collapse_paths();
        if self.size() < self.max_changed_paths {
            return;
        }
        let Some(root) = self.root.clone() else {
            self.force_full_reconcile = true;
            return;
        };
        // Exact watcher events stay trustworthy when the batch is large:
        // widen leaf scopes to parent directories to bound memory without
        // turning an exact update into a full reconciliation.
        let leaf_scopes: Vec<String> = self
            .touched_files
            .iter()
            .chain(self.deleted_prefixes.iter())
            .cloned()
            .collect();
        if !leaf_scopes.is_empty() {
            self.touched_files.clear();
            self.deleted_prefixes.clear();
            for path in &leaf_scopes {
                self.rescan_directories.insert(parent_scope(&root, path));
            }
            self.collapse_paths();
        }
        while self.size() >= self.max_changed_paths {
            let previous_size = self.size();
            let widened: Vec<String> = self
                .rescan_directories
                .iter()
                .map(|path| parent_scope(&root, path))
                .collect();
            self.rescan_directories.clear();
            self.rescan_directories.extend(widened);
            self.collapse_paths();
            if self.size() >= previous_size {
                break;
            }
        }
    }
}

/// Widen one scope level toward the workspace root (zvec `parentScope`).
fn parent_scope(root: &str, path: &str) -> String {
    if path == root {
        return path.to_string();
    }
    let parent = parent_dir(path).unwrap_or_else(|| root.to_string());
    if parent == *path {
        return root.to_string();
    }
    if parent == *root || parent.starts_with(&format!("{root}/")) {
        return parent;
    }
    root.to_string()
}

fn collapse_set(paths: &mut HashSet<String>) {
    let sorted: Vec<String> = common_core::sort::sorted_by_vec(
        paths.iter().cloned().collect(),
        |left, right| left.len().cmp(&right.len()),
    );
    for path in sorted {
        if has_ancestor(paths, &path) {
            paths.remove(&path);
        }
    }
}

fn path_covered_by(paths: &HashSet<String>, path: &str) -> bool {
    paths.contains(path) || has_ancestor(paths, path)
}

fn has_ancestor(paths: &HashSet<String>, target: &str) -> bool {
    let mut current = parent_dir(target);
    while let Some(dir) = current {
        if paths.contains(&dir) {
            return true;
        }
        let next = parent_dir(&dir);
        if next.as_deref() == Some(&dir) || next.is_none() {
            break;
        }
        current = next;
    }
    false
}

pub(crate) fn is_absolute_path(path: &str) -> bool {
    common_core::path::is_absolute_lexical(path)
}

/// Normalize separators to `/` and clean `.`/`..` lexically (zvec
/// `normalizePath`; no I/O so deleted paths stay representable).
/// Delegates to the canonical `common_core::path::normalize_lexical`.
pub(crate) fn normalize_path(path: &str) -> String {
    common_core::path::normalize_lexical(path)
}

pub(crate) fn parent_dir(path: &str) -> Option<String> {
    common_core::path::parent_lexical(path)
}

pub(crate) fn file_name(path: &str) -> Option<&str> {
    common_core::path::file_name_lexical(path)
}

#[cfg(test)]
#[path = "../tests/change_set.rs"]
mod tests;
