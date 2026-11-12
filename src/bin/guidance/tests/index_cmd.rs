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

#[ignore = "heavy (~1.6s): two full workspace ingests; run via `make guidance-soak`"]
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

#[ignore = "heavy (~2.7s): 1.1s mtime-tick sleep plus two full workspace ingests; run via `make guidance-soak`"]
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
fn fragment_ingest_filters_structural_punctuation_and_stop_words() {
    // Structural punctuation and syntax words never become lemma rows;
    // FTS still owns the raw lexical row.
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir(&src).expect("mkdir src");
    // Data files emit no lemmas at all: keys stay FTS-only.
    std::fs::write(
        src.join("Cargo.toml"),
        "[package]\nname = \"guidance\"\n[dependencies]\n",
    )
    .expect("write toml");
    // Code without comments emits no lemmas either: bodies are FTS-only.
    std::fs::write(src.join("lib.rs"), "pub fn alpha_beta() {}\n").expect("write rs");
    let db_path = dir.path().join("frag.db");
    let roots = [src.clone()];
    ingest_workspace_fragments(&db_path, dir.path(), &roots).expect("ingest");
    let db = search_vector::GuidanceDb::open(&db_path).expect("open");
    for noise in ["[", "]", "=", "\n", "pub", "fn"] {
        assert!(
            db.fragments_for_lemmas(&[noise.to_string()], 10)
                .expect("lookup")
                .is_empty(),
            "structural token leaked into lemmas: {noise:?}"
        );
    }
    for absent in ["package", "alpha", "beta"] {
        assert!(
            db.fragments_for_lemmas(&[absent.to_string()], 10)
                .expect("lookup")
                .is_empty(),
            "non-comment token must stay out of lemmas: {absent:?}"
        );
    }
    // Lexical recall is unaffected: bodies stay FTS-visible.
    assert!(
        !db.search_fts(
            "alpha_beta",
            5,
            &search_vector::db::FragmentFilter::default()
        )
        .expect("fts")
        .is_empty(),
        "code bodies must stay FTS-visible"
    );
}

