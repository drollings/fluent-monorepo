//! P3 coordinator tests (port of zvec-grep
//! `test/index-coordinator.test.mjs`): follow-up revisions, same-snapshot
//! retry, unconfirmed full reconciliation, incremental high-ratio batches,
//! exact-batch compaction.

use crate::change_set::ChangeSetSnapshot;
use crate::coordinator::{EnqueueReason, IndexCoordinator, ReconciliationProof, RootRuntime};
use crate::scheduler::{BoxFuture, JobError, JobScheduler, JobState};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

struct StubRuntime {
    canonical_root: String,
    dirty_revision: AtomicU64,
    indexed_revision: AtomicU64,
    reconciled_revision: AtomicU64,
    full_reconciliations: AtomicU64,
}

impl StubRuntime {
    fn stub() -> Arc<Self> {
        Arc::new(Self {
            canonical_root: "/repo".to_string(),
            dirty_revision: AtomicU64::new(0),
            indexed_revision: AtomicU64::new(0),
            reconciled_revision: AtomicU64::new(0),
            full_reconciliations: AtomicU64::new(0),
        })
    }
}

impl RootRuntime for StubRuntime {
    fn canonical_root(&self) -> String {
        self.canonical_root.clone()
    }
    fn mark_dirty(&self) -> u64 {
        self.dirty_revision.fetch_add(1, Ordering::SeqCst) + 1
    }
    fn mark_indexed(&self, revision: u64) {
        self.indexed_revision.store(revision, Ordering::SeqCst);
    }
    fn mark_reconciled(&self, revision: u64, _epoch: u64) {
        self.reconciled_revision.store(revision, Ordering::SeqCst);
    }
    fn require_full_reconciliation(&self) {
        self.full_reconciliations.fetch_add(1, Ordering::SeqCst);
    }
}

fn change(path: &str) -> ChangeSetSnapshot {
    ChangeSetSnapshot {
        touched_files: vec![path.to_string()],
        rescan_directories: Vec::new(),
        deleted_prefixes: Vec::new(),
        force_full_reconcile: false,
    }
}

fn exact_batch(directory: &str, count: usize) -> ChangeSetSnapshot {
    ChangeSetSnapshot {
        touched_files: (0..count).map(|index| format!("{directory}/{index}.ts")).collect(),
        rescan_directories: Vec::new(),
        deleted_prefixes: Vec::new(),
        force_full_reconcile: false,
    }
}

async fn wait_for<F>(mut predicate: F)
where
    F: FnMut() -> bool,
{
    for _ in 0..500 {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("condition was not reached");
}

#[tokio::test]
async fn changes_during_a_write_index_in_one_followup_revision() {
    let scheduler = JobScheduler::new(1, 1, 1);
    let runtime = StubRuntime::stub();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let snapshots = Arc::new(std::sync::Mutex::new(Vec::new()));
    let coordinator = Arc::new(IndexCoordinator::new(
        runtime.clone(),
        scheduler.clone(),
        {
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            let snapshots = Arc::clone(&snapshots);
            Arc::new(move |changes: ChangeSetSnapshot, _, _| {
                let entered = Arc::clone(&entered);
                let release = Arc::clone(&release);
                let snapshots = Arc::clone(&snapshots);
                Box::pin(async move {
                    snapshots.lock().unwrap().push(changes);
                    if snapshots.lock().unwrap().len() == 1 {
                        entered.notify_one();
                        release.notified().await;
                    }
                    Ok(None)
                }) as BoxFuture<Result<Option<ReconciliationProof>, JobError>>
            })
        },
    ));
    coordinator.enqueue(&change("/repo/a.ts"), EnqueueReason::Watch);
    wait_for(|| snapshots.lock().unwrap().len() == 1).await;
    coordinator.enqueue(&change("/repo/b.ts"), EnqueueReason::Watch);
    coordinator.enqueue(&change("/repo/c.ts"), EnqueueReason::Watch);
    release.notify_one();
    scheduler.wait_for_root_idle("/repo").await;
    let snapshots = snapshots.lock().unwrap().clone();
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0].touched_files, vec!["/repo/a.ts".to_string()]);
    assert_eq!(
        snapshots[1].touched_files,
        vec!["/repo/b.ts".to_string(), "/repo/c.ts".to_string()]
    );
    assert_eq!(runtime.dirty_revision.load(Ordering::SeqCst), 3);
    assert_eq!(runtime.indexed_revision.load(Ordering::SeqCst), 3);
    scheduler.close();
}

#[tokio::test]
async fn retry_reuses_the_same_change_snapshot() {
    let scheduler = JobScheduler::new(1, 2, 1);
    let runtime = StubRuntime::stub();
    let snapshots = Arc::new(std::sync::Mutex::new(Vec::new()));
    let coordinator = Arc::new(IndexCoordinator::new(
        runtime.clone(),
        scheduler.clone(),
        {
            let snapshots = Arc::clone(&snapshots);
            Arc::new(move |changes: ChangeSetSnapshot, _, _| {
                let snapshots = Arc::clone(&snapshots);
                Box::pin(async move {
                    snapshots.lock().unwrap().push(changes);
                    if snapshots.lock().unwrap().len() == 1 {
                        return Err(JobError::retryable("busy"));
                    }
                    Ok(None)
                }) as BoxFuture<Result<Option<ReconciliationProof>, JobError>>
            })
        },
    ));
    let job = coordinator.enqueue(&change("/repo/a.ts"), EnqueueReason::Watch);
    let snapshot = scheduler.wait(job.id).await.expect("wait");
    assert_eq!(snapshot.state, JobState::Succeeded);
    let snapshots = snapshots.lock().unwrap().clone();
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0].touched_files, vec!["/repo/a.ts".to_string()]);
    assert_eq!(snapshots[1].touched_files, vec!["/repo/a.ts".to_string()]);
    assert_eq!(runtime.indexed_revision.load(Ordering::SeqCst), 1);
    scheduler.close();
}

