//! Trading chart control plane: chart config, plan timing, range windows.
//!
//! [`ChartConfig`] is the toolbar-facing struct — timeframe, pane switches,
//! oscillator pair, display threshold, ribbon fill bounds, geometry, theme
//! key — deriving the accessor, validator, and schema from one definition.
//! It validates fail-closed through [`Validate`] and executes through the
//! [`check_for_execute`] bridge by building the standard pane topology, so
//! [`timed_plan_build`] times exactly that construction (never per-bar
//! paint). [`window_for_range`] maps toolbar presets to trailing data
//! windows through the canonical preset math.

use std::sync::Arc;

use common_core::metrics::LatencyHistogram;
use fluent_egui::{EguiError, PaneConfig};
use fluent_wvr::prelude::*;
use fluent_wvr::{Describable, FieldAccess};
use protopyne_chart_model::{ChartRow, DateRange};
use protopyne_types::{Validate, check_for_execute, valid_output};

use crate::oscillators::parse_oscillator_kind;
use crate::topology::standard_plan;

fn default_timeframe() -> String {
    "M15".to_owned()
}

fn default_fast_oscillator() -> String {
    "rsi".to_owned()
}

fn default_slow_oscillator() -> String {
    "cci".to_owned()
}

fn default_theme() -> String {
    "default".to_owned()
}

/// Top-level chart configuration: toolbar selection, pane switches, display
/// threshold, ribbon fill bounds, geometry, and theme key.
///
/// `execute` validates, then builds the standard pane plan for the configured
/// oscillator pair and reports its size — plan construction is the unit's
/// work, so [`timed_plan_build`] times exactly this.
#[derive(
    Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize, FieldAccess, Describable
)]
#[builder(start_fn = new)]
pub struct ChartConfig {
    /// Bar timeframe key selected in the toolbar.
    #[field(desc = "Bar timeframe key selected in the toolbar", max_len = 16)]
    #[builder(default = default_timeframe())]
    #[serde(default = "default_timeframe")]
    pub timeframe: String,
    /// Whether the price pane paints.
    #[field(desc = "Whether the price pane paints")]
    #[builder(default = true)]
    pub show_price: bool,
    /// Whether the volume strip paints.
    #[field(desc = "Whether the volume strip paints")]
    #[builder(default = true)]
    pub show_volume: bool,
    /// Whether the oscillator pane paints.
    #[field(desc = "Whether the oscillator pane paints")]
    #[builder(default = true)]
    pub show_oscillator: bool,
    /// Fast oscillator kind name (settings spelling, for example "rsi").
    #[field(desc = "Fast oscillator kind name", max_len = 16)]
    #[builder(default = default_fast_oscillator())]
    #[serde(default = "default_fast_oscillator")]
    pub fast_oscillator: String,
    /// Slow oscillator kind name (settings spelling, for example "cci").
    #[field(desc = "Slow oscillator kind name", max_len = 16)]
    #[builder(default = default_slow_oscillator())]
    #[serde(default = "default_slow_oscillator")]
    pub slow_oscillator: String,
    /// Pivot threshold on the normalized band; zero never fires meaningfully.
    #[field(desc = "Pivot threshold on the normalized band", min = 1, max = 100000)]
    #[builder(default = 200)]
    pub threshold: u32,
    /// Ribbon fill lower bound.
    #[field(desc = "Ribbon fill lower bound", min = 0, max = 1)]
    #[builder(default = 0.14)]
    pub alpha_low: f64,
    /// Ribbon fill upper bound.
    #[field(desc = "Ribbon fill upper bound", min = 0, max = 1)]
    #[builder(default = 0.8)]
    pub alpha_high: f64,
    /// Pane height in pixels.
    #[field(desc = "Pane height in pixels", min = 1)]
    #[builder(default = 400)]
    pub height: u32,
    /// Pixels per bar.
    #[field(desc = "Pixels per bar")]
    #[builder(default = 8.0)]
    pub bar_spacing: f64,
    /// Theme key resolving style keys to colors.
    #[field(desc = "Theme key resolving style keys to colors", max_len = 32)]
    #[builder(default = default_theme())]
    #[serde(default = "default_theme")]
    pub theme: String,
}

impl Default for ChartConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

