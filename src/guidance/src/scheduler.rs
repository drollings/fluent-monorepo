//! P3 reconcile job queue: the coalescing core the `IndexCoordinator`
//! composes (per-root active + followup, retryable retry, wait, idle,
//! close). The full daemon `job-scheduler` (priority, progress fan-out,
//! logger) ports in P5; this core carries the coordinator's tested
//! semantics: `followupIfRunning` coalescing + same-snapshot retry.

use fluent_concurrency::stream::StreamAbort;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{mpsc, oneshot};

/// Boxed sendable future (local alias; no new future runtime).
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Shared run-closure handle.
pub type JobRun = Arc<dyn Fn(JobContext) -> BoxFuture<Result<(), JobError>> + Send + Sync>;

/// Job failure (ports `DaemonError`'s retryable flag).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobError {
    /// Human message.
    pub message: String,
    /// Retryable (same run retried) vs terminal (job fails).
    pub retryable: bool,
}

impl JobError {
    /// Retryable failure (busy, transient).
    pub fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }

    /// Terminal failure (no retry).
    pub fn terminal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }
}

/// Per-run control handed to the job body.
#[derive(Clone)]
pub struct JobContext {
    /// Cancellation signal (close / abort).
    pub abort: StreamAbort,
    /// Attempt number (1-based).
    pub attempt: u32,
}

/// Job lifecycle state (ports `JobState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// Waiting for a worker slot.
    Queued,
    /// Executing.
    Running,
    /// Completed.
    Succeeded,
    /// Terminally failed.
    Failed,
    /// Cancelled (close).
    Cancelled,
}

/// Submission reason (ports `JobReason`; priority ordering Watch <
/// Reconcile < Manual mirrors the daemon's `priority()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JobReason {
    /// Watcher event.
    Watch,
    /// Scheduled reconcile.
    Reconcile,
    /// Operator request.
    Manual,
}

/// Observable job snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSnapshot {
    /// Job id.
    pub id: u64,
    /// Canonical root.
    pub root: String,
    /// Reason.
    pub reason: JobReason,
    /// State.
    pub state: JobState,
    /// Attempts used.
    pub attempt: u32,
}

/// Submission (ports `SubmitIndexJob`).
pub struct SubmitJob {
    /// Canonical root.
    pub root: String,
    /// Reason.
    pub reason: JobReason,
    /// Coalesce into the active/followup job when one is running.
    pub followup_if_running: bool,
    /// Job body.
    pub run: JobRun,
}

/// Submission result (ports `SubmitIndexJobResult`).
#[derive(Debug, Clone)]
pub struct SubmitResult {
    /// Snapshot at submit time.
    pub job: JobSnapshot,
    /// True when coalesced into an existing job.
    pub reused: bool,
}

struct JobRecord {
    id: u64,
    root: String,
    reason: JobReason,
    state: JobState,
    attempt: u32,
    run: JobRun,
    abort: StreamAbort,
    waiters: Vec<oneshot::Sender<JobSnapshot>>,
    error: Option<String>,
}

impl JobRecord {
    fn snapshot(&self) -> JobSnapshot {
        JobSnapshot {
            id: self.id,
            root: self.root.clone(),
            reason: self.reason,
            state: self.state,
            attempt: self.attempt,
        }
    }
}

struct SchedulerInner {
    next_id: AtomicU64,
    concurrency: usize,
    max_attempts: u32,
    retry_base_delay_ms: u64,
    running: usize,
    active_by_root: HashMap<String, u64>,
    followup_by_root: HashMap<String, u64>,
    queue: VecDeque<u64>,
    jobs: HashMap<u64, JobRecord>,
    idle_waiters: HashMap<String, Vec<oneshot::Sender<()>>>,
    closed: bool,
}

enum DriverMsg {
    Pump,
    JobDone { id: u64, result: Result<(), JobError> },
    Closed,
}

/// Reconcile job queue with per-root `followupIfRunning` coalescing.
#[derive(Clone)]
pub struct JobScheduler {
    inner: Arc<std::sync::Mutex<SchedulerInner>>,
    tx: mpsc::UnboundedSender<DriverMsg>,
}

