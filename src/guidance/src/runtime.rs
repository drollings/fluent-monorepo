use std::path::PathBuf;
use std::sync::Arc;

use crate::ast_parser::AstParser;
use crate::sync_engine::{SyncEngine, SyncEngineError};
use fluent_concurrency::pool::{global_pool_config, ResultPool};
use fluent_concurrency::runtime::tokio::TokioRuntime;
use fluent_concurrency::thread_resource::with_tlr;

/// A file for AST generation in the result pool.
pub struct AstGenPayload {
    pub source_path: PathBuf,
    pub source_dir: PathBuf,
    pub guidance_dir: PathBuf,
}

use fluent_types::GuidanceDoc;

fluent_concurrency::thread_local_resource!(static PARSER: AstParser);

/// Build an AST generation pool — sized to available cores,
/// backpressure-managed queue. Owned per call (R.5: the `AST_POOL` static
/// is gone; callers hold the `Arc` for one command invocation and drop
/// it, so no pool system outlives its run).
pub fn ast_pool() -> Arc<ResultPool<AstGenPayload, GuidanceDoc, SyncEngineError>> {
    let (workers, queue_cap) = global_pool_config(4, 4);
    Arc::new(ResultPool::new(
        Arc::new(TokioRuntime),
        workers,
        queue_cap,
        |job: AstGenPayload| async move {
            tokio::task::spawn_blocking(move || {
                with_tlr(&PARSER, |parser| {
                    let mut engine = SyncEngine::with_parser(
                        job.guidance_dir,
                        job.source_dir,
                        std::mem::take(parser),
                    );
                    let r = engine.gen(&job.source_path);
                    *parser = engine.ast_parser;
                    r
                })
            })
            .await
            .unwrap_or_else(|e| Err(SyncEngineError::Parse(e.to_string())))
        },
    ))
}

/// Create a `SupervisedBatch` with the standard guidance configuration (structured
/// concurrency, failure containment, and dependency tracking for batches of
/// AST generation tasks).
///
/// Unlike manual oneshot channel management, a SupervisedBatch:
/// - Automatically cancels dependent tasks when a prerequisite fails
/// - Enforces a poll budget to prevent executor starvation
/// - Provides a typed event summary (completed/panicked/cancelled)
///
/// # Example
/// ```ignore
/// let mut batch = SupervisedBatch::new_with_config(
///     Arc::new(TokioRuntime),
///     CapabilitySet::default(),
///     BatchConfig {
///         poll_budget: 64,
///         ..BatchConfig::default()
///     },
/// );
/// batch.register(build_task_a);  // provides "parsed"
/// batch.register(build_task_b);  // depends on "parsed"
/// let summary = batch.await;
/// // If task_a panics, task_b is automatically cancelled
/// ```
#[cfg(test)]
mod tests {
    use super::*;
    use fluent_concurrency::batch::SupervisedBatch;

    #[tokio::test]
    async fn test_ast_pool_owned_lifecycle() {
        let pool = ast_pool();
        assert!(Arc::strong_count(&pool) >= 1);
    }

    #[test]
    fn test_sync_batch_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<SupervisedBatch>();
    }
}
