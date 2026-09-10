//! P2 file-status tests: `replace_file` / `mark_file_failed` /
//! `delete_file` lifecycle (ports `service.test.mjs:957` storage half).

use crate::db::{GuidanceDb, ZgFileRecord};

fn file(id: &str) -> ZgFileRecord {
    ZgFileRecord {
        id: id.to_string(),
        absolute_path: format!("/repo/{id}"),
        relative_path: id.to_string(),
        root_path: "/repo".to_string(),
        size_bytes: 10,
        last_modified_time: 100,
        kind: None,
        format: "rust".to_string(),
        content_hash: Some("hash-1".to_string()),
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    }
}

#[test]
fn replace_file_upserts_status_columns() {
    let db = GuidanceDb::open_in_memory().expect("db");
    db.replace_file(&file("a.rs"), &[], &[]).expect("replace");
    let got = db.zg_get_file("a.rs").expect("get").expect("present");
    assert_eq!(got.content_hash.as_deref(), Some("hash-1"));
    assert_eq!(got.index_status.as_deref(), Some("indexed"));
    assert_eq!(got.fail_count, 0);
}

#[test]
fn mark_file_failed_records_error_and_counts() {
    let db = GuidanceDb::open_in_memory().expect("db");
    db.replace_file(&file("f.rs"), &[], &[]).expect("replace");
    db.mark_file_failed(&file("f.rs"), "boom").expect("mark");
    let got = db.zg_get_file("f.rs").expect("get").expect("present");
    assert_eq!(got.index_status.as_deref(), Some("failed"));
    assert_eq!(got.last_error.as_deref(), Some("boom"));
    assert_eq!(got.fail_count, 1);
    db.mark_file_failed(&file("f.rs"), "boom again").expect("mark");
    let got = db.zg_get_file("f.rs").expect("get").expect("present");
    assert_eq!(got.fail_count, 2);
}

#[test]
fn delete_file_removes_fragments_and_record() {
    let db = GuidanceDb::open_in_memory().expect("db");
    db.replace_file(&file("d.rs"), &[], &[]).expect("replace");
    db.delete_file("d.rs").expect("delete");
    assert!(db.zg_get_file("d.rs").expect("get").is_none());
    assert!(db.zg_list_files().expect("list").is_empty());
}

#[test]
fn failed_files_are_listed_for_retry() {
    let db = GuidanceDb::open_in_memory().expect("db");
    db.replace_file(&file("ok.rs"), &[], &[]).expect("replace");
    db.replace_file(&file("bad.rs"), &[], &[]).expect("replace");
    db.mark_file_failed(&file("bad.rs"), "x").expect("mark");
    let ids = db.failed_file_ids().expect("failed");
    assert_eq!(ids, vec!["bad.rs".to_string()]);
}