impl JobScheduler {
    /// Create a scheduler (`concurrency` worker slots, `max_attempts`
    /// per job, `retry_base_delay_ms` backoff base).
    #[must_use]
    pub fn new(concurrency: usize, max_attempts: u32, retry_base_delay_ms: u64) -> Self {
        let inner = Arc::new(std::sync::Mutex::new(SchedulerInner {
            next_id: AtomicU64::new(1),
            concurrency: concurrency.max(1),
            max_attempts: max_attempts.max(1),
            retry_base_delay_ms,
            running: 0,
            active_by_root: HashMap::new(),
            followup_by_root: HashMap::new(),
            queue: VecDeque::new(),
            jobs: HashMap::new(),
            idle_waiters: HashMap::new(),
            closed: false,
        }));
        let (tx, rx) = mpsc::unbounded_channel();
        let driver = Self {
            inner: Arc::clone(&inner),
            tx: tx.clone(),
        };
        tokio::spawn(async move { driver.drive(rx).await });
        Self { inner, tx }
    }

    /// Submit a job (ports `JobScheduler.submit` coalescing).
    pub fn submit(&self, input: SubmitJob) -> SubmitResult {
        let mut guard = common_core::sync::lock(&self.inner);
        if guard.closed {
            let id = guard.next_id.fetch_add(1, Ordering::SeqCst);
            let snapshot = JobSnapshot {
                id,
                root: input.root.clone(),
                reason: input.reason,
                state: JobState::Cancelled,
                attempt: 0,
            };
            return SubmitResult { job: snapshot, reused: false };
        }
        // Active job for this root: coalesce watch/followup submissions.
        // The run closure moves exactly once (take from the holder).
        let mut run_holder = Some(input.run);
        let active_id = guard.active_by_root.get(&input.root).copied();
        if let Some(active_id) = active_id {
            let coalesce = input.reason == JobReason::Watch || input.followup_if_running;
            if coalesce {
                if let Some(followup_id) = guard.followup_by_root.get(&input.root).copied() {
                    // Merge into the queued followup (latest run wins; the
                    // coordinator snapshots pending at run start, so merged
                    // changes ride the same followup revision).
                    let snapshot = {
                        let followup = guard.jobs.get_mut(&followup_id);
                        followup.map(|followup| {
                            followup.run = run_holder.take().expect("run held");
                            if input.reason > followup.reason {
                                followup.reason = input.reason;
                            }
                            followup.snapshot()
                        })
                    };
                    if let Some(job) = snapshot {
                        return SubmitResult { job, reused: true };
                    }
                } else {
                    let running = guard
                        .jobs
                        .get(&active_id)
                        .is_some_and(|active| active.state == JobState::Running);
                    if running {
                        let id = guard.next_id.fetch_add(1, Ordering::SeqCst);
                        let run = run_holder.take().expect("run held");
                        guard.jobs.insert(id, JobRecord {
                            id,
                            root: input.root.clone(),
                            reason: input.reason,
                            state: JobState::Queued,
                            attempt: 0,
                            run,
                            abort: StreamAbort::new(),
                            waiters: Vec::new(),
                            error: None,
                        });
                        guard.followup_by_root.insert(input.root.clone(), id);
                        let job = guard.jobs.get(&id).map(JobRecord::snapshot).expect("inserted");
                        return SubmitResult { job, reused: true };
                    }
                    // Active hasn't started: same coalescing shape.
                    if let Some(active) = guard.jobs.get(&active_id) {
                        let snapshot = active.snapshot();
                        return SubmitResult { job: snapshot, reused: true };
                    }
                }
            }
            if let Some(active) = guard.jobs.get(&active_id) {
                let snapshot = active.snapshot();
                return SubmitResult { job: snapshot, reused: true };
            }
        }
        let id = guard.next_id.fetch_add(1, Ordering::SeqCst);
        let run = run_holder.take().expect("run held");
        guard.jobs.insert(id, JobRecord {
            id,
            root: input.root.clone(),
            reason: input.reason,
            state: JobState::Queued,
            attempt: 0,
            run,
            abort: StreamAbort::new(),
            waiters: Vec::new(),
            error: None,
        });
        // The new job owns its root from submit time (zvec: submit-while-
        // active coalesces even before the first job starts).
        guard.active_by_root.insert(input.root.clone(), id);
        guard.queue.push_back(id);
        let snapshot = guard.jobs.get(&id).map(JobRecord::snapshot).expect("inserted");
        drop(guard);
        let _ = self.tx.send(DriverMsg::Pump);
        SubmitResult { job: snapshot, reused: false }
    }

