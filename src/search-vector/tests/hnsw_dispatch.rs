use super::*;

fn dispatch_db() -> GuidanceDb {
    GuidanceDb::open_in_memory().expect("in-memory db")
}

fn dispatch_vec(i: usize) -> Vec<f32> {
    (0..8)
        .map(|j| (((i * 31 + j * 17 + 7) % 97) as f32 + 1.0) / 98.0)
        .collect()
}

fn insert_embedded(db: &GuidanceDb, n: usize) {
    for i in 0..n {
        let emb = dispatch_vec(i);
        db.insert_node(&format!("n{i}"), "s", None, None, "m", "l", Some(&emb))
            .expect("insert");
    }
}

#[test]
fn malformed_probe_returns_brute_force_without_panic() {
    // Built index, malformed probes: the raw probe would reach the distance
    // kernel's dim assertion (panic) — the shared lookup short-circuits to
    // the brute-force tail instead.
    let db = dispatch_db();
    insert_embedded(&db, 2);
    let empty_probe = db.vector_search(&[], 5).expect("empty query falls back");
    assert!(
        empty_probe.is_empty(),
        "no stored embedding matches a zero-dim query"
    );
    let emb = dispatch_vec(0);
    let zero_k = db.vector_search(&emb, 0).expect("k == 0 falls back");
    assert!(zero_k.is_empty(), "k == 0 yields no candidates");
}

#[test]
fn unbuilt_index_matches_brute_force_byte_for_byte() {
    // Rows without embeddings never build the index: `vector_search` must
    // equal the brute-force tail exactly (the pre-gate behavior, pinned).
    let db = dispatch_db();
    for i in 0..2 {
        db.insert_node(&format!("p{i}"), "s", None, None, "m", "l", None)
            .expect("insert");
    }
    let query = dispatch_vec(0);
    let searched = db.vector_search(&query, 5).expect("search");
    let ground = db
        .bruteforce_vector_search(&query, 5)
        .expect("ground truth");
    assert_eq!(searched.len(), ground.len());
    assert!(
        searched.is_empty(),
        "no embeddings → no hits on either path"
    );
}

#[test]
fn dispatch_gate_threshold_boundary() {
    // Below/at the threshold a built index still routes to brute force
    // (`hnsw_search` abstains); above it the probe runs.
    use common_core::constants::DEFAULT_HNSW_THRESHOLD;
    let db = dispatch_db();
    insert_embedded(&db, 2);
    let query = dispatch_vec(0);
    assert!(
        db.hnsw_search(&query, 2).is_none(),
        "small built index abstains (gate closed)"
    );

    let big = dispatch_db();
    insert_embedded(&big, DEFAULT_HNSW_THRESHOLD + 1);
    assert_eq!(
        big.get_embedding_count().expect("count"),
        (DEFAULT_HNSW_THRESHOLD + 1) as i64
    );
    assert!(
        big.hnsw_search(&query, 5).is_some(),
        "above-threshold built index probes"
    );
}
