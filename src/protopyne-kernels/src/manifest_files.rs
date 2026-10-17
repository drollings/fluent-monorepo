//! Boot-time manifest driver: the filesystem walk behind the Pine indicator
//! manifest.
//!
//! The language surface ([`crate::pine::manifest`]) is pure over
//! caller-supplied files and owns no path to disk (the no-cache guard pins
//! this). This driver owns the walk instead: it reads `<root>/indicators`
//! and `<root>/libraries` into memory and hands them to the pure builder.
//! Call it from a CLI step or a capability-holding boot path — never from
//! paint code or a `WorkUnit::execute` body.

use std::path::{Path, PathBuf};

use crate::error::KernelError;
use crate::pine::manifest::{IndicatorManifestEntry, ManifestFile, build_manifest};

/// Read every `.pine` file directly under `dir`, sorted by file name. An
/// unreadable file still lists (as empty text, which the builder refuses
/// with a reason) so one bad file never hides its siblings.
fn read_pine_dir(dir: &Path) -> Result<Vec<ManifestFile>, KernelError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| KernelError::UnreadableSource(format!("cannot list {}: {error}", dir.display())))?;
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            KernelError::UnreadableSource(format!("cannot list {}: {error}", dir.display()))
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".pine") {
            names.push(name);
        }
    }
    names.sort();
    let prefix = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(names
        .into_iter()
        .map(|name| {
            let text = std::fs::read_to_string(dir.join(&name)).unwrap_or_default();
            ManifestFile { relpath: format!("{prefix}/{name}"), text }
        })
        .collect())
}

/// Discover every indicator under `<root>/indicators`, resolving imports
/// against `<root>/libraries`. Sorted by id; a rerun over unchanged files
/// is identical. A missing root refuses; an empty directory yields no rows.
pub fn build_indicator_manifest(data_root: &Path) -> Result<Vec<IndicatorManifestEntry>, KernelError> {
    let root = PathBuf::from(data_root);
    let indicators = read_pine_dir(&root.join("indicators"))?;
    let libraries = read_pine_dir(&root.join("libraries")).unwrap_or_default();
    Ok(build_manifest(&indicators, &libraries))
}

#[cfg(test)]
#[path = "../tests/pine_manifest.rs"]
mod tests;
