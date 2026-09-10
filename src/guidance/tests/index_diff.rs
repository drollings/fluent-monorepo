//! P2 diff tests: added/modified/pending/deleted/unchanged buckets,
//! size+mtime fast path, sha256 tiebreak (G0.7), failed-retry-once.
//! Ports the `index-status` diff-count cases + `computeDiffFromFiles`.

use super::*;
use search_vector::db::ZgFileRecord;

fn scanned(id: &str, size: u64, mtime: i64, hash: Option<&str>) -> ScannedFile {
    ScannedFile {
        id: id.to_string(),
        absolute_path: format!("/repo/{id}"),
        relative_path: id.to_string(),
        root_path: "/repo".to_string(),
        size_bytes: size,
        last_modified_time: mtime,
        kind: Some("code".to_string()),
        format: "rust".to_string(),
        content_hash: hash.map(str::to_string),
    }
}

fn stored(id: &str, size: u64, mtime: i64, hash: Option<&str>, status: Option<&str>) -> ZgFileRecord {
    ZgFileRecord {
        id: id.to_string(),
        absolute_path: format!("/repo/{id}"),
        relative_path: id.to_string(),
        root_path: "/repo".to_string(),
        size_bytes: size,
        last_modified_time: mtime,
        kind: Some("code".to_string()),
        format: "rust".to_string(),
        content_hash: hash.map(str::to_string),
        index_status: status.map(str::to_string),
        fail_count: 0,
        last_error: None,
    }
}

#[test]
fn diff_sorts_files_into_five_buckets() {
    let scanned = vec![
        scanned("new.rs", 10, 100, Some("h-new")),
        scanned("same.rs", 10, 100, None),
        scanned("touched.rs", 10, 200, Some("h-same")),
        scanned("changed.rs", 12, 200, Some("h-changed")),
        scanned("failed.rs", 10, 100, Some("h-failed")),
    ];
    let existing = vec![
        stored("same.rs", 10, 100, Some("h-same"), Some("indexed")),
        stored("touched.rs", 10, 100, Some("h-same"), Some("indexed")),
        stored("changed.rs", 10, 100, Some("h-old"), Some("indexed")),
        stored("failed.rs", 10, 100, Some("h-failed"), Some("failed")),
        stored("gone.rs", 10, 100, Some("h-gone"), Some("indexed")),
    ];
    let diff = compute_diff(&scanned, &existing);
    assert_eq!(ids(&diff.added), vec!["new.rs"]);
    // size+mtime match + stored hash: unchanged without hashing (hash None).
    assert_eq!(ids(&diff.unchanged), vec!["same.rs", "touched.rs"]);
    assert_eq!(ids(&diff.modified), vec!["changed.rs"]);
    // Failed files are retried once via pending.
    assert_eq!(ids(&diff.pending), vec!["failed.rs"]);
    assert_eq!(diff.deleted, vec!["gone.rs".to_string()]);
}

#[test]
fn hash_wins_over_stale_mtime() {
    // mtime differs but content hash agrees: unchanged (G0.7).
    let scanned = vec![scanned("a.rs", 10, 999, Some("h-a"))];
    let existing = vec![stored("a.rs", 10, 100, Some("h-a"), Some("indexed"))];
    let diff = compute_diff(&scanned, &existing);
    assert!(diff.modified.is_empty(), "{diff:?}");
    assert_eq!(ids(&diff.unchanged), vec!["a.rs"]);
}

#[test]
fn missing_hash_forces_modified() {
    // mtime differs and no content hash to tiebreak: modified.
    let scanned = vec![scanned("a.rs", 10, 999, None)];
    let existing = vec![stored("a.rs", 10, 100, None, Some("indexed"))];
    let diff = compute_diff(&scanned, &existing);
    assert_eq!(ids(&diff.modified), vec!["a.rs"]);
}

#[test]
fn file_ids_are_stable() {
    // Identity is the absolute path alone: no root parameter exists, so
    // scoped reconciles reproduce full-run ids by construction.
    assert_eq!(make_file_id("/repo/a.rs"), make_file_id("/repo/a.rs"));
    assert_ne!(make_file_id("/repo/a.rs"), make_file_id("/repo/b.rs"));
    assert_ne!(make_file_id("/r1/a.rs"), make_file_id("/r2/a.rs"));
}

#[test]
fn hashing_is_deterministic() {
    assert_eq!(hash_file_bytes(b"hello"), hash_file_bytes(b"hello"));
    assert_ne!(hash_file_bytes(b"hello"), hash_file_bytes(b"world"));
    assert_eq!(hash_file_bytes(b"hello").len(), 64);
}

fn ids(files: &[ScannedFile]) -> Vec<&str> {
    files.iter().map(|f| f.relative_path.as_str()).collect()
}
