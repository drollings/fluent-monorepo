//! Speaker embeddings: batched extraction behind a seam.
//!
//! The ONNX embedding model lands later; this module owns the batching
//! shape (bounded `WorkerPool`, explicit caps, background priority) so
//! the model slots in without touching callers. Tests serve scripted
//! vectors through the same seam.

use std::sync::Arc;

use fluent_concurrency::pool::{ResultPool, ResultPoolError};

/// Embedding dimensionality for scripted vectors in tests.
pub const TEST_DIMS: usize = 8;

/// Embedding seam: one vector per segment. Implementations hold the
/// model session; fakes hold a script.
pub trait EmbedSeam: Send + Sync {
    /// Embed the segment at `index` (audio slice available to real
    /// backends; fakes key off the index).
    fn embed(&self, index: usize, samples: &[f32]) -> Vec<f32>;
}

/// Batch-embed `segments` through `seam` on a bounded pool (cap
/// `parallel`, background priority). Results return in index order.
/// Pool refusal or an empty worker slot errors instead of panicking.
pub fn batch_embed(
    seam: Arc<dyn EmbedSeam>,
    segments: &[Vec<f32>],
    parallel: usize,
) -> Result<Vec<Vec<f32>>, String> {
    common_core::runtime::block_on(async {
        let pool = ResultPool::new(
            fluent_concurrency::tokio_runtime(),
            parallel.max(1),
            segments.len().max(1),
            move |(index, samples): (usize, Vec<f32>)| {
                let seam = Arc::clone(&seam);
                async move {
                    let vector = seam.embed(index, &samples);
                    Ok::<(usize, Vec<f32>), std::convert::Infallible>((index, vector))
                }
            },
        );
        let mut indexed = futures_util::future::try_join_all(
            segments.iter().cloned().enumerate().map(|(index, samples)| {
                let pool = &pool;
                async move {
                    pool.submit((index, samples)).await.map_err(|error| match error {
                        ResultPoolError::Pool(error) => {
                            format!("orator: embed pool refused segment {index}: {error}")
                        }
                        ResultPoolError::Canceled => {
                            "orator: embed worker left a segment empty".to_string()
                        }
                        ResultPoolError::Inner(never) => match never {},
                    })
                }
            }),
        )
        .await?;
        pool.shutdown().await;
        indexed.sort_by_key(|(index, _)| *index);
        Ok::<Vec<Vec<f32>>, String>(
            indexed.into_iter().map(|(_, vector)| vector).collect(),
        )
    })
}

/// Scripted seam for tests: returns one canned vector per index,
/// cycling the script.
pub struct ScriptSeam {
    /// Vectors to cycle through.
    pub script: Vec<Vec<f32>>,
}

impl EmbedSeam for ScriptSeam {
    fn embed(&self, index: usize, _samples: &[f32]) -> Vec<f32> {
        self.script[index % self.script.len()].clone()
    }
}

#[cfg(test)]
#[path = "../../tests/diary_embed.rs"]
mod tests;

#[cfg(test)]
#[path = "../../tests/diary_result_pool_shape.rs"]
mod shape_tests;
