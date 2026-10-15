//! "Fast MAs": a decoupled moving-average overlay module.
//!
//! [`evaluate_fast_mas`] turns converted [`ChartRow`]s into one
//! [`FastMasLine`] per enabled average by delegating every arm to the shared
//! [`builtin_registry`] — no moving-average formula is re-implemented here.
//! The module owns its own [`FastMasConfig`] `Component` (the single schema
//! behind its settings pane) and shares nothing with the IntegrationLite
//! pane, so the two indicators configure, evaluate, and cache independently.
//!
//! Display provenance is fenced: close and volume legs enter as
//! [`DisplayOnly`] and are revealed only to build the kernel preview frame —
//! the auditable escape, and nothing here converts toward a decision type.

use fluent_wvr::prelude::*;
use fluent_wvr::{Describable, FieldAccess};
use protopyne_chart_model::{ChartRow, DisplayOnly};
use protopyne_kernels::indicator::{OwnedOhlcv, ParamSet};
use protopyne_types::{Validate, check_for_execute, valid_output};

use crate::topology::kernel_series;

use fluent_egui::{
    EguiError, IndicatorStyle, ThemeConfig, Viewport, oscillator_points, paint_oscillator_styled,
    translate_points,
};

/// Moving-average kernels the module accepts as a line's smoothing.
///
/// These are exactly the smoothing families the reference `factorySmoothing`
/// dispatches; the check keeps an unsupported name from reaching the kernel
/// registry and failing later with a less specific reason.
pub const SUPPORTED_MA_KINDS: &[&str] = &["sma", "ema", "wma", "hma", "rma", "vwma", "alma"];

/// ALMA offset applied when an arm selects `alma` (reference default).
pub const ALMA_OFFSET: f64 = 0.85;
/// ALMA sigma applied when an arm selects `alma` (reference default).
pub const ALMA_SIGMA: f64 = 6.0;

/// The fixed SMA trio the reference plots (lengths are not parameterized).
const SMA_LENGTHS: [(&str, i64); 3] = [("SMA 50", 50), ("SMA 100", 100), ("SMA 200", 200)];

/// Build the fenced close/volume frame the MA kernels evaluate over.
///
/// Every leg enters as [`DisplayOnly`] and is revealed exactly here — the
/// auditable escape for a display-descended preview frame. `close` is
/// authoritative for length and fills the unread OHLC legs; `volume` carries
/// the real per-bar volume so `vwma` weights with data rather than zeros.
fn close_frame(rows: &[ChartRow]) -> OwnedOhlcv {
    let close: Vec<f64> =
        rows.iter().map(|row| DisplayOnly::new(row.close).reveal()).collect();
    let volume: Vec<f64> =
        rows.iter().map(|row| DisplayOnly::new(row.volume as f64).reveal()).collect();
    OwnedOhlcv {
        open: close.clone(),
        high: close.clone(),
        low: close.clone(),
        close,
        volume,
    }
}

/// Parameter set for one smoothing arm; `alma` carries its reference offset
/// and sigma, every other arm reads only `length`.
fn ma_params(kind: &str, length: i64) -> Result<ParamSet, EguiError> {
    if !SUPPORTED_MA_KINDS.contains(&kind) {
        return Err(EguiError::InvalidParam {
            what: "ma_type".to_owned(),
            reason: format!("unsupported smoothing {kind:?}"),
        });
    }
    let params = ParamSet::new().with_int("length", length);
    if kind == "alma" {
        Ok(params.with_float("offset", ALMA_OFFSET).with_float("sigma", ALMA_SIGMA))
    } else {
        Ok(params)
    }
}

/// One evaluated moving-average line: its name, per-bar display values with
/// warmup holes preserved, and the theme style key resolved at paint time.
#[derive(Debug, Clone, PartialEq)]
pub struct FastMasLine {
    /// Display name (for example `"First MA"`, `"SMA 50"`).
    pub name: String,
    /// Per-bar values aligned with the source rows, holes preserved.
    pub values: Vec<Option<f64>>,
    /// Theme style key resolved against the theme config at paint time.
    pub style: String,
}

/// The evaluated overlay: one line per enabled average, in plot order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FastMasView {
    /// Enabled lines, in plot order.
    pub lines: Vec<FastMasLine>,
}

fn default_first_ma_type() -> String {
    "wma".to_owned()
}

fn default_first_ma_length() -> i64 {
    3
}

fn default_second_ma_type() -> String {
    "alma".to_owned()
}

fn default_second_ma_length() -> i64 {
    8
}

fn default_ema_type() -> String {
    "ema".to_owned()
}

fn default_signal_ma_length() -> i64 {
    8
}

fn default_mean_ma_length() -> i64 {
    21
}

fn default_trend_ma_length() -> i64 {
    34
}

fn default_base_ma_length() -> i64 {
    55
}

fn default_line_style() -> String {
    "slow".to_owned()
}

fn default_accent_style() -> String {
    "fast".to_owned()
}

fn default_sma_style() -> String {
    "bull_down".to_owned()
}

