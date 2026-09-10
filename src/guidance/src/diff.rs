//! P2 diff: scanned files vs stored records into
//! added/modified/pending/deleted/unchanged (port of `computeDiffFromFiles`).
//! Freshness authority (G0.7): size+mtime is the fast path, the sha256
//! content hash is the tiebreak — hash wins on disagreement.

use search_vector::db::ZgFileRecord;

/// A scanned file with its content hash (when computed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFile {
    /// Stable file id ([`make_file_id`]).
    pub id: String,
    /// Absolute path.
    pub absolute_path: String,
    /// Root-relative path.
    pub relative_path: String,
    /// Owning root.
    pub root_path: String,
    /// Size in bytes.
    pub size_bytes: u64,
    /// Last-modified time (ms epoch).
    pub last_modified_time: i64,
    /// Kind tag.
    pub kind: Option<String>,
    /// Format tag.
    pub format: String,
    /// Content hash (sha256 hex), when computed.
    pub content_hash: Option<String>,
}

/// Five-bucket diff result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffResult {
    /// Never indexed before.
    pub added: Vec<ScannedFile>,
    /// Content changed since the stored record.
    pub modified: Vec<ScannedFile>,
    /// Stored but never successfully indexed, or failed (retry once).
    pub pending: Vec<ScannedFile>,
    /// Stored but no longer on disk (file ids).
    pub deleted: Vec<String>,
    /// Stored and current.
    pub unchanged: Vec<ScannedFile>,
}

/// Stable file id: the absolute path alone. Selection roots are scan
/// scope, never identity — a scoped reconcile must reproduce the full
/// run's ids or every file looks deleted+added (P3 gate finding).
#[must_use]
pub fn make_file_id(absolute_path: &str) -> String {
    common_core::hash::sha256_hex(absolute_path.as_bytes())
}

/// sha256 hex of file bytes (content-hash tiebreak).
#[must_use]
pub fn hash_file_bytes(bytes: &[u8]) -> String {
    common_core::hash::sha256_hex(bytes)
}

/// Hash the file at `path`; `None` when the file cannot be read
/// (the caller treats unreadable files as modified, never as unchanged).
#[must_use]
pub fn hash_file(path: &std::path::Path) -> Option<String> {
    std::fs::read(path).ok().map(|bytes| hash_file_bytes(&bytes))
}

/// Sort `scanned` against `existing` into the five buckets.
#[must_use]
pub fn compute_diff(scanned: &[ScannedFile], existing: &[ZgFileRecord]) -> DiffResult {
    let mut diff = DiffResult::default();
    let by_id: std::collections::HashMap<&str, &ZgFileRecord> =
        existing.iter().map(|file| (file.id.as_str(), file)).collect();
    let mut seen = std::collections::HashSet::new();
    for file in scanned {
        seen.insert(file.id.as_str());
        let Some(stored) = by_id.get(file.id.as_str()) else {
            diff.added.push(file.clone());
            continue;
        };
        if stored.index_status.as_deref() == Some("failed") || stored.index_status.is_none() {
            diff.pending.push(file.clone());
            continue;
        }
        if stored.size_bytes == file.size_bytes
            && stored.last_modified_time == file.last_modified_time
            && stored.content_hash.is_some()
        {
            diff.unchanged.push(file.clone());
            continue;
        }
        // Fast path disagrees: the content hash decides (G0.7).
        if let (Some(want), Some(have)) = (stored.content_hash.as_ref(), file.content_hash.as_ref()) {
            if want == have && stored.size_bytes == file.size_bytes {
                diff.unchanged.push(file.clone());
                continue;
            }
        }
        diff.modified.push(file.clone());
    }
    for stored in existing {
        if !seen.contains(stored.id.as_str()) {
            diff.deleted.push(stored.id.clone());
        }
    }
    diff
}

#[cfg(test)]
#[path = "../tests/index_diff.rs"]
mod tests;
