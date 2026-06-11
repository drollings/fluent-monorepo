use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use fluent_concurrency::pool::WorkerPool;
use fluent_concurrency::runtime::tokio::TokioRuntime;
use tokio::sync::oneshot;

use crate::sync_engine::{GenConfig, SyncEngine, SyncEngineError};
use guidance_types::GuidanceDoc;
use guidance_search_vector::GuidanceDb;

/// Shared multi-threaded tokio runtime for all async guidance operations.
pub static RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("failed to build guidance runtime")
});

/// A file for AST generation in the worker pool.
pub struct AstGenJob {
    pub source_path: PathBuf,
    pub source_dir: PathBuf,
    pub guidance_dir: PathBuf,
    pub config: GenConfig,
    pub result_tx: oneshot::Sender<Result<GuidanceDoc, SyncEngineError>>,
}

/// A database sync job for the worker pool.
pub struct DbSyncJob {
    pub json_dir: PathBuf,
    pub db_path: PathBuf,
    pub result_tx: oneshot::Sender<Result<usize, String>>,
}

/// Shared AST generation pool — up to 4 concurrent parsers, queue capacity of 200.
pub static AST_POOL: LazyLock<Arc<WorkerPool<AstGenJob>>> = LazyLock::new(|| {
    RT.block_on(async {
        Arc::new(WorkerPool::new(
            Arc::new(TokioRuntime),
            4,
            200,
            |job: AstGenJob| async move {
                let result = tokio::task::spawn_blocking(move || {
                    let mut engine = SyncEngine::new(job.guidance_dir, job.source_dir);
                    engine.gen_with_config(&job.source_path, &job.config)
                })
                .await
                .unwrap_or_else(|e| Err(SyncEngineError::Parse(e.to_string())));
                let _ = job.result_tx.send(result);
            },
        ))
    })
});

/// Shared database sync pool — serializes writes to avoid SQLite contention.
pub static DB_POOL: LazyLock<Arc<WorkerPool<DbSyncJob>>> = LazyLock::new(|| {
    RT.block_on(async {
        Arc::new(WorkerPool::new(
            Arc::new(TokioRuntime),
            1,
            100,
            |job: DbSyncJob| async move {
                let result = tokio::task::spawn_blocking(move || {
                    let db = GuidanceDb::open(&job.db_path)
                        .map_err(|e| e.to_string())?;
                    db.sync_from_dir(&job.json_dir)
                        .map_err(|e| e.to_string())
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
                let _ = job.result_tx.send(result);
            },
        ))
    })
});

/// Block on a future using the shared runtime.
pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
    RT.block_on(future)
}
