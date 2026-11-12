//! Work-queue persistence: dep order, idempotence, crash-resume, guard.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::{
    run_to_fixpoint, serial_loop_tests, ActOutcome, LoopGuard, MaintenanceQueue, QueueError, Tier,
    WorkState,
};
use crate::offload::{ApplyReport, ApplyVerdict};
use crate::policy::MaintenancePolicy;
use fluent_concurrency::stream::StreamAbort;
use fluent_wvr_testutil::tempdir;

type Act = Arc<Mutex<dyn FnMut(&str, Tier) -> ActOutcome + Send>>;

fn kept_report(file: &str) -> ApplyReport {
    ApplyReport {
        file: file.to_string(),
        tool: "rustfmt",
        changed: true,
        verdict: ApplyVerdict::Kept,
        attempts_used: 1,
        gate_output: "ok".to_string(),
        before_hash: "a".to_string(),
        after_hash: "b".to_string(),
    }
}

fn policy() -> MaintenancePolicy {
    MaintenancePolicy::default()
}

#[test]
fn dep_ordered_idempotent_and_never_redone() {
    let _serial = serial_loop_tests();
    let guard = LoopGuard::acquire().expect("guard");
    let mut queue = MaintenanceQueue::new();
    queue
        .enqueue("a", "src", "rustfmt", &[])
        .expect("enqueue a");
    queue
        .enqueue("b", "src", "rustfmt", &["a"])
        .expect("enqueue b");
    let order = Arc::new(MutexOrder::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let act: Act = Arc::new(Mutex::new({
        let order = Arc::clone(&order);
        let calls = Arc::clone(&calls);
        move |id: &str, _tier: Tier| {
            calls.fetch_add(1, Ordering::SeqCst);
            order.push(id.to_string());
            ActOutcome::Verified(kept_report(id))
        }
    }));
    let abort = StreamAbort::new();
    let first = run_to_fixpoint(&guard, &mut queue, &policy(), 10, &abort, &act);
    assert!(first.stopped.is_none(), "fixpoint: {:?}", first.stopped);
    assert_eq!(first.verified, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(order.take(), vec!["a".to_string(), "b".to_string()]);
    assert_eq!(queue.state("a"), Some(WorkState::Verified));
    let before = calls.load(Ordering::SeqCst);
    let second = run_to_fixpoint(&guard, &mut queue, &policy(), 10, &abort, &act);
    assert_eq!(second.passes, 0);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        before,
        "verified work never redone"
    );
    assert!(second.render().contains("fixpoint"));
}

#[test]
fn crash_resumes_without_redoing_verified() {
    let _serial = serial_loop_tests();
    let guard = LoopGuard::acquire().expect("guard");
    let mut queue = MaintenanceQueue::new();
    queue
        .enqueue("a", "src", "rustfmt", &[])
        .expect("enqueue a");
    queue
        .enqueue("b", "src", "rustfmt", &["a"])
        .expect("enqueue b");
    let counts = Arc::new(CountMap::new());
    let act: Act = Arc::new(Mutex::new({
        let counts = Arc::clone(&counts);
        move |id: &str, _tier: Tier| {
            counts.bump(id);
            ActOutcome::Verified(kept_report(id))
        }
    }));
    let abort = StreamAbort::new();
    let partial = run_to_fixpoint(&guard, &mut queue, &policy(), 1, &abort, &act);
    assert_eq!(partial.passes, 1);
    assert_eq!(partial.verified, vec!["a".to_string()]);
    let snapshot = queue.snapshot();
    drop(queue);
    let mut resumed = MaintenanceQueue::new();
    resumed.restore(&snapshot).expect("restore");
    let done = run_to_fixpoint(&guard, &mut resumed, &policy(), 10, &abort, &act);
    assert!(done.stopped.is_none());
    assert_eq!(done.verified, vec!["b".to_string()]);
    assert_eq!(counts.get("a"), 1, "crashed run never redoes verified work");
    assert_eq!(counts.get("b"), 1);
}

#[test]
fn duplicate_ids_and_second_driver_are_typed_errors() {
    let _serial = serial_loop_tests();
    let first = LoopGuard::acquire().expect("first");
    assert_eq!(LoopGuard::acquire().err(), Some(QueueError::AlreadyRunning));
    let mut queue = MaintenanceQueue::new();
    queue
        .enqueue("a", "src", "rustfmt", &[])
        .expect("enqueue a");
    assert_eq!(
        queue.enqueue("a", "src", "rustfmt", &[]).err(),
        Some(QueueError::Duplicate("a".to_string()))
    );
    drop(first);
    let _back = LoopGuard::acquire().expect("released");
}

#[derive(Default)]
struct MutexOrder {
    inner: std::sync::Mutex<Vec<String>>,
}

impl MutexOrder {
    fn new() -> Self {
        Self::default()
    }

    fn push(&self, id: String) {
        self.inner.lock().expect("order").push(id);
    }

    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.inner.lock().expect("order"))
    }
}

#[derive(Default)]
struct CountMap {
    inner: std::sync::Mutex<std::collections::HashMap<String, usize>>,
}

impl CountMap {
    fn new() -> Self {
        Self::default()
    }

    fn bump(&self, id: &str) {
        *self
            .inner
            .lock()
            .expect("counts")
            .entry(id.to_string())
            .or_insert(0) += 1;
    }

    fn get(&self, id: &str) -> usize {
        self.inner
            .lock()
            .expect("counts")
            .get(id)
            .copied()
            .unwrap_or(0)
    }
}

