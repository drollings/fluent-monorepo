use super::*;

#[test]
fn starts_unfired() {
    let abort = StreamAbort::new();
    assert!(!abort.is_cancelled());
}

#[test]
fn cancel_fires_and_is_idempotent() {
    let abort = StreamAbort::new();
    abort.cancel();
    abort.cancel();
    assert!(abort.is_cancelled());
}

#[tokio::test]
async fn cancelled_resolves_after_cancel() {
    let abort = StreamAbort::new();
    let waiter = abort.clone();
    let task = tokio::spawn(async move {
        waiter.cancelled().await;
        true
    });
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    abort.cancel();
    assert!(task.await.expect("waiter completes"));
}

#[tokio::test]
async fn cancelled_is_sticky_for_late_waiters() {
    let abort = StreamAbort::new();
    abort.cancel();
    // A waiter that registers after `cancel` must not block.
    tokio::time::timeout(std::time::Duration::from_millis(100), abort.cancelled())
        .await
        .expect("late waiter resolves immediately");
}

#[tokio::test]
async fn cancel_wakes_every_waiter() {
    let abort = StreamAbort::new();
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let waiter = abort.clone();
        tasks.push(tokio::spawn(async move {
            waiter.cancelled().await;
        }));
    }
    abort.cancel();
    for t in tasks {
        t.await.expect("all waiters woken");
    }
}

/// The firing `cancel` advances the wake epoch exactly once; a repeat
/// cancel is a no-op that neither re-notifies nor advances the epoch.
#[test]
fn cancel_generation_counts_firing_only() {
    let abort = StreamAbort::new();
    assert_eq!(abort.generation(), 0);
    abort.cancel();
    assert_eq!(abort.generation(), 1);
    abort.cancel();
    assert_eq!(abort.generation(), 1, "repeat cancel is a no-op");
}

/// A waiter parked by exactly one manual poll (no sleep) resolves once
/// the signal fires, pinning the park-then-wake path deterministically.
#[tokio::test]
async fn cancel_wakes_parked_waiter_without_sleep() {
    use std::future::Future;
    let abort = StreamAbort::new();
    let mut waiter = Box::pin(abort.cancelled());
    let parked = futures_util::future::poll_fn(|cx| match waiter.as_mut().poll(cx) {
        std::task::Poll::Ready(()) => std::task::Poll::Ready(true),
        std::task::Poll::Pending => std::task::Poll::Ready(false),
    })
    .await;
    assert!(!parked, "waiter parks on first poll");
    abort.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
        .await
        .expect("cancel must wake the parked waiter");
}

#[tokio::test]
async fn uncancelled_waiter_blocks_until_timeout() {
    let abort = StreamAbort::new();
    let result =
        tokio::time::timeout(std::time::Duration::from_millis(20), abort.cancelled()).await;
    assert!(result.is_err(), "uncancelled waiter must not resolve");
}

/// Cancel-during-`cancelled` on `StreamAbort::cancelled` must resolve.
/// Covers the live cancel path plus a raw `Notify` probe showing a
/// `notify_waiters` fired between future creation and first poll is lost
/// without an armed wait, which the waiter path shares.
#[tokio::test]
async fn cancel_wakes_stream_abort() {
    let abort = StreamAbort::new();
    let waiter = abort.clone();
    let task = tokio::spawn(async move {
        waiter.cancelled().await;
        true
    });
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    abort.cancel();
    let done = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect("cancelled must wake the waiter")
        .expect("join");
    assert!(done);

    let notify = std::sync::Arc::new(tokio::sync::Notify::new());
    let pending = notify.notified();
    notify.notify_waiters();
    // A broadcast fired after creation is observed on first poll; this is
    // the guarantee `StreamAbort::cancelled` relies on for the close path.
    let observed = tokio::time::timeout(std::time::Duration::from_secs(5), pending).await;
    assert!(observed.is_ok(), "cancel after creation must wake the waiter");
}