impl ChartConfig {
    /// Fail-closed self-consistency check shared by schema writes
    /// (`execute`) and direct callers.
    pub fn validate(&self) -> Result<(), EguiError> {
        if self.timeframe.trim().is_empty() {
            return Err(EguiError::InvalidParam {
                what: "timeframe".to_owned(),
                reason: "timeframe must not be empty".to_owned(),
            });
        }
        if self.threshold == 0 {
            return Err(EguiError::InvalidParam {
                what: "threshold".to_owned(),
                reason: "pivot threshold must be at least 1".to_owned(),
            });
        }
        for (what, value) in [("alpha_low", self.alpha_low), ("alpha_high", self.alpha_high)] {
            if !value.is_finite() || value < 0.0 || value > 1.0 {
                return Err(EguiError::InvalidParam {
                    what: what.to_owned(),
                    reason: "fill bounds must sit within [0, 1]".to_owned(),
                });
            }
        }
        if self.alpha_low > self.alpha_high {
            return Err(EguiError::InvalidParam {
                what: "alpha_low".to_owned(),
                reason: "lower fill bound must not exceed the upper bound".to_owned(),
            });
        }
        if self.height == 0 {
            return Err(EguiError::InvalidParam {
                what: "height".to_owned(),
                reason: "pane height must be at least 1 pixel".to_owned(),
            });
        }
        if !self.bar_spacing.is_finite() || self.bar_spacing <= 0.0 {
            return Err(EguiError::InvalidParam {
                what: "bar_spacing".to_owned(),
                reason: "pixels per bar must be positive and finite".to_owned(),
            });
        }
        if self.theme.trim().is_empty() {
            return Err(EguiError::InvalidParam {
                what: "theme".to_owned(),
                reason: "theme key must not be empty".to_owned(),
            });
        }
        for (what, kind) in
            [("fast_oscillator", &self.fast_oscillator), ("slow_oscillator", &self.slow_oscillator)]
        {
            if parse_oscillator_kind(kind).is_none() {
                return Err(EguiError::InvalidParam {
                    what: what.to_owned(),
                    reason: format!("unknown oscillator kind {kind:?}"),
                });
            }
        }
        Ok(())
    }

    /// Pane switches as generic layout entries (one per visible pane).
    #[must_use]
    pub fn pane_layout(&self) -> Vec<PaneConfig> {
        [
            ("price", self.show_price),
            ("volume", self.show_volume),
            ("oscillator", self.show_oscillator),
        ]
        .into_iter()
        .filter(|(_, visible)| *visible)
        .map(|(name, _)| PaneConfig::new().name(name.to_owned()).build())
        .collect()
    }
}

impl Validate for ChartConfig {
    type Err = EguiError;
    fn validate(&self) -> Result<(), Self::Err> {
        ChartConfig::validate(self)
    }
}

impl WorkUnit for ChartConfig {
    fn name(&self) -> &str {
        "chart_config"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        check_for_execute(self, "chart config invalid")?;
        if parse_oscillator_kind(&self.fast_oscillator).is_none() {
            return Err(WorkError::Execution("chart config invalid: bad fast kind".to_owned()));
        }
        if parse_oscillator_kind(&self.slow_oscillator).is_none() {
            return Err(WorkError::Execution("chart config invalid: bad slow kind".to_owned()));
        }
        let panes = standard_plan()
            .map_err(|e| WorkError::Execution(format!("chart config invalid: {e}")))?
            .order()
            .map_err(|e| WorkError::Execution(format!("chart config invalid: {e}")))?;
        Ok(valid_output(format!("plan ready ({} panes)", panes.len())))
    }
}

impl_component!(ChartConfig);

/// Wrap plan construction in latency timing: the returned unit validates,
/// builds the pane plan on `execute`, and records the observed duration
/// into `histogram`. Timing covers plan builds, never per-bar paint.
#[must_use]
pub fn timed_plan_build(
    config: ChartConfig,
    histogram: Arc<LatencyHistogram>,
) -> Instrumented<ChartConfig> {
    Instrumented::with_metrics(config, "plan.build", histogram)
}

/// Trailing window for a toolbar range preset.
///
/// Keeps the last `range.estimated_bars(bar_duration_seconds)` rows (or every
/// row when fewer exist), so a range pick maps to a data window through the
/// canonical preset math instead of a second estimator.
#[must_use]
pub fn window_for_range(
    rows: &[ChartRow],
    range: DateRange,
    bar_duration_seconds: u64,
) -> &[ChartRow] {
    let keep = range.estimated_bars(bar_duration_seconds).min(rows.len());
    &rows[rows.len() - keep..]
}

#[cfg(test)]
#[path = "../tests/chart.rs"]
mod tests;
