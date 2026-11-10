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
    queue.enqueue("a", "src", "rustfmt", &[]).expect("enqueue a");
    queue.enqueue("b", "src", "rustfmt", &["a"]).expect("enqueue b");
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
    assert_eq!(calls.load(Ordering::SeqCst), before, "verified work never redone");
    assert!(second.render().contains("fixpoint"));
}

#[test]
fn crash_resumes_without_redoing_verified() {
    let _serial = serial_loop_tests();
    let guard = LoopGuard::acquire().expect("guard");
    let mut queue = MaintenanceQueue::new();
    queue.enqueue("a", "src", "rustfmt", &[]).expect("enqueue a");
    queue.enqueue("b", "src", "rustfmt", &["a"]).expect("enqueue b");
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
    queue.enqueue("a", "src", "rustfmt", &[]).expect("enqueue a");
    assert_eq!(queue.enqueue("a", "src", "rustfmt", &[]).err(), Some(QueueError::Duplicate("a".to_string())));
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
        *self.inner.lock().expect("counts").entry(id.to_string()).or_insert(0) += 1;
    }

    fn get(&self, id: &str) -> usize {
        self.inner.lock().expect("counts").get(id).copied().unwrap_or(0)
    }
}
