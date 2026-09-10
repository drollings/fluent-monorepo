//! P6 calibration bench: HNSW vs int8 brute-force KNN across corpus sizes.
//!
//! Method (mirrors the router `threshold_calibration` pattern):
//! deterministic synthetic vectors (LCG, dims 384), k = 7. Reports
//! per-query latency for both paths plus HNSW top-1 ∈ brute top-3
//! agreement. The `AdaptiveHnsw` policy (brute force at or below
//! `DEFAULT_HNSW_THRESHOLD = 512`) is pinned by unit tests; this bench
//! supplies the timing evidence behind it.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use fluent_db::hnsw::HnswIndex;
use fluent_db::vector::{QuantizedEmbedding, knn_brute_force_q8};

const DIMS: usize = 384;
const K: usize = 7;

/// Deterministic pseudo-vector (LCG over the seed; no RNG dependency).
fn synthetic(seed: u64) -> Vec<f32> {
    let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (0..DIMS)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((state >> 33) as f32) / (u32::MAX as f32) - 0.5
        })
        .collect()
}

fn bench_hnsw_vs_brute(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("hnsw_vs_brute_k7_dims384");
    for size in [128usize, 512, 2048] {
        let vectors: Vec<Vec<f32>> = (0..size as u64).map(synthetic).collect();
        let quantized: Vec<QuantizedEmbedding> =
            vectors.iter().map(|v| QuantizedEmbedding::from_f32(v)).collect();
        let query = synthetic(0xC0FFEE);

        // Agreement (computed once, printed for the record): HNSW top-1
        // inside brute-force top-3.
        let index = HnswIndex::new();
        for (i, vec) in vectors.iter().enumerate() {
            index.insert(i as i64, vec);
        }
        let brute_top3: Vec<usize> = knn_brute_force_q8(
            &QuantizedEmbedding::from_f32(&query),
            quantized.iter().enumerate().map(|(i, q)| (i, q)),
            3,
        )
        .into_iter()
        .map(|(id, _)| id)
        .collect();
        let hnsw_top1: Vec<usize> = index.search(&query, 1).into_iter().map(|(id, _)| id).collect();
        let agreement = hnsw_top1.first().is_some_and(|top| brute_top3.contains(top));
        println!("size {size}: hnsw-top1-in-brute-top3 = {agreement}");

        group.bench_with_input(BenchmarkId::new("brute_q8", size), &size, |bencher, _| {
            bencher.iter(|| {
                knn_brute_force_q8(
                    black_box(&QuantizedEmbedding::from_f32(&query)),
                    black_box(quantized.iter().enumerate().map(|(i, q)| (i, q))),
                    black_box(K),
                )
            });
        });
        group.bench_with_input(BenchmarkId::new("hnsw", size), &size, |bencher, _| {
            bencher.iter(|| index.search(black_box(&query), black_box(K)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_hnsw_vs_brute);
criterion_main!(benches);
