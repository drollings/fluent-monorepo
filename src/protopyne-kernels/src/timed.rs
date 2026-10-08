//! Supervised kernel execution with per-kernel latency recording.
//!
//! [`KernelUnit`] adapts one [`Indicator`](crate::indicator::Indicator)
//! batch evaluation to the [`WorkUnit`](fluent_wvr::prelude::WorkUnit)
//! contract so kernels run under `SupervisedBatch` supervision like every
//! other fallible stage, and [`instrumented_kernel`] wraps it in
//! [`Instrumented::with_metrics`](fluent_wvr::prelude::Instrumented) before
//! type erasure so each kernel owns its latency histogram. The hot loop
//! itself stays free-function based; this is the control-plane path.

use std::sync::Arc;

use common_core::metrics::LatencyHistogram;
use fluent_wvr::prelude::*;

use crate::error::KernelError;
use crate::indicator::{Indicator, OwnedOhlcv, ParamSet};

/// One supervised batch evaluation of a kernel over owned window data.
pub struct KernelUnit {
    indicator: Arc<dyn Indicator>,
    params: ParamSet,
    input: Arc<OwnedOhlcv>,
}

impl KernelUnit {
    /// Bind a kernel, its parameters, and the window it evaluates.
    #[must_use]
    pub fn new(indicator: Arc<dyn Indicator>, params: ParamSet, input: Arc<OwnedOhlcv>) -> Self {
        Self {
            indicator,
            params,
            input,
        }
    }

    /// Borrow the evaluated result type for error mapping.
    fn run(&self) -> Result<crate::indicator::IndicatorResult, KernelError> {
        self.indicator.batch(&self.input.views(), &self.params)
    }
}

impl WorkUnit for KernelUnit {
    fn name(&self) -> &str {
        &self.indicator.spec().id
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let result = self
            .run()
            .map_err(|e| WorkError::Execution(e.to_string()))?;
        let warmed = result.values.iter().flatten().count();
        Ok(WorkOutput::ok_with_data(
            format!(
                "{} evaluated {} bars ({} warmed)",
                self.indicator.spec().id,
                result.len(),
                warmed
            ),
            serde_json::to_value(&result).unwrap_or(serde_json::Value::Null),
        ))
    }
}

impl FieldAccess for KernelUnit {
    fn set_field(&mut self, name: &str, _value: &str) -> Result<(), FieldError> {
        Err(FieldError::NotFound(name.into()))
    }

    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        match name {
            "indicator" => Ok(self.indicator.spec().id.clone()),
            _ => Err(FieldError::NotFound(name.into())),
        }
    }

    fn field_names(&self) -> &'static [&'static str] {
        &["indicator"]
    }
}

impl Describable for KernelUnit {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "indicator": {
                    "type": "string",
                    "description": "Registry id of the evaluated kernel"
                }
            },
            "required": ["indicator"]
        })
    }
}

impl_component!(KernelUnit);

/// Wrap a kernel evaluation with per-kernel latency recording. The returned
/// unit is ready for `Arc<dyn Component>` erasure; observe the histogram
/// for count and aggregate latency.
#[must_use]
pub fn instrumented_kernel(
    indicator: Arc<dyn Indicator>,
    params: ParamSet,
    input: Arc<OwnedOhlcv>,
    histogram: &Arc<LatencyHistogram>,
) -> Instrumented<KernelUnit> {
    let label = indicator.spec().id.clone();
    Instrumented::with_metrics(
        KernelUnit::new(indicator, params, input),
        label,
        Arc::clone(histogram),
    )
}

#[cfg(test)]
#[path = "../tests/timed.rs"]
mod tests;
