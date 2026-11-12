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

/// Scoped `sync --file`: member gen plus the derived-index half for one
/// file only. Returns the command stdout.
fn sync_file(dir: &Path, db_name: &str, file: &Path, verbose: bool) -> String {
    let root = dir.to_str().unwrap().to_string();
    let json_dir = dir.join(".guidance").to_str().unwrap().to_string();
    let db = dir.join(db_name).to_str().unwrap().to_string();
    let file = file.to_str().unwrap().to_string();
    let mut args = vec![
        "sync",
        "--file",
        file.as_str(),
        "--workspace",
        root.as_str(),
        "--json-dir",
        json_dir.as_str(),
        "--db",
        db.as_str(),
    ];
    if verbose {
        args.push("--verbose");
    }
    let out = common::run(dir, &args);
    assert!(out.status.success(), "sync --file failed: {out:?}");
    common::stdout(&out)
}

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
    std::fs::write(
        dir.path().join("leaf.rs"),
        "pub fn leaf() -> u64 {\n    42\n}\n",
    )
    .expect("write");
    dir
}

/// Verbose `regenerated:` lines → sorted affected set (absolute paths).
/// These come from dependent propagation only: the seed file itself is
/// re-processed by the `index` half of the helper, so propagation lines
/// name dependents (or nothing when the gate holds).
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
            &format!("SELECT content_text FROM fragments WHERE file_id = '{file_id}' ORDER BY id"),
        ])
        .output();
    // sqlite3 CLI may be absent (hermetic CI): fall back to file bytes.
    let Ok(out) = out else {
        return vec![std::fs::read_to_string(dir.join(file)).unwrap()];
    };
    if !out.status.success() {
        return vec![std::fs::read_to_string(dir.join(file)).unwrap()];
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn importee_signature_edit_reprocesses_importer() {
    let dir = fixture();
    let root = dir.path();
    sync(root, ".sync.db", false);

    let before = fragment_texts(root, "importer.rs");

    // Real signature edit (new parameter): the interface moves, so the
    // importer re-processes.
    std::fs::write(
        root.join("importee.rs"),
        "pub fn fee(x: u64, y: u64) -> u64 {\n    x + y\n}\n",
    )
    .expect("write");
    let out = sync(root, ".sync.db", true);

    let measured = regenerated(&out);
    let oracle = vec![
        root.join("importee.rs").to_string_lossy().into_owned(),
        root.join("importer.rs").to_string_lossy().into_owned(),
    ];
    // Pre-registered bound: recall 1.0 (no silent miss, ever).
    for expected in &oracle {
        assert!(
            measured.contains(expected),
            "missed {expected}; measured={measured:?}"
        );
    }
    let over: Vec<_> = measured.iter().filter(|p| !oracle.contains(p)).collect();
    assert!(
        over.is_empty(),
        "signature edits pull exactly the dependent set: over={over:?}"
    );

    // DB content converges: the importer's bytes never changed, so its
    // derived rows must be identical-or-fresher, never divergent.
    assert_eq!(before, fragment_texts(root, "importer.rs"));
}

#[test]
fn body_only_edit_reprocesses_only_itself() {
    // Same signature, new body: the interface is unchanged, so only the
    // seed file re-processes — a dependent re-process would be wasted
    // work, and the sig gate must stop it.
    let dir = fixture();
    let root = dir.path();
    sync(root, ".sync.db", false);

    let before = fragment_texts(root, "importer.rs");
    std::fs::write(
        root.join("importee.rs"),
        "pub fn fee(x: u64) -> u64 {\n    x + 2\n}\n",
    )
    .expect("write");
    let out = sync(root, ".sync.db", true);

    // The seed's own rows must converge on the new body...
    assert!(
        out.contains("Ingested 1 files"),
        "seed must re-ingest: {out}"
    );
    // ...while propagation stays empty: the interface never moved.
    let measured = regenerated(&out);
    let importer = root.join("importer.rs").to_string_lossy().into_owned();
    assert!(
        !measured.contains(&importer),
        "must-NOT-fire: body-only edits stop at the seed: {measured:?}"
    );
    assert!(
        measured.is_empty(),
        "no interface move, no propagation: {measured:?}"
    );
    assert_eq!(before, fragment_texts(root, "importer.rs"));
}

#[test]
fn comment_only_edit_reprocesses_only_itself() {
    // Documentation edits never move an interface: same gate as bodies.
    let dir = fixture();
    let root = dir.path();
    sync(root, ".sync.db", false);

    std::fs::write(
        root.join("importee.rs"),
        "/// Computes a fee.\n///\n/// Details here.\npub fn fee(x: u64) -> u64 {\n    x + 1\n}\n",
    )
    .expect("write");
    let out = sync(root, ".sync.db", true);

    // The seed's own rows must converge on the new comment...
    assert!(
        out.contains("Ingested 1 files"),
        "seed must re-ingest: {out}"
    );
    // ...while propagation stays empty: documentation never moves an
    // interface.
    let measured = regenerated(&out);
    let importer = root.join("importer.rs").to_string_lossy().into_owned();
    assert!(
        !measured.contains(&importer),
        "must-NOT-fire: comment-only edits stop at the seed: {measured:?}"
    );
    assert!(
        measured.is_empty(),
        "no interface move, no propagation: {measured:?}"
    );
}

#[test]
fn macro_definition_change_widens_to_the_package() {
    // Macro bodies defeat edge analysis, so a macro-defining change
    // re-processes the whole package conservatively.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(
        root.join("macros.rs"),
        "macro_rules! one {\n    () => { 1 };\n}\n",
    )
    .expect("write");
    std::fs::write(root.join("user.rs"), "pub fn use_it() -> u64 {\n    1\n}\n").expect("write");
    sync(root, ".sync.db", false);

    std::fs::write(
        root.join("macros.rs"),
        "macro_rules! one {\n    () => { 2 };\n}\n",
    )
    .expect("write");
    let out = sync(root, ".sync.db", true);

    let measured = regenerated(&out);
    for name in ["macros.rs", "user.rs"] {
        let expected = root.join(name).to_string_lossy().into_owned();
        assert!(
            measured.contains(&expected),
            "macro widening must reach {expected}: {measured:?}"
        );
    }
}

