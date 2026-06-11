//! A partitioned router that distributes jobs across sharded worker pools.

use crate::pool::{PoolError, WorkerPool};

/// Routes jobs to sharded `WorkerPool` instances by hashing the key.
/// All jobs with the same key go to the same shard (same pool).
pub struct PartitionedRouter<K, J: Send + 'static> {
    shards: Vec<WorkerPool<J>>,
    hash: fn(&K) -> usize,
}

impl<K, J: Send + Sync + 'static> PartitionedRouter<K, J> {
    pub fn new(shards: Vec<WorkerPool<J>>, hash: fn(&K) -> usize) -> Self {
        Self { shards, hash }
    }

    pub async fn submit(&self, key: &K, job: J) -> Result<(), PoolError> {
        let idx = (self.hash)(key) % self.shards.len();
        self.shards[idx].submit(job).await
    }
}
