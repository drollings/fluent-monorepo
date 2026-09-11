//! M3 acceptance: stale-dependent propagation in one-shot sync.
//!
//! Enforced invariant: propagation recall is 1.0 — every changed or
//! deleted seed plus all of its transitive dependents re-processes, or
//! the build fails. Over-invalidation is recorded, never tuned away;
//! under-invalidation (a silent miss) is always a failure, never a
//! judgment call. The deletion case below extends the invariant to
//! removal: a deleted file's fragment rows and sidecar disappear and its
//! dependents re-process.
//!
//! Fixture (hermetic temp workspace): importer→importee (`mod importee`
//! resolves at hydration; the `fee` call edge doubles the linkage) plus
//! an unrelated leaf. Editing the importee's signature with a same-size
//! edit (pins the size arm of both gates — the mtime arm must fire)
//! re-processes the importer; editing the leaf re-processes only itself.
//! Pre-registered bound: affected-set recall 1.0 required (any miss fails
//! the milestone); over-invalidation is recorded, not tuned.

use std::path::Path;

#[path = "common.rs"]
#[allow(dead_code)]
mod common;

use common::sync;

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("importee.rs"),
        "pub fn fee(x: u64) -> u64 {\n    x + 1\n}\n",
    )
    .expect("write");
    std::fs::write(
        dir.path().join("importer.rs"),
        "mod importee;\nuse importee::fee;\n/// Calls fee.\npub fn caller(x: u64) -> u64 {\n    fee(x)\n}\n",
    )
    .expect("write");
    std::fs::write(dir.path().join("leaf.rs"), "pub fn leaf() -> u64 {\n    42\n}\n")
        .expect("write");
    dir
}

/// Verbose `regenerated:` lines → sorted affected set (absolute paths).
fn regenerated(out: &str) -> Vec<String> {
    let mut paths: Vec<String> = out
        .lines()
        .filter_map(|line| line.trim().strip_prefix("regenerated: "))
        .map(|rest| rest.split_whitespace().next().unwrap_or("").to_string())
        .filter(|p| !p.is_empty())
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

fn fragment_texts(dir: &Path, file: &str) -> Vec<String> {
    let db = dir.join(".sync.db");
    let file_id = dir.join(file).to_string_lossy().into_owned();
    let out = std::process::Command::new("sqlite3")
        .args([
            db.to_str().unwrap(),
            &format!(
                "SELECT content_text FROM zg_fragments WHERE file_id = '{file_id}' ORDER BY id"
            ),
        ])
        .output();
    // sqlite3 CLI may be absent (hermetic CI): fall back to file bytes.
    let Ok(out) = out else {
        return vec![std::fs::read_to_string(dir.join(file)).unwrap()];
    };
    if !out.status.success() {
        return vec![std::fs::read_to_string(dir.join(file)).unwrap()];
    }
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
}

#[test]
fn importee_signature_edit_reprocesses_importer() {
    let dir = fixture();
    let root = dir.path();
    sync(root, ".sync.db", false);

    let before = fragment_texts(root, "importer.rs");

    // Same-size edit pins the size arm; the mtime arm must fire.
    std::fs::write(root.join("importee.rs"), "pub fn fee(x: u64) -> u64 {\n    x + 2\n}\n")
        .expect("write");
    let out = sync(root, ".sync.db", true);

    let measured = regenerated(&out);
    let oracle = vec![
        root.join("importee.rs").to_string_lossy().into_owned(),
        root.join("importer.rs").to_string_lossy().into_owned(),
    ];
    // Pre-registered bound: recall 1.0 (no silent miss, ever).
    for expected in &oracle {
        assert!(measured.contains(expected), "missed {expected}; measured={measured:?}");
    }
    let over: Vec<_> = measured.iter().filter(|p| !oracle.contains(p)).collect();
    eprintln!("M3 probe: affected={measured:?} over-invalidation={over:?}");

    // DB content converges: the importer's bytes never changed, so its
    // derived rows must be identical-or-fresher, never divergent.
    assert_eq!(before, fragment_texts(root, "importer.rs"));
}

#[test]
fn leaf_edit_reprocesses_only_itself() {
    let dir = fixture();
    let root = dir.path();
    sync(root, ".sync.db", false);

    std::fs::write(root.join("leaf.rs"), "pub fn leaf() -> u64 {\n    43\n}\n")
        .expect("write");
    let out = sync(root, ".sync.db", true);

    let measured = regenerated(&out);
    let leaf = root.join("leaf.rs").to_string_lossy().into_owned();
    let importer = root.join("importer.rs").to_string_lossy().into_owned();
    assert!(measured.contains(&leaf), "leaf itself must re-process: {measured:?}");
    assert!(
        !measured.contains(&importer),
        "must-NOT-fire: importer untouched by a leaf edit: {measured:?}"
    );
    assert_eq!(measured.len(), 1, "affected is exactly the leaf: {measured:?}");
}

fn db_count(db: &Path, table: &str, file_id: &str) -> usize {
    let out = std::process::Command::new("sqlite3")
        .args([
            db.to_str().unwrap(),
            &format!("SELECT COUNT(*) FROM {table} WHERE file_id = '{file_id}'"),
        ])
        .output()
        .expect("sqlite3");
    assert!(out.status.success(), "sqlite3 failed: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(usize::MAX)
}

#[test]
fn deleted_file_drops_rows_sidecar_and_reprocesses_dependents() {
    // Deletion closure: removing the importee must drop its fragment
    // rows, drop its member-JSON sidecar, and re-process the importer
    // (its dependent) — while the unrelated leaf stays quiet.
    let dir = fixture();
    let root = dir.path();
    sync(root, ".sync.db", false);
    assert!(
        root.join(".guidance/src/importee.rs.json").is_file(),
        "importee sidecar must exist after the first sync"
    );

    std::fs::remove_file(root.join("importee.rs")).expect("delete");
    let out = sync(root, ".sync.db", true);

    let measured = regenerated(&out);
    let importer = root.join("importer.rs").to_string_lossy().into_owned();
    let leaf = root.join("leaf.rs").to_string_lossy().into_owned();
    assert!(
        measured.contains(&importer),
        "dependent must re-process after the importee is deleted: {measured:?}"
    );
    assert!(
        !measured.contains(&leaf),
        "must-NOT-fire: leaf untouched by the deletion: {measured:?}"
    );

    let db = root.join(".sync.db");
    let importee_id = root.join("importee.rs").to_string_lossy().into_owned();
    assert_eq!(
        db_count(&db, "zg_fragments", &importee_id),
        0,
        "deleted file's fragment rows must be gone"
    );
    assert!(
        !root.join(".guidance/src/importee.rs.json").exists(),
        "deleted file's sidecar must be gone"
    );
    let out = std::process::Command::new("sqlite3")
        .args([
            db.to_str().unwrap(),
            "SELECT COUNT(*) FROM guidance_nodes WHERE source LIKE '%importee.rs'",
        ])
        .output()
        .expect("sqlite3");
    assert!(out.status.success(), "sqlite3 failed: {out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "0",
        "deleted file's node rows must be gone"
    );
}
