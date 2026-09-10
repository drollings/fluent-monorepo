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
            &search_vector::db::ZgFragmentFilter::default(),
        )
        .expect("fts");
    assert!(!hits.is_empty(), "ingested text must be FTS-visible");
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