    /// Wait for a job's terminal snapshot (`None` when unknown).
    pub async fn wait(&self, id: u64) -> Option<JobSnapshot> {
        let waiter = {
            let mut guard = common_core::sync::lock(&self.inner);
            let record = guard.jobs.get_mut(&id)?;
            match record.state {
                JobState::Succeeded | JobState::Failed | JobState::Cancelled => {
                    return Some(record.snapshot())
                }
                _ => {
                    let (tx, rx) = oneshot::channel();
                    record.waiters.push(tx);
                    rx
                }
            }
        };
        waiter.await.ok()
    }

    /// Resolve when no active/queued job remains for the root.
    pub async fn wait_for_root_idle(&self, root: &str) {
        let waiter = {
            let mut guard = common_core::sync::lock(&self.inner);
            let busy = guard.active_by_root.contains_key(root)
                || guard.followup_by_root.contains_key(root)
                || guard.queue.iter().any(|id| {
                    guard.jobs.get(id).is_some_and(|job| job.root == root)
                });
            if !busy {
                return;
            }
            let (tx, rx) = oneshot::channel();
            guard.idle_waiters.entry(root.to_string()).or_default().push(tx);
            rx
        };
        let _ = waiter.await;
    }

    /// Shut down: queued jobs cancel, the driver drains.
    pub fn close(&self) {
        {
            let mut guard = common_core::sync::lock(&self.inner);
            if guard.closed {
                return;
            }
            guard.closed = true;
            guard.queue.clear();
            // Cancel queued followups and queued actives; release root
            // ownership so post-close submits fail fast and idle resolves.
            let cancelled_ids: Vec<u64> = guard
                .jobs
                .iter()
                .filter(|(_, record)| record.state == JobState::Queued)
                .map(|(&id, _)| id)
                .collect();
            let mut cancelled = Vec::new();
            for id in cancelled_ids {
                let (snapshot, waiters, root) = {
                    let Some(record) = guard.jobs.get_mut(&id) else {
                        continue;
                    };
                    record.state = JobState::Cancelled;
                    (
                        record.snapshot(),
                        std::mem::take(&mut record.waiters),
                        record.root.clone(),
                    )
                };
                if guard.active_by_root.get(&root) == Some(&id) {
                    guard.active_by_root.remove(&root);
                }
                if guard.followup_by_root.get(&root) == Some(&id) {
                    guard.followup_by_root.remove(&root);
                }
                cancelled.push((id, snapshot, waiters));
            }
            guard.followup_by_root.clear();
            for (id, snapshot, waiters) in cancelled {
                for waiter in waiters {
                    let _ = waiter.send(snapshot.clone());
                }
                let _ = id;
            }
            Self::notify_idle_locked(&mut guard);
        }
        let _ = self.tx.send(DriverMsg::Closed);
    }

    async fn drive(&self, mut rx: mpsc::UnboundedReceiver<DriverMsg>) {
        while let Some(msg) = rx.recv().await {
            match msg {
                DriverMsg::Pump | DriverMsg::JobDone { .. } => {
                    if let DriverMsg::JobDone { id, result } = msg {
                        self.finish_job(id, result).await;
                    }
                    self.pump();
                }
                DriverMsg::Closed => {
                    // Drain: running jobs report through JobDone; nothing
                    // new is pumped after close.
                    if common_core::sync::lock(&self.inner).running == 0 {
                        return;
                    }
                }
            }
            if common_core::sync::lock(&self.inner).closed
                && common_core::sync::lock(&self.inner).running == 0
            {
                return;
            }
        }
    }

    fn pump(&self) {
        let spawn: Option<(u64, JobRun, JobContext)> = {
            let mut guard = common_core::sync::lock(&self.inner);
            if guard.closed || guard.running >= guard.concurrency {
                None
            } else {
                let next = guard.queue.pop_front().and_then(|id| {
                    guard.jobs.get(&id).map(|record| (id, record.root.clone()))
                });
                next.map(|(id, root)| {
                    let (run, abort, attempt) = {
                        let record = guard.jobs.get_mut(&id).expect("queued");
                        record.state = JobState::Running;
                        record.attempt += 1;
                        (record.run.clone(), record.abort.clone(), record.attempt)
                    };
                    guard.running += 1;
                    guard.active_by_root.insert(root, id);
                    let context = JobContext { abort, attempt };
                    (id, run, context)
                })
            }
        };
        if let Some((id, run, context)) = spawn {
            let scheduler = self.clone();
            tokio::spawn(async move {
                let result = run(context).await;
                let _ = scheduler.tx.send(DriverMsg::JobDone { id, result });
            });
        }
    }