#[test]
fn live_ingest_lemmas_come_from_comments_not_bodies() {
    // Comment-scoped lemmas: comment words land in the lemma table while
    // body-only identifiers stay out (FTS still serves them).
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir(&src).expect("mkdir src");
    std::fs::write(
        src.join("lib.rs"),
        "/// Adds one to the total.\npub fn alpha_beta() {}\n",
    )
    .expect("write rs");
    let db_path = dir.path().join("frag.db");
    let roots = [src.clone()];
    ingest_workspace_fragments(&db_path, dir.path(), &roots).expect("ingest");
    let db = search_vector::GuidanceDb::open(&db_path).expect("open");
    for word in ["adds", "total"] {
        assert!(
            !db.fragments_for_lemmas(&[word.to_string()], 10)
                .expect("lookup")
                .is_empty(),
            "comment word missing from lemmas: {word:?}"
        );
    }
    for body in ["alpha", "beta"] {
        assert!(
            db.fragments_for_lemmas(&[body.to_string()], 10)
                .expect("lookup")
                .is_empty(),
            "body-only identifier leaked into lemmas: {body:?}"
        );
    }
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
    for (id, path) in [
        ("a", "/repo/a.rs"),
        ("b", "/repo/b.rs"),
        ("c", "/repo/c.rs"),
    ] {
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
    assert_eq!(
        affected,
        vec!["/repo/a.rs".to_string(), "/repo/b.rs".to_string()]
    );
}

#[test]
fn affected_expands_deleted_seeds() {
    let db = propagation_db();
    let mut affected = affected_for_sync(&db, &[], &["/repo/a.rs".to_string()], &roots());
    affected.sort();
    assert_eq!(
        affected,
        vec!["/repo/a.rs".to_string(), "/repo/b.rs".to_string()]
    );
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
    assert!(
        second.changed.is_empty(),
        "nothing changed: {:?}",
        second.changed
    );
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
    assert!(!db
        .graph_symbol_rows()
        .expect("symbols")
        .iter()
        .any(|r| r.file.ends_with("gone.rs")));
}

#[test]
fn live_ingest_writes_item_chunks_with_graph() {
    // The live path commits item chunks (never whole-file rows): fragment
    // ids differ from the file id, ranges are item spans, the module
    // fragment carries the top-of-file comment, and graph rows come from
    // the same pass. A second pass over untouched files ingests nothing.
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir(&src).expect("mkdir src");
    std::fs::write(
        src.join("lib.rs"),
        "//! Crate helpers.\n/// Adds one.\npub fn alpha() -> i32 { 1 }\n/// Removes two.\npub fn beta() -> i32 { 2 }\n",
    )
    .expect("write rs");
    let db_path = dir.path().join("frag.db");
    let roots = [src.clone()];
    let first = ingest_workspace_fragments(&db_path, dir.path(), &roots).expect("ingest");
    assert_eq!(first.files, 1, "{first:?}");
    assert_eq!(first.failed, 0, "{first:?}");
    let db = GuidanceDb::open(&db_path).expect("open");
    let file_id = db
        .list_files()
        .expect("files")
        .into_iter()
        .find(|file| file.relative_path.ends_with("lib.rs"))
        .expect("lib row")
        .id;
    let adds_hits = db
        .fragments_for_lemmas(&["adds".to_string()], 10)
        .expect("lookup");
    assert!(
        !adds_hits.is_empty(),
        "comment lemmas must land on item chunks"
    );
    assert!(
        !adds_hits.contains(&file_id),
        "item lemmas must not sit on the whole-file id: {adds_hits:?}"
    );
    assert!(
        db.fragments_for_lemmas(&["alpha".to_string()], 10)
            .expect("lookup")
            .is_empty(),
        "body identifiers stay out of lemmas"
    );
    let symbols = db.graph_symbol_rows().expect("symbols");
    assert!(
        symbols.iter().any(|row| row.name == "alpha"),
        "same-pass graph harvest must record items: {symbols:?}"
    );
    let second = ingest_workspace_fragments(&db_path, dir.path(), &roots).expect("ingest");
    assert_eq!(second.files, 0, "{second:?}");
    assert_eq!(second.skipped_unchanged, 1, "{second:?}");
}

#[test]
fn live_ingest_writes_no_embedding_status() {
    // The live path has no embedder: it commits unembedded item chunks
    // without recording embedding status. Status rows belong to the
    // batch pipeline alone.
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir(&src).expect("mkdir src");
    std::fs::write(src.join("lib.rs"), "/// Adds one.\npub fn alpha() {}\n").expect("write rs");
    let db_path = dir.path().join("frag.db");
    let roots = [src.clone()];
    ingest_workspace_fragments(&db_path, dir.path(), &roots).expect("ingest");
    let db = search_vector::GuidanceDb::open(&db_path).expect("open");
    let file_id = db
        .list_files()
        .expect("files")
        .into_iter()
        .find(|file| file.relative_path.ends_with("lib.rs"))
        .expect("lib row")
        .id;
    assert!(
        db.embedding_status_for_file(&file_id)
            .expect("status")
            .is_empty(),
        "live commits carry no embedding provenance"
    );
    assert!(
        !db.fragment_freshness(&file_id)
            .expect("fingerprint")
            .expect("row")
            .embedded,
        "live rows stay unembedded"
    );
}

#[test]
fn fragment_ingestion_scopes_to_a_single_file() {
    // The `sync --file` building block: a file root ingests only that
    // file, and the delete purge stays scoped — out-of-scope indexed
    // files are untouched, never "missing".
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir(&src).expect("mkdir src");
    std::fs::write(src.join("a.rs"), "pub fn scoped_anchor() {}\n").expect("write");
    std::fs::write(src.join("b.rs"), "pub fn bystander_fn() {}\n").expect("write");
    let db_path = dir.path().join("frag.db");
    let roots = [src.clone()];
    let first = ingest_workspace_fragments(&db_path, dir.path(), &roots).expect("ingest");
    assert_eq!(first.files, 2, "{first:?}");
    std::fs::write(
        src.join("a.rs"),
        "pub fn scoped_anchor(x: u64) -> u64 {\n    x\n}\n",
    )
    .expect("write");
    let scoped =
        ingest_workspace_fragments(&db_path, dir.path(), &[src.join("a.rs")]).expect("scoped");
    assert_eq!(scoped.files, 1, "{scoped:?}");
    assert_eq!(scoped.failed, 0, "{scoped:?}");
    assert!(scoped.deleted.is_empty(), "{scoped:?}");
    assert!(
        scoped.changed.iter().any(|p| p.ends_with("a.rs")),
        "{scoped:?}"
    );
    // The bystander keeps its rows: scoping is a lens, not a wipe.
    let db = search_vector::GuidanceDb::open(&db_path).expect("open");
    for term in ["scoped_anchor", "bystander_fn"] {
        let hits = db
            .search_fts(term, 5, &search_vector::db::FragmentFilter::default())
            .expect("fts");
        assert!(!hits.is_empty(), "{term} must stay FTS-visible");
    }
}
