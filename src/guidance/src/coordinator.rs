//! P3 index coordinator: watcher batches become ordered reconcile
//! revisions executed through the coalescing scheduler, with the
//! revision/step proof trail kept on `CheckpointedStepGraph` — reconcile
//! steps register as steps, reconciled epochs become checkpoint markers,
//! the scheduler shell (coalescing + empty short-circuit + retry) stays
//! thin around it. Port of zvec-grep `src/daemon/index-coordinator.ts`.

use crate::change_set::{ChangeSet, ChangeSetOptions, ChangeSetSnapshot};
use crate::scheduler::{
    BoxFuture, JobContext, JobError, JobReason, JobScheduler, JobSnapshot, SubmitJob,
};
use fluent_concurrency::stream::StreamAbort;
use fluent_dag::checkpointed::CheckpointedStepGraph;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Runtime revision authority (the daemon `RootRuntime` surface the
/// coordinator drives: dirty/indexed/reconciled markers).
pub trait RootRuntime: Send + Sync + 'static {
    /// Canonical workspace root.
    fn canonical_root(&self) -> String;
    /// Mark dirty; returns the new target revision.
    fn mark_dirty(&self) -> u64;
    /// Mark a revision indexed.
    fn mark_indexed(&self, revision: u64);
    /// Confirm a full reconciliation at an epoch.
    fn mark_reconciled(&self, revision: u64, epoch: u64);
    /// Demand a full reconciliation.
    fn require_full_reconciliation(&self);
}

/// Proof returned by a reconcile run (ports `IndexReconciliationProof`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciliationProof {
    /// Full reconcile confirmed.
    pub reconciled: bool,
    /// Reconciliation epoch.
    pub reconciliation_epoch: u64,
}

/// Progress report for a running reconcile.
#[derive(Debug, Clone, Default)]
pub struct JobProgress {
    /// Files completed.
    pub done: usize,
    /// Files planned.
    pub total: usize,
    /// Human message.
    pub message: String,
}

/// Progress sink shared with reconcile runs.
pub type ProgressSink = Arc<dyn Fn(JobProgress) + Send + Sync>;

/// Reconcile body: drained snapshot → optional proof.
pub type ReconcileRun = Arc<
    dyn Fn(ChangeSetSnapshot, ProgressSink, StreamAbort) -> BoxFuture<Result<Option<ReconciliationProof>, JobError>>
        + Send
        + Sync,
>;

/// Enqueue reason (watch event vs scheduled reconcile).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueReason {
    /// Watcher event.
    Watch,
    /// Scheduled reconcile probe.
    Reconcile,
}

/// Per-revision step state kept on the `CheckpointedStepGraph`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileStep {
    /// Revision number.
    pub revision: u64,
    /// Lifecycle phase.
    pub phase: StepPhase,
    /// Drained path counts (proof summary, not the full snapshot).
    pub touched: usize,
    /// Drained rescan count.
    pub rescans: usize,
    /// Drained deleted-prefix count.
    pub deleted: usize,
}

/// Revision step lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepPhase {
    /// Reconciling.
    Reconciling,
    /// Indexed (incremental or empty).
    Indexed,
    /// Fully reconciled at an epoch.
    Reconciled {
        /// Confirming epoch.
        epoch: u64,
    },
    /// Terminally failed (stored, never an aborted run).
    Failed {
        /// Failure message.
        message: String,
    },
}

/// Step key for a revision.
fn step_key(revision: u64) -> String {
    format!("rev-{revision}")
}

/// Checkpoint marker for a reconciled epoch.
fn epoch_marker(epoch: u64) -> String {
    format!("epoch-{epoch}")
}

/// Index coordinator: merges drained watcher batches into ordered
/// revisions and runs them through the coalescing scheduler.
pub struct IndexCoordinator {
    runtime: Arc<dyn RootRuntime>,
    scheduler: JobScheduler,
    run: ReconcileRun,
    pending: Arc<std::sync::Mutex<ChangeSet>>,
    pending_root: String,
    target_revision: Arc<AtomicU64>,
    last_indexed: Arc<AtomicU64>,
    steps: Arc<std::sync::Mutex<CheckpointedStepGraph<String, ReconcileStep>>>,
    progress_sink: Arc<std::sync::RwLock<Option<ProgressSink>>>,
}

