use crate::outbox;
use crate::store::SqliteStore;
use crate::tests::common::{db_caps, in_memory_pool};
use fluent_wvr::capability::CURRENT_CAPS;

fn mem_store() -> SqliteStore {
    let store = SqliteStore::open_in_memory().unwrap();
    store.with_conn(outbox::init_schema).unwrap();
    store
}

#[test]
fn outbox_placeholder() {
    // Scaffolding placeholder: replaced by the real outbox suites below.
    let _ = outbox::OUTBOX_DDL.len();
}

#[test]
fn fifo_order() {
    let store = mem_store();
    let a = store
        .with_conn(|conn| outbox::append(conn, "k1", b"one"))
        .unwrap();
    let b = store
        .with_conn(|conn| outbox::append(conn, "k2", b"two"))
        .unwrap();
    let c = store
        .with_conn(|conn| outbox::append(conn, "k3", b"three"))
        .unwrap();
    assert!(a < b && b < c);
    let batch = store.outbox_claim(10, 60_000, 1_000).unwrap();
    let keys: Vec<&str> = batch.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(keys, vec!["k1", "k2", "k3"]);
    assert_eq!(batch[0].payload, b"one");
}

#[test]
fn duplicate_append_is_idempotent() {
    let store = mem_store();
    let first = store
        .with_conn(|conn| outbox::append(conn, "dup", b"v1"))
        .unwrap();
    let second = store
        .with_conn(|conn| outbox::append(conn, "dup", b"v1"))
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(store.outbox_pending().unwrap(), 1);
}

#[test]
fn claimed_rows_are_leased_not_visible() {
    let store = mem_store();
    store
        .with_conn(|conn| outbox::append(conn, "a", b"1"))
        .unwrap();
    let first = store.outbox_claim(10, 60_000, 1_000).unwrap();
    assert_eq!(first.len(), 1);
    // Still within the lease: a second claimant gets nothing.
    let second = store.outbox_claim(10, 60_000, 2_000).unwrap();
    assert!(second.is_empty());
}

#[test]
fn crash_between_claim_and_ack_redelivers() {
    let store = mem_store();
    store
        .with_conn(|conn| outbox::append(conn, "a", b"1"))
        .unwrap();
    let first = store.outbox_claim(10, 1_000, 5_000).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].attempts, 0);
    // Simulate a crash: never ack, let the lease expire.
    let redelivered = store.outbox_claim(10, 1_000, 9_000).unwrap();
    assert_eq!(redelivered.len(), 1);
    assert_eq!(redelivered[0].id, first[0].id);
    assert_eq!(redelivered[0].attempts, 1);
}

#[test]
fn ack_removes_rows() {
    let store = mem_store();
    store
        .with_conn(|conn| outbox::append(conn, "a", b"1"))
        .unwrap();
    store
        .with_conn(|conn| outbox::append(conn, "b", b"2"))
        .unwrap();
    let batch = store.outbox_claim(10, 60_000, 1_000).unwrap();
    let ids: Vec<i64> = batch.iter().map(|e| e.id).collect();
    assert_eq!(store.outbox_ack(&ids[..1]).unwrap(), 1);
    assert_eq!(store.outbox_pending().unwrap(), 1);
    // The acked row never redelivers, even past the lease.
    let batch = store.outbox_claim(10, 60_000, 500_000).unwrap();
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].key, "b");
    assert_eq!(store.outbox_ack(&[]).unwrap(), 0);
}

#[test]
fn concurrent_claimants_never_share_a_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("outbox.db");
    let seed = SqliteStore::open(&path).unwrap();
    seed.with_conn(outbox::init_schema).unwrap();
    for i in 0..20 {
        seed.with_conn(|conn| outbox::append(conn, &format!("k{i:02}"), b"x"))
            .unwrap();
    }
    drop(seed);
    let run_claim = |path: std::path::PathBuf| {
        let store = SqliteStore::open(&path).unwrap();
        store
            .with_conn(|conn| {
                conn.busy_timeout(std::time::Duration::from_millis(5_000))
                    .map_err(crate::error::DbError::from)
            })
            .unwrap();
        store
            .outbox_claim(100, 60_000, 1_000)
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect::<Vec<_>>()
    };
    let p1 = path.clone();
    let p2 = path.clone();
    let h1 = std::thread::spawn(move || run_claim(p1));
    let h2 = std::thread::spawn(move || run_claim(p2));
    let mut a = h1.join().unwrap();
    let mut b = h2.join().unwrap();
    a.sort_unstable();
    b.sort_unstable();
    let mut both = a.clone();
    both.extend(b.iter().copied());
    both.sort_unstable();
    both.dedup();
    assert_eq!(
        both.len(),
        20,
        "every row claimed exactly once across both claimants"
    );
    assert!(a.iter().all(|id| !b.contains(id)), "no row claimed by both");
}

