//! Bounded async queue, worker pool, and concurrency limiter.
//!
//! Compatibility facade: the public pool types live in the leaf modules
//! below and are re-exported here, so every existing
//! `fluent_concurrency::pool::X` path keeps resolving byte-identically.
//! New code may import the leaf path (for example
//! `fluent_concurrency::pool::queue::Queue`) directly.

mod common;
pub mod limiter;
pub mod priority;
pub mod queue;
pub mod result;
pub mod traits;
pub mod worker;

pub use self::common::global_pool_config;
pub use self::limiter::Limiter;
pub use self::priority::PriorityResultPool;
pub use self::queue::Queue;
pub use self::result::{ResultPool, ResultPoolError};
pub use self::traits::{PoolError, PriorityRequestPool, RequestPool, SinkPool};
pub use self::worker::WorkerPool;

#[cfg(test)]
#[path = "../tests/pool.rs"]
mod tests;
