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
use protopyne_chart_model::{ChartRow, ChartType, DateRange, heikin_ashi, rows_of};
use protopyne_types::{Bar, Validate, check_for_execute, valid_output};

use crate::oscillators::parse_oscillator_kind;
use crate::topology::standard_plan;

fn default_timeframe() -> String {
    "M15".to_owned()
}

/// The terminal's default ticker symbol.
#[must_use]
pub fn default_symbol() -> String {
    DEFAULT_SYMBOL.to_owned()
}

/// Ticker symbol shown by default in the top-bar symbol dropdown.
pub const DEFAULT_SYMBOL: &str = "GLD";

/// Intraday-to-daily bar timeframe key behind the terminal toolbar.
///
/// Display-only selector: each key maps to bar spacing (for range-window
/// math), to the store's [`Timeframe`](protopyne_types::Timeframe) grid, and
/// to a history slot in the shell. Higher keys (`H1`, `H4`, `D1`) resample
/// the `M15` slot upward in the shell when no native slot was seeded — the
/// kernels always see plain closed bars, never a timeframe concept.
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
    /// Daily bars (resampled ×96 from `M15`).
    D1,
}

impl TimeframeKey {
    /// Every key in toolbar order.
    #[must_use]
    pub fn all() -> Vec<Self> {
        vec![Self::M1, Self::M5, Self::M15, Self::H1, Self::H4, Self::D1]
    }

    /// Short button label (`1M`, `5M`, `15M`, `1H`, `4H`, `1D`).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::M1 => "1M",
            Self::M5 => "5M",
            Self::M15 => "15M",
            Self::H1 => "1H",
            Self::H4 => "4H",
            Self::D1 => "1D",
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
            Self::D1 => 86_400,
        }
    }

    /// The store/kernel grid spacing behind this key: resolved through the
    /// one toolbar-label bridge
    /// ([`Timeframe::from_chart_label`](protopyne_types::Timeframe::from_chart_label)),
    /// so load-time folding and read planning cannot disagree about it.
    #[must_use]
    pub fn timeframe(self) -> protopyne_types::Timeframe {
        protopyne_types::Timeframe::from_chart_label(self.label())
            .expect("every toolbar label resolves on the store grid")
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
    /// Ticker symbol selected in the top bar.
    #[field(desc = "Ticker symbol selected in the top bar", max_len = 16)]
    #[builder(default = default_symbol())]
    #[serde(default = "default_symbol")]
    pub symbol: String,
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
        if self.symbol.trim().is_empty() {
            return Err(EguiError::InvalidParam {
                what: "symbol".to_owned(),
                reason: "symbol must not be empty".to_owned(),
            });
        }
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

/// Where one chart type's terminal picture comes from.
///
/// The single classification every dispatcher consumes: bar-backed pictures
/// render candle geometry from bars, rows-backed pictures render a series
/// from converted display rows. Anything outside the four wired kinds is
/// not a third source — it refuses through [`refuse_chart_type`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PictureSource {
    /// Candle geometry pictures from bars (`Candles`, `Heikin`).
    Bars,
    /// Series pictures from display rows (`Line`, `Area`).
    Rows,
}

/// The one `chart_type` refusal: names the parameter, never renders a
/// guess. Every dispatcher — the bars path, the rows path, and the demo's
/// paint match — shares this construction instead of keeping its own
/// wording in sync by hand.
#[must_use]
pub fn refuse_chart_type(kind: ChartType) -> EguiError {
    EguiError::InvalidParam {
        what: "chart_type".to_owned(),
        reason: format!("{kind:?} has no terminal picture"),
    }
}

/// Decide whether a `ChartType` has a terminal picture, and from what
/// source. Consumed by [`chart_price_bars`], [`picture_rows`], and the
/// demo's paint dispatch — no caller re-decides it.
pub fn picture_source(kind: ChartType) -> Result<PictureSource, EguiError> {
    match kind {
        ChartType::Candles | ChartType::Heikin => Ok(PictureSource::Bars),
        ChartType::Line | ChartType::Area => Ok(PictureSource::Rows),
        unsupported => Err(refuse_chart_type(unsupported)),
    }
}

