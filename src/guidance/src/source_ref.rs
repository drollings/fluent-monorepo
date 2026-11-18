//! One source identity for findings, citations, globs, and live reads.
//!
//! Paths arrive from three places (workspace, source root, filesystem).
//! Carrying all three in one value prevents every consumer from
//! re-deriving the same relativization and disagreeing about staleness.

use std::path::{Path, PathBuf};

use crate::search_types::FileInfo;

/// A source file with its three path views.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceRef {
    /// Workspace-relative path: citation, display, and glob identity.
    pub workspace_path: String,
    /// Source-root-relative path retained for receipt provenance.
    pub root_path: String,
    /// Absolute path used for live reads only.
    pub absolute_path: String,
}

impl SourceRef {
    /// Build from an indexed file record.
    #[must_use]
    pub fn from_file(file: &FileInfo) -> Self {
        Self {
            workspace_path: normalize(&file.relative_path),
            root_path: normalize(&file.root_path),
            absolute_path: file.absolute_path.clone(),
        }
    }

    /// Build from a workspace root and one concrete path.
    #[must_use]
    pub fn from_workspace(workspace: &Path, path: &Path) -> Self {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            workspace.join(path)
        };
        let workspace_path = absolute.strip_prefix(workspace).map_or_else(
            |_| normalize(&absolute.to_string_lossy()),
            |rel| normalize(&rel.to_string_lossy()),
        );
        let root_path = workspace_path.clone();
        let absolute_path = if Path::new(&absolute).starts_with(workspace) {
            workspace.join(&workspace_path)
        } else {
            absolute
        };
        Self {
            workspace_path,
            root_path,
            absolute_path: absolute_path.to_string_lossy().into_owned(),
        }
    }

    /// The path a reader and citation display.
    #[must_use]
    pub fn display(&self) -> &str {
        &self.workspace_path
    }

    /// The path live bytes are read from.
    #[must_use]
    pub fn live_path(&self) -> PathBuf {
        PathBuf::from(&self.absolute_path)
    }

    /// One citation for a line range.
    #[must_use]
    pub fn citation(&self, start: Option<u32>, end: Option<u32>) -> Citation {
        Citation {
            source: self.clone(),
            start,
            end,
            stale: false,
            unverified: false,
            digest: String::new(),
        }
    }
}

impl std::fmt::Display for SourceRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.workspace_path)
    }
}

/// A displayed citation with its verification state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    /// Source identity.
    pub source: SourceRef,
    /// First line, when the citation is a range.
    pub start: Option<u32>,
    /// Last line, when the citation is a range.
    pub end: Option<u32>,
    /// Live bytes no longer match the indexed row.
    pub stale: bool,
    /// The excerpt could not be verified against the stored row.
    pub unverified: bool,
    /// Digest of the served excerpt (`blake3_hex`, same scheme as
    /// project cards). Binds the stamp to the rendered text.
    pub digest: String,
}

impl Citation {
    /// Mark this citation stale or unverified.
    #[must_use]
    pub fn with_status(mut self, stale: bool, unverified: bool) -> Self {
        self.stale = stale;
        self.unverified = unverified;
        self
    }

    /// Stamp the served-excerpt digest (`blake3_hex` of the rendered
    /// text, same scheme as project cards).
    #[must_use]
    pub fn with_digest(mut self, digest: String) -> Self {
        self.digest = digest;
        self
    }

    /// Stable `path:start-end` text (without the status suffix).
    #[must_use]
    pub fn text(&self) -> String {
        match (self.start, self.end) {
            (Some(start), Some(end)) if start != end => {
                format!("{}:{start}-{end}", self.source.display())
            }
            (Some(start), _) => format!("{}:{start}", self.source.display()),
            _ => self.source.display().to_string(),
        }
    }
}

impl std::fmt::Display for Citation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text())?;
        if self.stale {
            f.write_str(" (stale)")?;
        } else if self.unverified {
            f.write_str(" (unverified)")?;
        }
        Ok(())
    }
}

impl serde::Serialize for Citation {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

fn normalize(path: &str) -> String {
    common_core::path::normalize_lexical(path)
        .trim_start_matches("./")
        .to_string()
}

#[cfg(test)]
#[path = "../tests/source_ref.rs"]
mod source_ref_tests;

#[cfg(test)]
#[path = "../tests/citation_truth.rs"]
mod citation_truth_tests;