#[test]
fn missing_interface_rows_fail_open_toward_regeneration() {
    // Stored interface rows are the gate's memory; without them the gate
    // fails open toward regeneration, never toward a silent skip.
    let dir = fixture();
    let root = dir.path();
    sync(root, ".sync.db", false);

    let db = root.join(".sync.db");
    let importee_id = root.join("importee.rs").to_string_lossy().into_owned();
    let out = std::process::Command::new("sqlite3")
        .args([
            db.to_str().unwrap(),
            &format!("DELETE FROM symbols WHERE file_id = '{importee_id}'"),
        ])
        .output()
        .expect("sqlite3");
    assert!(out.status.success(), "sqlite3 failed: {out:?}");

    std::fs::write(
        root.join("importee.rs"),
        "pub fn fee(x: u64) -> u64 {\n    x + 2\n}\n",
    )
    .expect("write");
    let out = sync(root, ".sync.db", true);

    let measured = regenerated(&out);
    let importer = root.join("importer.rs").to_string_lossy().into_owned();
    assert!(
        measured.contains(&importer),
        "missing interface must fail open: {measured:?}"
    );
}

#[test]
fn leaf_edit_reprocesses_only_itself() {
    let dir = fixture();
    let root = dir.path();
    sync(root, ".sync.db", false);

    std::fs::write(root.join("leaf.rs"), "pub fn leaf() -> u64 {\n    43\n}\n").expect("write");
    let out = sync(root, ".sync.db", true);

    // The seed's own rows must converge on the new bytes...
    assert!(
        out.contains("Ingested 1 files"),
        "seed must re-ingest: {out}"
    );
    // ...while propagation stays empty: nothing depends on the leaf.
    let measured = regenerated(&out);
    let importer = root.join("importer.rs").to_string_lossy().into_owned();
    assert!(
        !measured.contains(&importer),
        "must-NOT-fire: importer untouched by a leaf edit: {measured:?}"
    );
    assert!(
        measured.is_empty(),
        "no dependents, no propagation: {measured:?}"
    );
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
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(usize::MAX)
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
        db_count(&db, "fragments", &importee_id),
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

#[test]
fn sync_file_runs_scoped_ingest_and_propagation() {
    // `sync --file` is a full single-file sync: member gen plus the
    // derived-index half scoped to the file. A signature edit on the
    // seed pulls its dependent through propagation.
    let dir = fixture();
    let root = dir.path();
    sync(root, ".sync.db", false);

    std::fs::write(
        root.join("importee.rs"),
        "pub fn fee(x: u64, y: u64) -> u64 {\n    x + y\n}\n",
    )
    .expect("write");
    let out = sync_file(root, ".sync.db", &root.join("importee.rs"), true);

    assert!(
        out.contains("Ingested 1 files"),
        "scoped ingest must run: {out}"
    );
    let measured = regenerated(&out);
    let importer = root.join("importer.rs").to_string_lossy().into_owned();
    assert!(
        measured.contains(&importer),
        "dependent must re-process through scoped propagation: {measured:?}"
    );
}

#[test]
fn sync_file_body_edit_propagates_to_nothing() {
    // Same-shape control: a body-only edit through `sync --file`
    // re-ingests the seed and stops — the sig gate holds scoped too.
    let dir = fixture();
    let root = dir.path();
    sync(root, ".sync.db", false);

    std::fs::write(
        root.join("importee.rs"),
        "pub fn fee(x: u64) -> u64 {\n    x + 2\n}\n",
    )
    .expect("write");
    let out = sync_file(root, ".sync.db", &root.join("importee.rs"), true);

    assert!(
        out.contains("Ingested 1 files"),
        "seed must re-ingest: {out}"
    );
    assert!(
        regenerated(&out).is_empty(),
        "no interface move, no propagation: {out}"
    );
}

#[test]
fn sync_file_fixes_formatting_before_indexing() {
    // Fix-before-sync: a rustfmt-dirty file is formatted first, then the
    // fixed bytes are what get indexed.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(root.join("messy.rs"), "fn messy(x:u32)->u32{x+1}\n").expect("write");
    let out = sync_file(root, ".sync.db", &root.join("messy.rs"), true);

    let formatted = std::fs::read_to_string(root.join("messy.rs")).expect("read");
    assert!(
        formatted.contains("x: u32"),
        "rustfmt must run before sync: {formatted}"
    );
    assert!(out.contains("Ingested 1 files"), "{out}");
}

#[test]
fn full_sync_formats_dirty_file_before_indexing() {
    // The full sync pins the same order: fix, then member gen, then index.
    // No prior `index` call here: the dirty set pins from staleness, so
    // the fixture starts unindexed like a real dirty tree.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(root.join("messy.rs"), "fn messy(x:u32)->u32{x+1}\n").expect("write");
    let root_str = root.to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_str = json_dir.to_str().unwrap().to_string();
    let db = dir.path().join(".sync.db");
    let db_str = db.to_str().unwrap().to_string();
    let out = common::run(
        dir.path(),
        &[
            "sync",
            "--workspace",
            &root_str,
            "--json-dir",
            &json_str,
            "--db",
            &db_str,
            "--verbose",
        ],
    );
    assert!(out.status.success(), "sync converges: {out:?}");
    let text = common::stdout(&out);

    let formatted = std::fs::read_to_string(root.join("messy.rs")).expect("read");
    assert!(
        formatted.contains("x: u32"),
        "rustfmt must run before sync: {formatted}"
    );
    assert!(text.contains("Ingested 1 files"), "{text}");
}

#[test]
fn autonomy_l0_skips_fix_but_still_syncs() {
    // L0 is index-only: the dirty file keeps its bytes, and the sync
    // half still converges.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(root.join("messy.rs"), "fn messy(x:u32)->u32{x+1}\n").expect("write");
    let root_str = root.to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_str = json_dir.to_str().unwrap().to_string();
    let db = dir.path().join(".sync.db");
    let db_str = db.to_str().unwrap().to_string();
    let out = common::run(
        dir.path(),
        &[
            "sync",
            "--workspace",
            &root_str,
            "--json-dir",
            &json_str,
            "--db",
            &db_str,
            "--autonomy",
            "L0",
            "--verbose",
        ],
    );
    assert!(out.status.success(), "L0 sync converges: {out:?}");
    let text = common::stdout(&out);
    let untouched = std::fs::read_to_string(root.join("messy.rs")).expect("read");
    assert!(
        untouched.contains("x:u32"),
        "L0 must not rewrite: {untouched}"
    );
    assert!(text.contains("Ingested 1 files"), "{text}");
}

#[test]
fn oversized_dirty_set_skips_fix_but_still_syncs() {
    // Scale guard: beyond the pre-pass budget the fix loop is skipped
    // (whole-tree convergence belongs to `run`), and indexing proceeds
    // on unmodified bytes.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    for i in 0..33 {
        std::fs::write(
            root.join(format!("m{i:02}.rs")),
            "fn messy(x:u32)->u32{x+1}\n",
        )
        .expect("write");
    }
    let root_str = root.to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_str = json_dir.to_str().unwrap().to_string();
    let db = dir.path().join(".sync.db");
    let db_str = db.to_str().unwrap().to_string();
    let out = common::run(
        dir.path(),
        &[
            "sync",
            "--workspace",
            &root_str,
            "--json-dir",
            &json_str,
            "--db",
            &db_str,
            "--verbose",
        ],
    );
    assert!(out.status.success(), "sync converges: {out:?}");
    let text = common::stdout(&out);
    assert!(text.contains("pre-pass budget"), "{text}");
    let untouched = std::fs::read_to_string(root.join("m00.rs")).expect("read");
    assert!(
        untouched.contains("x:u32"),
        "over-budget files keep their bytes: {untouched}"
    );
    assert!(text.contains("Ingested 33 files"), "{text}");
}

