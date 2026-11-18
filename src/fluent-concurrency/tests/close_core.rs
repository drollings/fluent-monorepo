use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

/// A close landing between the predicate check and the park must still
/// wake the waiter. The latch fires on first predicate evaluation, the
/// test closes only after that, and the waiter returns within bounds.
#[tokio::test]
async fn close_wakes_waiter_in_check_park_window() {
    let waker = Arc::new(CloseWaker::new());
    let empty = Arc::new(Mutex::new(true));
    let latched = Arc::new(tokio::sync::Notify::new());
    let fired = Arc::new(AtomicBool::new(false));
    let w2 = Arc::clone(&waker);
    let e2 = Arc::clone(&empty);
    let l2 = Arc::clone(&latched);
    let f2 = Arc::clone(&fired);
    let waiter = tokio::spawn(async move {
        w2.wait_while(WaitChannel::Items, &e2, |is_empty| {
            if !f2.swap(true, Ordering::SeqCst) {
                l2.notify_one();
            }
            *is_empty && !w2.is_closed()
        })
        .await;
    });
    latched.notified().await;
    waker.close();
    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("close must wake the waiter")
        .expect("join");
    assert!(waker.generation() > 0);
}

/// A space wake landing between the predicate check and the park must
/// still release the waiter. State flips before the wake, mirroring the
/// pop-then-wake order, so the re-check observes it either way.
#[tokio::test]
async fn space_wake_releases_waiter_in_check_park_window() {
    let waker = Arc::new(CloseWaker::new());
    let full = Arc::new(Mutex::new(true));
    let latched = Arc::new(tokio::sync::Notify::new());
    let fired = Arc::new(AtomicBool::new(false));
    let w2 = Arc::clone(&waker);
    let e2 = Arc::clone(&full);
    let l2 = Arc::clone(&latched);
    let f2 = Arc::clone(&fired);
    let waiter = tokio::spawn(async move {
        w2.wait_while(WaitChannel::Space, &e2, |is_full| {
            if !f2.swap(true, Ordering::SeqCst) {
                l2.notify_one();
            }
            *is_full && !w2.is_closed()
        })
        .await;
    });
    latched.notified().await;
    *full.lock().expect("test mutex") = false;
    waker.wake_space();
    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("space wake must release the waiter")
        .expect("join");
    assert!(waker.generation() > 0);
}

/// Closing twice stays closed and keeps waking: the second close is a
/// safe no-op, never a panic or a lost epoch.
#[test]
fn double_close_is_idempotent() {
    let waker = CloseWaker::new();
    let before = waker.generation();
    waker.close();
    waker.close();
    assert!(waker.is_closed());
    assert!(waker.generation() > before);
}
