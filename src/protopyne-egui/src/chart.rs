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
use fluent_egui::{EguiError, PaneConfig, RibbonView};
use fluent_wvr::prelude::*;
use fluent_wvr::{Describable, FieldAccess};
use protopyne_chart_model::{ChartRow, ChartType, DateRange, heikin_ashi, rows_of};
use protopyne_types::{Bar, Validate, check_for_execute, valid_output};

use crate::oscillators::parse_oscillator_kind;
use crate::topology::standard_plan;

fn default_timeframe() -> String {
    "M15".to_owned()
}

/// Intraday bar timeframe key behind the terminal toolbar.
///
/// Display-only selector: each key maps to bar spacing (for range-window
/// math) and to a history slot in the shell. Higher keys (`H1`, `H4`)
/// resample the `M15` slot upward in the shell — the kernels always see
/// plain closed bars, never a timeframe concept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TimeframeKey {
    /// One-minute bars.
    M1,
    /// Five-minute bars.
    M5,
    /// Fifteen-minute bars (default).
    #[default]
    M15,
    /// One-hour bars (resampled ×4 from `M15`).
    H1,
    /// Four-hour bars (resampled ×16 from `M15`).
    H4,
}

impl TimeframeKey {
    /// Every key in toolbar order.
    #[must_use]
    pub fn all() -> Vec<Self> {
        vec![Self::M1, Self::M5, Self::M15, Self::H1, Self::H4]
    }

    /// Short button label (`1M`, `5M`, `15M`, `1H`, `4H`).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::M1 => "1M",
            Self::M5 => "5M",
            Self::M15 => "15M",
            Self::H1 => "1H",
            Self::H4 => "4H",
        }
    }

    /// Bar spacing in seconds, for range-window math.
    #[must_use]
    pub fn seconds(self) -> u64 {
        match self {
            Self::M1 => 60,
            Self::M5 => 300,
            Self::M15 => 900,
            Self::H1 => 3_600,
            Self::H4 => 14_400,
        }
    }

    /// Resample factor over the `M15` slot (`1` where a native file exists).
    #[must_use]
    pub fn m15_group(self) -> usize {
        match self {
            Self::M1 | Self::M5 | Self::M15 => 1,
            Self::H1 => 4,
            Self::H4 => 16,
        }
    }
}

impl std::fmt::Display for TimeframeKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
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

/// Price bars behind the terminal's picture for one chart type: classic
/// candles pass bars through, Heikin-Ashi delegates to the headless
/// transform (no second smoothing math lives here). Only these two render
/// from bars — line and area render from display rows — so every other
/// taxonomy variant refuses with the parameter named instead of rendering
/// a guess.
pub fn chart_price_bars(bars: &[Bar], kind: ChartType) -> Result<Vec<Bar>, EguiError> {
    match kind {
        ChartType::Candles => Ok(bars.to_vec()),
        ChartType::Heikin => Ok(heikin_ashi(bars)),
        unsupported => Err(EguiError::InvalidParam {
            what: "chart_type".to_owned(),
            reason: format!(
                "{unsupported:?} has no terminal picture from bars; line and area render from rows"
            ),
        }),
    }
}

/// Line series behind the terminal's line picture: the row closes, shaped
/// for the polyline emitter.
#[must_use]
pub fn line_values(rows: &[ChartRow]) -> Vec<Option<f64>> {
    rows.iter().map(|row| Some(row.close)).collect()
}

/// Rows painted for one chart type over a bar history: candles, line, and
/// area share the converted window rows; Heikin-Ashi converts over the full
/// history first (its opens recurse — windowing first would miscompute
/// every open), then windows. Refuses dropped conversions (downstream keys
/// would misalign) and unwired types with the parameter named.
pub fn picture_rows(
    bars: &[Bar],
    range: DateRange,
    bar_seconds: u64,
    kind: ChartType,
) -> Result<Vec<ChartRow>, EguiError> {
    let convert = |source: &[Bar]| {
        let converted = rows_of(source);
        if converted.dropped != 0 {
            return Err(EguiError::InvalidParam {
                what: "bars".to_owned(),
                reason: "conversion dropped rows; keys would misalign".to_owned(),
            });
        }
        Ok(window_for_range(&converted.rows, range, bar_seconds).to_vec())
    };
    match kind {
        ChartType::Candles | ChartType::Line | ChartType::Area => convert(bars),
        ChartType::Heikin => convert(&chart_price_bars(bars, ChartType::Heikin)?),
        unsupported => Err(EguiError::InvalidParam {
            what: "chart_type".to_owned(),
            reason: format!("{unsupported:?} has no terminal picture"),
        }),
    }
}

/// Area-fill view over one value series above a baseline: the upper edge is
/// the series, the lower edge repeats the baseline, so `ribbon_polygons`
/// closes one run per unbroken stretch with no new polygon code. Fill
/// colors come from caller-resolved colors at paint time, never the trend
/// key, so every run carries `false`. Mirrors `ribbon_view`'s alpha
/// discipline: alpha outside `[0, 1]` refuses.
pub fn area_view(
    values: &[Option<f64>],
    baseline: f64,
    alpha: f64,
) -> Result<RibbonView, EguiError> {
    if !alpha.is_finite() || alpha < 0.0 || alpha > 1.0 {
        return Err(EguiError::InvalidParam {
            what: "alpha".to_owned(),
            reason: "area fill alpha must sit in [0, 1]".to_owned(),
        });
    }
    Ok(RibbonView {
        upper: values.to_vec(),
        lower: vec![Some(baseline); values.len()],
        bullish: vec![false; values.len()],
        alpha,
    })
}

#[cfg(test)]
#[path = "../tests/chart.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/chart_types.rs"]
mod chart_types_tests;
