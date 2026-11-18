#![forbid(unsafe_code)]

pub mod affinity;
// The `capability` module provides the concrete Fs/Net capability tokens and
// the `io` module the gated fs/net engines behind them; both are available
// without the `db` feature so non-DB consumers pay nothing for the database
// layer. Only `io::db` and `capability::capability_set_with_db` sit behind
// `db` (they compose `fluent-db`).
pub mod capability;
pub mod close;
pub mod credit_pool;
pub mod feed_worker;
pub mod flow;
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
/// Test-only reconnect/backfill sketch: no production consumer exists,
/// and its `enqueue`-on-full-feed drop is lossy, so it must not ship as
/// public API. Exercised by its own tests; wire a real consumer before
/// promoting it back to a public module.
#[cfg(test)]
pub mod reconnect;
pub mod retry;
pub mod runtime;
pub mod scope;
pub mod stream;
pub mod sync;
pub mod thread_resource;

use std::sync::Arc;

/// Returns a new `Arc<dyn Runtime>` wrapping the production Tokio runtime.
///
/// Prefer this seam over `tokio::spawn` so tests can substitute a fake.
pub fn tokio_runtime() -> Arc<dyn fluent_wvr::Runtime> {
    Arc::new(runtime::tokio::TokioRuntime)
}
#[cfg(test)]
#[path = "../tests/mod.rs"]
mod e2e_tests;

#[cfg(test)]
#[path = "../tests/affinity_calibration.rs"]
mod affinity_calibration_tests;
