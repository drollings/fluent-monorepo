//! Retry/backoff drivers for concurrency consumers — a re-export surface,
//! not a reimplementation.
//!
//! The canonical schedule math (`backoff_ms`, `capped_backoff_ms`,
//! `capped_exp_backoff_ms`) and the async drivers (`retry_async`,
//! `PollWithBackoff`, `PollResult`) all live in `common_core::retry`: the
//! math is pure and zero-domain, and the drivers cannot move here without
//! creating a dependency cycle (`fluent-wvr` consumes the math but must not
//! depend on this crate). Import retry behavior from here when the consumer
//! already depends on `fluent-concurrency`; import from `common_core::retry`
//! otherwise. Both paths resolve to the same functions.

pub use common_core::retry::{
    backoff_ms, capped_backoff_ms, capped_exp_backoff_ms, retry_async, PollResult, PollWithBackoff,
};

#[cfg(test)]
#[path = "../tests/retry_reexport.rs"]
mod retry_reexport_tests;
