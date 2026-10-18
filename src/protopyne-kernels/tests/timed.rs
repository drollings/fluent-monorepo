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

#[test]
fn kernel_unit_reports_counts_with_decodable_data() {
    // Loud by contract: the message carries the counts and the payload
    // decodes — a null-payload success would let a broken result read empty.
    use crate::indicator::IndicatorResult;
    use crate::timed::KernelUnit;
    let input = Arc::new(OwnedOhlcv {
        open: vec![],
        high: vec![],
        low: vec![],
        close: vec![1.0, 2.0, 3.0, 4.0],
        volume: vec![],
    });
    let unit = KernelUnit::new(
        Arc::new(SmaIndicator::new()),
        ParamSet::new().with_int("length", 2),
        input,
    );
    let output = unit
        .execute(&WorkContext::default())
        .expect("kernel executes");
    assert!(output.success);
    assert!(
        output.message.contains("sma"),
        "message names the kernel, got: {}",
        output.message
    );
    assert!(
        output.message.contains('4'),
        "message carries the bar count, got: {}",
        output.message
    );
    let decoded: IndicatorResult = output.data_take().expect("payload decodes, never null");
    assert_eq!(decoded.values.len(), 4);
}
