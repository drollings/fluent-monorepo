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
use protopyne_chart_model::ChartRow;
use protopyne_kernels::indicator::OwnedOhlcv;
use protopyne_kernels::smoothing::{SmoothingArgs, SmoothingFamily};
use protopyne_types::{Validate, check_for_execute, valid_output};

use fluent_egui::{
    EguiError, IndicatorStyle, ThemeConfig, Viewport, needs_column_aggregation,
    paint_oscillator_styled, proportional_line_points, sampled_line_points, translate_points,
    visible_rows,
};

/// The fixed SMA trio the reference plots (lengths are not parameterized).
const SMA_LENGTHS: [(&str, i64); 3] = [("SMA 50", 50), ("SMA 100", 100), ("SMA 200", 200)];

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

fn default_source() -> String {
    crate::topology::default_source_spelling()
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
    #[field(desc = "First MA smoothing", variants = "protopyne_kernels::smoothing::length_smoothing_ids")]
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
    #[field(desc = "Second MA smoothing", variants = "protopyne_kernels::smoothing::length_smoothing_ids")]
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
    #[field(desc = "Signal MA smoothing", variants = "protopyne_kernels::smoothing::length_smoothing_ids")]
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
    #[field(desc = "Mean MA smoothing", variants = "protopyne_kernels::smoothing::length_smoothing_ids")]
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
    #[field(desc = "Trend MA smoothing", variants = "protopyne_kernels::smoothing::length_smoothing_ids")]
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
    #[field(desc = "Base MA smoothing", variants = "protopyne_kernels::smoothing::length_smoothing_ids")]
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
    /// Input series: a price-source name (`close`, `hl2`, ...) or
    /// `series:<node>` naming a published line (`fast_mas:first`, ...).
    #[field(desc = "Input series (price source or series:<node>)")]
    #[builder(default = default_source())]
    pub source: String,
}

