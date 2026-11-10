//! Writer and gate tests: credit-gated order, backup gate, instance lock.

use std::sync::Arc;

use crate::store::events::EventLog;
use crate::store::{open_store, writer_feed_config, InstanceLock, WriteReq};

#[test]
fn writer_config_stays_small_and_serial() {
    let config = writer_feed_config();
    assert_eq!(config.max_concurrent, 1, "single writer, never parallel");
    assert!(config.queue_capacity <= 512);
    assert!(config.credit_limit <= 128);
}

#[test]
fn credit_gated_writes_land_in_order() {
    common_core::runtime::block_on(async {
        let log = EventLog::in_memory().expect("in-memory log");
        let worker = Arc::new(fluent_concurrency::feed_worker::CreditedFeedWorker::new(
            fluent_concurrency::tokio_runtime(),
            writer_feed_config(),
            move |req: WriteReq| {
                let log = Arc::clone(&log);
                async move {
                    let result = log
                        .append(
                            &req.session,
                            req.epoch,
                            &req.kind,
                            &req.payload,
                            req.wall_ms,
                        )
                        .map(|event| event.seq)
                        .map_err(|error| error.to_string());
                    let _ = req.ack.send(result);
                }
            },
        ));
        let handle = worker.start();
        let mut seqs = Vec::new();
        for index in 0..20 {
            let (send, recv) = tokio::sync::oneshot::channel();
            worker
                .enqueue_with_credit(WriteReq {
                    session: "s1".to_string(),
                    epoch: 1,
                    kind: "ReplyFinal".to_string(),
                    payload: format!("{{\"n\":{index}}}"),
                    wall_ms: index,
                    ack: send,
                })
                .await
                .expect("enqueue");
            seqs.push(recv.await.expect("ack").expect("insert"));
        }
        let mut sorted = seqs.clone();
        sorted.sort_unstable();
        assert_eq!(seqs, sorted, "credit-gated writes stay ordered");
        sorted.dedup();
        assert_eq!(sorted.len(), 20);
        worker.drain();
        drop(handle);
    });
}

#[test]
fn backup_gate_copies_before_migrating() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("orator.db");
    // Seed a version-0 database (no migrations yet).
    {
        let conn = rusqlite::Connection::open(&path).expect("create");
        conn.execute_batch("CREATE TABLE legacy (id INTEGER PRIMARY KEY);")
            .expect("seed");
    }
    let store = open_store(&path).expect("open migrates");
    let version = store
        .with_conn(fluent_db::migrate::schema_version)
        .expect("version");
    assert_eq!(version, crate::store::schema::CURRENT_VERSION);
    let backup = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| {
            path.extension()
                .is_some_and(|ext| ext.to_string_lossy().starts_with("bak-"))
        });
    assert!(backup.is_some(), "migration must leave a backup sibling");
}

#[test]
fn instance_lock_refuses_a_live_holder_and_heals_stale_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let first = InstanceLock::acquire(dir.path(), "orator.sock").expect("first acquires");
    let second = InstanceLock::acquire(dir.path(), "orator.sock");
    assert!(
        matches!(second, Err(crate::store::OpenError::AlreadyRunning(_))),
        "live holder must refuse, got {second:?}"
    );
    drop(first);
    // Drop closes the listener but leaves the socket file: the next
    // acquire must treat it as stale, unlink, and succeed.
    let third = InstanceLock::acquire(dir.path(), "orator.sock").expect("stale heals");
    assert!(third.path.ends_with("orator.sock"));
}

#[test]
fn same_second_backups_never_share_a_name() {
    use crate::store::backup_db;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("orator.db");
    std::fs::write(&path, b"data").expect("seed");
    let first = backup_db(&path).expect("first backup");
    let second = backup_db(&path).expect("second backup");
    assert_ne!(first, second, "same-second migrates must not overwrite");
    assert!(first.exists() && second.exists(), "both backups survive");
}

#[test]
fn symlink_parents_refuse_without_binding() {
    use crate::store::InstanceLock;
    let outside = tempfile::tempdir().expect("outside");
    let dir = tempfile::tempdir().expect("tempdir");
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).expect("link");
    // A runtime dir under a symlink parent refuses before binding.
    let refused = InstanceLock::acquire(&dir.path().join("link"), "orator.sock");
    assert!(refused.is_err(), "symlink parent must refuse: {refused:?}");
}

/// M14-S1: the live holder maps to `AlreadyRunning` carrying the exact
/// socket path — the daemon keeps the raw `AddrInUse`, the store names
/// the path.
#[test]
fn instance_lock_live_holder_names_the_path() {
    use crate::store::InstanceLock;
    let dir = tempfile::tempdir().expect("tempdir");
    let first = InstanceLock::acquire(dir.path(), "orator.sock").expect("first acquires");
    let error = InstanceLock::acquire(dir.path(), "orator.sock").expect_err("live refuses");
    assert_eq!(
        error,
        crate::store::OpenError::AlreadyRunning(
            dir.path().join("orator.sock").display().to_string()
        ),
        "live holder names the exact path: {error}"
    );
    drop(first);
}

/// M14-S1: a directory at the socket path binds `AddrInUse`, proves
/// stale, then fails to unlink — the store keeps its `Store` wording
/// with the path and the remove error.
#[test]
fn instance_lock_directory_at_socket_path_reports_remove_failure() {
    use crate::store::InstanceLock;
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("orator.sock")).expect("directory at socket path");
    let error =
        InstanceLock::acquire(dir.path(), "orator.sock").expect_err("directory never binds");
    let text = error.to_string();
    assert!(
        text.contains("stale socket"),
        "remove failure names the stale file: {text}"
    );
    assert!(
        text.contains("remove failed"),
        "remove failure names the cause: {text}"
    );
    assert!(
        text.contains("orator.sock"),
        "remove failure names the socket: {text}"
    );
}

/// M14-S1: a non-`AddrInUse` failure keeps the store's `mkdir` wording —
/// a file as the runtime dir fails directory creation, never entering
/// the reclaim loop.
#[test]
fn instance_lock_non_addrinuse_failure_keeps_mkdir_wording() {
    use crate::store::InstanceLock;
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("runtime");
    std::fs::write(&file, b"not a dir").expect("runtime file writes");
    let error = InstanceLock::acquire(&file, "orator.sock").expect_err("file never binds");
    let text = error.to_string();
    assert!(
        text.contains("mkdir"),
        "directory-creation failure keeps mkdir wording: {text}"
    );
    assert!(
        !text.contains("stale socket"),
        "no reclaim wording on unrelated failures: {text}"
    );
}