impl IndexCoordinator {
    /// Create a coordinator over a runtime, scheduler, and reconcile body.
    pub fn new(
        runtime: Arc<dyn RootRuntime>,
        scheduler: JobScheduler,
        run: ReconcileRun,
    ) -> Self {
        let pending_root = runtime.canonical_root();
        let pending = ChangeSet::new(ChangeSetOptions {
            root: Some(pending_root.clone()),
            max_changed_paths: crate::search_constants::CHANGE_SET_PATH_BUDGET,
        });
        Self {
            runtime,
            scheduler,
            run,
            pending: Arc::new(std::sync::Mutex::new(pending)),
            pending_root,
            target_revision: Arc::new(AtomicU64::new(0)),
            last_indexed: Arc::new(AtomicU64::new(0)),
            steps: Arc::new(std::sync::Mutex::new(CheckpointedStepGraph::new())),
            progress_sink: Arc::new(std::sync::RwLock::new(None)),
        }
    }

    /// Observe reconcile progress (P5 status surface consumes this).
    pub fn on_progress(&self, sink: ProgressSink) {
        *self.progress_sink.write().expect("progress lock") = Some(sink);
    }

    /// Current target revision (newest dirty mark).
    #[must_use]
    pub fn target_revision(&self) -> u64 {
        self.target_revision.load(Ordering::SeqCst)
    }

    /// Newest indexed revision.
    #[must_use]
    pub fn indexed_revision(&self) -> u64 {
        self.last_indexed.load(Ordering::SeqCst)
    }

    /// Step state for a revision, if registered.
    #[must_use]
    pub fn revision_step(&self, revision: u64) -> Option<ReconcileStep> {
        common_core::sync::lock(&self.steps)
            .status(&step_key(revision))
            .cloned()
    }

    /// Enqueue a drained watcher batch (ports `IndexCoordinator.enqueue`).
    pub fn enqueue(&self, changes: &ChangeSetSnapshot, reason: EnqueueReason) -> JobSnapshot {
        if changes.force_full_reconcile {
            self.runtime.require_full_reconciliation();
        }
        {
            let mut pending = common_core::sync::lock(&self.pending);
            pending.merge(changes);
        }
        let revision = self.runtime.mark_dirty();
        self.target_revision.store(revision, Ordering::SeqCst);
        self.register_step(revision);
        let job_state: Arc<std::sync::Mutex<Option<(ChangeSetSnapshot, u64)>>> =
            Arc::new(std::sync::Mutex::new(None));
        let runtime = Arc::clone(&self.runtime);
        let pending = Arc::clone(&self.pending);
        let pending_root = self.pending_root.clone();
        let target = Arc::clone(&self.target_revision);
        let last_indexed = Arc::clone(&self.last_indexed);
        let steps = Arc::clone(&self.steps);
        let run = Arc::clone(&self.run);
        let progress_sink = Arc::clone(&self.progress_sink);
        let root = self.runtime.canonical_root();
        let submitted = self.scheduler.submit(SubmitJob {
            root,
            reason: match reason {
                EnqueueReason::Watch => JobReason::Watch,
                EnqueueReason::Reconcile => JobReason::Reconcile,
            },
            followup_if_running: true,
            run: Arc::new(move |context: JobContext| {
                let job_state = Arc::clone(&job_state);
                let runtime = Arc::clone(&runtime);
                let pending = Arc::clone(&pending);
                let pending_root = pending_root.clone();
                let target = Arc::clone(&target);
                let last_indexed = Arc::clone(&last_indexed);
                let steps = Arc::clone(&steps);
                let run = Arc::clone(&run);
                let progress_sink = Arc::clone(&progress_sink);
                Box::pin(async move {
                    // Capture once: retries reuse the same snapshot.
                    let (snapshot, job_revision) = {
                        let mut guard = common_core::sync::lock(&job_state);
                        if let Some(captured) = guard.clone() {
                            captured
                        } else {
                            let mut pending = common_core::sync::lock(&pending);
                            let snapshot = pending.snapshot();
                            let job_revision = target.load(Ordering::SeqCst);
                            *pending = ChangeSet::new(ChangeSetOptions {
                                root: Some(pending_root.clone()),
                                max_changed_paths:
                                    crate::search_constants::CHANGE_SET_PATH_BUDGET,
                            });
                            *guard = Some((snapshot.clone(), job_revision));
                            (snapshot, job_revision)
                        }
                    };
                    // Empty short-circuit: nothing to do, still indexed.
                    if !snapshot.force_full_reconcile
                        && snapshot.touched_files.is_empty()
                        && snapshot.rescan_directories.is_empty()
                        && snapshot.deleted_prefixes.is_empty()
                    {
                        runtime.mark_indexed(job_revision);
                        last_indexed.store(job_revision, Ordering::SeqCst);
                        complete_step(&steps, job_revision, StepPhase::Indexed);
                        return Ok(());
                    }
                    let report: ProgressSink = progress_sink
                        .read()
                        .expect("progress lock")
                        .clone()
                        .unwrap_or_else(|| Arc::new(|_| {}));
                    match run(snapshot.clone(), report, context.abort).await {
                        Ok(proof) => {
                            if snapshot.force_full_reconcile
                                && proof.as_ref().is_some_and(|proof| proof.reconciled)
                            {
                                let epoch = proof.map_or(0, |proof| proof.reconciliation_epoch);
                                runtime.mark_reconciled(job_revision, epoch);
                                last_indexed.store(job_revision, Ordering::SeqCst);
                                reconcile_step(&steps, job_revision, epoch);
                            } else {
                                runtime.mark_indexed(job_revision);
                                last_indexed.store(job_revision, Ordering::SeqCst);
                                complete_step(&steps, job_revision, StepPhase::Indexed);
                            }
                            Ok(())
                        }
                        Err(error) => {
                            complete_step(
                                &steps,
                                job_revision,
                                StepPhase::Failed { message: error.message.clone() },
                            );
                            Err(error)
                        }
                    }
                }) as BoxFuture<Result<(), JobError>>
            }),
        });
        submitted.job
    }