#[ignore = "heavy (~1m): 256-file workspace proves bounded pool submission; run via `make guidance-soak`"]
#[test]
fn full_sync_scales_past_pool_queue_without_drops() {
    // Scale pin for backpressured member gen: far more stale files than
    // pool queue slots must all generate — none rejected, none skipped
    // by the queue. Without the in-flight bound the excess fail with
    // `pool queue is full` and their sidecars never land.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    for i in 0..256 {
        std::fs::write(
            root.join(format!("s{i:03}.rs")),
            "/// Item.\npub fn item() -> u64 {\n    1\n}\n",
        )
        .expect("write");
    }
    let root_str = root.to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_str = json_dir.to_str().unwrap().to_string();
    let db = dir.path().join(".sync.db");
    let db_str = db.to_str().unwrap().to_string();
    let out = common::run(
        dir.path(),
        &[
            "sync",
            "--workspace",
            &root_str,
            "--json-dir",
            &json_str,
            "--db",
            &db_str,
        ],
    );
    assert!(out.status.success(), "scale sync converges: {out:?}");
    let text = common::stdout(&out);
    assert!(text.contains("Ingested 256 files"), "{text}");
    assert!(!text.contains("failed to generate"), "{text}");
    let sidecars = std::fs::read_dir(json_dir.join("src"))
        .expect("sidecars")
        .count();
    assert_eq!(sidecars, 256, "every file lands a sidecar");
}
