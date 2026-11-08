use std::time::{Duration, Instant};

use fluent_concurrency::pool::ResultPool;

#[tokio::test]
async fn sequential_submit_loop_costs_sum_of_jobs() {
    let pool = ResultPool::new(
        fluent_concurrency::tokio_runtime(),
        8,
        4,
        move |job: u64| async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            Ok::<u64, String>(job)
        },
    );
    let start = Instant::now();
    let mut ordered = Vec::new();
    for job in 0..4u64 {
        let value = pool.submit(job).await.expect("job succeeds");
        ordered.push(value);
    }
    let elapsed = start.elapsed();
    pool.shutdown().await;
    assert_eq!(ordered, vec![0, 1, 2, 3]);
    assert!(
        elapsed >= Duration::from_millis(240),
        "four 80ms jobs through one-at-a-time submit take their sum: {elapsed:?}"
    );
}
