//! Signal-cancellation tests: a real SIGTERM to this process releases
//! the endpoint and reports the interruption.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::{cancel_on_signal, SignalWatch};

#[test]
fn signal_releases_and_cancels() {
    common_core::runtime::block_on(async {
        let released = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&released);
        let pid = std::process::id();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let _ = std::process::Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .status();
        });
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            cancel_on_signal(std::future::pending::<u32>(), move || {
                flag.store(true, Ordering::SeqCst);
            }),
        )
        .await
        .expect("the signal must arrive");
        assert_eq!(out, None, "a signalled task reports cancellation");
        assert!(released.load(Ordering::SeqCst), "release runs on signal");
    });
}

#[test]
fn watch_marks_and_releases_on_signal() {
    let released = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&released);
    let watch = SignalWatch::arm(move || {
        flag.store(true, Ordering::SeqCst);
    });
    assert!(!watch.signalled(), "no signal yet");
    std::thread::sleep(std::time::Duration::from_millis(300));
    let pid = std::process::id();
    let _ = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !watch.signalled() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(watch.signalled(), "the watch observes the signal");
    assert!(released.load(Ordering::SeqCst), "the watch releases on signal");
}
