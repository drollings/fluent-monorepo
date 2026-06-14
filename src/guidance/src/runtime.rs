use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use fluent_concurrency::pool::WorkerPool;
use fluent_concurrency::runtime::tokio::TokioRuntime;
use tokio::sync::oneshot;

use crate::ast_parser::AstParser;
use crate::sync_engine::{GenConfig, SyncEngine, SyncEngineError};
use guidance_search_vector::GuidanceDb;
use guidance_types::GuidanceDoc;

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

/// Shared AST generation pool — sized to available cores, backpressure-managed queue.
pub static AST_POOL: LazyLock<Arc<WorkerPool<AstGenJob>>> = LazyLock::new(|| {
    let workers = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    Arc::new(WorkerPool::new(
        Arc::new(TokioRuntime),
        workers,
        workers * 4,
        |job: AstGenJob| async move {
            let result = tokio::task::spawn_blocking(move || {
                thread_local! {
                    static PARSER: RefCell<Option<AstParser>> = const { RefCell::new(None) };
                }
                PARSER.with(|cell| {
                    let parser = cell.borrow_mut().take().unwrap_or_else(AstParser::new);
                    let mut engine =
                        SyncEngine::with_parser(job.guidance_dir, job.source_dir, parser);
                    let r = engine.gen_with_config(&job.source_path, &job.config);
                    *cell.borrow_mut() = Some(engine.ast_parser);
                    r
                })
            })
            .await
            .unwrap_or_else(|e| Err(SyncEngineError::Parse(e.to_string())));
            let _ = job.result_tx.send(result);
        },
    ))
});

/// Shared database sync pool — serializes writes to avoid SQLite contention.
pub static DB_POOL: LazyLock<Arc<WorkerPool<DbSyncJob>>> = LazyLock::new(|| {
    Arc::new(WorkerPool::new(
        Arc::new(TokioRuntime),
        1,
        100,
        |job: DbSyncJob| async move {
            let result = tokio::task::spawn_blocking(move || {
                let db = GuidanceDb::open(&job.db_path).map_err(|e| e.to_string())?;
                db.sync_from_dir(&job.json_dir).map_err(|e| e.to_string())
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            let _ = job.result_tx.send(result);
        },
    ))
});