/// Price bars behind the terminal's picture for one chart type: classic
/// candles pass bars through, Heikin-Ashi delegates to the headless
/// transform (no second smoothing math lives here). Rows-backed kinds
/// refuse with the rows reason — they picture, just not from bars — while
/// unwired kinds refuse through the shared decision above.
pub fn chart_price_bars(bars: &[Bar], kind: ChartType) -> Result<Vec<Bar>, EguiError> {
    match picture_source(kind)? {
        PictureSource::Bars => Ok(match kind {
            ChartType::Candles => bars.to_vec(),
            ChartType::Heikin => heikin_ashi(bars),
            // `Bars` resolves exactly for these two; anything else already
            // refused inside the decision. The arm stays so a future
            // bar-backed kind fails closed here instead of panicking.
            other => return Err(refuse_chart_type(other)),
        }),
        PictureSource::Rows => Err(EguiError::InvalidParam {
            what: "chart_type".to_owned(),
            reason: format!("{kind:?} renders from display rows, not bars"),
        }),
    }
}

/// Line series behind the terminal's line picture: the row closes, shaped
/// for the polyline emitter.
///
/// The `Option` is the paint edge's hole currency, kept so the result feeds
/// [`area_view`] and `oscillator_points` without re-wrapping: closes have
/// no holes, so every element is unconditionally `Some` here. Dropping the
/// marker would force both call sites to re-wrap per frame — the same two
/// allocations with more code — so the contract records the marker instead.
#[must_use]
pub fn line_values(rows: &[ChartRow]) -> Vec<Option<f64>> {
    rows.iter().map(|row| Some(row.close)).collect()
}

/// Validated area-fill input: the owned upper-edge series, the baseline it
/// closes against, and the fill strength.
///
/// Built once by [`area_view`] (which validates every leg) and closed into
/// polygons by the scaffolding's `area_polygons` — the lower edge is never
/// materialized as a per-bar vector because it repeats one baseline, and
/// the trend key is never materialized because an area fill carries none.
#[derive(Debug, Clone, PartialEq)]
pub struct AreaView {
    /// Per-bar upper edge with holes preserved; every present leg is finite.
    pub values: Vec<Option<f64>>,
    /// Fill foot; finite by construction.
    pub baseline: f64,
    /// Fill strength within `[0, 1]`.
    pub alpha: f64,
}

/// Area-fill view over one value series above a baseline: takes ownership
/// of the caller's series (no copy — the caller already owns and drops it)
/// and validates every leg, not just the fill strength. Holes stay `None`
/// and break polygon runs downstream; non-finite values, a non-finite
/// baseline, and an empty series refuse with the parameter named instead
/// of painting a guess.
pub fn area_view(
    values: Vec<Option<f64>>,
    baseline: f64,
    alpha: f64,
) -> Result<AreaView, EguiError> {
    if !alpha.is_finite() || alpha < 0.0 || alpha > 1.0 {
        return Err(EguiError::InvalidParam {
            what: "alpha".to_owned(),
            reason: "area fill alpha must sit in [0, 1]".to_owned(),
        });
    }
    if !baseline.is_finite() {
        return Err(EguiError::InvalidParam {
            what: "baseline".to_owned(),
            reason: "area fill baseline must be finite".to_owned(),
        });
    }
    if values.is_empty() {
        return Err(EguiError::InvalidParam {
            what: "values".to_owned(),
            reason: "area values must not be empty".to_owned(),
        });
    }
    if values.iter().any(|slot| matches!(slot, Some(value) if !value.is_finite())) {
        return Err(EguiError::InvalidParam {
            what: "values".to_owned(),
            reason: "area values must be finite where present; holes stay None".to_owned(),
        });
    }
    Ok(AreaView { values, baseline, alpha })
}

/// Rows painted for one chart type over a bar history: candles, line, and
/// area share the converted window rows; Heikin-Ashi converts over the full
/// history first (its opens recurse — windowing first would miscompute
/// every open), then windows. Refuses dropped conversions (downstream keys
/// would misalign) and unwired types with the parameter named. Which kinds
/// picture from bars is not re-decided here: every bar-backed kind converts
/// through [`chart_price_bars`], candles passing through untouched.
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
    match picture_source(kind)? {
        PictureSource::Rows => convert(bars),
        PictureSource::Bars => match kind {
            ChartType::Candles => convert(bars),
            // Heikin-Ashi and any future bar-backed kind convert over the
            // bars the single bars dispatcher produces.
            other => convert(&chart_price_bars(bars, other)?),
        },
    }
}

#[cfg(test)]
#[path = "../tests/chart.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/chart_types.rs"]
mod chart_types_tests;
