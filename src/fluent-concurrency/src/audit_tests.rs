//! Audit H5 + H8-ladder + H13-boundary probes for fluent-concurrency.
//! Included via `#[path]` forwarder in lib.rs (autotests=false).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fluent_wvr::prelude::*;
use internment::ArcIntern;

struct SleepUnit {
    name: ArcIntern<str>,
    sleep_ms: u64,
    finished: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
}
impl WorkUnit for SleepUnit {
    fn name(&self) -> &str {
        &self.name
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(self.sleep_ms));
        self.finished.store(true, Ordering::SeqCst);
        Ok(WorkOutput::ok("slept"))
    }
}
impl FieldAccess for SleepUnit {
    fn set_field(&mut self, _: &str, _: &str) -> Result<(), FieldError> {
        Err(FieldError::NotFound("none".into()))
    }
    fn get_field(&self, _: &str) -> Result<String, FieldError> {
        Err(FieldError::NotFound("none".into()))
    }
    fn field_names(&self) -> &'static [&'static str] {
        &[]
    }
}
impl Describable for SleepUnit {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({})
    }
}
fluent_wvr::impl_component!(SleepUnit);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn h5_sync_sleep_blocks_timeout_characterization() {
    let finished = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let unit: Arc<dyn Component> = Arc::new(SleepUnit {
        name: ArcIntern::from("sleeper"),
        sleep_ms: 400,
        finished: Arc::clone(&finished),
        calls: Arc::clone(&calls),
    });
    let rt = crate::tokio_runtime();
    let mut batch = crate::batch::SupervisedBatch::new(rt, CapabilitySet::new());
    let mut ctx = WorkContext::for_unit(&*unit, CapabilitySet::new());
    ctx.timeout_ms = 50;
    ctx.max_retries = 0;
    batch.register_with_context(unit, ctx).unwrap();
    let start = Instant::now();
    let summary = batch.await;
    let elapsed = start.elapsed();
    eprintln!(
        "elapsed={elapsed:?} completed={} failed={} cancelled={} panicked={}",
        summary.completed.len(),
        summary.failed.len(),
        summary.cancelled.len(),
        summary.panicked.len()
    );
    for e in &summary.completed {
        eprintln!("completed: {e:?}");
    }
    for e in &summary.cancelled {
        eprintln!("cancelled: {e:?}");
    }
    // CONFIRMED H5: sync `std::thread::sleep(400ms)` with 50ms budget does NOT
    // land in cancelled. tokio::time::timeout polls inner first; a blocked
    // task cannot observe the deadline until execute returns, and then inner
    // wins the race -> Ok(completed). The timeout is silently ignored.
    assert_eq!(
        summary.completed.len(),
        1,
        "characterization: blocking execute wins race, timeout ignored"
    );
    assert_eq!(summary.cancelled.len(), 0);
    assert!(
        elapsed >= Duration::from_millis(350),
        "summary waits for blocking execute; elapsed={elapsed:?}"
    );
    assert!(finished.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "AUDIT-H5: sync execute must be interruptible (spawn_blocking + timeout); currently timeout is silently ignored"]
async fn h5_sync_sleep_timeout_desired() {
    let finished = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let unit: Arc<dyn Component> = Arc::new(SleepUnit {
        name: ArcIntern::from("sleeper-desired"),
        sleep_ms: 400,
        finished: Arc::clone(&finished),
        calls: Arc::clone(&calls),
    });
    let rt = crate::tokio_runtime();
    let mut batch = crate::batch::SupervisedBatch::new(rt, CapabilitySet::new());
    let mut ctx = WorkContext::for_unit(&*unit, CapabilitySet::new());
    ctx.timeout_ms = 50;
    ctx.max_retries = 0;
    batch.register_with_context(unit, ctx).unwrap();
    let start = Instant::now();
    let summary = batch.await;
    let elapsed = start.elapsed();
    assert_eq!(summary.cancelled.len(), 1, "timeout must land in cancelled");
    assert!(
        elapsed < Duration::from_millis(300),
        "summary must return near budget, got {elapsed:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn h5_busy_loop_blocks_sibling_characterization() {
    struct Busy {
        n: ArcIntern<str>,
        ms: u64,
    }
    impl WorkUnit for Busy {
        fn name(&self) -> &str {
            &self.n
        }
        fn depends(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn provides(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
            let end = Instant::now() + Duration::from_millis(self.ms);
            let mut x = 0u64;
            while Instant::now() < end {
                x = x.wrapping_add(1);
                std::hint::spin_loop();
            }
            std::hint::black_box(x);
            Ok(WorkOutput::ok("busy"))
        }
    }
    impl FieldAccess for Busy {
        fn set_field(&mut self, _: &str, _: &str) -> Result<(), FieldError> {
            Err(FieldError::NotFound("none".into()))
        }
        fn get_field(&self, _: &str) -> Result<String, FieldError> {
            Err(FieldError::NotFound("none".into()))
        }
        fn field_names(&self) -> &'static [&'static str] {
            &[]
        }
    }
    impl Describable for Busy {
        fn describe(&self) -> serde_json::Value {
            serde_json::json!({})
        }
    }
    fluent_wvr::impl_component!(Busy);
    let fast_done = Arc::new(AtomicBool::new(false));
    struct Fast {
        n: ArcIntern<str>,
        flag: Arc<AtomicBool>,
    }
    impl WorkUnit for Fast {
        fn name(&self) -> &str {
            &self.n
        }
        fn depends(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn provides(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
            self.flag.store(true, Ordering::SeqCst);
            Ok(WorkOutput::ok("fast"))
        }
    }
    impl FieldAccess for Fast {
        fn set_field(&mut self, _: &str, _: &str) -> Result<(), FieldError> {
            Err(FieldError::NotFound("none".into()))
        }
        fn get_field(&self, _: &str) -> Result<String, FieldError> {
            Err(FieldError::NotFound("none".into()))
        }
        fn field_names(&self) -> &'static [&'static str] {
            &[]
        }
    }
    impl Describable for Fast {
        fn describe(&self) -> serde_json::Value {
            serde_json::json!({})
        }
    }
    fluent_wvr::impl_component!(Fast);

    let rt = crate::tokio_runtime();
    let mut batch = crate::batch::SupervisedBatch::new(rt, CapabilitySet::new());
    let busy: Arc<dyn Component> = Arc::new(Busy {
        n: ArcIntern::from("busy"),
        ms: 300,
    });
    let fast: Arc<dyn Component> = Arc::new(Fast {
        n: ArcIntern::from("fast"),
        flag: Arc::clone(&fast_done),
    });
    let mut c1 = WorkContext::for_unit(&*busy, CapabilitySet::new());
    c1.timeout_ms = 5_000;
    let mut c2 = WorkContext::for_unit(&*fast, CapabilitySet::new());
    c2.timeout_ms = 5_000;
    batch.register_with_context(busy, c1).unwrap();
    batch.register_with_context(fast, c2).unwrap();
    let start = Instant::now();
    let summary = batch.await;
    let elapsed = start.elapsed();
    // With 4 workers the fast task still completes; documents dispatch is per-task JoinSet::spawn.
    assert!(fast_done.load(Ordering::SeqCst));
    assert_eq!(
        summary.completed.len(),
        2,
        "both complete; busy blocked its worker {elapsed:?}"
    );
}

// ── M3: the timeout guarantee, pinned ─────────────────────────────────────
// The per-attempt timeout bounds the await/retry loop around
// `execute_with_timeout_and_retry`. It does NOT preempt a synchronous
// `execute` body: a body that blocks past its budget holds its worker,
// keeps running, and the finished inner wins the race — the summary
// reports `completed`, late, with the completion flag set (the zombie).
// `cancelled`/`Timeout` is reachable only where the task suspends at an
// await point (between-attempt backoff) so the deadline can fire.

async fn timeout_summary(
    unit: Arc<dyn Component>,
    timeout_ms: u64,
    max_retries: u32,
) -> (crate::batch::SupervisedBatchSummary, std::time::Duration) {
    let rt = crate::tokio_runtime();
    let mut batch = crate::batch::SupervisedBatch::new(rt, CapabilitySet::new());
    let mut ctx = WorkContext::for_unit(&*unit, CapabilitySet::new());
    ctx.timeout_ms = timeout_ms;
    ctx.max_retries = max_retries;
    batch.register_with_context(unit, ctx).unwrap();
    let start = Instant::now();
    let summary = batch.await;
    (summary, start.elapsed())
}

fn is_timeout_cancel(summary: &crate::batch::SupervisedBatchSummary, name: &str) -> bool {
    summary.cancelled.iter().any(|event| {
        matches!(
            event,
            crate::batch::SupervisedBatchEvent::Cancelled {
                reason: crate::batch::CancelReason::Timeout,
                ..
            }
        ) && format!("{event:?}").contains(name)
    })
}

/// M3.t1(a): `thread::sleep` past the budget reports completed late with
/// the completion flag set — the zombie, not a prompt cancel.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m3_sleep_overrun_completes_late_with_flag() {
    let finished = Arc::new(AtomicBool::new(false));
    let unit: Arc<dyn Component> = Arc::new(SleepUnit {
        name: ArcIntern::from("m3-sleeper"),
        sleep_ms: 300,
        finished: Arc::clone(&finished),
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let (summary, elapsed) = timeout_summary(unit, 50, 0).await;
    assert_eq!(
        summary.completed.len(),
        1,
        "overrun body wins the race late"
    );
    assert_eq!(
        summary.cancelled.len(),
        0,
        "deadline cannot fire into a blocked body"
    );
    assert!(
        elapsed >= Duration::from_millis(250),
        "summary waits for the blocking body; elapsed={elapsed:?}"
    );
    assert!(finished.load(Ordering::SeqCst), "zombie ran to completion");
}

/// M3.t1(b): busy-loop past the budget reports completed late.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m3_busy_overrun_completes_late() {
    struct Spin {
        n: ArcIntern<str>,
        ms: u64,
        finished: Arc<AtomicBool>,
    }
    impl WorkUnit for Spin {
        fn name(&self) -> &str {
            &self.n
        }
        fn depends(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn provides(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
            let end = Instant::now() + Duration::from_millis(self.ms);
            let mut x = 0u64;
            while Instant::now() < end {
                x = x.wrapping_add(1);
                std::hint::spin_loop();
            }
            std::hint::black_box(x);
            self.finished.store(true, Ordering::SeqCst);
            Ok(WorkOutput::ok("spun"))
        }
    }
    impl FieldAccess for Spin {
        fn set_field(&mut self, _: &str, _: &str) -> Result<(), FieldError> {
            Err(FieldError::NotFound("none".into()))
        }
        fn get_field(&self, _: &str) -> Result<String, FieldError> {
            Err(FieldError::NotFound("none".into()))
        }
        fn field_names(&self) -> &'static [&'static str] {
            &[]
        }
    }
    impl Describable for Spin {
        fn describe(&self) -> serde_json::Value {
            serde_json::json!({})
        }
    }
    fluent_wvr::impl_component!(Spin);
    let finished = Arc::new(AtomicBool::new(false));
    let unit: Arc<dyn Component> = Arc::new(Spin {
        n: ArcIntern::from("m3-spin"),
        ms: 200,
        finished: Arc::clone(&finished),
    });
    let (summary, elapsed) = timeout_summary(unit, 50, 0).await;
    assert_eq!(
        summary.completed.len(),
        1,
        "spinning body wins the race late"
    );
    assert_eq!(summary.cancelled.len(), 0);
    assert!(
        elapsed >= Duration::from_millis(150),
        "summary waits for the spinning body; elapsed={elapsed:?}"
    );
    assert!(finished.load(Ordering::SeqCst), "zombie ran to completion");
}

/// M3.t1(c): control — a body that finishes inside its budget completes
/// promptly with its flag set. The guarantee changes nothing here.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m3_quick_body_completes_promptly() {
    let finished = Arc::new(AtomicBool::new(false));
    let unit: Arc<dyn Component> = Arc::new(SleepUnit {
        name: ArcIntern::from("m3-quick"),
        sleep_ms: 0,
        finished: Arc::clone(&finished),
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let (summary, elapsed) = timeout_summary(unit, 5_000, 0).await;
    assert_eq!(summary.completed.len(), 1);
    assert_eq!(summary.cancelled.len(), 0);
    assert!(
        elapsed < Duration::from_millis(1_000),
        "no artificial delay; elapsed={elapsed:?}"
    );
    assert!(finished.load(Ordering::SeqCst));
}

struct RetryableFail {
    n: ArcIntern<str>,
    calls: Arc<AtomicUsize>,
}
impl WorkUnit for RetryableFail {
    fn name(&self) -> &str {
        &self.n
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(WorkError::Dependency(
            "prerequisite not yet provided".into(),
        ))
    }
}
impl FieldAccess for RetryableFail {
    fn set_field(&mut self, _: &str, _: &str) -> Result<(), FieldError> {
        Err(FieldError::NotFound("none".into()))
    }
    fn get_field(&self, _: &str) -> Result<String, FieldError> {
        Err(FieldError::NotFound("none".into()))
    }
    fn field_names(&self) -> &'static [&'static str] {
        &[]
    }
}
impl Describable for RetryableFail {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({})
    }
}
fluent_wvr::impl_component!(RetryableFail);

/// M3.t1(d): the ONLY shape that lands in `cancelled`/`Timeout` — the body
/// returns fast with a retryable error, and the deadline fires while the
/// task suspends in between-attempt backoff. The body yielded in time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m3_backoff_suspend_lands_in_cancelled_timeout() {
    let calls = Arc::new(AtomicUsize::new(0));
    let unit: Arc<dyn Component> = Arc::new(RetryableFail {
        n: ArcIntern::from("m3-retryable"),
        calls: Arc::clone(&calls),
    });
    let rt = crate::tokio_runtime();
    let config = crate::batch::SupervisedBatchConfig {
        backoff_base_ms: 500,
        ..Default::default()
    };
    let mut batch =
        crate::batch::SupervisedBatch::new_with_config(rt, CapabilitySet::new(), config);
    let mut ctx = WorkContext::for_unit(&*unit, CapabilitySet::new());
    ctx.timeout_ms = 50;
    ctx.max_retries = 3;
    batch.register_with_context(unit, ctx).unwrap();
    let summary = batch.await;
    assert_eq!(summary.completed.len(), 0);
    assert!(
        is_timeout_cancel(&summary, "m3-retryable"),
        "deadline fires during backoff suspend; summary={summary:?}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "timeout during first backoff: no second attempt overlaps the zombie"
    );
}

