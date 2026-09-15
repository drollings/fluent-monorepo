//! The common-core prelude — import this in every consumer to get the
//! 80% case for I/O, hashing, formatting, metrics, JSON-RPC, tokens, retry
//! schedules, locking, caching, and wall-clock time.
//!
//! ```rust
//! use common_core::prelude::*;
//! ```
//!
//! Token budgets are intentionally absent: they are LLM policy owned by
//! `fluent_llm::tokens`, not zero-domain utilities — import them from there.
//! Async retry drivers are intentionally absent too: the pure schedule math
//! (`backoff_ms`, `capped_backoff_ms`) is here, while `retry_async` and
//! `PollWithBackoff` are re-exported for concurrency consumers from
//! `fluent_concurrency::retry`.
pub use crate::{
    backoff_ms, blake3_hex, capped_backoff_ms, capped_exp_backoff_ms, ensure_dir,
    ensure_dir_or_panic, fnv1a64, format_json, format_size, hex_encode, lock, lock_read,
    lock_write, method_not_found, read_to_string_err, sha256_hex, write_atomic, IoError,
    JsonRpcError, JsonRpcHandler, JsonRpcRequest, JsonRpcResponse, LatencyHistogram, LoadCache,
    now_secs,
};
