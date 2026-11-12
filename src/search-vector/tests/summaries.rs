use super::*;
use crate::db::GuidanceDb;
use crate::db::fragments::{FileRecord, FragmentRecord};

fn file_a() -> FileRecord {
    FileRecord {
        id: "file-a".to_string(),
        absolute_path: "/repo/src/a.rs".to_string(),
        relative_path: "src/a.rs".to_string(),
        root_path: "/repo".to_string(),
        size_bytes: 100,
        last_modified_time: 100,
        kind: Some("code".to_string()),
        format: "rust".to_string(),
        content_hash: None,
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    }
}

fn frag(id: &str) -> FragmentRecord {
    FragmentRecord {
        id: id.to_string(),
        group: None,
        file_id: "file-a".to_string(),
        range_json: "{\"kind\":\"file\"}".to_string(),
        content_kind: "text".to_string(),
        content_text: Some("fn alpha() {}".to_string()),
        cjk_text: String::new(),
        symbol_type: Some("function".to_string()),
        symbol_name: Some("alpha".to_string()),
        scope: None,
        signature: Some("fn alpha()".to_string()),
        doc: Some("Adds one.".to_string()),
        modifiers: String::new(),
        heading: None,
        heading_level: None,
        embedding: None,
    }
}

fn summary(fragment_id: &str, text: &str) -> SummaryRecord {
    SummaryRecord {
        fragment_id: fragment_id.to_string(),
        comment_hash: "hash-a".to_string(),
        model: "test-model".to_string(),
        prompt_version: 1,
        temperature: 0.0,
        seed: 42,
        summary: text.to_string(),
        status: "ready".to_string(),
        generated_at: 1700000000000,
        authorized: false,
    }
}

fn seed_file(db: &GuidanceDb) {
    db.upsert_fragments(&file_a(), &[frag("entity-a"), frag("entity-b")], &[])
        .expect("upsert");
}

#[test]
fn summary_rows_round_trip_with_provenance() {
    let db = GuidanceDb::open_in_memory().expect("db");
    seed_file(&db);
    let rows = vec![summary("entity-a", "Adds one to the total.")];
    assert_eq!(db.replace_summaries(&rows).expect("replace"), 1);
    let stored = db.summaries_for_fragment("entity-a").expect("read");
    assert_eq!(stored.len(), 1);
    let row = &stored[0];
    assert_eq!(row.summary, "Adds one to the total.");
    assert_eq!(row.comment_hash, "hash-a");
    assert_eq!(row.model, "test-model");
    assert_eq!(row.prompt_version, 1);
    assert_eq!(row.status, "ready");
    assert_eq!(row.generated_at, 1700000000000);
    assert!(!row.authorized);
}

#[test]
fn summary_fts_finds_generated_terms_only() {
    // Generated text is lexically reachable through its own FTS index;
    // source-only terms must not match the summary route.
    let db = GuidanceDb::open_in_memory().expect("db");
    seed_file(&db);
    db.replace_summaries(&[summary("entity-a", "Adds one to the total.")])
        .expect("replace");
    let hits = db
        .search_summaries("total", 10)
        .expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "entity-a");
    assert_eq!(hits[0].file_id, "file-a");
    let hits = db
        .search_summaries("alpha", 10)
        .expect("search");
    assert!(
        hits.is_empty(),
        "source-only terms stay out of the summary index: {hits:?}"
    );
}

#[test]
fn summary_replace_updates_fts_rows() {
    let db = GuidanceDb::open_in_memory().expect("db");
    seed_file(&db);
    db.replace_summaries(&[summary("entity-a", "Adds one to the total.")])
        .expect("replace");
    let mut second = summary("entity-a", "Removes two from the count.");
    second.comment_hash = "hash-a2".to_string();
    db.replace_summaries(&[second]).expect("replace");
    assert!(
        db.search_summaries("total", 10)
            .expect("search")
            .is_empty(),
        "stale summary text must leave the index"
    );
    let hits = db.search_summaries("removes", 10).expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "entity-a");
}

#[test]
fn summary_search_skips_fragments_without_rows() {
    // Summaries whose fragment row is gone stay invisible to recall.
    let db = GuidanceDb::open_in_memory().expect("db");
    seed_file(&db);
    db.replace_summaries(&[summary("missing-fragment", "Adds one to the total.")])
        .expect("replace");
    assert!(
        db.search_summaries("total", 10)
            .expect("search")
            .is_empty(),
        "orphan summaries must not recall"
    );
}

#[test]
fn summary_fts_never_matches_everything() {
    let db = GuidanceDb::open_in_memory().expect("db");
    seed_file(&db);
    db.replace_summaries(&[summary("entity-a", "Adds one to the total.")])
        .expect("replace");
    for query in ["", "   ", "!!!"] {
        assert!(
            db.search_summaries(query, 10)
                .expect("search")
                .is_empty(),
            "query {query:?} must match nothing"
        );
    }
}
