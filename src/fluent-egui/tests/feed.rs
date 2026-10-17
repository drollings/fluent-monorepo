use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fluent_concurrency::batch::{CancelReason, SupervisedBatch, SupervisedBatchEvent};
use fluent_wvr::{Describable, FieldAccess};
use protopyne_chart_model::{ChartRow, ChartBars};

fn batch_at(start: i64, count: usize) -> ChartBars {
    ChartBars {
        rows: (0..count)
            .map(|i| ChartRow {
                time: start + i as i64,
                open: 100.0,
                high: 101.0,
                low: 99.0,
                close: 100.0,
                volume: 10,
            })
            .collect(),
        dropped: 0,
    }
}

fn event_name(event: &SupervisedBatchEvent) -> String {
    match event {
        SupervisedBatchEvent::Completed { name, .. }
        | SupervisedBatchEvent::Panicked { name, .. }
        | SupervisedBatchEvent::Failed { name, .. }
        | SupervisedBatchEvent::Cancelled { name, .. } => name.to_string(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn feed_delivers_history_in_order_then_drains() {
    let (mut feed, mut rx) = ChartFeed::new(8).expect("valid feed");
    feed.spawn_history(vec![batch_at(0, 2), batch_at(2, 2), batch_at(4, 1)]);
    for expect in [0, 2, 4] {
        let got = rx.recv().await.expect("batch arrives");
        assert_eq!(got.rows[0].time, expect);
    }
    feed.close().await;
    assert!(ChartFeed::new(0).is_err(), "zero capacity refuses");
}

#[test]
fn burst_intake_stays_within_cap() {
    let (feed, _rx) = ChartFeed::new(2).expect("valid feed");
    let tx = feed.sender();
    tx.try_send(batch_at(0, 1)).expect("first fits");
    tx.try_send(batch_at(1, 1)).expect("second fits");
    assert!(tx.try_send(batch_at(2, 1)).is_err(), "burst past cap refuses instead of growing");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pane_close_stops_tasks_at_the_next_yield() {
    let handle = PaneHandle::new();
    assert!(!handle.is_closed());
    let counter = Arc::new(AtomicUsize::new(0));
    let probe = Arc::clone(&counter);
    let abort = handle.abort().clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                () = abort.cancelled() => break,
                () = tokio::task::yield_now() => {
                    probe.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
    });
    while counter.load(Ordering::SeqCst) < 5 {
        tokio::task::yield_now().await;
    }
    handle.close();
    assert!(handle.is_closed());
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("close terminates the task promptly")
        .expect("join succeeds");
    let frozen = counter.load(Ordering::SeqCst);
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(counter.load(Ordering::SeqCst), frozen, "no progress after close");

    // The sticky signal stops tasks before their first progress.
    let fresh = Arc::new(AtomicUsize::new(0));
    let probe = Arc::clone(&fresh);
    let abort = handle.abort().clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                () = abort.cancelled() => break,
                () = tokio::task::yield_now() => {
                    probe.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
    })
    .await
    .expect("join succeeds");
    assert_eq!(fresh.load(Ordering::SeqCst), 0, "closed panes admit no new work");
}

/// Fault-injection double for the cancellation test. The batch spawns every
/// unit at registration and a synchronous `execute` never yields, so an
/// instant unit can never observe a cancel — it is already done. The
/// retryable-fault mode returns a retryable `Dependency` error instead, which
/// parks the task in the batch's backoff sleep: a genuine suspension point
/// where the dependent-cancel abort deterministically lands mid-flight.
#[derive(Debug, Clone, FieldAccess, Describable)]
struct SlowPaneUnit {
    /// Pane name.
    #[field(desc = "Pane name")]
    name: String,
    /// Upstream pane names.
    #[field(skip)]
    depends: Vec<ArcIntern<str>>,
    /// Provided asset (the pane itself).
    #[field(skip)]
    provides: Vec<ArcIntern<str>>,
    /// Immediate non-retryable failure when true.
    #[field(skip)]
    fault: bool,
    /// Retryable failure (parks in backoff sleep) when true.
    #[field(skip)]
    retryable: bool,
}

impl WorkUnit for SlowPaneUnit {
    fn name(&self) -> &str {
        &self.name
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &self.depends
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &self.provides
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        if self.fault {
            return Err(WorkError::Execution(format!("{} injected fault", self.name)));
        }
        if self.retryable {
            return Err(WorkError::Dependency(format!("{} waiting on upstream", self.name)));
        }
        Ok(WorkOutput::ok(format!("{} done", self.name)))
    }
}

fluent_wvr::impl_component!(SlowPaneUnit);

fn slow(name: &str, depends: &[&str], fault: bool, retryable: bool) -> SlowPaneUnit {
    SlowPaneUnit {
        name: name.to_owned(),
        depends: depends.iter().map(|d| ArcIntern::from(*d)).collect(),
        provides: vec![ArcIntern::from(name)],
        fault,
        retryable,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_wave_cancels_overlays_only() {
    use fluent_wvr::WorkContext;
    let runtime = tokio_runtime();
    let caps = default_capability_set();
    let mut batch = SupervisedBatch::new(Arc::clone(&runtime), caps.clone());
    batch.register(Arc::new(slow("root", &[], false, false))).expect("registers");
    batch.register(Arc::new(slow("mid", &["root"], true, false))).expect("registers");
    // The leaf parks in retry backoff (a real suspension point), so the
    // dependent-cancel abort lands while it is still in flight.
    let leaf_ctx =
        WorkContext::for_unit_in_batch(&runtime, &caps, |ctx| ctx.max_retries = 2);
    batch
        .register_with_context(Arc::new(slow("leaf", &["mid"], false, true)), leaf_ctx)
        .expect("registers");
    batch.register(Arc::new(slow("side", &[], false, false))).expect("registers");
    let summary = batch.await;

    let failed: Vec<String> = summary.failed.iter().map(event_name).collect();
    assert_eq!(failed, vec!["mid".to_owned()], "only the faulty unit fails");
    let completed: Vec<String> = summary.completed.iter().map(event_name).collect();
    assert!(completed.contains(&"root".to_owned()), "upstream completes");
    assert!(completed.contains(&"side".to_owned()), "independents continue");
    assert!(!completed.contains(&"leaf".to_owned()), "dependents never complete");
    assert_eq!(summary.cancelled.len(), 1, "exactly the dependent cancels");
    match &summary.cancelled[0] {
        SupervisedBatchEvent::Cancelled { name, reason } => {
            assert_eq!(name.to_string(), "leaf");
            assert!(
                matches!(reason, CancelReason::DependencyFailed),
                "cancel names the cause: {reason:?}"
            );
        }
        other => panic!("leaf must cancel, got {other:?}"),
    }
}

fn wave_plan() -> PanePlan {
    let mut plan = PanePlan::new();
    plan.register_node("price", &[], 0).expect("price registers");
    plan.register_node("signal", &["price"], 0).expect("signal registers");
    plan.register_node("ribbon", &["signal"], 0).expect("ribbon registers");
    plan
}

#[test]
fn wave_units_count_kept_and_fault_on_demand() {
    let plan = wave_plan();
    assert_eq!(
        plan.dependencies("ribbon"),
        vec!["signal".to_owned()],
        "wave edges come from the plan"
    );
    let unit = PaneWaveUnit::for_plan(&plan, "ribbon", vec![Some(1.0), None, Some(f64::NAN)]);
    let output = unit.execute(&WorkContext::default()).expect("healthy unit");
    assert!(output.success);
    assert!(output.message.contains("1/3 kept"), "skip tally reported: {}", output.message);
    let faulty = unit.with_fault("boom");
    assert!(faulty.execute(&WorkContext::default()).is_err(), "injected fault fails");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wave_runs_plan_units_and_reports() {
    let plan = wave_plan();
    let order = plan.order().expect("ordered");
    let units: Vec<PaneWaveUnit> = order
        .iter()
        .map(|name| PaneWaveUnit::for_plan(&plan, name, vec![Some(1.0), None]))
        .collect();
    let summary = run_wave(units).await.expect("healthy wave drains");
    assert_eq!(summary.completed.len(), 3, "every pane completes");
    assert!(summary.failed.is_empty() && summary.cancelled.is_empty());

    let units: Vec<PaneWaveUnit> = order
        .iter()
        .map(|name| {
            let unit = PaneWaveUnit::for_plan(&plan, name, vec![Some(1.0)]);
            if name == "signal" { unit.with_fault("signal lost") } else { unit }
        })
        .collect();
    let summary = run_wave(units).await.expect("faulty wave still drains");
    let failed: Vec<String> = summary.failed.iter().map(event_name).collect();
    assert_eq!(failed, vec!["signal".to_owned()]);
    let completed: Vec<String> = summary.completed.iter().map(event_name).collect();
    assert!(completed.contains(&"price".to_owned()), "independents continue past failure");
    // Dependent cancellation of instant units races completion (they never
    // yield); the deterministic cancel property lives in
    // `failed_wave_cancels_overlays_only`, whose leaf parks in backoff.

    let dupe = vec![
        PaneWaveUnit::for_plan(&plan, "price", Vec::new()),
        PaneWaveUnit::for_plan(&plan, "price", Vec::new()),
    ];
    assert!(run_wave(dupe).await.is_err(), "duplicate pane names refuse");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ladder_first_ok_wins() {
    let warm = batch_at(0, 2);
    let hot = batch_at(100, 2);
    let out = load_history(vec![
        HistoryRung { name: "cold".to_owned(), rows: None, terminal: false },
        HistoryRung { name: "warm".to_owned(), rows: Some(warm), terminal: false },
        HistoryRung { name: "hot".to_owned(), rows: Some(hot), terminal: false },
    ])
    .await
    .expect("ladder loads");
    assert_eq!(out.expect("hit").rows[0].time, 0, "first hit wins");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ladder_terminal_aborts() {
    let late = batch_at(100, 1);
    let err = load_history(vec![
        HistoryRung { name: "cold".to_owned(), rows: None, terminal: false },
        HistoryRung { name: "dead".to_owned(), rows: None, terminal: true },
        HistoryRung { name: "late".to_owned(), rows: Some(late), terminal: false },
    ])
    .await
    .expect_err("terminal rung aborts");
    assert_eq!(err, HistoryError::Terminal("dead".to_owned()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ladder_exhaustion_is_clean() {
    let out = load_history(vec![
        HistoryRung { name: "a".to_owned(), rows: None, terminal: false },
        HistoryRung { name: "b".to_owned(), rows: None, terminal: false },
    ])
    .await
    .expect("exhaustion is not an error");
    assert_eq!(out, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn credit_intake_blocks_and_releases() {
    let intake = CreditIntake::new(1, 1).expect("valid intake");
    assert!(!intake.is_blocked());
    intake.send(|| async { 1_u32 }).await;
    // Polling the second send once parks it on the exhausted credit
    // deterministically — a single poll observes zero credit and records the
    // producer as blocked before returning `Pending` — so the assertion below
    // never depends on the scheduler running a spawned task in time.
    let mut parked = Box::pin(intake.send(|| async { 2_u32 }));
    let woken = std::future::Future::poll(
        parked.as_mut(),
        &mut std::task::Context::from_waker(std::task::Waker::noop()),
    );
    assert!(woken.is_pending(), "exhausted credit parks the producer");
    assert!(intake.is_blocked(), "exhausted credit parks producers");
    intake.release();
    assert_eq!(parked.await, 2, "released credit runs the waiter");
    assert!(CreditIntake::new(0, 1).is_err() && CreditIntake::new(1, 0).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refresh_gate_bounds_concurrency() {
    let gate = Arc::new(RefreshGate::new(2).expect("valid gate"));
    let current = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for _ in 0..6 {
        let (gate, current, peak) = (Arc::clone(&gate), Arc::clone(&current), Arc::clone(&peak));
        tasks.push(tokio::spawn(async move {
            gate.run(|| async {
                let held = current.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(held, Ordering::SeqCst);
                tokio::task::yield_now().await;
                current.fetch_sub(1, Ordering::SeqCst);
            })
            .await;
        }));
    }
    for task in tasks {
        task.await.expect("join succeeds");
    }
    assert!(peak.load(Ordering::SeqCst) <= 2, "gate binds concurrency");
    assert!(RefreshGate::new(0).is_err(), "zero permits refuse");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transport_retry_recovers_then_exhausts() {
    let policy = TransportRetry::new(3, 1, 0).expect("valid policy");
    let attempts = Arc::new(AtomicUsize::new(0));
    let probe = Arc::clone(&attempts);
    let out = policy
        .run(|_: &String| true, move || {
            let probe = Arc::clone(&probe);
            async move {
                let n = probe.fetch_add(1, Ordering::SeqCst) + 1;
                if n < 3 { Err("flaky".to_owned()) } else { Ok(n) }
            }
        })
        .await;
    assert_eq!(out, Ok(3));
    assert_eq!(attempts.load(Ordering::SeqCst), 3);

    let attempts = Arc::new(AtomicUsize::new(0));
    let probe = Arc::clone(&attempts);
    let err = TransportRetry::default()
        .run(|_: &String| true, move || {
            let probe = Arc::clone(&probe);
            async move {
                probe.fetch_add(1, Ordering::SeqCst);
                Err::<u32, String>("down".to_owned())
            }
        })
        .await
        .expect_err("exhaustion surfaces");
    assert_eq!(err, "down");
    assert_eq!(attempts.load(Ordering::SeqCst), 3, "default policy tries three times");
    assert!(TransportRetry::new(0, 1, 0).is_err(), "zero attempts refuse");
}
