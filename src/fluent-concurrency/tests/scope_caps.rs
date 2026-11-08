use std::sync::Arc;

use fluent_concurrency::pool::{ResultPool, ResultPoolError};
use fluent_concurrency::scope::CURRENT_CAPS;
use fluent_wvr::{CapabilitySet, FsCapability};

#[tokio::test]
async fn bare_pool_worker_denies_gated_io() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("probe.txt");
    std::fs::write(&path, b"hello").expect("seed file");
    let path = Arc::new(path);
    let pool = ResultPool::new(
        fluent_concurrency::tokio_runtime(),
        2,
        4,
        move |_job: u8| {
            let path = Arc::clone(&path);
            async move {
                let fs = FsCapability::new();
                fs.read(path.as_path())
                    .await
                    .map_err(|e| e.to_string())
                    .map(|data| data.len())
            }
        },
    );
    let err = pool
        .submit(0)
        .await
        .expect_err("bare worker sees empty caps and must deny");
    match err {
        ResultPoolError::Inner(detail) => {
            assert!(
                detail.contains("missing"),
                "denial names the missing token: {detail}"
            );
        }
        other => panic!("expected handler denial, got {other:?}"),
    }
    pool.shutdown().await;
}

#[tokio::test]
async fn explicitly_scoped_work_allows_gated_io() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("probe.txt");
    std::fs::write(&path, b"hello").expect("seed file");
    let caps = CapabilitySet::new().with(FsCapability::new());
    let len = CURRENT_CAPS
        .scope(caps, async {
            let fs = FsCapability::new();
            fs.read(&path).await.expect("scoped read allows").len()
        })
        .await;
    assert_eq!(len, 5);
}
