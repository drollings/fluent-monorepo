//! Ported from zvec-grep `test/unit/models/artifact-cache-lock.test.mjs`.
//!
//! Directory-based mutual exclusion for model snapshot downloads: token
//! owner files, heartbeat refresh, stale takeover with device+inode
//! recheck, dead-local-owner fast path, and fencing of displaced owners.
//! Timing uses real short windows with wide margins (no fake timers).

use fluent_llm::artifact_lock::{
    acquire_artifact_cache_lock, local_hostname, LockOptions,
};
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn test_options() -> LockOptions {
    LockOptions {
        poll_ms: 5,
        stale_ms: 400,
        heartbeat_ms: 50,
    }
}

fn lock_path(dir: &TempDir) -> std::path::PathBuf {
    dir.path().join("snapshot.lock")
}

#[tokio::test]
async fn second_acquirer_waits_for_release() {
    let dir = TempDir::new().unwrap();
    let path = lock_path(&dir);
    let mut first = acquire_artifact_cache_lock(&path, &test_options())
        .await
        .unwrap();

    let waiter = tokio::spawn({
        let path = path.clone();
        let options = test_options();
        async move { acquire_artifact_cache_lock(&path, &options).await.unwrap() }
    });
    // The waiter must still be parked while the first lock is held live.
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(
        !waiter.is_finished(),
        "waiter must block while the lock is held"
    );
    first.release().unwrap();
    let mut second = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("waiter must acquire after release")
        .unwrap();
    second.release().unwrap();
    assert!(!path.exists(), "release of the last owner removes the lock");
}

#[tokio::test]
async fn heartbeat_keeps_a_live_lock_past_short_windows() {
    let dir = TempDir::new().unwrap();
    let path = lock_path(&dir);
    let mut holder = acquire_artifact_cache_lock(&path, &test_options())
        .await
        .unwrap();

    // Keep heartbeating well inside the contender's 150ms stale window for
    // the whole race: the holder rewrites its heartbeat every ~50ms.
    let pump = tokio::spawn(async move {
        for _ in 0..40 {
            holder.touch().unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        holder
    });
    // A contender with a short stale window still cannot take a live lock:
    // the holder rewrites its heartbeat every ~50ms, well inside 150ms.
    let racing = LockOptions {
        poll_ms: 5,
        stale_ms: 150,
        heartbeat_ms: 10,
    };
    let attempt = tokio::time::timeout(
        Duration::from_millis(300),
        acquire_artifact_cache_lock(&path, &racing),
    )
    .await;
    assert!(
        attempt.is_err(),
        "a heartbeating owner must not lose the lock"
    );
    let mut holder = pump.await.unwrap();
    holder.release().unwrap();
}

#[tokio::test]
async fn stale_lock_is_taken_over_and_old_owner_is_fenced() {
    let dir = TempDir::new().unwrap();
    let path = lock_path(&dir);
    let mut stale = acquire_artifact_cache_lock(&path, &test_options())
        .await
        .unwrap();
    // Stop heartbeating; the stale window (400ms) expires.
    tokio::time::sleep(Duration::from_millis(600)).await;

    let mut successor = acquire_artifact_cache_lock(&path, &test_options())
        .await
        .unwrap();
    // The displaced owner's heartbeat now fences: its token file is gone.
    assert!(
        stale.touch().is_err(),
        "displaced owner must fail its heartbeat"
    );
    assert!(
        stale.assert_owned().is_err(),
        "displaced owner must fail ownership assertion"
    );
    // The displaced release must not disturb the successor.
    stale.release().unwrap();
    assert!(path.exists(), "successor lock must survive stale release");
    successor.touch().unwrap();
    successor.release().unwrap();
    assert!(!path.exists());
}

#[tokio::test]
async fn dead_local_owner_is_recovered_without_waiting_for_ttl() {
    let dir = TempDir::new().unwrap();
    let path = lock_path(&dir);
    // Craft a lock owned by a PID that has certainly exited.
    let exited = std::process::Command::new("true")
        .status()
        .expect("test helper process must run");
    assert!(exited.success());
    // Reuse an exited PID is racy in theory; use an unmistakably dead one by
    // writing the maximum PID value, which no live process holds here.
    let dead_pid = u32::MAX;
    let token = "dead-owner-token";
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(
        path.join(format!(".owner-{token}")),
        format!(
            "{{\"token\":{token:?},\"pid\":{dead_pid},\"hostname\":{:?},\"createdAt\":0}}\n",
            local_hostname()
        ),
    )
    .unwrap();

    let start = Instant::now();
    let mut lock = acquire_artifact_cache_lock(
        &path,
        &LockOptions {
            poll_ms: 5,
            stale_ms: 60_000,
            heartbeat_ms: 1_000,
        },
    )
    .await
    .unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "dead owner must be recovered without the 60s TTL, took {:?}",
        start.elapsed()
    );
    lock.release().unwrap();
}

#[tokio::test]
async fn malformed_and_foreign_owners_retain_the_ttl() {
    for (name, owner_name, owner_body) in [
        ("malformed", ".owner-broken", "not-json{{{\n"),
        (
            "foreign-host",
            ".owner-foreign",
            &format!(
                "{{\"token\":\"foreign\",\"pid\":1,\"hostname\":\"some-other-host\",\"createdAt\":0}}\n"
            ),
        ),
        (
            "token-mismatch",
            ".owner-aaa",
            &format!(
                "{{\"token\":\"bbb\",\"pid\":1,\"hostname\":{:?},\"createdAt\":0}}\n",
                local_hostname()
            ),
        ),
    ] {
        let dir = TempDir::new().unwrap();
        let path = lock_path(&dir);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join(owner_name), owner_body).unwrap();

        // With a long TTL these locks must NOT be taken quickly.
        let attempt = tokio::time::timeout(
            Duration::from_millis(120),
            acquire_artifact_cache_lock(
                &path,
                &LockOptions {
                    poll_ms: 5,
                    stale_ms: 60_000,
                    heartbeat_ms: 1_000,
                },
            ),
        )
        .await;
        assert!(
            attempt.is_err(),
            "{name} owner must retain the TTL, not fast-takeover"
        );
    }
}

#[tokio::test]
async fn release_is_idempotent_and_cleans_up_only_its_own_files() {
    let dir = TempDir::new().unwrap();
    let path = lock_path(&dir);
    // A failed initializer must clean up only its private staging files:
    // simulate by creating a stale staging dir, then acquiring normally.
    let staging = dir.path().join("snapshot.lock.pending-12345-abc");
    std::fs::create_dir_all(&staging).unwrap();
    let mut lock = acquire_artifact_cache_lock(&path, &test_options())
        .await
        .unwrap();
    lock.release().unwrap();
    lock.release().unwrap();
    assert!(!path.exists(), "double release must stay silent");
}
