//! Unit tests for the `index` command shell. Included via `#[path]`
//! forwarder from `src/index_cmd.rs`, per repo convention.

use super::*;

#[test]
fn index_counts_one_rust_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("lib.rs"), "pub fn indexed_fn() {}\n").expect("write");
    let workspace = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_dir = json_dir.to_str().unwrap().to_string();
    let (generated, failed) = run_index(&workspace, &json_dir, None).expect("index");
    assert_eq!(generated + failed, 1, "one file in, one outcome out");
}

#[test]
fn index_scopes_to_a_single_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.rs"), "pub fn a_fn() {}\n").expect("write");
    std::fs::write(dir.path().join("b.rs"), "pub fn b_fn() {}\n").expect("write");
    let workspace = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_dir = json_dir.to_str().unwrap().to_string();
    let target = dir.path().join("a.rs");
    let target = target.to_str().unwrap().to_string();
    let (generated, failed) = run_index(&workspace, &json_dir, Some(&target)).expect("index");
    assert_eq!(generated + failed, 1, "path scope indexes one file");
}

#[test]
fn fragment_ingestion_populates_fts_and_skips_image_noise() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.rs"), "pub fn ingested_anchor() {}\n").expect("write");
    // Raster images are default-ignored noise (Gate 0 §5: discover +
    // kind-tag at `detect_file_type`, skip with counts here).
    std::fs::write(dir.path().join("pic.png"), [0x89, 0x50, 0x4E, 0x47]).expect("write");
    let db_path = dir.path().join("frag.db");
    let stats = ingest_workspace_fragments(&db_path, dir.path(), &[dir.path().to_path_buf()])
        .expect("ingest");
    assert_eq!(stats.files, 1, "{stats:?}");
    assert_eq!(stats.failed, 0, "{stats:?}");
    assert!(stats.fragments >= 1, "{stats:?}");
    let db = search_vector::GuidanceDb::open(&db_path).expect("open");
    let hits = db
        .search_fts(
            "ingested_anchor",
            5,
            &search_vector::db::FragmentFilter::default(),
        )
        .expect("fts");
    assert!(!hits.is_empty(), "ingested text must be FTS-visible");
}

#[test]
fn fragment_ingest_skips_unchanged_files_on_second_pass() {
    // Warm sync must not pay read + lemmatize + upsert per file again:
    // a second ingest over untouched files ingests nothing and says so.
    // (Sources live under `src/` with the db beside the roots: the db's
    // own `-shm`/`-wal` sidecars must never be selection candidates.)
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir(&src).expect("mkdir src");
    std::fs::write(src.join("a.rs"), "pub fn steady_one() {}\n").expect("write");
    std::fs::write(src.join("b.rs"), "pub fn steady_two() {}\n").expect("write");
    let db_path = dir.path().join("frag.db");
    let roots = [src.clone()];
    let first = ingest_workspace_fragments(&db_path, dir.path(), &roots).expect("ingest");
    assert_eq!(first.files, 2, "{first:?}");
    let second = ingest_workspace_fragments(&db_path, dir.path(), &roots).expect("ingest");
    assert_eq!(second.files, 0, "{second:?}");
    assert_eq!(second.skipped_unchanged, 2, "{second:?}");
    assert_eq!(second.failed, 0, "{second:?}");
    // The index itself is intact — skipping is a read shortcut, not a wipe.
    let db = search_vector::GuidanceDb::open(&db_path).expect("open");
    let hits = db
        .search_fts(
            "steady_one",
            5,
            &search_vector::db::FragmentFilter::default(),
        )
        .expect("fts");
    assert!(!hits.is_empty(), "skipped files must stay FTS-visible");
}

#[test]
fn fragment_ingest_reingests_modified_files() {
    // Must-NOT-fire control for the skip above: changed content (size
    // differs) re-ingests exactly that file; the untouched file skips.
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir(&src).expect("mkdir src");
    std::fs::write(src.join("a.rs"), "pub fn edited() {}\n").expect("write");
    std::fs::write(src.join("b.rs"), "pub fn calm() {}\n").expect("write");
    let db_path = dir.path().join("frag.db");
    let roots = [src.clone()];
    ingest_workspace_fragments(&db_path, dir.path(), &roots).expect("ingest");
    std::fs::write(src.join("a.rs"), "pub fn edited() {}\n// another line\n").expect("write");
    let second = ingest_workspace_fragments(&db_path, dir.path(), &roots).expect("ingest");
    assert_eq!(second.files, 1, "{second:?}");
    assert_eq!(second.skipped_unchanged, 1, "{second:?}");
}

#[test]
fn fragment_ingest_reingests_same_size_edits() {
    // Must-NOT-fire control for the mtime arm: same byte length but
    // newer mtime is still a change (mtime-only staleness is real —
    // size alone would keep a stale row forever).
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir(&src).expect("mkdir src");
    std::fs::write(src.join("a.rs"), "pub fn v1aa() {}\n").expect("write");
    let db_path = dir.path().join("frag.db");
    let roots = [src.clone()];
    ingest_workspace_fragments(&db_path, dir.path(), &roots).expect("ingest");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(src.join("a.rs"), "pub fn v2bb() {}\n").expect("write");
    let second = ingest_workspace_fragments(&db_path, dir.path(), &roots).expect("ingest");
    assert_eq!(second.files, 1, "{second:?}");
    assert_eq!(second.skipped_unchanged, 0, "{second:?}");
}