#[tokio::test]
async fn full_reconciliation_without_proof_remains_unconfirmed() {
    let scheduler = JobScheduler::new(1, 1, 1);
    let runtime = StubRuntime::stub();
    let coordinator = Arc::new(IndexCoordinator::new(
        runtime.clone(),
        scheduler.clone(),
        Arc::new(|_, _, _| {
            Box::pin(async { Ok(None) })
                as BoxFuture<Result<Option<ReconciliationProof>, JobError>>
        }),
    ));
    let job = coordinator.enqueue(&ChangeSetSnapshot {
            touched_files: Vec::new(),
            rescan_directories: Vec::new(),
            deleted_prefixes: Vec::new(),
            force_full_reconcile: true,
        },
        EnqueueReason::Watch,
    );
    let snapshot = scheduler.wait(job.id).await.expect("wait");
    assert_eq!(snapshot.state, JobState::Succeeded);
    assert_eq!(runtime.indexed_revision.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.reconciled_revision.load(Ordering::SeqCst), 0);
    scheduler.close();
}

#[tokio::test]
async fn high_ratio_exact_batch_remains_incremental() {
    let scheduler = JobScheduler::new(1, 1, 1);
    let runtime = StubRuntime::stub();
    let snapshots = Arc::new(std::sync::Mutex::new(Vec::new()));
    let coordinator = Arc::new(IndexCoordinator::new(
        runtime.clone(),
        scheduler.clone(),
        {
            let snapshots = Arc::clone(&snapshots);
            Arc::new(move |changes: ChangeSetSnapshot, _, _| {
                let snapshots = Arc::clone(&snapshots);
                Box::pin(async move {
                    snapshots.lock().unwrap().push(changes);
                    Ok(None)
                }) as BoxFuture<Result<Option<ReconciliationProof>, JobError>>
            })
        },
    ));
    let job = coordinator.enqueue(&ChangeSetSnapshot {
            touched_files: vec![
                "/repo/a.ts".to_string(),
                "/repo/b.ts".to_string(),
                "/repo/c.ts".to_string(),
            ],
            rescan_directories: Vec::new(),
            deleted_prefixes: Vec::new(),
            force_full_reconcile: false,
        },
        EnqueueReason::Watch,
    );
    let snapshot = scheduler.wait(job.id).await.expect("wait");
    assert_eq!(snapshot.state, JobState::Succeeded);
    let snapshots = snapshots.lock().unwrap().clone();
    assert!(!snapshots[0].force_full_reconcile);
    assert_eq!(runtime.full_reconciliations.load(Ordering::SeqCst), 0);
    scheduler.close();
}

#[tokio::test]
async fn queued_exact_batches_compact_without_full_reconciliation() {
    let scheduler = JobScheduler::new(1, 1, 1);
    let runtime = StubRuntime::stub();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let snapshots = Arc::new(std::sync::Mutex::new(Vec::new()));
    let coordinator = Arc::new(IndexCoordinator::new(
        runtime.clone(),
        scheduler.clone(),
        {
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            let snapshots = Arc::clone(&snapshots);
            Arc::new(move |changes: ChangeSetSnapshot, _, _| {
                let entered = Arc::clone(&entered);
                let release = Arc::clone(&release);
                let snapshots = Arc::clone(&snapshots);
                Box::pin(async move {
                    snapshots.lock().unwrap().push(changes);
                    if snapshots.lock().unwrap().len() == 1 {
                        entered.notify_one();
                        release.notified().await;
                    }
                    Ok(None)
                }) as BoxFuture<Result<Option<ReconciliationProof>, JobError>>
            })
        },
    ));
    coordinator.enqueue(&change("/repo/initial.ts"), EnqueueReason::Watch);
    wait_for(|| snapshots.lock().unwrap().len() == 1).await;
    coordinator.enqueue(&exact_batch("/repo/package-a", 600), EnqueueReason::Watch);
    coordinator.enqueue(&exact_batch("/repo/package-b", 600), EnqueueReason::Watch);
    release.notify_one();
    scheduler.wait_for_root_idle("/repo").await;
    let snapshots = snapshots.lock().unwrap().clone();
    assert_eq!(snapshots.len(), 2);
    assert!(snapshots[1].touched_files.is_empty(), "{:?}", snapshots[1]);
    assert_eq!(
        snapshots[1].rescan_directories,
        vec!["/repo/package-a".to_string(), "/repo/package-b".to_string()]
    );
    assert!(snapshots[1].deleted_prefixes.is_empty(), "{:?}", snapshots[1]);
    assert!(!snapshots[1].force_full_reconcile);
    scheduler.close();
}
