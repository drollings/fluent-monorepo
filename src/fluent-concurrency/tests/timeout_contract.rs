use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fluent_wvr::prelude::*;
use internment::ArcIntern;

struct BlockingUnit {
    name: ArcIntern<str>,
    sleep_ms: u64,
    finished: Arc<AtomicBool>,
}

impl WorkUnit for BlockingUnit {
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
        std::thread::sleep(Duration::from_millis(self.sleep_ms));
        self.finished.store(true, Ordering::SeqCst);
        Ok(WorkOutput::ok("blocked"))
    }
}

impl FieldAccess for BlockingUnit {
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

impl Describable for BlockingUnit {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({})
    }
}

fluent_wvr::impl_component!(BlockingUnit);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn whole_unit_budget_reports_late_completed_never_cancelled() {
    let finished = Arc::new(AtomicBool::new(false));
    let unit: Arc<dyn Component> = Arc::new(BlockingUnit {
        name: ArcIntern::from("blocking-twice-budget"),
        sleep_ms: 100,
        finished: Arc::clone(&finished),
    });
    let rt = crate::tokio_runtime();
    let mut batch = crate::batch::SupervisedBatch::new(rt, CapabilitySet::new());
    let mut ctx = WorkContext::for_unit(&*unit, CapabilitySet::new());
    ctx.timeout_ms = 50;
    ctx.max_retries = 0;
    batch.register_with_context(unit, ctx).unwrap();
    let start = Instant::now();
    let summary = batch.await;
    assert_eq!(summary.completed.len(), 1);
    assert_eq!(summary.cancelled.len(), 0);
    assert_eq!(summary.failed.len(), 0);
    assert_eq!(summary.panicked.len(), 0);
    assert!(start.elapsed() >= Duration::from_millis(100));
    assert!(finished.load(Ordering::SeqCst));
}