#[tokio::test]
async fn pool_async_wrappers_round_trip() {
    let pool = in_memory_pool();
    CURRENT_CAPS
        .scope(db_caps(), async {
            pool.outbox_init().await.unwrap();
            let id = pool.outbox_append("pk", b"payload").await.unwrap();
            assert!(id > 0);
            // Idempotent over the pool path too.
            let again = pool.outbox_append("pk", b"payload").await.unwrap();
            assert_eq!(id, again);
            assert_eq!(pool.outbox_pending().await.unwrap(), 1);
            let batch = pool.outbox_claim(10, 60_000, 1_000).await.unwrap();
            assert_eq!(batch.len(), 1);
            assert_eq!(batch[0].payload, b"payload");
            assert_eq!(pool.outbox_ack(&[batch[0].id]).await.unwrap(), 1);
            assert_eq!(pool.outbox_pending().await.unwrap(), 0);
        })
        .await;
}

/// Calibration gate for the outbox claim heuristic (lease + batch limit).
///
/// Detector framing: "claim_batch returns the row" fires (score 1.0) or not
/// (score 0.0); the label is whether the row is truly due (unacked and never
/// claimed, or lease-expired). The control group — leased-but-unexpired rows
/// and already-acked rows — must NOT fire. Gate: zero FP, zero FN.
#[test]
fn outbox_claim_calibration() {
    use common_core::calibration::{calibrate_threshold, emit_markdown_artifact, sweep_thresholds};
    struct Case {
        fired: bool,
        due: bool,
    }
    let mut cases: Vec<Case> = Vec::new();
    let store = mem_store();
    for i in 0..5 {
        store
            .with_conn(|conn| outbox::append(conn, &format!("due{i}"), b"x"))
            .unwrap();
    }
    // 5 due rows fire on first claim.
    let claimed = store.outbox_claim(100, 60_000, 1_000).unwrap();
    assert_eq!(claimed.len(), 5);
    for _ in &claimed {
        cases.push(Case {
            fired: true,
            due: true,
        });
    }
    // Control: still leased, so an immediate re-claim fires nothing.
    let leased = store.outbox_claim(100, 60_000, 2_000).unwrap();
    assert!(leased.is_empty());
    for _ in 0..5 {
        cases.push(Case {
            fired: false,
            due: false,
        });
    }
    // Ack 2 rows; expire the other 3 leases. Only the 3 must redeliver.
    let acked: Vec<i64> = claimed.iter().take(2).map(|e| e.id).collect();
    assert_eq!(store.outbox_ack(&acked).unwrap(), 2);
    let redelivered = store.outbox_claim(100, 1_000, 500_000).unwrap();
    assert_eq!(redelivered.len(), 3);
    for _ in &redelivered {
        cases.push(Case {
            fired: true,
            due: true,
        });
    }
    // Control: the 2 acked rows stay silent past any lease.
    for _ in 0..2 {
        cases.push(Case {
            fired: false,
            due: false,
        });
    }
    let report = calibrate_threshold(&cases, |c| if c.fired { 1.0 } else { 0.0 }, |c| c.due, 0.5);
    assert_eq!(
        report.fp, 0,
        "leased/acked control rows must never fire: {report:?}"
    );
    assert_eq!(report.r#fn, 0, "due rows must always fire: {report:?}");
    assert!(
        report.passes_gate(),
        "outbox claim calibration must pass: {report:?}"
    );
    emit_markdown_artifact(
        "outbox-claim",
        &sweep_thresholds(
            &cases,
            |c| if c.fired { 1.0 } else { 0.0 },
            |c| c.due,
            &[0.5],
        ),
    );
}
