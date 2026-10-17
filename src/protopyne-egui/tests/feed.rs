use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn heavy_recompute_matches_direct_compute() {
    let pool = RecomputePool::new(2, 8).expect("valid pool");
    assert_eq!(pool.worker_count(), 2);
    let job =
        RecomputeJob { values: vec![None, Some(50.0), Some(100.0)], kind: OscillatorKind::Rsi };
    let rebuilt = pool.submit(job.clone()).await.expect("rebuilt");
    assert_eq!(rebuilt, normalize_series(&job.values, job.kind).expect("evaluable kind"));
    assert!(RecomputePool::new(0, 8).is_err() && RecomputePool::new(2, 0).is_err());
    pool.shutdown().await;
}
