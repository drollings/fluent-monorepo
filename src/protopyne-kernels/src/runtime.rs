//! Bar-by-bar Pine runtime: configuration plus the evaluation driver.
//!
//! [`PineRuntimeConfig`] is the control-plane handle (uniform `Component`,
//! runtime-configurable via `set_field` at registration). [`PineRuntime`]
//! exposes the same surface by delegation and additionally drives
//! [`RunCtx`] evaluation bar by bar. Hot per-bar access stays on direct
//! fields (Tier 1); `set_field` runs only at registration (Tier 3).

use fluent_wvr::prelude::*;
use fluent_wvr::{Describable, FieldAccess};
use protopyne_types::Bar;

use crate::context::RunCtx;
use crate::error::KernelError;
use crate::inputs::InputSet;

/// Default chart symbol for runs without an explicit harness scope.
pub(crate) fn default_chart_symbol() -> String {
    "LOCAL".to_owned()
}

/// Default chart period for runs without an explicit harness scope.
pub(crate) fn default_chart_period() -> String {
    "1".to_owned()
}

/// Configuration for one Pine runtime: chart scope plus caps and warmup as
/// `Component` state.
#[derive(Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize, FieldAccess, Describable)]
#[builder(start_fn = new)]
pub struct PineRuntimeConfig {
    /// Chart symbol backing `syminfo.tickerid` for the run.
    #[field(desc = "Chart symbol backing syminfo.tickerid", max_len = 32)]
    #[builder(default = default_chart_symbol())]
    #[serde(default = "default_chart_symbol")]
    pub chart_symbol: String,
    /// Chart timeframe period string backing `timeframe.period` (for
    /// example `"1"`, `"5"`, `"D"`).
    #[field(desc = "Chart timeframe period backing timeframe.period", max_len = 8)]
    #[builder(default = default_chart_period())]
    #[serde(default = "default_chart_period")]
    pub chart_period: String,
    /// Maximum bars evaluated in one run.
    #[field(desc = "Maximum bars evaluated in one run", min = 1, max = 1000000)]
    #[builder(default = 10_000)]
    pub max_bars: u64,
    /// Warmup bars required before the first trustworthy output.
    /// ENFORCED by [`PineRuntime::run_over`]: the first `warmup_bars` bars
    /// advance without evaluation, so no callback ever observes them.
    #[field(
        desc = "Warmup bars skipped before the first evaluated bar (enforced by run_over)",
        min = 0,
        max = 1000000
    )]
    #[builder(default)]
    pub warmup_bars: u64,
}

impl protopyne_types::Validate for PineRuntimeConfig {
    type Err = crate::error::KernelError;
    fn validate(&self) -> Result<(), Self::Err> {
        PineRuntimeConfig::validate(self)
    }
}

impl Default for PineRuntimeConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

impl PineRuntimeConfig {
    /// Fail-closed self-consistency check shared by scheduling (`execute`)
    /// and driving (`PineRuntime::run_over`).
    pub fn validate(&self) -> Result<(), KernelError> {
        if self.chart_symbol.trim().is_empty() {
            return Err(KernelError::ConfigConflict(
                "chart_symbol must be non-empty".to_owned(),
            ));
        }
        if let Err(reason) = crate::pine::builtins_core::check_period(&self.chart_period) {
            return Err(KernelError::ConfigConflict(reason));
        }
        if self.max_bars < 1 {
            return Err(KernelError::ConfigConflict(format!(
                "max_bars must be at least 1, got {}",
                self.max_bars
            )));
        }
        if self.warmup_bars > self.max_bars {
            return Err(KernelError::ConfigConflict(format!(
                "warmup_bars {} exceeds max_bars {}",
                self.warmup_bars, self.max_bars
            )));
        }
        Ok(())
    }
}

impl WorkUnit for PineRuntimeConfig {
    fn name(&self) -> &str {
        "pine_runtime_config"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        protopyne_types::check_for_execute(self, "pine runtime invalid")?;
        Ok(WorkOutput::ok(format!(
            "pine runtime ready (max_bars={}, warmup_bars={})",
            self.max_bars, self.warmup_bars
        )))
    }
}

impl_component!(PineRuntimeConfig);

/// The evaluation driver. Owns its config; per-bar work flows through
/// [`run_over`](Self::run_over) with a caller-supplied callback so future
/// kernels compose without touching this loop.
pub struct PineRuntime {
    config: PineRuntimeConfig,
}

impl PineRuntime {
    /// Bind a driver to a config.
    #[must_use]
    pub fn new(config: PineRuntimeConfig) -> Self {
        Self { config }
    }

    /// Borrow the config. Hot-path reads go through here (Tier 1).
    #[must_use]
    pub fn config(&self) -> &PineRuntimeConfig {
        &self.config
    }

    /// Evaluate `on_bar` against every closed bar in order, starting from a
    /// fresh [`RunCtx`]. Returns the number of bars evaluated. Refuses input
    /// beyond `max_bars` instead of silently truncating.
    ///
    /// `warmup_bars` is enforced, not advisory: the first `warmup_bars` bars
    /// advance the context without evaluation, so callbacks never observe
    /// untrustworthy early bars. The returned count covers evaluated bars
    /// only, and [`RunCtx::cursor`](crate::context::RunCtx::cursor) inside
    /// `on_bar` starts at `warmup_bars`.
    pub fn run_over(
        &self,
        bars: &[Bar],
        inputs: InputSet,
        mut on_bar: impl FnMut(&mut RunCtx<'_>),
    ) -> Result<usize, KernelError> {
        self.config.validate()?;
        let got = bars.len() as u64;
        if got > self.config.max_bars {
            return Err(KernelError::TooManyBars {
                got,
                max: self.config.max_bars,
            });
        }
        let mut ctx = RunCtx::new(bars, inputs);
        for _ in 0..self.config.warmup_bars {
            if ctx.current().is_none() {
                break;
            }
            ctx.advance();
        }
        let mut evaluated = 0_usize;
        while ctx.current().is_some() {
            on_bar(&mut ctx);
            evaluated += 1;
            ctx.advance();
        }
        Ok(evaluated)
    }
}

impl WorkUnit for PineRuntime {
    fn name(&self) -> &str {
        "pine_runtime"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        self.config.execute(ctx)
    }
}

impl FieldAccess for PineRuntime {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        self.config.set_field(name, value)
    }

    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        self.config.get_field(name)
    }

    fn field_names(&self) -> &'static [&'static str] {
        self.config.field_names()
    }
}

impl Describable for PineRuntime {
    fn describe(&self) -> serde_json::Value {
        self.config.describe()
    }
}

impl_component!(PineRuntime);

#[cfg(test)]
#[path = "../tests/runtime.rs"]
mod tests;