/// M3.t2: current-thread control — executor shape does not rescue a
/// blocking body. Same overrun shapes as the multi-thread matrix.
#[tokio::test(flavor = "current_thread")]
async fn m3_current_thread_sleep_overrun_completes_late() {
    let finished = Arc::new(AtomicBool::new(false));
    let unit: Arc<dyn Component> = Arc::new(SleepUnit {
        name: ArcIntern::from("m3-ct-sleeper"),
        sleep_ms: 200,
        finished: Arc::clone(&finished),
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let rt = crate::tokio_runtime();
    let mut batch = crate::batch::SupervisedBatch::new(rt, CapabilitySet::new());
    let mut ctx = WorkContext::for_unit(&*unit, CapabilitySet::new());
    ctx.timeout_ms = 50;
    ctx.max_retries = 0;
    batch.register_with_context(unit, ctx).unwrap();
    let start = Instant::now();
    let summary = batch.await;
    let elapsed = start.elapsed();
    assert_eq!(
        summary.completed.len(),
        1,
        "overrun body wins the race late"
    );
    assert_eq!(summary.cancelled.len(), 0);
    assert!(elapsed >= Duration::from_millis(150), "elapsed={elapsed:?}");
    assert!(finished.load(Ordering::SeqCst), "zombie ran to completion");
}

/// M3.t2 control: quick body on a current-thread runtime completes promptly.
#[tokio::test(flavor = "current_thread")]
async fn m3_current_thread_quick_body_completes() {
    let finished = Arc::new(AtomicBool::new(false));
    let unit: Arc<dyn Component> = Arc::new(SleepUnit {
        name: ArcIntern::from("m3-ct-quick"),
        sleep_ms: 0,
        finished: Arc::clone(&finished),
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let rt = crate::tokio_runtime();
    let mut batch = crate::batch::SupervisedBatch::new(rt, CapabilitySet::new());
    let mut ctx = WorkContext::for_unit(&*unit, CapabilitySet::new());
    ctx.timeout_ms = 5_000;
    ctx.max_retries = 0;
    batch.register_with_context(unit, ctx).unwrap();
    let summary = batch.await;
    assert_eq!(summary.completed.len(), 1);
    assert!(finished.load(Ordering::SeqCst));
}

/// M3.t3(i): starvation probe — a fast sibling completes while a sibling
/// blocks. Dispatch is per-task `JoinSet::spawn`, not a global wedge
/// (while workers remain).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m3_fast_sibling_completes_while_sibling_blocks() {
    let fast_done = Arc::new(AtomicBool::new(false));
    struct Fast {
        n: ArcIntern<str>,
        flag: Arc<AtomicBool>,
    }
    impl WorkUnit for Fast {
        fn name(&self) -> &str {
            &self.n
        }
        fn depends(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn provides(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
            self.flag.store(true, Ordering::SeqCst);
            Ok(WorkOutput::ok("fast"))
        }
    }
    impl FieldAccess for Fast {
        fn set_field(&mut self, _: &str, _: &str) -> Result<(), FieldError> {
            Err(FieldError::NotFound("none".into()))
        }
        fn get_field(&self, _: &str) -> Result<String, FieldError> {
            Err(FieldError::NotFound("none".into()))
        }
        fn field_names(&self) -> &'static [&'static str] {
            &[]
        }
    }
    impl Describable for Fast {
        fn describe(&self) -> serde_json::Value {
            serde_json::json!({})
        }
    }
    fluent_wvr::impl_component!(Fast);
    let slow: Arc<dyn Component> = Arc::new(SleepUnit {
        name: ArcIntern::from("m3-slow"),
        sleep_ms: 300,
        finished: Arc::new(AtomicBool::new(false)),
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let fast: Arc<dyn Component> = Arc::new(Fast {
        n: ArcIntern::from("m3-fast"),
        flag: Arc::clone(&fast_done),
    });
    let rt = crate::tokio_runtime();
    let mut batch = crate::batch::SupervisedBatch::new(rt, CapabilitySet::new());
    let mut c1 = WorkContext::for_unit(&*slow, CapabilitySet::new());
    c1.timeout_ms = 5_000;
    let mut c2 = WorkContext::for_unit(&*fast, CapabilitySet::new());
    c2.timeout_ms = 5_000;
    batch.register_with_context(slow, c1).unwrap();
    batch.register_with_context(fast, c2).unwrap();
    let summary = batch.await;
    assert!(
        fast_done.load(Ordering::SeqCst),
        "fast sibling is not wedged"
    );
    assert_eq!(
        summary.completed.len(),
        2,
        "both complete; slow blocked one worker"
    );
    assert_eq!(summary.cancelled.len(), 0);
}

/// M3.t3(ii): retry-bounded probe — a retryable failure with a generous
/// budget runs at most the configured attempts, then lands in `failed`
/// exactly once: no silent double-execution claim.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m3_retry_runs_bounded_attempts_then_fails_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let unit: Arc<dyn Component> = Arc::new(RetryableFail {
        n: ArcIntern::from("m3-bounded"),
        calls: Arc::clone(&calls),
    });
    let rt = crate::tokio_runtime();
    let config = crate::batch::SupervisedBatchConfig {
        backoff_base_ms: 10,
        backoff_jitter_ms: 0,
        ..Default::default()
    };
    let mut batch =
        crate::batch::SupervisedBatch::new_with_config(rt, CapabilitySet::new(), config);
    let mut ctx = WorkContext::for_unit(&*unit, CapabilitySet::new());
    ctx.timeout_ms = 10_000;
    ctx.max_retries = 2;
    batch.register_with_context(unit, ctx).unwrap();
    let summary = batch.await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "initial attempt plus exactly the configured retries"
    );
    assert_eq!(summary.completed.len(), 0);
    assert_eq!(summary.cancelled.len(), 0);
    assert_eq!(summary.failed.len(), 1, "the unit is counted exactly once");
}
