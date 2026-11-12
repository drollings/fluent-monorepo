//! Fixpoint behavior: clean reruns do zero work, stuck loops say why.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::{run_to_fixpoint, serial_loop_tests, ActOutcome, LoopGuard, MaintenanceQueue, Tier};
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

fn unlimited() -> MaintenancePolicy {
    MaintenancePolicy {
        attempt_limit: 100,
        ..MaintenancePolicy::default()
    }
}

#[test]
fn clean_rerun_does_zero_work() {
    let _serial = serial_loop_tests();
    let guard = LoopGuard::acquire().expect("guard");
    let mut queue = MaintenanceQueue::new();
    queue.enqueue("a", "src", "rustfmt", &[]).expect("enqueue");
    let act: Act = Arc::new(Mutex::new(|id: &str, _tier: Tier| {
        ActOutcome::Verified(kept_report(id))
    }));
    let abort = StreamAbort::new();
    let first = run_to_fixpoint(
        &guard,
        &mut queue,
        &MaintenancePolicy::default(),
        10,
        &abort,
        &act,
    );
    assert!(first.stopped.is_none());
    let second = run_to_fixpoint(
        &guard,
        &mut queue,
        &MaintenancePolicy::default(),
        10,
        &abort,
        &act,
    );
    assert_eq!(second.passes, 0);
    assert!(second.stopped.is_none());
    assert!(second.verified.is_empty());
}

#[test]
fn unconvergeable_work_escalates_to_a_fixpoint() {
    let _serial = serial_loop_tests();
    let guard = LoopGuard::acquire().expect("guard");
    let mut queue = MaintenanceQueue::new();
    queue
        .enqueue("a", "src", "rustfmt", &[])
        .expect("enqueue a");
    queue
        .enqueue("b", "src", "rustfmt", &[])
        .expect("enqueue b");
    let act: Act = Arc::new(Mutex::new(|_id: &str, _tier: Tier| {
        ActOutcome::Retryable("red gate".to_string())
    }));
    let abort = StreamAbort::new();
    let report = run_to_fixpoint(&guard, &mut queue, &unlimited(), 50, &abort, &act);
    assert!(
        report.stopped.is_none(),
        "escalation converges: {:?}",
        report.stopped
    );
    assert!(report.verified.is_empty());
    assert_eq!(report.escalated.len(), 2);
    assert!(!report.packets.is_empty());
    assert!(report.render().contains("escalated"));
}

#[test]
fn pass_bound_says_why() {
    let _serial = serial_loop_tests();
    let guard = LoopGuard::acquire().expect("guard");
    let mut queue = MaintenanceQueue::new();
    queue
        .enqueue("a", "src", "rustfmt", &[])
        .expect("enqueue a");
    queue
        .enqueue("b", "src", "rustfmt", &[])
        .expect("enqueue b");
    queue
        .enqueue("c", "src", "rustfmt", &[])
        .expect("enqueue c");
    let act: Act = Arc::new(Mutex::new(|_id: &str, _tier: Tier| {
        ActOutcome::Retryable("red gate".to_string())
    }));
    let abort = StreamAbort::new();
    let report = run_to_fixpoint(&guard, &mut queue, &unlimited(), 4, &abort, &act);
    assert_eq!(report.passes, 4);
    assert_eq!(report.stopped.as_deref(), Some("max passes reached"));
}

#[test]
fn cancelled_abort_stops_before_any_pass() {
    let _serial = serial_loop_tests();
    let guard = LoopGuard::acquire().expect("guard");
    let mut queue = MaintenanceQueue::new();
    queue.enqueue("a", "src", "rustfmt", &[]).expect("enqueue");
    let calls = Arc::new(AtomicUsize::new(0));
    let act: Act = Arc::new(Mutex::new({
        let calls = Arc::clone(&calls);
        move |id: &str, _tier: Tier| {
            calls.fetch_add(1, Ordering::SeqCst);
            ActOutcome::Verified(kept_report(id))
        }
    }));
    let abort = StreamAbort::new();
    abort.cancel();
    let report = run_to_fixpoint(
        &guard,
        &mut queue,
        &MaintenancePolicy::default(),
        10,
        &abort,
        &act,
    );
    assert_eq!(report.passes, 0);
    assert_eq!(report.stopped.as_deref(), Some("aborted"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn tiers_escalate_on_evidence_only() {
    let _serial = serial_loop_tests();
    let guard = LoopGuard::acquire().expect("guard");
    let mut queue = MaintenanceQueue::new();
    queue.enqueue("a", "src", "rustfmt", &[]).expect("enqueue");
    let tiers = Arc::new(std::sync::Mutex::new(Vec::new()));
    let act: Act = Arc::new(Mutex::new({
        let tiers = Arc::clone(&tiers);
        move |_id: &str, tier: Tier| {
            tiers.lock().expect("tiers").push(tier);
            ActOutcome::Retryable("red gate".to_string())
        }
    }));
    let abort = StreamAbort::new();
    let report = run_to_fixpoint(
        &guard,
        &mut queue,
        &MaintenancePolicy::default(),
        10,
        &abort,
        &act,
    );
    assert_eq!(report.escalated.len(), 1);
    assert_eq!(report.summary.entries.len(), 0);
    assert_eq!(
        *tiers.lock().expect("tiers"),
        vec![Tier::Deterministic, Tier::Triage, Tier::Draft],
        "escalation takes no fourth act call"
    );
}

#[test]
fn terminal_verdict_escalates_dependents_with_owner() {
    let _serial = serial_loop_tests();
    let guard = LoopGuard::acquire().expect("guard");
    let mut queue = MaintenanceQueue::new();
    queue
        .enqueue("b", "src", "rustfmt", &[])
        .expect("enqueue b");
    queue
        .enqueue("a", "src", "rustfmt", &["b"])
        .expect("enqueue a");
    let acted = Arc::new(std::sync::Mutex::new(Vec::new()));
    let act: Act = Arc::new(Mutex::new({
        let acted = Arc::clone(&acted);
        move |id: &str, _tier: Tier| {
            acted.lock().expect("acted").push(id.to_string());
            ActOutcome::Terminal("judgment required".to_string())
        }
    }));
    let abort = StreamAbort::new();
    let report = run_to_fixpoint(
        &guard,
        &mut queue,
        &MaintenancePolicy::default(),
        10,
        &abort,
        &act,
    );
    assert!(report.stopped.is_none());
    assert_eq!(
        acted.lock().expect("acted").len(),
        1,
        "dependents never act"
    );
    assert_eq!(report.escalated.len(), 2);
    assert!(!report.packets.is_empty());
    assert!(report.render().contains("escalated"));
}

#[test]
fn blocked_without_owner_says_why() {
    let _serial = serial_loop_tests();
    let guard = LoopGuard::acquire().expect("guard");
    let mut queue = MaintenanceQueue::new();
    queue
        .enqueue("orphan", "src", "rustfmt", &["ghost"])
        .expect("enqueue");
    let act: Act = Arc::new(Mutex::new(|id: &str, _tier: Tier| {
        ActOutcome::Verified(kept_report(id))
    }));
    let abort = StreamAbort::new();
    let report = run_to_fixpoint(
        &guard,
        &mut queue,
        &MaintenancePolicy::default(),
        10,
        &abort,
        &act,
    );
    assert_eq!(report.stopped.as_deref(), Some("blocked without owner"));
}
