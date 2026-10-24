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
    eprintln!("elapsed={elapsed:?} completed={} failed={} cancelled={} panicked={}",
        summary.completed.len(), summary.failed.len(), summary.cancelled.len(), summary.panicked.len());
    for e in &summary.completed { eprintln!("completed: {e:?}"); }
    for e in &summary.cancelled { eprintln!("cancelled: {e:?}"); }
    // CONFIRMED H5: sync `std::thread::sleep(400ms)` with 50ms budget does NOT
    // land in cancelled. tokio::time::timeout polls inner first; a blocked
    // task cannot observe the deadline until execute returns, and then inner
    // wins the race -> Ok(completed). The timeout is silently ignored.
    assert_eq!(summary.completed.len(), 1, "characterization: blocking execute wins race, timeout ignored");
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
    assert!(elapsed < Duration::from_millis(300), "summary must return near budget, got {elapsed:?}");
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
    assert_eq!(summary.completed.len(), 2, "both complete; busy blocked its worker {elapsed:?}");
}
