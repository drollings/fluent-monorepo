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
    depends: Vec<ArcIntern<str>>,
    provides: Vec<ArcIntern<str>>,
}

impl KernelUnit {
    /// Bind a kernel, its parameters, and the window it evaluates.
    /// `depends`/`provides` are interned from the spec once, here — the same
    /// derivation as [`IndicatorComponent`](crate::component::IndicatorComponent),
    /// so per-`execute` dispatch pays no allocation and both supervision
    /// adapters order through one rule.
    #[must_use]
    pub fn new(indicator: Arc<dyn Indicator>, params: ParamSet, input: Arc<OwnedOhlcv>) -> Self {
        let depends = crate::kernel_eval::kernel_depends(&indicator);
        let provides = crate::kernel_eval::kernel_provides(&indicator);
        Self {
            indicator,
            params,
            input,
            depends,
            provides,
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
        &self.depends
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &self.provides
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let result = self.run().map_err(WorkError::execution_display)?;
        let warmed = crate::kernel_eval::warmed_count(&result);
        // A serialization failure fails the unit loudly: succeeding with a
        // null payload would let a broken result read as an empty one.
        let data = serde_json::to_value(&result).map_err(|e| {
            WorkError::execution_display(format!(
                "{} result failed to serialize: {e}",
                self.indicator.spec().id
            ))
        })?;
        Ok(WorkOutput::ok_with_data(
            crate::kernel_eval::kernel_message(&self.indicator.spec().id, result.len(), warmed),
            data,
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
/// Wrap a kernel evaluation with per-kernel latency recording. The returned
/// unit is ready for `Arc<dyn Component>` erasure; observe the histogram
/// for count and aggregate latency.
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