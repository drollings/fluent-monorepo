//! Node-sync change-gate goldens: `sync_from_dir` must skip the
//! DELETE + full rebuild when member docs are unchanged, and must
//! still converge on modification, removal, and empty-dir clears.
//! Included via `#[path]` forwarder from `src/db.rs`, per convention.

use super::*;

fn doc(source: &str, member: &str) -> String {
    serde_json::json!({
        "meta": {"source": source, "module": "m", "language": "rust"},
        "comment": "c",
        "members": [{"name": member, "signature": format!("fn {member}()")}]
    })
    .to_string()
}

fn write_doc(dir: &std::path::Path, name: &str, source: &str, member: &str) {
    std::fs::write(dir.join(name), doc(source, member)).expect("write doc");
}

/// Bump a file's mtime into the future: deterministic staleness
/// without sleeping (mirrors the production mtime discipline, where
/// any tick movement is a change).
fn touch_future(path: &std::path::Path) {
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
    std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open")
        .set_modified(later)
        .expect("touch");
}

#[test]
fn node_sync_skips_when_docs_unchanged() {
    // A second sync over untouched member docs writes nothing: the
    // DELETE + full rebuild is pure waste on a warm index.
    let dir = tempfile::tempdir().expect("tempdir");
    let json_dir = dir.path().join("json");
    std::fs::create_dir(&json_dir).expect("mkdir");
    write_doc(&json_dir, "a.json", "src/a.rs", "fn_a");
    write_doc(&json_dir, "b.json", "src/b.rs", "fn_b");
    let db = GuidanceDb::open(&dir.path().join("n.db")).expect("open");
    assert_eq!(db.sync_from_dir(&json_dir).expect("sync"), 2);
    assert_eq!(db.sync_from_dir(&json_dir).expect("sync"), 0);
    assert_eq!(db.get_node_count().expect("count"), 2);
}

#[test]
fn node_sync_resyncs_modified_doc() {
    // Must-NOT-fire control: a moved mtime re-runs the rebuild and
    // the rows stay intact (the gate is whole-sync, not per-doc).
    let dir = tempfile::tempdir().expect("tempdir");
    let json_dir = dir.path().join("json");
    std::fs::create_dir(&json_dir).expect("mkdir");
    write_doc(&json_dir, "a.json", "src/a.rs", "fn_a");
    let db = GuidanceDb::open(&dir.path().join("n.db")).expect("open");
    assert_eq!(db.sync_from_dir(&json_dir).expect("sync"), 1);
    touch_future(&json_dir.join("a.json"));
    assert_eq!(db.sync_from_dir(&json_dir).expect("sync"), 1);
    assert_eq!(db.get_node_count().expect("count"), 1);
}

#[test]
fn node_sync_converges_deleted_doc() {
    // Must-NOT-fire control: removing a doc still removes its nodes
    // (the DELETE path the gate skips must survive for real change).
    let dir = tempfile::tempdir().expect("tempdir");
    let json_dir = dir.path().join("json");
    std::fs::create_dir(&json_dir).expect("mkdir");
    write_doc(&json_dir, "a.json", "src/a.rs", "fn_a");
    write_doc(&json_dir, "b.json", "src/b.rs", "fn_b");
    let db = GuidanceDb::open(&dir.path().join("n.db")).expect("open");
    assert_eq!(db.sync_from_dir(&json_dir).expect("sync"), 2);
    std::fs::remove_file(json_dir.join("b.json")).expect("remove");
    assert_eq!(db.sync_from_dir(&json_dir).expect("sync"), 1);
    assert_eq!(db.get_node_count().expect("count"), 1);
}

#[test]
fn node_sync_dedupes_duplicate_members_within_one_rebuild() {
    // Two docs can describe the same member (the tree has duplicate path
    // spellings); in-memory dedupe matches the old NOT-EXISTS behavior.
    let dir = tempfile::tempdir().expect("tempdir");
    let json_dir = dir.path().join("json");
    std::fs::create_dir(&json_dir).expect("mkdir");
    write_doc(&json_dir, "a.json", "src/a.rs", "same_member");
    write_doc(&json_dir, "b.json", "src/b.rs", "same_member");
    let db = GuidanceDb::open(&dir.path().join("n.db")).expect("open");
    assert_eq!(db.sync_from_dir(&json_dir).expect("sync"), 1);
    assert_eq!(db.get_node_count().expect("count"), 1);
}

#[test]
fn node_sync_empty_dir_clears_stale_nodes() {
    // Must-NOT-fire control: with zero docs the sync always runs, so
    // an emptied doc dir clears nodes exactly as before (never a
    // false-fresh skip over a clear).
    let dir = tempfile::tempdir().expect("tempdir");
    let json_dir = dir.path().join("json");
    std::fs::create_dir(&json_dir).expect("mkdir");
    write_doc(&json_dir, "a.json", "src/a.rs", "fn_a");
    let db = GuidanceDb::open(&dir.path().join("n.db")).expect("open");
    assert_eq!(db.sync_from_dir(&json_dir).expect("sync"), 1);
    std::fs::remove_file(json_dir.join("a.json")).expect("remove");
    assert_eq!(db.sync_from_dir(&json_dir).expect("sync"), 0);
    assert_eq!(db.get_node_count().expect("count"), 0);
}

#[test]
fn legacy_guidance_nodes_table_migrates_to_nodes() {
    // Pre-rename databases keep their rows: open renames the shared
    // node table in place, and the generic API reads through.
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("legacy.db");
    let conn = rusqlite::Connection::open(&db_path).expect("open");
    conn.execute_batch(
        "CREATE TABLE guidance_nodes (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, source TEXT NOT NULL, signature TEXT, comment TEXT, module TEXT NOT NULL, language TEXT NOT NULL DEFAULT 'zig', embedding BLOB, created_at TEXT DEFAULT (datetime('now'))); INSERT INTO guidance_nodes (name, source, module) VALUES ('a', 's', 'm');",
    )
    .expect("seed legacy");
    drop(conn);
    let db = GuidanceDb::open(&db_path).expect("open migrates");
    assert_eq!(db.get_node_count().expect("count"), 1);
    assert_eq!(db.sync_from_dir(dir.path()).expect("sync empty dir"), 0);
}
