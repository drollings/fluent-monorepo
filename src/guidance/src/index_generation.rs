//! Generation guard for the derived index: one value over the
//! sidecar manifest plus the index schema versions, checked before
//! any read. A mismatch refuses the read so the caller rebuilds from
//! sidecars, which stay the source of truth. A missing, empty, or
//! corrupt marker also refuses: the crash window between sidecar write
//! and index commit must never serve a mixed pair.

use std::path::Path;

/// Marker file name inside the index directory.
pub const GENERATION_FILE: &str = "index.generation";

/// Index directory for one database path: markers live beside the DB.
#[must_use]
pub fn index_dir_for_db(db_path: &Path) -> std::path::PathBuf {
    db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

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
    format!(
        "{}:{}",
        schema_generation(),
        content_generation(sidecar_dir)
    )
}

/// Schema half of the marker: index and sidecar versions only.
/// A schema mismatch means stored rows may misread: rebuild fully.
/// Content drift alone needs only the incremental ingest.
#[must_use]
pub fn schema_generation() -> String {
    let mut framed = String::new();
    framed.push_str("index-schema\0");
    framed.push_str(&crate::index_pipeline::INDEX_SCHEMA_VERSION.to_string());
    framed.push('\0');
    framed.push_str("sidecar-schema\0");
    framed.push_str(&crate::sync::json_store::SIDECAR_SCHEMA_VERSION.to_string());
    framed.push('\0');
    common_core::hash::blake3_hex(framed.as_bytes())
}

/// Content half of the marker: sorted sidecar header lines.
/// Drifts on every edit; the incremental ingest reconverges it.
#[must_use]
pub fn content_generation(sidecar_dir: &Path) -> String {
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
    common_core::hash::blake3_hex(framed.as_bytes())
}

/// Schema half recorded in a marker, if the marker parses.
/// Missing, empty, or unstructured markers read as unknown (rebuild),
/// never as a schema.
#[must_use]
pub fn stored_schema(index_dir: &Path) -> Option<String> {
    let text = common_core::io::read_to_string_err(&index_dir.join(GENERATION_FILE)).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (schema, content) = trimmed.split_once(':')?;
    if schema.is_empty() || content.is_empty() {
        return None;
    }
    Some(schema.to_string())
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

/// Tree-fingerprint marker beside the generation marker.
pub const FINGERPRINT_FILE: &str = "index.fingerprint";

/// Tree-level change fingerprint: sorted (file id, content hash,
/// sig-set hash) triples over the indexed tree. Identity for the
/// propagation gate — no confidence value is involved.
#[must_use]
pub fn tree_fingerprint(db: &crate::query::search_db::GuidanceSearchDb) -> String {
    use std::collections::HashMap;
    let files = db.list_files().unwrap_or_default();
    let symbols = db.symbol_interface_hashes().unwrap_or_default();
    let mut sig_sets: HashMap<&str, Vec<&str>> = HashMap::new();
    for (file_id, symbol_id, hash) in &symbols {
        // Module headers carry comments, never interfaces.
        if symbol_id.ends_with("#$file") {
            continue;
        }
        sig_sets
            .entry(file_id.as_str())
            .or_default()
            .push(hash.as_str());
    }
    let mut rows: Vec<(String, String, String)> = files
        .iter()
        .map(|file| {
            let mut hashes = sig_sets.get(file.id.as_str()).cloned().unwrap_or_default();
            hashes.sort_unstable();
            let mut framed = String::new();
            for hash in hashes {
                framed.push_str(hash);
                framed.push('\0');
            }
            (
                file.id.clone(),
                file.content_hash.clone().unwrap_or_default(),
                common_core::hash::blake3_hex(framed.as_bytes()),
            )
        })
        .collect();
    rows.sort();
    let mut frame = String::new();
    for (id, content, sig) in rows {
        frame.push_str(&id);
        frame.push('\0');
        frame.push_str(&content);
        frame.push('\0');
        frame.push_str(&sig);
        frame.push('\0');
    }
    common_core::hash::blake3_hex(frame.as_bytes())
}

/// Read the stored tree fingerprint, if any.
#[must_use]
pub fn read_fingerprint(index_dir: &Path) -> Option<String> {
    let text = common_core::io::read_to_string_err(&index_dir.join(FINGERPRINT_FILE)).ok()?;
    let trimmed = text.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Atomically store the tree fingerprint for one index directory.
pub fn write_fingerprint(index_dir: &Path, value: &str) -> std::io::Result<()> {
    common_core::io::ensure_dir(index_dir).map_err(std::io::Error::other)?;
    common_core::io::write_atomic(&index_dir.join(FINGERPRINT_FILE), value.as_bytes())
        .map_err(std::io::Error::other)
}

/// Check the stored marker against the sidecar manifest.
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
    // M3.3: shared header-line reader.
    common_core::io::read_first_line(path)
}

#[cfg(test)]
#[path = "../tests/generation_guard.rs"]
mod generation_guard_tests;