#[test]
fn ready_lists_leaves_before_dependents_with_blast_radius_tiebreak() {
    // Same-depth items order by ascending blast radius: `y` (no
    // dependents) before `x` (blocks `z`), regardless of enqueue order.
    // Chains surface only their leaves: `a` before blocked `b`, `c`.
    let mut queue = MaintenanceQueue::new();
    queue
        .enqueue("x", "src", "rustfmt", &[])
        .expect("enqueue x");
    queue
        .enqueue("y", "src", "rustfmt", &[])
        .expect("enqueue y");
    queue
        .enqueue("z", "src", "rustfmt", &["x"])
        .expect("enqueue z");
    assert_eq!(queue.ready(), vec!["y".to_string(), "x".to_string()]);
    let mut chained = MaintenanceQueue::new();
    chained
        .enqueue("c", "src", "rustfmt", &["b"])
        .expect("enqueue c");
    chained
        .enqueue("b", "src", "rustfmt", &["a"])
        .expect("enqueue b");
    chained
        .enqueue("a", "src", "rustfmt", &[])
        .expect("enqueue a");
    assert_eq!(chained.ready(), vec!["a".to_string()]);
}

#[test]
fn snapshots_persist_and_resume_without_redoing() {
    // Kill mid-run: one pass, snapshot on disk, verified kept.
    let _serial = serial_loop_tests();
    let guard = LoopGuard::acquire().expect("guard");
    let mut queue = MaintenanceQueue::new();
    queue
        .enqueue("a", "src", "rustfmt", &[])
        .expect("enqueue a");
    queue
        .enqueue("b", "src", "rustfmt", &["a"])
        .expect("enqueue b");
    let dir = tempdir();
    let snap = dir.path().join("queue.json");
    queue.set_snapshot_path(&snap);
    let act: Act = Arc::new(Mutex::new(|id: &str, _tier: Tier| {
        ActOutcome::Verified(kept_report(id))
    }));
    let abort = StreamAbort::new();
    let partial = run_to_fixpoint(&guard, &mut queue, &policy(), 1, &abort, &act);
    assert_eq!(partial.verified, vec!["a".to_string()]);
    assert!(snap.is_file(), "state-changing pass writes a snapshot");
    // Resume in a fresh queue from the snapshot: `a` stays verified.
    let mut resumed = MaintenanceQueue::new();
    resumed.load_snapshot(&snap).expect("load");
    assert_eq!(resumed.state("a"), Some(WorkState::Verified));
    assert_eq!(resumed.state("b"), Some(WorkState::Queued));
    let calls = Arc::new(CountMap::new());
    let act_counts: Act = Arc::new(Mutex::new({
        let counts = Arc::clone(&calls);
        move |id: &str, _tier: Tier| {
            counts.bump(id);
            ActOutcome::Verified(kept_report(id))
        }
    }));
    let done = run_to_fixpoint(&guard, &mut resumed, &policy(), 10, &abort, &act_counts);
    assert!(done.stopped.is_none());
    assert_eq!(done.verified, vec!["b".to_string()]);
    assert_eq!(calls.get("a"), 0, "verified work is never redone");
    assert_eq!(calls.get("b"), 1);
    // Missing file loads as a fresh queue, never an error.
    let mut fresh = MaintenanceQueue::new();
    fresh
        .load_snapshot(&dir.path().join("absent.json"))
        .expect("missing snapshot is a fresh start");
    assert!(fresh.ready().is_empty());
}

#[test]
fn unavailable_model_waits_without_consuming_attempts() {
    // A background model-tier item whose backend is unreachable waits:
    // no attempt consumed, no escalation, listed under an explicit
    // `model: unavailable` report state for the next run to retry.
    let _serial = serial_loop_tests();
    let guard = LoopGuard::acquire().expect("guard");
    let mut queue = MaintenanceQueue::new();
    queue
        .enqueue("a", "src", "rustfmt", &[])
        .expect("enqueue a");
    let act: Act = Arc::new(Mutex::new(|_id: &str, _tier: Tier| {
        ActOutcome::Unavailable("backend busy".to_string())
    }));
    let abort = StreamAbort::new();
    let report = run_to_fixpoint(&guard, &mut queue, &policy(), 1, &abort, &act);
    let item = queue.items.get("a").expect("item still queued");
    assert_eq!(item.attempts, 0, "waiting consumes no attempts");
    assert_eq!(item.state, WorkState::Queued);
    assert_eq!(
        report.model_unavailable,
        vec![("a".to_string(), "backend busy".to_string())]
    );
    let text = report.render();
    assert!(text.contains("model: unavailable"), "{text}");
    assert!(text.contains('a'), "{text}");
}

#[test]
fn set_state_records_driver_outcomes() {
    // The `run` command records wave-loop outcomes on the durable
    // queue: verified/escalated complete the step, queued reopens it,
    // attempts are never touched by recording.
    let mut queue = MaintenanceQueue::new();
    queue
        .enqueue("a", "src", "clippy-fix", &[])
        .expect("enqueue a");
    queue
        .enqueue("b", "src", "clippy-fix", &[])
        .expect("enqueue b");
    queue.set_state("a", WorkState::Verified);
    queue.set_state("b", WorkState::Escalated);
    assert_eq!(queue.state("a"), Some(WorkState::Verified));
    assert_eq!(queue.state("b"), Some(WorkState::Escalated));
    assert!(queue.is_fixpoint(), "terminal states leave no work");
    queue.set_state("a", WorkState::Queued);
    assert_eq!(queue.state("a"), Some(WorkState::Queued));
    assert!(!queue.is_fixpoint());
    queue.set_state("missing", WorkState::Verified);
}