    async fn finish_job(&self, id: u64, result: Result<(), JobError>) {
        enum After {
            Retry { run: JobRun, context: JobContext, delay_ms: u64 },
            Terminal { snapshot: JobSnapshot, waiters: Vec<oneshot::Sender<JobSnapshot>> },
        }
        // Decide under one scoped borrow, then act without holding the guard.
        let planned = {
            let guard = common_core::sync::lock(&self.inner);
            let Some(record) = guard.jobs.get(&id) else {
                return;
            };
            let retry = match &result {
                Ok(()) => false,
                Err(error) => error.retryable && record.attempt < guard.max_attempts,
            };
            (retry, record.run.clone(), record.abort.clone(), record.attempt, guard.retry_base_delay_ms)
        };
        let after = {
            let mut guard = common_core::sync::lock(&self.inner);
            guard.running = guard.running.saturating_sub(1);
            let (retry, run, abort, attempt, base_delay_ms) = planned;
            if retry {
                let delay_ms = base_delay_ms * 2u64.pow(attempt.saturating_sub(1).min(8));
                if let Some(record) = guard.jobs.get_mut(&id) {
                    record.attempt = attempt + 1;
                }
                let context = JobContext { abort, attempt: attempt + 1 };
                After::Retry { run, context, delay_ms }
            } else {
                let (state, error) = match result {
                    Ok(()) => (JobState::Succeeded, None),
                    Err(error) => (JobState::Failed, Some(error.message)),
                };
                let (snapshot, waiters, root) = {
                    let Some(record) = guard.jobs.get_mut(&id) else {
                        return;
                    };
                    record.state = state;
                    record.error = error;
                    let snapshot = JobSnapshot {
                        id: record.id,
                        root: record.root.clone(),
                        reason: record.reason,
                        state: record.state,
                        attempt: record.attempt,
                    };
                    let waiters = std::mem::take(&mut record.waiters);
                    (snapshot, waiters, record.root.clone())
                };
                if guard.active_by_root.get(&root) == Some(&id) {
                    guard.active_by_root.remove(&root);
                }
                Self::notify_idle_locked(&mut guard);
                After::Terminal { snapshot, waiters }
            }
        };
        match after {
            After::Retry { run, context, delay_ms } => {
                if delay_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
                let scheduler = self.clone();
                {
                    let mut guard = common_core::sync::lock(&self.inner);
                    guard.running += 1;
                }
                tokio::spawn(async move {
                    let result = run(context).await;
                    let _ = scheduler.tx.send(DriverMsg::JobDone { id, result });
                });
            }
            After::Terminal { snapshot, waiters } => {
                for waiter in waiters {
                    let _ = waiter.send(snapshot.clone());
                }
                // Promote the followup, if any.
                let promoted = {
                    let mut guard = common_core::sync::lock(&self.inner);
                    guard.followup_by_root.remove(&snapshot.root).and_then(|followup_id| {
                        guard.jobs.get(&followup_id).map(|record| {
                            debug_assert_eq!(record.state, JobState::Queued);
                            followup_id
                        })
                    })
                };
                if let Some(followup_id) = promoted {
                    let mut guard = common_core::sync::lock(&self.inner);
                    if guard.closed {
                        if let Some(record) = guard.jobs.get_mut(&followup_id) {
                            record.state = JobState::Cancelled;
                            let snapshot = record.snapshot();
                            let waiters = std::mem::take(&mut record.waiters);
                            drop(guard);
                            for waiter in waiters {
                                let _ = waiter.send(snapshot.clone());
                            }
                        }
                    } else {
                        guard.active_by_root.insert(snapshot.root.clone(), followup_id);
                        guard.queue.push_back(followup_id);
                        drop(guard);
                        let _ = self.tx.send(DriverMsg::Pump);
                    }
                } else {
                    let _ = self.tx.send(DriverMsg::Pump);
                }
            }
        }
    }

    fn notify_idle_locked(guard: &mut SchedulerInner) {
        let idle_roots: Vec<String> = guard
            .idle_waiters
            .keys()
            .filter(|root| {
                !guard.active_by_root.contains_key(*root)
                    && !guard.followup_by_root.contains_key(*root)
                    && !guard.queue.iter().any(|id| {
                        guard.jobs.get(id).is_some_and(|job| &job.root == *root)
                    })
            })
            .cloned()
            .collect();
        for root in idle_roots {
            if let Some(waiters) = guard.idle_waiters.remove(&root) {
                for waiter in waiters {
                    let _ = waiter.send(());
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/scheduler.rs"]
mod tests;