impl FastMasConfig {
    /// Fail-closed self-consistency check.
    pub fn validate(&self) -> Result<(), EguiError> {
        if crate::topology::SeriesSource::parse(&self.source).is_none() {
            return Err(EguiError::InvalidParam {
                what: "source".to_owned(),
                reason: format!(
                    "unknown input source {:?}: expected a price source (close, hl2, ...) or series:<node>",
                    self.source
                ),
            });
        }
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
            if !is_supported_arm(kind) {
                return Err(EguiError::InvalidParam {
                    what: format!("{label}_type"),
                    reason: format!(
                        "smoothing {kind:?} is not in the smoothing catalog (free-text names outside the catalog no longer validate)"
                    ),
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

/// Whether a smoothing name is a length-driven catalog arm: every family but
/// `Eit`, whose alpha length-only arms cannot set.
fn is_supported_arm(kind: &str) -> bool {
    matches!(SmoothingFamily::parse(kind), Some(family) if family.takes_length())
}

/// Evaluate one arm into a line through the smoothing catalog, reporting
/// the arm's own warmup alongside the values so the caller can compose
/// holes along the producer path.
fn eval_line(
    name: &str,
    kind: &str,
    length: i64,
    style: &str,
    frame: &OwnedOhlcv,
) -> Result<(FastMasLine, usize), EguiError> {
    let family = SmoothingFamily::parse(kind).ok_or_else(|| EguiError::InvalidParam {
        what: "ma_type".to_owned(),
        reason: format!(
            "smoothing {kind:?} is not in the smoothing catalog (free-text names outside the catalog no longer validate)"
        ),
    })?;
    if !family.takes_length() {
        return Err(EguiError::InvalidParam {
            what: "ma_type".to_owned(),
            reason: format!("{kind:?} needs an alpha parameter length-only arms cannot set"),
        });
    }
    let warmup = family
        .warmup_for_length(length)
        .map_err(|error| crate::topology::describe_kernel(&error))?;
    let values = family
        .evaluate(frame, &SmoothingArgs::length(length))
        .map_err(|error| EguiError::Dependency { reason: error.to_string() })?;
    Ok((FastMasLine { name: name.to_owned(), values, style: style.to_owned() }, warmup))
}

/// Extend a line's leading holes over the composed warmup: the producer's
/// leading holes plus the arm's own. With no producer there is nothing to
/// extend — the kernels' native holes stand, so bar-sourced output is
/// byte-identical to the pre-series evaluation.
fn extend_holes(line: &mut FastMasLine, producer_warmup: usize, own_warmup: usize) {
    if producer_warmup == 0 {
        return;
    }
    let holes = (producer_warmup + own_warmup).min(line.values.len());
    for value in line.values.iter_mut().take(holes) {
        *value = None;
    }
}

/// Evaluate every enabled average over the configured input.
///
/// Each arm evaluates through the smoothing catalog (pass-through, kernel, or
/// recipe), so every length-driven `factorySmoothing` family reads one truth.
/// The input is the configured [`SeriesSource`](crate::topology::SeriesSource):
/// raw bars by default, or a published series resolved from `series`.
/// Refuses a config its own validation refuses. Every output line aligns with
/// `rows` index-for-index, warmup holes preserved and composed.
pub fn evaluate_fast_mas(
    rows: &[ChartRow],
    config: &FastMasConfig,
    series: &crate::topology::SeriesCatalog,
) -> Result<FastMasView, EguiError> {
    config.validate()?;
    let source = crate::topology::SeriesSource::parse(&config.source).ok_or_else(|| {
        EguiError::InvalidParam {
            what: "source".to_owned(),
            reason: format!("unknown input source {:?}", config.source),
        }
    })?;
    // Bar inputs read the shared source frame (byte-identical to the
    // pre-series evaluation); series inputs resolve from the catalog.
    let (frame, producer) = match &source {
        crate::topology::SeriesSource::Bars(price) => {
            (crate::topology::source_frame(rows, *price), 0)
        }
        crate::topology::SeriesSource::Series(node) => {
            let values = series.resolve(node).ok_or_else(|| EguiError::InvalidParam {
                what: "series".to_owned(),
                reason: format!("unknown input series {node:?}"),
            })?;
            let warmup = series.leading_holes(node).unwrap_or(0);
            (crate::topology::series_frame(rows, values)?, warmup)
        }
    };
    let mut lines = Vec::new();
    let mut push = |name: &str, kind: &str, length: i64, style: &str| -> Result<(), EguiError> {
        let (mut line, warmup) = eval_line(name, kind, length, style, &frame)?;
        extend_holes(&mut line, producer, warmup);
        lines.push(line);
        Ok(())
    };
    if config.show_first_ma {
        push("First MA", &config.first_ma_type, config.first_ma_length, &config.first_ma_style)?;
    }
    if config.show_second_ma {
        push(
            "Second MA",
            &config.second_ma_type,
            config.second_ma_length,
            &config.second_ma_style,
        )?;
    }
    if config.show_extra_mas {
        push("Signal MA", &config.signal_ma_type, config.signal_ma_length, &config.signal_ma_style)?;
        push("Mean MA", &config.mean_ma_type, config.mean_ma_length, &config.mean_ma_style)?;
        push("Trend MA", &config.trend_ma_type, config.trend_ma_length, &config.trend_ma_style)?;
        push("Base MA", &config.base_ma_type, config.base_ma_length, &config.base_ma_style)?;
    }
    if config.show_smas {
        for (name, length) in SMA_LENGTHS {
            push(name, "sma", length, &config.sma_style)?;
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
/// themed. `base` is the global bar index of the view's first value, so the
/// trailing window borrows through the shared slicer and column grouping lands
/// on screen pixels; `width_px` bounds the native branch to the twice-visible
/// window (display-only cull, same pixels fewer shapes): at sub-pixel zoom each
/// line samples per column run through the shared display sampler
/// (display-only, values untouched); at or above one pixel per bar the native
/// mapping runs over the twice-visible borrow only.
pub fn paint_fast_mas(
    painter: &egui::Painter,
    view: &FastMasView,
    viewport: &Viewport,
    origin: egui::Pos2,
    base: usize,
    width_px: f32,
    theme: &ThemeConfig,
    style: &IndicatorStyle,
) -> Result<(), EguiError> {
    let aggregate = needs_column_aggregation(viewport.scale().bar_spacing());
    for line in &view.lines {
        let points = if aggregate {
            sampled_line_points(&line.values, viewport, base)
        } else {
            let len = line.values.len().saturating_sub(base);
            let window = visible_rows(line.values.as_slice(), base, len);
            proportional_line_points(window, viewport, width_px)
        };
        let placed = translate_points(points, (origin.x, origin.y));
        paint_oscillator_styled(painter, &placed, &line.style, theme, style, 1.0)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/fast_mas.rs"]
mod tests;
