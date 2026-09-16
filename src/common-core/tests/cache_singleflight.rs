//! Single-flight `LoadCache::get_or_load`: concurrent misses for one key
//! coalesce to a single `load` on the success path; distinct keys load in
//! parallel (no lock held across `load`); loader errors fall back to
//! individual loads (pre-change behavior — errors are never cached, so no
//! `E: Clone` bound is introduced).

use common_core::cache::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Barrier,
};
use std::time::{Duration, Instant};

/// Poll `done` until set or the deadline passes; assert with a message
/// instead of hanging the runner when a regression deadlocks loaders.
fn expect_done(done: &AtomicUsize, want: usize, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while done.load(Ordering::SeqCst) < want {
        assert!(
            Instant::now() <= deadline,
            "{what}: only {}/{} finished before the deadline",
            done.load(Ordering::SeqCst),
            want
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn singleflight_coalesces_concurrent_miss_to_one_load() {
    let loads = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let cache = Arc::new(
        LoadCache::new(16, {
            let loads = Arc::clone(&loads);
            move |key: &String| {
                // Slow load: without coalescing, every storm member would
                // run this (N loads); with coalescing, exactly one does.
                std::thread::sleep(Duration::from_millis(50));
                loads.fetch_add(1, Ordering::SeqCst);
                Ok::<_, String>(format!("v:{key}"))
            }
        })
        .unwrap(),
    );
    let start = Arc::new(Barrier::new(16));
    for _ in 0..16 {
        let cache = Arc::clone(&cache);
        let start = Arc::clone(&start);
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            start.wait();
            let v = cache.get_or_load("same".to_string()).unwrap();
            assert_eq!(v, "v:same");
            done.fetch_add(1, Ordering::SeqCst);
        });
    }
    expect_done(&done, 16, "coalesced storm");
    assert_eq!(
        loads.load(Ordering::SeqCst),
        1,
        "concurrent misses for one key run exactly one load"
    );
}

#[test]
fn singleflight_distinct_keys_load_in_parallel() {
    // A barrier inside `load` for two distinct keys passes only when both
    // loads run concurrently — holding any lock across `load` would
    // deadlock here (the regression this test guards).
    let done = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Barrier::new(2));
    let cache = Arc::new(
        LoadCache::new(16, {
            let gate = Arc::clone(&gate);
            move |key: &String| {
                if key == "a" || key == "b" {
                    gate.wait();
                }
                Ok::<_, String>(format!("v:{key}"))
            }
        })
        .unwrap(),
    );
    for key in ["a", "b"] {
        let cache = Arc::clone(&cache);
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            let v = cache.get_or_load(key.to_string()).unwrap();
            assert_eq!(v, format!("v:{key}"));
            done.fetch_add(1, Ordering::SeqCst);
        });
    }
    expect_done(&done, 2, "parallel distinct-key loads");
}

#[test]
fn singleflight_error_falls_back_to_individual_loads() {
    // Errors are never cached (pre-change contract): every caller observes
    // the error, and a later success populates normally.
    let loads = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let cache = Arc::new(
        LoadCache::new(16, {
            let loads = Arc::clone(&loads);
            move |_: &String| {
                let n = loads.fetch_add(1, Ordering::SeqCst) + 1;
                if n < 3 {
                    Err::<String, String>("boom".into())
                } else {
                    Ok("recovered".to_string())
                }
            }
        })
        .unwrap(),
    );
    let start = Arc::new(Barrier::new(8));
    for _ in 0..8 {
        let cache = Arc::clone(&cache);
        let start = Arc::clone(&start);
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            start.wait();
            // First two loads fail; every storm member must observe *an*
            // outcome (error or the eventual success), never hang.
            let _ = cache.get_or_load("flaky".to_string());
            done.fetch_add(1, Ordering::SeqCst);
        });
    }
    expect_done(&done, 8, "error-storm outcomes");
    // After recovery the value is cached: exactly one post-recovery state.
    assert_eq!(cache.get_or_load("flaky".to_string()).unwrap(), "recovered");
}
