use super::*;

fn make_db() -> VectorDocStore {
    VectorDocStore::open_in_memory().expect("in-memory db")
}

#[test]
fn test_insert_and_count() {
    let db = make_db();
    let id = db
        .insert_node(
            "hello",
            "src/test.zig",
            Some("fn hello() void"),
            Some("Says hello"),
            "test",
            "zig",
            None,
        )
        .expect("insert");
    assert!(id > 0);
    assert_eq!(db.get_node_count().expect("count"), 1);
}

#[test]
fn test_keyword_search() {
    let db = make_db();
    db.insert_node(
        "greet",
        "src/test.zig",
        Some("fn greet() void"),
        Some("Greets the user"),
        "test",
        "zig",
        None,
    )
    .expect("insert");
    db.insert_node(
        "add",
        "src/math.zig",
        Some("fn add() i32"),
        Some("Adds numbers"),
        "math",
        "zig",
        None,
    )
    .expect("insert");

    let results = db.keyword_search("greet").expect("search");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "greet");
}

#[test]
fn test_vector_search() {
    let db = make_db();
    let emb1: Vec<f32> = (0..4).map(|i| i as f32).collect();
    let emb2: Vec<f32> = (0..4).map(|i| (i + 10) as f32).collect();

    db.insert_node("a", "src/a.zig", None, None, "test", "zig", Some(&emb1))
        .expect("insert");
    db.insert_node("b", "src/b.zig", None, None, "test", "zig", Some(&emb2))
        .expect("insert");

    let query = vec![0.5, 1.5, 2.5, 3.5];
    let results = db.vector_search(&query, 2).expect("search");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].name, "a");
}

#[test]
fn test_empty_search() {
    let db = make_db();
    let results = db.keyword_search("nonexistent").expect("search");
    assert!(results.is_empty());
}

#[test]
fn brute_force_order_truncation_and_tie_winner() {
    // Characterization (M3b): locks the brute-force tail's order,
    // k-truncation, and tie-break so the P2 migration must preserve them.
    // "twin-a"/"twin-b" carry identical embeddings (a contested tie);
    // the sort is stable over rowid-ordered rows, so insertion order wins.
    let db = make_db();
    let near = vec![1.0f32, 0.0, 0.0, 0.0];
    let far = vec![0.0f32, 1.0, 0.0, 0.0];
    db.insert_node("twin-a", "s", None, None, "m", "l", Some(&near))
        .expect("insert");
    db.insert_node("twin-b", "s", None, None, "m", "l", Some(&near))
        .expect("insert");
    db.insert_node("far", "s", None, None, "m", "l", Some(&far))
        .expect("insert");

    let all = db.bruteforce_vector_search(&near, 10).expect("search");
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].name, "twin-a", "tie: insertion order wins");
    assert_eq!(all[1].name, "twin-b");
    assert_eq!(all[2].name, "far");
    assert!(all[0].similarity >= all[1].similarity);
    assert!(all[1].similarity >= all[2].similarity);

    // k truncation keeps the head in order.
    let top2 = db.bruteforce_vector_search(&near, 2).expect("search");
    assert_eq!(
        top2.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["twin-a", "twin-b"]
    );
    // k == 0 → empty.
    assert!(db
        .bruteforce_vector_search(&near, 0)
        .expect("search")
        .is_empty());
}

#[test]
fn test_hybrid_search() {
    let db = make_db();
    let emb = vec![0.1, 0.2, 0.3, 0.4];

    db.insert_node(
        "hello_fn",
        "src/test.zig",
        Some("fn hello() void"),
        Some("Says hello"),
        "test",
        "zig",
        Some(&emb),
    )
    .expect("insert");

    let results = db
        .hybrid_search("hello", Some(&emb), 5)
        .expect("hybrid search");
    assert!(!results.is_empty());
}

fn recall_fixture_vec(i: usize, dim: usize) -> Vec<f32> {
    (0..dim)
        .map(|j| (((i * 31 + j * 17 + 7) % 97) as f32 + 1.0) / 98.0)
        .collect()
}

#[test]
fn vector_search_recall_baseline_is_pinned() {
    // Baseline pin: `vector_search` against brute-force ground truth.
    // Small corpora exercise the fill-to-k top-up (k > N forces the
    // brute-force tail, so results are identical); larger corpora record
    // the current approximate recall. The embedding counter backs the
    // probe-gate decision, so it is pinned here too.
    for n in [8usize, 64] {
        let db = make_db();
        let dim = 8;
        for i in 0..n {
            let emb = recall_fixture_vec(i, dim);
            db.insert_node(&format!("n{i}"), "s", None, None, "m", "l", Some(&emb))
                .expect("insert");
        }
        assert_eq!(
            db.get_embedding_count().expect("count"),
            n as i64,
            "embedding counter tracks N={n}"
        );
        let query = recall_fixture_vec(0, dim);
        // k > N: top-up cannot fill from HNSW alone → brute-force tail →
        // byte-identical to ground truth.
        let searched = db.vector_search(&query, n + 2).expect("search");
        let ground = db
            .bruteforce_vector_search(&query, n + 2)
            .expect("ground truth");
        assert_eq!(
            searched.iter().map(|r| r.id).collect::<Vec<_>>(),
            ground.iter().map(|r| r.id).collect::<Vec<_>>(),
            "k > N is brute-force identical at N={n}"
        );
        // k <= N: measure approximate recall (characterization only —
        // the calibration gate owns the strict bound).
        let k = 5;
        let approx = db.vector_search(&query, k).expect("approx");
        let exact = db.bruteforce_vector_search(&query, k).expect("exact");
        let exact_ids: std::collections::HashSet<i64> = exact.iter().map(|r| r.id).collect();
        let hits = approx.iter().filter(|r| exact_ids.contains(&r.id)).count();
        let recall = hits as f64 / k as f64;
        assert!(
            recall >= 0.5,
            "baseline recall at N={n}: {recall:.2} (hits {hits}/{k})"
        );
    }
}