/// One Fast MAs overlay's configuration: the single schema-driven control
/// truth behind its settings pane.
///
/// Every field is a `Component` field, so the settings modal renders from the
/// `describe()` schema and a new knob appears without a UI edit. The
/// `*_ma_type` / `*_ma_length` fields route to the Inputs tab, the
/// `*_style` theme keys to Style, and the `show_*` toggles to Visibility.
///
/// The reference exposes four independent visibility toggles (first, second,
/// the EMA group, and the SMA trio); splitting them into sub-structs would
/// fragment the one settings schema the modal renders from.
#[allow(clippy::struct_excessive_bools)]
#[derive(
    Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize, FieldAccess, Describable
)]
#[builder(start_fn = new)]
pub struct FastMasConfig {
    /// Smoothing family for the first average.
    #[field(desc = "First MA smoothing")]
    #[builder(default = default_first_ma_type())]
    pub first_ma_type: String,
    /// Length of the first average.
    #[field(desc = "First MA length")]
    #[builder(default = default_first_ma_length())]
    pub first_ma_length: i64,
    /// Theme style key for the first average.
    #[field(desc = "First MA theme style key")]
    #[builder(default = default_line_style())]
    pub first_ma_style: String,
    /// Smoothing family for the second average.
    #[field(desc = "Second MA smoothing")]
    #[builder(default = default_second_ma_type())]
    pub second_ma_type: String,
    /// Length of the second average.
    #[field(desc = "Second MA length")]
    #[builder(default = default_second_ma_length())]
    pub second_ma_length: i64,
    /// Theme style key for the second average.
    #[field(desc = "Second MA theme style key")]
    #[builder(default = default_accent_style())]
    pub second_ma_style: String,
    /// Smoothing family for the signal average.
    #[field(desc = "Signal MA smoothing")]
    #[builder(default = default_ema_type())]
    pub signal_ma_type: String,
    /// Length of the signal average.
    #[field(desc = "Signal MA length")]
    #[builder(default = default_signal_ma_length())]
    pub signal_ma_length: i64,
    /// Theme style key for the signal average.
    #[field(desc = "Signal MA theme style key")]
    #[builder(default = default_accent_style())]
    pub signal_ma_style: String,
    /// Smoothing family for the mean average.
    #[field(desc = "Mean MA smoothing")]
    #[builder(default = default_ema_type())]
    pub mean_ma_type: String,
    /// Length of the mean average.
    #[field(desc = "Mean MA length")]
    #[builder(default = default_mean_ma_length())]
    pub mean_ma_length: i64,
    /// Theme style key for the mean average.
    #[field(desc = "Mean MA theme style key")]
    #[builder(default = default_line_style())]
    pub mean_ma_style: String,
    /// Smoothing family for the trend average.
    #[field(desc = "Trend MA smoothing")]
    #[builder(default = default_ema_type())]
    pub trend_ma_type: String,
    /// Length of the trend average.
    #[field(desc = "Trend MA length")]
    #[builder(default = default_trend_ma_length())]
    pub trend_ma_length: i64,
    /// Theme style key for the trend average.
    #[field(desc = "Trend MA theme style key")]
    #[builder(default = default_line_style())]
    pub trend_ma_style: String,
    /// Smoothing family for the base average.
    #[field(desc = "Base MA smoothing")]
    #[builder(default = default_ema_type())]
    pub base_ma_type: String,
    /// Length of the base average.
    #[field(desc = "Base MA length")]
    #[builder(default = default_base_ma_length())]
    pub base_ma_length: i64,
    /// Theme style key for the base average.
    #[field(desc = "Base MA theme style key")]
    #[builder(default = default_line_style())]
    pub base_ma_style: String,
    /// Theme style key for the SMA trio.
    #[field(desc = "SMA theme style key")]
    #[builder(default = default_sma_style())]
    pub sma_style: String,
    /// Whether the first average paints.
    #[field(desc = "Show the first average")]
    #[builder(default = true)]
    pub show_first_ma: bool,
    /// Whether the second average paints.
    #[field(desc = "Show the second average")]
    #[builder(default = false)]
    pub show_second_ma: bool,
    /// Whether the signal/mean/trend/base averages paint.
    #[field(desc = "Show the signal, mean, trend, and base averages")]
    #[builder(default = true)]
    pub show_extra_mas: bool,
    /// Whether the SMA 50/100/200 trio paints.
    #[field(desc = "Show the SMA trio")]
    #[builder(default = false)]
    pub show_smas: bool,
}

