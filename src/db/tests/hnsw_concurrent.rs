//! Concurrent HNSW behavior: generation purity under rebuild races +
//! single-pass rebuild consumption.

use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use crate::vector::{try_bytes_to_vec, vec_to_bytes};

fn next_f32(state: &mut u64) -> f32 {
    // xorshift64* — deterministic, no new dev-dependency.
    let mut x = *state | 1;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    ((x.wrapping_mul(0x2545F4914F6CDD1D) >> 33) as f64 / f64::from(u32::MAX) * 2.0 - 1.0) as f32
}

fn dataset(base_id: i64, n: usize, dim: usize, seed: u64) -> Vec<(i64, Vec<f32>)> {
    let mut state = seed;
    (0..n)
        .map(|i| {
            let mut v: Vec<f32> = (0..dim).map(|_| next_f32(&mut state)).collect();
            if v.iter().all(|x| *x == 0.0) {
                v[0] = 0.5;
            }
            (base_id + i as i64, v)
        })
        .collect()
}

fn decode(blob: &[u8]) -> Option<Vec<f32>> {
    try_bytes_to_vec(blob)
}

fn blobs(rows: &[(i64, Vec<f32>)]) -> Vec<(i64, Vec<u8>)> {
    rows.iter()
        .map(|(id, emb)| (*id, vec_to_bytes(emb)))
        .collect()
}

#[test]
fn concurrent_rebuild_never_mixes_generations() {
    const N: usize = 128;
    const DIM: usize = 8;
    const K: usize = 5;
    // Disjoint id ranges per generation: a torn read (hits resolved against
    // the wrong generation's map) shows up as a mixed A/B result, which an
    // approximate index could never produce from a clean read.
    let a = dataset(0, N, DIM, 11);
    let b = dataset(10_000, N, DIM, 77);
    let queries: Vec<Vec<f32>> = (0..4).map(|s| dataset(-1, 1, DIM, 1000 + s)[0].1.clone()).collect();

    let index = Arc::new(HnswIndex::new());
    index
        .rebuild_from(blobs(&a).into_iter(), decode)
        .expect("prebuild A");

    let stop = Arc::new(AtomicBool::new(false));
    let non_empty = Arc::new(AtomicUsize::new(0));

    std::thread::scope(|scope| {
        let (index_rb, a_rb, b_rb, stop_rb) = (
            Arc::clone(&index),
            &a,
            &b,
            Arc::clone(&stop),
        );
        scope.spawn(move || {
            for round in 0..60 {
                let rows = if round % 2 == 0 { b_rb } else { a_rb };
                index_rb
                    .rebuild_from(blobs(rows).into_iter(), decode)
                    .expect("rebuild");
            }
            stop_rb.store(true, Ordering::Release);
        });

        for worker in 0..4 {
            let (index_s, queries_s, stop_s, non_empty_s) = (
                Arc::clone(&index),
                &queries,
                Arc::clone(&stop),
                Arc::clone(&non_empty),
            );
            scope.spawn(move || {
                let mut probe = worker;
                while !stop_s.load(Ordering::Acquire) {
                    let query = &queries_s[probe % queries_s.len()];
                    probe += 1;
                    let Some(hits) = crate::hnsw::hnsw_lookup(&index_s, query, K) else {
                        continue; // empty index or retry-exhausted race: the brute-force path
                    };
                    assert!(!hits.is_empty());
                    non_empty_s.fetch_add(1, Ordering::Relaxed);
                    let all_a = hits.iter().all(|(id, _)| (0..N as i64).contains(id));
                    let all_b = hits
                        .iter()
                        .all(|(id, _)| (10_000..10_000 + N as i64).contains(id));
                    assert!(
                        all_a || all_b,
                        "mixed-generation result: {hits:?} (torn id-map read)"
                    );
                }
            });
        }
    });

    assert!(
        non_empty.load(Ordering::Relaxed) > 0,
        "searchers must observe populated generations"
    );
}

#[test]
fn rebuild_consumes_rows_in_a_single_pass() {
    let rows = dataset(0, 64, 8, 5);
    let pulls = Arc::new(AtomicUsize::new(0));
    let pulls_iter = Arc::clone(&pulls);
    // A lazy generator, not a collected `Vec`: the rebuild must accept a
    // pure single-pass iterator and pull each row exactly once.
    let iter = rows.iter().map(move |(id, emb)| {
        pulls_iter.fetch_add(1, Ordering::Relaxed);
        (*id, vec_to_bytes(emb))
    });
    let index = HnswIndex::new();
    let examined = index.rebuild_from(iter, decode).expect("rebuild");
    assert_eq!(examined, rows.len());
    assert_eq!(pulls.load(Ordering::Relaxed), rows.len());
    assert_eq!(index.len(), rows.len());
}

#[test]
fn rebuild_counts_skipped_rows_as_examined() {
    let mut rows = blobs(&dataset(0, 16, 8, 9));
    rows.push((10_001, vec![1_u8, 2, 3])); // not a multiple of 4: undecodable
    rows.push((10_002, vec![])); // empty blob: skipped
    let index = HnswIndex::new();
    let examined = index.rebuild_from(rows.into_iter(), decode).expect("rebuild");
    assert_eq!(examined, 18, "every row counts as examined, valid or not");
    assert_eq!(index.len(), 16);
}
