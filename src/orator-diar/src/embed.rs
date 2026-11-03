//! Speaker embeddings: batched extraction behind a seam.
//!
//! The ONNX embedding model lands later; this module owns the batching
//! shape (bounded `WorkerPool`, explicit caps, background priority) so
//! the model slots in without touching callers. Tests serve scripted
//! vectors through the same seam.

use std::sync::{Arc, Mutex};

use fluent_concurrency::pool::WorkerPool;

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
    let out: Arc<Mutex<Vec<Option<Vec<f32>>>>> = Arc::new(Mutex::new(
        (0..segments.len()).map(|_| None).collect(),
    ));
    common_core::runtime::block_on(async {
        let worker_out = Arc::clone(&out);
        let pool = WorkerPool::new(
            fluent_concurrency::tokio_runtime(),
            parallel.max(1),
            segments.len().max(1),
            move |(index, samples): (usize, Vec<f32>)| {
                let seam = Arc::clone(&seam);
                let out = Arc::clone(&worker_out);
                async move {
                    let vector = seam.embed(index, &samples);
                    common_core::sync::lock(&out)[index] = Some(vector);
                }
            },
        );
        for (index, samples) in segments.iter().cloned().enumerate() {
            pool.submit((index, samples)).await.map_err(|error| {
                format!("orator: embed pool refused segment {index}: {error}")
            })?;
        }
        pool.shutdown().await;
        Ok::<(), String>(())
    })?;
    let mut guard = common_core::sync::lock(&out);
    guard
        .drain(..)
        .map(|slot| {
            slot.ok_or_else(|| "orator: embed worker left a segment empty".to_string())
        })
        .collect()
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
#[path = "../tests/embed.rs"]
mod tests;