impl FastMasConfig {
    /// Fail-closed self-consistency check.
    pub fn validate(&self) -> Result<(), EguiError> {
        for (label, kind, length) in [
            ("first_ma", &self.first_ma_type, self.first_ma_length),
            ("second_ma", &self.second_ma_type, self.second_ma_length),
            ("signal_ma", &self.signal_ma_type, self.signal_ma_length),
            ("mean_ma", &self.mean_ma_type, self.mean_ma_length),
            ("trend_ma", &self.trend_ma_type, self.trend_ma_length),
            ("base_ma", &self.base_ma_type, self.base_ma_length),
        ] {
            if length <= 0 {
                return Err(EguiError::InvalidParam {
                    what: format!("{label}_length"),
                    reason: "moving-average length must be positive".to_owned(),
                });
            }
            if !SUPPORTED_MA_KINDS.contains(&kind.as_str()) {
                return Err(EguiError::InvalidParam {
                    what: format!("{label}_type"),
                    reason: format!("unsupported smoothing {kind:?}"),
                });
            }
        }
        for (label, style) in [
            ("first_ma_style", &self.first_ma_style),
            ("second_ma_style", &self.second_ma_style),
            ("signal_ma_style", &self.signal_ma_style),
            ("mean_ma_style", &self.mean_ma_style),
            ("trend_ma_style", &self.trend_ma_style),
            ("base_ma_style", &self.base_ma_style),
            ("sma_style", &self.sma_style),
        ] {
            if style.trim().is_empty() {
                return Err(EguiError::InvalidParam {
                    what: label.to_owned(),
                    reason: "style key must not be empty".to_owned(),
                });
            }
        }
        Ok(())
    }
}

impl Default for FastMasConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

impl Validate for FastMasConfig {
    type Err = EguiError;
    fn validate(&self) -> Result<(), Self::Err> {
        FastMasConfig::validate(self)
    }
}

impl WorkUnit for FastMasConfig {
    fn name(&self) -> &str {
        "fast_mas_config"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        check_for_execute(self, "fast mas config invalid")?;
        Ok(valid_output(format!("fast mas ready (first {})", self.first_ma_type)))
    }
}

impl_component!(FastMasConfig);

/// Evaluate one arm into a line, delegating to the shared kernel registry.
fn eval_line(
    name: &str,
    kind: &str,
    length: i64,
    style: &str,
    frame: &OwnedOhlcv,
) -> Result<FastMasLine, EguiError> {
    Ok(FastMasLine {
        name: name.to_owned(),
        values: kernel_series(kind, &ma_params(kind, length)?, frame)?,
        style: style.to_owned(),
    })
}

/// Evaluate every enabled average over converted rows.
///
/// Delegates each arm to the shared [`builtin_registry`], so `wma`, `alma`,
/// `ema`, `sma`, `hma`, `rma`, and `vwma` all read the kernel truth. Refuses a
/// config its own validation refuses. Every output line aligns with `rows`
/// index-for-index, warmup holes preserved.
pub fn evaluate_fast_mas(
    rows: &[ChartRow],
    config: &FastMasConfig,
) -> Result<FastMasView, EguiError> {
    config.validate()?;
    let frame = close_frame(rows);
    let mut lines = Vec::new();
    if config.show_first_ma {
        lines.push(eval_line(
            "First MA",
            &config.first_ma_type,
            config.first_ma_length,
            &config.first_ma_style,
            &frame,
        )?);
    }
    if config.show_second_ma {
        lines.push(eval_line(
            "Second MA",
            &config.second_ma_type,
            config.second_ma_length,
            &config.second_ma_style,
            &frame,
        )?);
    }
    if config.show_extra_mas {
        lines.push(eval_line(
            "Signal MA",
            &config.signal_ma_type,
            config.signal_ma_length,
            &config.signal_ma_style,
            &frame,
        )?);
        lines.push(eval_line(
            "Mean MA",
            &config.mean_ma_type,
            config.mean_ma_length,
            &config.mean_ma_style,
            &frame,
        )?);
        lines.push(eval_line(
            "Trend MA",
            &config.trend_ma_type,
            config.trend_ma_length,
            &config.trend_ma_style,
            &frame,
        )?);
        lines.push(eval_line(
            "Base MA",
            &config.base_ma_type,
            config.base_ma_length,
            &config.base_ma_style,
            &frame,
        )?);
    }
    if config.show_smas {
        for (name, length) in SMA_LENGTHS {
            lines.push(eval_line(name, "sma", length, &config.sma_style, &frame)?);
        }
    }
    Ok(FastMasView { lines })
}

/// Blit the overlay: map each line's values through the pane viewport, shift
/// by the pane origin, and stroke it under its own theme style key.
///
/// Geometry is pane-local ([`oscillator_points`] maps against the pane's own
/// viewport), so the caller shifts the whole batch by its pane rect's origin.
/// Holes stay holes; an unknown or malformed style key refuses by name.
///
/// `style` is the owning slot's [`IndicatorStyle`]: each line paints under its
/// own key, so one Style tab can restyle a single arm and leave the rest
/// themed.
pub fn paint_fast_mas(
    painter: &egui::Painter,
    view: &FastMasView,
    viewport: &Viewport,
    origin: egui::Pos2,
    theme: &ThemeConfig,
    style: &IndicatorStyle,
) -> Result<(), EguiError> {
    for line in &view.lines {
        let points = oscillator_points(&line.values, viewport);
        let placed = translate_points(points, (origin.x, origin.y));
        paint_oscillator_styled(painter, &placed, &line.style, theme, style, 1.0)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/fast_mas.rs"]
mod tests;
