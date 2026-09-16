//! The `crate::retry` surface is the single concurrency-side import path for
//! the shared backoff/retry drivers: identical symbols, identical behavior,
//! zero reimplementation.

use crate::retry::{backoff_ms, capped_backoff_ms, PollWithBackoff};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

#[test]
fn reexport_is_the_same_function() {
    // Non-generic free functions: fn-pointer identity proves no second
    // implementation exists behind the re-export.
    assert_eq!(
        backoff_ms as fn(u64, u32, u32) -> u64 as usize,
        common_core::retry::backoff_ms as fn(u64, u32, u32) -> u64 as usize
    );
    assert_eq!(
        capped_backoff_ms as fn(u64, u32, u32) -> u64 as usize,
        common_core::retry::capped_backoff_ms as fn(u64, u32, u32) -> u64 as usize
    );
    assert_eq!(crate::retry::backoff_ms(100, 2, 0), 200);
}

#[tokio::test]
async fn retry_async_succeeds_first_try() {
    let calls = Arc::new(AtomicU32::new(0));
    let probe = Arc::clone(&calls);
    let out = crate::retry::retry_async(
        3,
        1,
        0,
        |_: &String| true,
        || {
            let probe = Arc::clone(&probe);
            async move {
                probe.fetch_add(1, Ordering::SeqCst);
                Ok::<_, String>(7u32)
            }
        },
    )
    .await;
    assert_eq!(out.unwrap(), 7);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn retry_async_retries_then_returns_last_error() {
    let calls = Arc::new(AtomicU32::new(0));
    let probe = Arc::clone(&calls);
    let out = crate::retry::retry_async(
        3,
        1,
        0,
        |_: &String| true,
        || {
            let probe = Arc::clone(&probe);
            async move {
                let n = probe.fetch_add(1, Ordering::SeqCst) + 1;
                Err::<u32, String>(format!("boom-{n}"))
            }
        },
    )
    .await;
    assert_eq!(out.unwrap_err(), "boom-3");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn retry_async_short_circuits_non_retryable() {
    let calls = Arc::new(AtomicU32::new(0));
    let probe = Arc::clone(&calls);
    let out = crate::retry::retry_async(
        5,
        1_000,
        50,
        |_: &String| false,
        || {
            let probe = Arc::clone(&probe);
            async move {
                probe.fetch_add(1, Ordering::SeqCst);
                Err::<u32, String>("fatal".into())
            }
        },
    )
    .await;
    // No sleep burned (base 1s would be observable); exactly one attempt.
    assert_eq!(out.unwrap_err(), "fatal");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn poll_with_backoff_ready_and_exhausted() {
    let ready = PollWithBackoff::new(std::time::Duration::from_millis(1), 2)
        .run(|| async { true })
        .await;
    assert_eq!(ready, crate::retry::PollResult::Ready);

    let attempts = Arc::new(AtomicU32::new(0));
    let probe = Arc::clone(&attempts);
    let done = PollWithBackoff::new(std::time::Duration::from_millis(1), 2)
        .with_max_failures(2)
        .run(|| {
            let probe = Arc::clone(&probe);
            async move {
                probe.fetch_add(1, Ordering::SeqCst);
                false
            }
        })
        .await;
    // First immediate tick + 2 budgeted failures = 3 ticks, then exhausted.
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert!(matches!(
        done,
        crate::retry::PollResult::Exhausted { failures: 3 }
    ));
}
