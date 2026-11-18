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

/// KNN reads stay bounded: one chunked lookup, not one round trip per
/// hit. Exercises `VectorDocStore::vector_search` ordering while
/// pinning the chunked `IN` fetch shape of `hnsw_search`.
#[test]
fn knn_single_round_trip() {
    let docs = VectorDocStore::open_in_memory().expect("open");
    let dim = 4;
    let base: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];
    for i in 0..4 {
        let mut emb = base.clone();
        emb[(i as usize) % dim] += i as f32 * 0.1;
        docs.insert_node(
            &format!("node_{i}"),
            "src/lib.rs",
            Some("fn f()"),
            None,
            "m",
            "rust",
            Some(&emb),
        )
        .unwrap();
    }
    let ranked = docs.vector_search(&base, 2).unwrap();
    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].name, "node_0");

    let src = include_str!("../src/vector_store.rs");
    assert!(
        src.contains("WHERE id IN ("),
        "VectorDocStore::hnsw_search fetches hits with a chunked IN query"
    );
    assert!(
        !src.contains("WHERE id = ?1"),
        "no per-hit SELECT loop remains on the HNSW path"
    );
}

/// The fetch chunk stays below the SQLite host-parameter limit: the
/// bound is a correctness invariant, not a tuning knob.
#[test]
fn knn_fetch_chunk_stays_below_variable_limit() {
    const { assert!(HNSW_FETCH_CHUNK >= 1, "chunk must be positive") };
    const {
        assert!(
            HNSW_FETCH_CHUNK < 999,
            "chunk must stay below the SQLite variable limit"
        );
    };
}

/// Chunk arithmetic for hostile hit counts: the fetch issues exactly
/// one statement per chunk, so a search costs `ceil(hits / chunk)`.
#[test]
fn knn_chunk_count_matches_ceil_bound() {
    for (hits, chunks) in [(0, 0), (1, 1), (499, 1), (500, 1), (501, 2), (1000, 2), (1001, 3), (1500, 3)] {
        let ids: Vec<i64> = (0..i64::from(hits)).collect();
        assert_eq!(
            ids.chunks(HNSW_FETCH_CHUNK).len(),
            chunks,
            "{hits} hits must fetch in {chunks} statements"
        );
    }
}

/// HNSW distance order survives the rowid-ordered fetch: similarities
/// strictly decrease along truth order (reverse insertion), so a fetch
/// that returned SQL order would fail the monotonicity check.
#[test]
fn knn_restores_hnsw_order_and_skips_missing() {
    use std::f32::consts::PI;
    let docs = VectorDocStore::open_in_memory().expect("open");
    // Insert worst-first: truth order (nearest first) runs opposite to
    // rowid order, so SQL order can never accidentally pass.
    let mut ids = Vec::new();
    for i in (0..513).rev() {
        let theta = i as f32 * PI / 513.0;
        let id = docs
            .insert_node(
                &format!("arc_{i}"),
                "s",
                None,
                None,
                "m",
                "l",
                Some(&[theta.cos(), theta.sin()]),
            )
            .expect("insert");
        ids.push((i, id));
    }
    let query = [1.0_f32, 0.0];
    let hits = docs.hnsw_search(&query, 10).expect("HNSW probes above threshold");
    assert_eq!(hits.len(), 10);
    assert!(
        (hits[0].similarity - 1.0).abs() < 1e-6,
        "exact-match embedding ranks first"
    );
    assert!(
        hits.windows(2).all(|w| w[0].similarity >= w[1].similarity),
        "output follows HNSW distance order, not rowid order"
    );

    // A wide probe crosses the chunk boundary: every hit still resolves
    // in HNSW order through the multi-statement fetch.
    let many = docs.hnsw_search(&query, 501).expect("wide probe");
    assert_eq!(many.len(), 501);
    assert!(
        many.windows(2).all(|w| w[0].similarity >= w[1].similarity),
        "wide output keeps HNSW order across chunks"
    );

    // Delete the exact-match row between probe and fetch: the hit is
    // skipped, the rest keep HNSW order.
    let (_, exact_id) = ids.iter().find(|(i, _)| *i == 0).expect("theta zero inserted");
    docs.sqlite_store()
        .with_conn(|conn| {
            conn.execute("DELETE FROM nodes WHERE id = ?1", rusqlite::params![*exact_id])?;
            Ok(())
        })
        .expect("delete");
    let after = docs
        .hnsw_search(&query, 10)
        .expect("probe still resolves");
    assert!(!after.iter().any(|r| r.id == *exact_id), "deleted row skipped");
    assert!(
        after.windows(2).all(|w| w[0].similarity >= w[1].similarity),
        "remaining hits keep HNSW order"
    );

    // Malformed probes need no fetch: `None` before any store touch.
    assert!(docs.hnsw_search(&query, 0).is_none());
    assert!(docs.hnsw_search(&[], 5).is_none());
}

/// Pinned HNSW-vs-brute-force corpus: 513 arc points inserted worst-first
/// (deterministic angles, no ties), query at arc start.
fn pinned_arc_corpus() -> (VectorDocStore, [f32; 2]) {
    use std::f32::consts::PI;
    let docs = VectorDocStore::open_in_memory().expect("open");
    for i in (0..513).rev() {
        let theta = i as f32 * PI / 513.0;
        docs.insert_node(
            &format!("arc_{i}"),
            "s",
            None,
            None,
            "m",
            "l",
            Some(&[theta.cos(), theta.sin()]),
        )
        .expect("insert");
    }
    (docs, [1.0, 0.0])
}

fn recall_against_brute_force(docs: &VectorDocStore, query: &[f32], k: usize) -> f64 {
    use std::collections::HashSet;
    let approx: Vec<i64> = docs
        .vector_search(query, k)
        .expect("approx")
        .into_iter()
        .map(|r| r.id)
        .collect();
    let exact: Vec<i64> = docs
        .bruteforce_vector_search(query, k)
        .expect("exact")
        .into_iter()
        .map(|r| r.id)
        .collect();
    let exact_set: HashSet<i64> = exact.into_iter().collect();
    approx.iter().filter(|id| exact_set.contains(id)).count() as f64 / k as f64
}

/// Chunked-`IN` parity with brute-force ground truth at and above the
/// chunk boundary: recall holds the operating point, the top hit is
/// exact, and output order is HNSW order. Missing ids are covered by
/// the order-and-skip test (any miss drops below `k` into the exact
/// brute-force fallback by `vector_search` design); duplicate hit ids
/// cannot arise (unique external ids by index construction, and the
/// id-map restore consumes each hit once).
#[test]
fn knn_chunked_fetch_matches_brute_force_at_scale() {
    let (docs, query) = pinned_arc_corpus();
    for k in [10usize, 501usize] {
        let approx = docs.vector_search(&query, k).expect("approx");
        assert_eq!(approx.len(), k, "full top-{k} resolves");
        let recall = recall_against_brute_force(&docs, &query, k);
        assert!(
            recall >= 0.95,
            "recall {recall:.3} below operating point at k={k}"
        );
        assert!(
            (approx[0].similarity - 1.0).abs() < 1e-6,
            "top hit exact at k={k}"
        );
        assert!(
            approx.windows(2).all(|w| w[0].similarity >= w[1].similarity),
            "HNSW order at k={k}"
        );
    }
}
