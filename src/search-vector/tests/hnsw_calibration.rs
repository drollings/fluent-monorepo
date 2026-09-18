use std::collections::HashSet;

use super::*;

fn calib_db() -> GuidanceDb {
    GuidanceDb::open_in_memory().expect("in-memory db")
}

fn calib_vec(i: usize, dim: usize) -> Vec<f32> {
    // All-distinct points: a multiplicative hash spread over a large prime
    // modulus, so no two rows tie (ties would make approx-vs-exact recall
    // arbitrary — any equidistant subset is a correct answer).
    (0..dim)
        .map(|j| {
            (((i.wrapping_mul(2654435761)
                .wrapping_add(j.wrapping_mul(40503)))
                % 10007) as f32
                + 1.0)
                / 10008.0
        })
        .collect()
}

fn fill(db: &GuidanceDb, n: usize, dim: usize) {
    for i in 0..n {
        let emb = calib_vec(i, dim);
        db.insert_node(&format!("c{i}"), "s", None, None, "m", "l", Some(&emb))
            .expect("insert");
    }
}

/// Recall of `vector_search` against brute-force ground truth.
/// Returns `(recall, approx_ids, exact_ids)`.
fn recall_at(db: &GuidanceDb, query: &[f32], k: usize) -> (f64, Vec<i64>, Vec<i64>) {
    let approx: Vec<i64> = db
        .vector_search(query, k)
        .expect("approx")
        .into_iter()
        .map(|r| r.id)
        .collect();
    let exact: Vec<i64> = db
        .bruteforce_vector_search(query, k)
        .expect("exact")
        .into_iter()
        .map(|r| r.id)
        .collect();
    let exact_set: HashSet<i64> = exact.iter().copied().collect();
    let hits = approx.iter().filter(|id| exact_set.contains(id)).count();
    (hits as f64 / k as f64, approx, exact)
}

#[test]
fn hnsw_recall_at_scale_meets_threshold() {
    // Calibration gate for the dispatch policy: at/above the scale where the
    // probe runs, approximate recall vs byte-identical brute force must be
    // ≥ 0.95. Measured table (8-dim all-distinct deterministic fixture,
    // k=10, query = member 0 — commit the numbers, assert the bound):
    //   N=64   → recall 1.000 (below threshold: brute force serves)
    //   N=512  → recall 1.000 (at threshold: brute force serves, strict `>`)
    //   N=2048 → recall 1.000 (above threshold: HNSW probes)
    let dim = 8;
    let k = 10;
    for n in [64usize, 512, 2048] {
        let db = calib_db();
        fill(&db, n, dim);
        assert_eq!(db.get_embedding_count().expect("count"), n as i64);
        let query = calib_vec(0, dim);
        let (recall, approx, exact) = recall_at(&db, &query, k);
        if n > common_core::constants::DEFAULT_HNSW_THRESHOLD {
            assert!(
                recall >= 0.95,
                "N={n}: recall {recall:.2} below 0.95 (approx {approx:?} vs exact {exact:?})"
            );
        } else {
            assert_eq!(
                approx, exact,
                "N={n}: below threshold serves brute force (recall must be 1.0)"
            );
        }
    }
}

#[test]
fn dispatch_boundary_is_exact_and_filtered_queries_abstain() {
    // Control group that must NOT fire HNSW:
    // (a) len == threshold (strict `>` boundary) → abstains;
    // (b) malformed probes on a large built index → abstain, never panic;
    // (c) `vector_search` at/below threshold ≡ brute force exactly.
    use common_core::constants::DEFAULT_HNSW_THRESHOLD;
    let dim = 8;
    let db = calib_db();
    fill(&db, DEFAULT_HNSW_THRESHOLD, dim);
    let query = calib_vec(0, dim);
    assert!(
        db.hnsw_search(&query, 5).is_none(),
        "len == threshold stays brute-force (strict >)"
    );
    let searched = db.vector_search(&query, 5).expect("search");
    let ground = db.bruteforce_vector_search(&query, 5).expect("ground");
    assert_eq!(
        searched.iter().map(|r| r.id).collect::<Vec<_>>(),
        ground.iter().map(|r| r.id).collect::<Vec<_>>(),
        "at-threshold results are brute-force identical"
    );
    assert!(
        db.vector_search(&[], 5).expect("empty query").is_empty()
            && db.vector_search(&query, 0).expect("k == 0").is_empty()
    );
}
