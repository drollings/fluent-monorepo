use std::sync::Arc;

use common_core::metrics::LatencyHistogram;
use fluent_wvr::prelude::{Component, WorkContext, WorkUnit};

use crate::indicator::{OwnedOhlcv, ParamSet};
use crate::sma::SmaIndicator;
use crate::timed::instrumented_kernel;

#[test]
fn instrumented_kernel_records_latency_and_result() {
    let histogram = Arc::new(LatencyHistogram::new());
    let input = Arc::new(OwnedOhlcv {
        open: vec![],
        high: vec![],
        low: vec![],
        close: vec![1.0, 2.0, 3.0, 4.0],
        volume: vec![],
    });
    let unit = instrumented_kernel(
        Arc::new(SmaIndicator::new()),
        ParamSet::new().with_int("length", 2),
        input,
        &histogram,
    );
    let shared: Arc<dyn Component> = Arc::new(unit);
    let output = shared
        .execute(&WorkContext::default())
        .expect("kernel executes");
    assert!(output.success);
    assert_eq!(histogram.count(), 1);
}
