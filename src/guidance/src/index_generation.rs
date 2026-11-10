//! Generation guard for the derived index: one value over the
//! sidecar manifest plus the index schema versions, checked before
//! any read. A mismatch refuses the read so the caller rebuilds from
//! sidecars, which stay the source of truth. A missing, empty, or
//! corrupt marker also refuses: the crash window between sidecar write
//! and index commit must never serve a mixed pair.

use std::path::Path;

/// Marker file name inside the index directory.
pub const GENERATION_FILE: &str = "index.generation";

/// Verdict of a generation check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationVerdict {
    /// Marker matches the sidecar manifest: reads may proceed.
    Current,
    /// Marker missing, unreadable, or different: refuse and rebuild.
    Mismatch,
}

/// Compute the generation over one sidecar directory: sorted
/// first-line headers plus the fragment and sidecar schema versions.
/// Header-only: bodies never enter the value.
#[must_use]
pub fn generation_of(sidecar_dir: &Path) -> String {
    let mut sidecars = Vec::new();
    crate::walk::walk_files(sidecar_dir, &["json"], |path| {
        sidecars.push(path.to_path_buf());
    });
    sidecars.sort();
    let mut framed = String::new();
    for candidate in &sidecars {
        if let Some(line) = first_line(candidate) {
            framed.push_str(&line);
            framed.push('\0');
        }
    }
    framed.push_str("index-schema\0");
    framed.push_str(&crate::index_pipeline::INDEX_SCHEMA_VERSION.to_string());
    framed.push('\0');
    framed.push_str("sidecar-schema\0");
    framed.push_str(&crate::sync::json_store::SIDECAR_SCHEMA_VERSION.to_string());
    framed.push('\0');
    common_core::hash::blake3_hex(framed.as_bytes())
}

/// Read the stored marker, if any. Empty or whitespace-only counts as
/// absent: a half-committed write refuses like a missing one.
#[must_use]
pub fn read_generation(index_dir: &Path) -> Option<String> {
    let text = common_core::io::read_to_string_err(&index_dir.join(GENERATION_FILE)).ok()?;
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed)
}

/// Atomically store the generation for one index directory.
pub fn write_generation(index_dir: &Path, value: &str) -> std::io::Result<()> {
    common_core::io::ensure_dir(index_dir).map_err(std::io::Error::other)?;
    common_core::io::write_atomic(&index_dir.join(GENERATION_FILE), value.as_bytes())
        .map_err(std::io::Error::other)
}

/// Check the stored marker against the sidecar manifest.
#[must_use]
pub fn verify_generation(index_dir: &Path, sidecar_dir: &Path) -> GenerationVerdict {
    let Some(stored) = read_generation(index_dir) else {
        return GenerationVerdict::Mismatch;
    };
    if stored == generation_of(sidecar_dir) {
        GenerationVerdict::Current
    } else {
        GenerationVerdict::Mismatch
    }
}

fn first_line(path: &Path) -> Option<String> {
    use std::io::BufRead as _;
    let file = std::fs::File::open(path).ok()?;
    std::io::BufReader::new(file).lines().next()?.ok()
}

#[cfg(test)]
#[path = "../tests/generation_guard.rs"]
mod generation_guard_tests;
