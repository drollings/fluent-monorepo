//! Trading recompute pool: heavy oscillator normalization off the frame.
//!
//! The pool shape (bounded workers, queued jobs, result-returning submits)
//! composes the shared [`ResultPool`](fluent_concurrency::pool::ResultPool);
//! the only domain content is the handler — one oscillator series rebuilt
//! into the shared display band per job.

use fluent_concurrency::pool::{ResultPool, ResultPoolError};
use fluent_concurrency::tokio_runtime;
use fluent_egui::EguiError;

use crate::oscillators::{OscillatorKind, normalize_series};

/// One heavy series rebuild: normalize `values` into the shared band.
#[derive(Debug, Clone)]
pub struct RecomputeJob {
    /// Series under rebuild with holes preserved.
    pub values: Vec<Option<f64>>,
    /// Band constants to normalize with.
    pub kind: OscillatorKind,
}

/// Bounded worker pool for heavy series rebuilds.
pub struct RecomputePool {
    pool: ResultPool<RecomputeJob, Vec<Option<f64>>, String>,
}

impl RecomputePool {
    /// Open a pool with `cap` workers and `queue_capacity` queued jobs;
    /// refuses zero caps with reasons.
    pub fn new(
        cap: usize,
        queue_capacity: usize,
    ) -> Result<Self, EguiError> {
        if cap == 0 {
            return Err(EguiError::InvalidParam {
                what: "cap".to_owned(),
                reason: "recompute pool needs at least one worker".to_owned(),
            });
        }
        if queue_capacity == 0 {
            return Err(EguiError::InvalidParam {
                what: "queue_capacity".to_owned(),
                reason: "recompute queue must hold at least one job".to_owned(),
            });
        }
        let pool = ResultPool::new(tokio_runtime(), cap, queue_capacity, |job: RecomputeJob| async move {
            Ok(normalize_series(&job.values, job.kind))
        });
        Ok(Self { pool })
    }

    /// Rebuild one series, awaiting the worker's result.
    pub async fn submit(
        &self,
        job: RecomputeJob,
    ) -> Result<Vec<Option<f64>>, ResultPoolError<String>> {
        self.pool.submit(job).await
    }

    /// Number of workers.
    #[must_use]
    pub fn worker_count(&self) -> usize {
        self.pool.worker_count()
    }

    /// Close the queue and await workers.
    pub async fn shutdown(self) {
        self.pool.shutdown().await;
    }
}

#[cfg(test)]
#[path = "../tests/feed.rs"]
mod tests;