#[test]
fn missing_workspace_is_a_named_error() {
    let error = run_index("/no/such/workspace", "/tmp/g", None).expect_err("must fail");
    assert!(error.contains("workspace not found"), "{error}");
}

#[test]
fn missing_path_is_a_named_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_dir = json_dir.to_str().unwrap().to_string();
    let error = run_index(&workspace, &json_dir, Some("/no/such/file.rs")).expect_err("must fail");
    assert!(error.contains("path not found"), "{error}");
}

fn propagation_db() -> GuidanceDb {
    let db = GuidanceDb::open_in_memory().expect("db");
    // `files` rows seed the hydration's known-file set (paths resolve
    // against indexed files, never rows alone).
    for (id, path) in
        [("a", "/repo/a.rs"), ("b", "/repo/b.rs"), ("c", "/repo/c.rs")]
    {
        db.replace_file(
            &search_vector::db::FileRecord {
                id: id.to_string(),
                absolute_path: path.to_string(),
                relative_path: path.to_string(),
                root_path: "/repo".to_string(),
                size_bytes: 10,
                last_modified_time: 10,
                kind: Some("code".to_string()),
                format: "rust".to_string(),
                content_hash: None,
                index_status: Some("indexed".to_string()),
                fail_count: 0,
                last_error: None,
            },
            &[],
            &[],
        )
        .expect("file row");
    }
    // b declares `mod a` (resolves to /repo/a.rs at hydration).
    db.replace_file_graph("/repo/a.rs", &[], &[("alpha".to_string(), Vec::new())])
        .expect("rows");
    db.replace_file_graph(
        "/repo/b.rs",
        &["mod:a".to_string()],
        &[("beta".to_string(), vec!["alpha".to_string()])],
    )
    .expect("rows");
    db.replace_file_graph("/repo/c.rs", &[], &[("gamma".to_string(), Vec::new())])
        .expect("rows");
    db
}

fn roots() -> Vec<String> {
    vec!["/repo".to_string()]
}

#[test]
fn affected_expands_changed_through_dependents() {
    let db = propagation_db();
    let mut affected = affected_for_sync(&db, &["/repo/a.rs".to_string()], &[], &roots());
    affected.sort();
    assert_eq!(affected, vec!["/repo/a.rs".to_string(), "/repo/b.rs".to_string()]);
}

#[test]
fn affected_expands_deleted_seeds() {
    let db = propagation_db();
    let mut affected = affected_for_sync(&db, &[], &["/repo/a.rs".to_string()], &roots());
    affected.sort();
    assert_eq!(affected, vec!["/repo/a.rs".to_string(), "/repo/b.rs".to_string()]);
}

#[test]
fn affected_leaf_change_touches_only_itself() {
    let db = propagation_db();
    let affected = affected_for_sync(&db, &["/repo/c.rs".to_string()], &[], &roots());
    assert_eq!(affected, vec!["/repo/c.rs".to_string()]);
}

#[test]
fn affected_empty_seeds_is_empty() {
    let db = propagation_db();
    assert!(affected_for_sync(&db, &[], &[], &roots()).is_empty());
}

#[test]
fn ingest_reports_changed_and_purges_deleted() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.rs"), "pub fn seed_fn() {}\n").expect("write");
    std::fs::write(dir.path().join("gone.rs"), "pub fn gone_fn() {}\n").expect("write");
    // Dotfile db: its -wal/-shm sidecars skip selection (the db must not
    // ingest itself when it lives inside the roots).
    let db_path = dir.path().join(".frag.db");
    let srcs = vec![dir.path().to_path_buf()];
    let first = ingest_workspace_fragments(&db_path, dir.path(), &srcs).expect("ingest");
    assert_eq!(first.changed.len(), 2);
    assert!(first.deleted.is_empty());

    std::fs::remove_file(dir.path().join("gone.rs")).expect("delete");
    let second = ingest_workspace_fragments(&db_path, dir.path(), &srcs).expect("ingest");
    assert!(second.changed.is_empty(), "nothing changed: {:?}", second.changed);
    assert_eq!(second.deleted.len(), 1);
    assert!(second.deleted[0].ends_with("gone.rs"));

    // Purged rows are gone from every table (fragments + graph).
    let db = GuidanceDb::open(&db_path).expect("open");
    let files: Vec<String> = db
        .list_files()
        .expect("files")
        .into_iter()
        .map(|f| f.absolute_path)
        .collect();
    assert!(!files.iter().any(|f| f.ends_with("gone.rs")), "{files:?}");
    assert!(
        !db.graph_symbol_rows()
            .expect("symbols")
            .iter()
            .any(|r| r.file.ends_with("gone.rs"))
    );
}
