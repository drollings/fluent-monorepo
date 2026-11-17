use super::*;

#[test]
fn generic_store_creates_nodes_without_guidance_tables() {
    let docs = VectorDocStore::open_in_memory().expect("open");
    assert!(docs.has_table("nodes"), "generic nodes table present");
    assert!(
        docs.has_table("embedding_cache"),
        "generic cache table present"
    );
    assert!(!docs.has_table("fragments"), "no guidance tables");
    assert!(!docs.has_table("capabilities"), "no guidance tables");
    assert!(!docs.has_table("files"), "no guidance tables");
    assert!(!docs.has_table("answer_cache"), "no guidance tables");
    assert_eq!(docs.get_node_count().unwrap(), 0);
}

#[test]
fn generic_store_nodes_keyword_and_vector_search() {
    let docs = VectorDocStore::open_in_memory().expect("open");
    let a = docs
        .insert_node(
            "alpha_handler",
            "src/alpha.rs",
            Some("fn alpha()"),
            Some("handles alpha requests"),
            "alpha",
            "rust",
            Some(&[1.0, 0.0, 0.0, 0.0]),
        )
        .unwrap();
    let b = docs
        .insert_node(
            "beta_handler",
            "src/beta.rs",
            Some("fn beta()"),
            Some("handles beta requests"),
            "beta",
            "rust",
            Some(&[0.0, 1.0, 0.0, 0.0]),
        )
        .unwrap();
    assert!(a != b, "distinct row ids");
    assert_eq!(docs.get_node_count().unwrap(), 2);
    assert_eq!(docs.get_embedding_count().unwrap(), 2);

    // Keyword: exact substring wins.
    let hits = docs.keyword_search("alpha_handler").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, a);

    // Vector: nearest neighbor first.
    let ranked = docs.vector_search(&[1.0, 0.0, 0.0, 0.0], 2).unwrap();
    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].id, a, "exact embedding ranks first");

    // Hybrid fuses both signals.
    let hybrid = docs
        .hybrid_search("beta", Some(&[0.0, 1.0, 0.0, 0.0]), 2)
        .unwrap();
    assert!(!hybrid.is_empty());
    assert!(hybrid.iter().any(|r| r.id == b));

    // Rebuild keeps every embedded node retrievable.
    assert_eq!(docs.rebuild_hnsw().unwrap(), 2);
    assert!(docs.has_hnsw());
    let ranked = docs.vector_search(&[1.0, 0.0, 0.0, 0.0], 2).unwrap();
    assert_eq!(ranked[0].id, a);
}

#[test]
fn generic_store_open_persists_and_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("docs.db");
    let docs = VectorDocStore::open(&path).expect("open");
    docs.insert_node("n", "s", None, None, "m", "rust", None)
        .unwrap();
    assert_eq!(docs.get_node_count().unwrap(), 1);
    drop(docs);
    let reopened = VectorDocStore::open(&path).expect("reopen");
    assert_eq!(reopened.get_node_count().unwrap(), 1);
    assert!(!reopened.has_table("fragments"), "still no guidance tables");
}