    /// Shut down the scheduler; incomplete steps read back as aborted.
    pub fn close(&self) {
        self.scheduler.close();
    }

    fn register_step(&self, revision: u64) {
        let key = step_key(revision);
        let previous = if revision > 1 {
            vec![step_key(revision - 1)]
        } else {
            Vec::new()
        };
        let mut steps = common_core::sync::lock(&self.steps);
        // Registration is proof bookkeeping: a duplicate (same revision
        // enqueued twice cannot happen — revisions are monotonic) warns
        // instead of failing the enqueue.
        if steps
            .add_step(
                key,
                &previous,
                ReconcileStep {
                    revision,
                    phase: StepPhase::Reconciling,
                    touched: 0,
                    rescans: 0,
                    deleted: 0,
                },
            )
            .is_err()
        {
            tracing::warn!(revision, "duplicate reconcile step registration");
        }
    }
}

fn complete_step(
    steps: &Arc<std::sync::Mutex<CheckpointedStepGraph<String, ReconcileStep>>>,
    revision: u64,
    phase: StepPhase,
) {
    let mut guard = common_core::sync::lock(steps);
    if let Some(state) = guard.state_mut(&step_key(revision)) {
        state.phase = phase;
    }
    guard.complete(&step_key(revision));
}

fn reconcile_step(
    steps: &Arc<std::sync::Mutex<CheckpointedStepGraph<String, ReconcileStep>>>,
    revision: u64,
    epoch: u64,
) {
    let mut guard = common_core::sync::lock(steps);
    if let Some(state) = guard.state_mut(&step_key(revision)) {
        state.phase = StepPhase::Reconciled { epoch };
    }
    guard.complete(&step_key(revision));
    if guard.checkpoint(epoch_marker(epoch)).is_err() {
        tracing::warn!(revision, epoch, "duplicate reconcile epoch checkpoint");
    }
}

#[cfg(test)]
#[path = "../tests/coordinator.rs"]
mod tests;
