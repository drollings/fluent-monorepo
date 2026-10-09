#![forbid(unsafe_code)]

pub mod affinity;
// The `capability` module provides the concrete Fs/Net/Db capability tokens
// and the `io` module the gated engines behind them. Both are behind the `db`
// feature so a non-DB consumer pays nothing for the database layer.
#[cfg(feature = "db")]
pub mod capability;
pub mod close;
pub mod credit_pool;
pub mod feed_worker;
pub mod flow;
#[cfg(feature = "db")]
pub mod io;
pub mod ladder;
// The LLM protocol types (`ChatMessage`, `LlmConfig`, `LlmError`, `LlmTask`,
// `LlmQueueConfig`) and `LlmRequestQueue` are owned by
// `fluent_llm::protocol` — a thin wrapper over this crate's generic
// `ResultPool`. Import LLM queue types from `fluent_llm`, never from here.
// The generic executor (`pool::ResultPool`, `stream::StreamAbort`,
// `runtime::TestRuntime`) stays and is composed by the protocol owner.
pub mod batch;
pub mod pool;
pub mod queue;
pub mod reconnect;
pub mod retry;
pub mod runtime;
pub mod scope;
pub mod stream;
pub mod sync;
pub mod thread_resource;

use std::sync::Arc;

/// Returns a new `Arc<dyn Runtime>` wrapping the production Tokio runtime.
/// Use this instead of `crate::tokio_runtime()`.
pub fn tokio_runtime() -> Arc<dyn fluent_wvr::Runtime> {
    Arc::new(runtime::tokio::TokioRuntime)
}
#[cfg(test)]
#[path = "../tests/mod.rs"]
mod e2e_tests;

#[cfg(test)]
#[path = "../tests/affinity_calibration.rs"]
mod affinity_calibration_tests;
