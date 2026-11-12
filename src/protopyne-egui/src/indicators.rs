//! Indicator plugin system: the searchable indicator catalog and the
//! [`ChartIndicator`] contract every hot-slotted indicator implements.
//!
//! One catalog serves the Indicator Search & Management modal (My Scripts,
//! Technical, Financial) over the shared [`CatalogModal`]. One plugin trait
//! serves every indicator kind — the two scripted modules
//! ([`IntegrationLiteIndicator`], [`FastMasIndicator`]) and
//! [`KernelIndicator`], which hosts any kernel from the shared
//! [`shared_registry`]. A plugin is stateless behavior: it owns no
//! configuration, so the hot-slot stack stays the single settings authority
//! and an edit and its recompute can never diverge. Every plugin reads its
//! configuration through [`ChartIndicator::evaluate`], routing display legs
//! through the shared fenced kernel frame in `topology`.
//!
//! Indicator isolation is structural: each plugin exposes its own
//! [`default_config`](ChartIndicator::default_config) schema and its own
//! pane-plan node namespace, so two plugins never share a field, a
//! fingerprint, or a cache key.

use std::collections::BTreeMap;

use fluent_wvr::component_downcast_ref;
use fluent_wvr::prelude::*;
use protopyne_chart_model::ChartRow;
use protopyne_kernels::pine::manifest::{IndicatorManifestEntry, ManifestParity};
use protopyne_kernels::pine::projection::{PlotPrimitive, ProjectionRefusal};
use protopyne_kernels::shared_registry;
use protopyne_kernels::smoothing::{SmoothingArgs, SmoothingFamily, MA_PERIOD_MAX, MA_PERIOD_MIN};

use fluent_wvr::validate::{check_for_execute, valid_output, Validate};

use fluent_egui::{
    cache_key, needs_column_aggregation, paint_oscillator_styled, proportional_line_points,
    sampled_line_points, translate_points, visible_rows, CatalogModal, EguiError, IndicatorStyle,
    ReadoutValue, SlotAttachment, ThemeConfig, Viewport,
};

use crate::delta_volume_profile::{DeltaVolumeProfileIndicator, DELTA_VOLUME_PROFILE_ID};
use crate::fast_mas::{evaluate_fast_mas, FastMasConfig};
use crate::integration_lite::{
    integration_lite_sub_pane, oscillator_transparency, IntegrationLiteConfig,
};
use crate::oscillator_panel::{panel_kind_for_kernel, KindDefaults};
use crate::oscillators::{
    display_mapped_series, oscillator_mapping_for_output, OscParams, OscillatorKind, WhichLeg,
};
use crate::topology::{
    consumer_cache_key, describe_kernel, kernel_result, kernel_warmup, resolve_input_column,
    series_frame, SeriesCatalog, SeriesSource,
};

/// Stable id of the IntegrationLite script indicator.
const INTEGRATION_LITE_ID: &str = "integration_lite";
/// Stable id of the Fast MAs script indicator.
const FAST_MAS_ID: &str = "fast_mas";
/// Stable id of the market-regime monitor.
const REGIME_ID: &str = "regime";
/// Stable id of the MA-health monitor.
const HEALTH_ID: &str = "health";

/// The four bounded sub-pane oscillator nodes IntegrationLite registers.
const INTEGRATION_LITE_NODES: [&str; 4] = [
    "integration_lite:fast",
    "integration_lite:slow",
    "integration_lite:ribbon_health",
    "integration_lite:ma_health",
];

/// One catalog tab: the two selector tabs plus the three entry categories.
///
/// `All` and `Favorites` are selectors, not entry categories — the generic
/// catalog renders its own All tab and Favorites checkbox, so
/// [`entry_category`](Self::entry_category) maps them to `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// One catalog tab: the two selector tabs plus the three entry categories.
/// 
/// `All` and `Favorites` are selectors, not entry categories — the generic
/// catalog renders its own All tab and Favorites checkbox, so
/// [`entry_category`](Self::entry_category) maps them to `None`.
pub enum IndicatorCategory {
    /// Every entry, regardless of category.
    All,
    /// Starred entries only.
    Favorites,
    /// The shipped script indicators.
    MyScripts,
    /// The builtin technical-analysis kernels.
    Technical,
    /// The domain regime and health monitors.
    Financial,
}

impl IndicatorCategory {
    /// Every tab, in render order.
    #[must_use]
    pub fn all() -> [Self; 5] {
        [
            Self::All,
            Self::Favorites,
            Self::MyScripts,
            Self::Technical,
            Self::Financial,
        ]
    }

    /// The tab label the modal renders.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Favorites => "Favorites",
            Self::MyScripts => "My Scripts",
            Self::Technical => "Technical",
            Self::Financial => "Financial",
        }
    }

    /// The entry category this tab selects, if it is a real category.
    #[must_use]
    pub fn entry_category(self) -> Option<Self> {
        match self {
            Self::All | Self::Favorites => None,
            other => Some(other),
        }
    }
}

/// One evaluated series a plugin produces, bound to a pane-plan node.
#[derive(Debug, Clone, PartialEq)]
/// One evaluated series a plugin produces, bound to a pane-plan node.
pub struct IndicatorSeries {
    /// Pane-plan node this series computes (cache-key scope).
    pub node: String,
    /// Display name shown in readouts and legends.
    pub name: String,
    /// Per-bar raw values, holes preserved. Published under the node and
    /// key; never normalized, never substituted.
    pub values: Vec<Option<f64>>,
    /// Precomputed display values for painters and readouts, holes
    /// preserved. Mapped oscillators carry the catalog squash of the raw
    /// leg constrained into the extremes band; everything else mirrors the
    /// raw values.
    pub display_values: Vec<Option<f64>>,
    /// Display provenance: the catalog mapping behind the display values,
    /// or none for raw-passthrough series.
    pub display_mapping: Option<(OscillatorKind, WhichLeg)>,
    /// Theme style key resolved against the theme at paint time.
    pub style: String,
    /// Where this series paints.
    pub attachment: SlotAttachment,
    /// Cache key binding the node to its parameter fingerprint.
    pub key: u64,
    /// Pane identity; `None` keeps the shared default sub-pane.
    pub pane: Option<String>,
}

impl IndicatorSeries {
    /// Pane identity, falling back to `default`.
    #[must_use]
    pub fn pane_id<'a>(&'a self, default: &'a str) -> &'a str {
        self.pane.as_deref().unwrap_or(default)
    }

    /// The most recent display value, holes skipped. Readouts and legends
    /// read display, never raw.
    #[must_use]
    pub fn last_value(&self) -> Option<f64> {
        self.display_values.iter().rev().find_map(|slot| *slot)
    }

    /// The most recent raw value, holes skipped. Decision paths and
    /// `series:<node>` consumers read raw, never display.
    #[must_use]
    pub fn last_raw_value(&self) -> Option<f64> {
        self.values.iter().rev().find_map(|slot| *slot)
    }
}

/// One plugin evaluation: the series produced, in plan-node order, plus any
/// projected non-series visuals (Pine drawings, markers, levels) for the
/// pane to paint beside the series. Native plugins always carry an empty
/// `Ok`; a Pine `Err` carries the projection refusal the pane renders as an
/// "unsupported" chip instead of silent absence.
#[derive(Debug, Clone, PartialEq)]
/// One plugin evaluation: the series produced, in plan-node order, plus any
/// projected non-series visuals (Pine drawings, markers, levels) for the
/// pane to paint beside the series. Native plugins always carry an empty
/// `Ok`; a Pine `Err` carries the projection refusal the pane renders as an
/// "unsupported" chip instead of silent absence.
pub struct IndicatorEvaluation {
    /// Series produced by the evaluation.
    pub series: Vec<IndicatorSeries>,
    /// Projected non-series visuals, or the refusal that blocked them.
    pub overlay: Result<Vec<PlotPrimitive>, ProjectionRefusal>,
}

/// The indicator plugin contract: identity, default configuration, the
/// pane-plan nodes it registers, evaluation, readouts, and paint hooks.
///
/// A plugin owns no configuration. The hot-slot stack owns the settings
/// (one [`Component`] per slot), and every method that depends on settings
/// takes that config by reference, so the settings modal and the recompute
/// read the exact same values. `default_config` supplies the seed schema the
/// slot is inserted with.
pub trait ChartIndicator: Send + Sync {
    /// Stable plugin id (also the slot id).
    fn id(&self) -> &str;

    /// Display title rendered on the slot card.
    fn title(&self) -> &str;

    /// Where the plugin's series paint.
    fn attachment(&self) -> SlotAttachment;

    /// A fresh default configuration: the schema the settings pane renders.
    fn default_config(&self) -> Box<dyn Component>;

    /// The pane-plan node names this plugin registers for `config`.
    fn plan_nodes(&self, config: &dyn Component) -> Result<Vec<String>, EguiError>;

    /// Fingerprint of the live `config`, over the same serialized fields
    /// `evaluate` folds into its series keys — so a gate holding this key
    /// can never serve stale evaluations after a config edit.
    fn config_key(&self, config: &dyn Component) -> Result<u64, EguiError>;

    /// The published nodes this plugin reads for `config`: its
    /// [`SeriesSource`] edge, empty for bar-reading plugins. The workspace
    /// registers these as plan dependencies, so evaluation orders producer
    /// before consumer.
    fn plan_deps(&self, config: &dyn Component) -> Result<Vec<String>, EguiError> {
        let _ = config;
        Ok(Vec::new())
    }

    /// Evaluate `config` over `rows`, one series per plan node. A consumer
    /// resolves its [`SeriesSource`] input from `series` (published by
    /// earlier evaluations in plan order); bar readers ignore it.
    fn evaluate(
        &self,
        rows: &[ChartRow],
        config: &dyn Component,
        series: &SeriesCatalog,
    ) -> Result<IndicatorEvaluation, EguiError>;

    /// Live card readouts derived from an evaluation (last resolved value per
    /// series).
    fn readouts(&self, evaluation: &IndicatorEvaluation) -> Vec<ReadoutValue> {
        evaluation
            .series
            .iter()
            .map(|series| ReadoutValue {
                label: series.name.clone(),
                value: series.last_value(),
            })
            .collect()
    }

    /// Paint a main-overlay attachment (default: one polyline per series).
    fn paint_main_overlay(
        &self,
        painter: &egui::Painter,
        evaluation: &IndicatorEvaluation,
        viewport: &Viewport,
        origin: egui::Pos2,
        base: usize,
        width_px: f32,
        theme: &ThemeConfig,
        _config: &dyn Component,
        style: &IndicatorStyle,
    ) -> Result<(), EguiError> {
        paint_series(
            painter, evaluation, viewport, origin, base, width_px, theme, style,
        )
    }

    /// Paint a sub-pane attachment (default: one polyline per series).
    fn paint_sub_pane(
        &self,
        painter: &egui::Painter,
        evaluation: &IndicatorEvaluation,
        viewport: &Viewport,
        origin: egui::Pos2,
        base: usize,
        width_px: f32,
        theme: &ThemeConfig,
        _config: &dyn Component,
        style: &IndicatorStyle,
    ) -> Result<(), EguiError> {
        paint_series(
            painter, evaluation, viewport, origin, base, width_px, theme, style,
        )
    }
}

/// One polyline per series, shifted by the pane origin, under each series'
/// own style key at the slot's stroke width.
///
/// `style` is the owning slot's [`IndicatorStyle`], so a plugin never
/// restyles itself: the Style tab in its settings pane is the only thing that
/// decides a color override or a line width here. `base` is the global bar
/// index of the trailing window's first value within the full-history
/// evaluation, so the window borrows through the shared slicer (no per-frame
/// copy) and column grouping lands on screen pixels. `width_px` bounds the
/// native branch to the twice-visible window (display-only cull, same pixels
/// fewer shapes); at sub-pixel zoom the windowed series sample per column run
/// (display-only sampling over native-timeframe values).
pub(crate) fn paint_series(
    painter: &egui::Painter,
    evaluation: &IndicatorEvaluation,
    viewport: &Viewport,
    origin: egui::Pos2,
    base: usize,
    width_px: f32,
    theme: &ThemeConfig,
    style: &IndicatorStyle,
) -> Result<(), EguiError> {
    let aggregate = needs_column_aggregation(viewport.scale().bar_spacing());
    for series in &evaluation.series {
        let len = series.display_values.len().saturating_sub(base);
        let window = visible_rows(&series.display_values, base, len);
        let points = if aggregate {
            sampled_line_points(window, viewport, base)
        } else {
            proportional_line_points(window, viewport, width_px)
        };
        let placed = translate_points(points, (origin.x, origin.y));
        paint_oscillator_styled(painter, &placed, &series.style, theme, style, 1.0)?;
    }
    Ok(())
}

/// A config whose concrete type does not match the plugin refuses by name.
pub(crate) fn wrong_config(id: &str, expected: &str) -> EguiError {
    EguiError::InvalidParam {
        what: "config".to_owned(),
        reason: format!("{id} expects a {expected} configuration"),
    }
}

/// A stable fingerprint of a configuration, over its serialized fields, so a
/// cache key follows every edit (including string-valued fields). Thin
/// domain name over the shared JSON framer: same bytes, one implementation.
fn config_fingerprint<T: serde::Serialize>(config: &T) -> u64 {
    common_core::hash::fingerprint_json(config)
}

/// The plan edge a `source` field declares: the published node, or nothing
/// for bar inputs (and for unparsable spellings — validation refuses those
/// later with the real reason).
fn source_edge(source: &str) -> Vec<String> {
    SeriesSource::parse(source)
        .and_then(|parsed| parsed.series_node().map(str::to_owned))
        .into_iter()
        .collect()
}

/// The producer series key a consumer folds into its own key, if its source
/// names a published node that the catalog holds.
fn producer_series_key(source: &str, series: &SeriesCatalog) -> Option<u64> {
    SeriesSource::parse(source)
        .and_then(|parsed| parsed.series_node().map(str::to_owned))
        .and_then(|node| series.series_key(&node))
}

/// A series cache key: the plain node fingerprint for bar inputs, the
/// producer-composed key for series inputs (so the default-source pin —
/// byte-identical keys for `Bars(Close)` — holds by construction).
fn series_key_for(node: &str, fingerprint: u64, producer_key: Option<u64>) -> u64 {
    match producer_key {
        Some(producer) => consumer_cache_key(node, fingerprint, producer),
        None => cache_key(node, fingerprint),
    }
}

/// The IntegrationLite plugin: the four-readout bounded oscillator sub-pane.
pub struct IntegrationLiteIndicator;

impl IntegrationLiteIndicator {
    /// Create the plugin.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for IntegrationLiteIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl ChartIndicator for IntegrationLiteIndicator {
    fn id(&self) -> &str {
        INTEGRATION_LITE_ID
    }

    fn title(&self) -> &str {
        "Integration Lite"
    }

    fn attachment(&self) -> SlotAttachment {
        SlotAttachment::SubPane
    }

    fn default_config(&self) -> Box<dyn Component> {
        Box::new(IntegrationLiteConfig::default())
    }

    fn plan_nodes(&self, _config: &dyn Component) -> Result<Vec<String>, EguiError> {
        Ok(INTEGRATION_LITE_NODES
            .iter()
            .map(|node| (*node).to_owned())
            .collect())
    }

    fn config_key(&self, config: &dyn Component) -> Result<u64, EguiError> {
        let config = component_downcast_ref::<IntegrationLiteConfig>(config)
            .ok_or_else(|| wrong_config(self.id(), "IntegrationLite"))?;
        // Value identity, not whole-document identity: paint-only dials
        // keep the plan fingerprint still, exactly when the evaluations
        // would not move.
        Ok(config.evaluation_key())
    }

    fn evaluate(
        &self,
        rows: &[ChartRow],
        config: &dyn Component,
        _series: &SeriesCatalog,
    ) -> Result<IndicatorEvaluation, EguiError> {
        let config = component_downcast_ref::<IntegrationLiteConfig>(config)
            .ok_or_else(|| wrong_config(self.id(), "IntegrationLite"))?;
        let pane = integration_lite_sub_pane(rows, config)?;
        let fingerprint = config_fingerprint(config);
        let legs = [
            ("Fast", pane.fast_bounded, config.fast_kind.clone()),
            ("Slow", pane.slow_bounded, config.slow_kind.clone()),
            (
                "Ribbon Health",
                pane.ribbon_health_bounded,
                "fast".to_owned(),
            ),
            ("MA Health", pane.ma_health_bounded, "slow".to_owned()),
        ];
        let series = INTEGRATION_LITE_NODES
            .iter()
            .zip(legs)
            .map(|(node, (name, values, style))| IndicatorSeries {
                node: (*node).to_owned(),
                name: name.to_owned(),
                display_values: values.clone(),
                display_mapping: None,
                values,
                style,
                attachment: SlotAttachment::SubPane,
                key: cache_key(node, fingerprint),
                pane: None,
            })
            .collect();
        Ok(IndicatorEvaluation {
            series,
            overlay: Ok(Vec::new()),
        })
    }

    /// Paint the visible legs only, at the dialed brightness.
    ///
    /// The four evaluated series stay published (plan nodes and cache keys
    /// never move with a paint toggle), but only the shown legs reach the
    /// emitter: `osc_show` gates both raw legs, the health flags gate their
    /// folds. Brightness scales line opacity exactly like the script's
    /// `90 - brightness` transparency; paint-only, values never move.
    fn paint_sub_pane(
        &self,
        painter: &egui::Painter,
        evaluation: &IndicatorEvaluation,
        viewport: &Viewport,
        origin: egui::Pos2,
        base: usize,
        width_px: f32,
        theme: &ThemeConfig,
        config: &dyn Component,
        style: &IndicatorStyle,
    ) -> Result<(), EguiError> {
        let config = component_downcast_ref::<IntegrationLiteConfig>(config)
            .ok_or_else(|| wrong_config(self.id(), "IntegrationLite"))?;
        let visible = |name: &str| match name {
            "Fast" | "Slow" => config.osc_show,
            "Ribbon Health" => config.ribbon_health_show,
            "MA Health" => config.ma_health_show,
            _ => true,
        };
        let alpha =
            (1.0 - oscillator_transparency(config.brightness.clamp(0, 100) as u32) / 100.0) as f32;
        let aggregate = needs_column_aggregation(viewport.scale().bar_spacing());
        for series in evaluation
            .series
            .iter()
            .filter(|series| visible(&series.name))
        {
            let len = series.display_values.len().saturating_sub(base);
            let window = visible_rows(&series.display_values, base, len);
            let points = if aggregate {
                sampled_line_points(window, viewport, base)
            } else {
                proportional_line_points(window, viewport, width_px)
            };
            let placed = translate_points(points, (origin.x, origin.y));
            paint_oscillator_styled(painter, &placed, &series.style, theme, style, alpha)?;
        }
        Ok(())
    }
}

/// The Fast MAs plugin: one moving-average line per enabled arm.
pub struct FastMasIndicator;

impl FastMasIndicator {
    /// Create the plugin.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for FastMasIndicator {
    fn default() -> Self {
        Self::new()
    }
}

/// The Fast MAs pane-plan nodes for a config, in plot order (mirrors
/// [`evaluate_fast_mas`]'s line order exactly).
fn fast_mas_nodes(config: &FastMasConfig) -> Vec<&'static str> {
    let mut nodes = Vec::new();
    if config.show_first_ma {
        nodes.push("fast_mas:first");
    }
    if config.show_second_ma {
        nodes.push("fast_mas:second");
    }
    if config.show_extra_mas {
        nodes.push("fast_mas:signal");
        nodes.push("fast_mas:mean");
        nodes.push("fast_mas:trend");
        nodes.push("fast_mas:base");
    }
    if config.show_smas {
        nodes.push("fast_mas:sma50");
        nodes.push("fast_mas:sma100");
        nodes.push("fast_mas:sma200");
    }
    nodes
}

impl ChartIndicator for FastMasIndicator {
    fn id(&self) -> &str {
        FAST_MAS_ID
    }

    fn title(&self) -> &str {
        "Fast MAs"
    }

    fn attachment(&self) -> SlotAttachment {
        SlotAttachment::MainOverlay
    }

    fn default_config(&self) -> Box<dyn Component> {
        Box::new(FastMasConfig::default())
    }

    fn plan_nodes(&self, config: &dyn Component) -> Result<Vec<String>, EguiError> {
        let config = component_downcast_ref::<FastMasConfig>(config)
            .ok_or_else(|| wrong_config(self.id(), "FastMas"))?;
        Ok(fast_mas_nodes(config)
            .into_iter()
            .map(str::to_owned)
            .collect())
    }

    fn config_key(&self, config: &dyn Component) -> Result<u64, EguiError> {
        let config = component_downcast_ref::<FastMasConfig>(config)
            .ok_or_else(|| wrong_config(self.id(), "FastMas"))?;
        Ok(config_fingerprint(config))
    }

    fn plan_deps(&self, config: &dyn Component) -> Result<Vec<String>, EguiError> {
        let config = component_downcast_ref::<FastMasConfig>(config)
            .ok_or_else(|| wrong_config(self.id(), "FastMas"))?;
        Ok(source_edge(&config.source))
    }

    fn evaluate(
        &self,
        rows: &[ChartRow],
        config: &dyn Component,
        series: &SeriesCatalog,
    ) -> Result<IndicatorEvaluation, EguiError> {
        let config = component_downcast_ref::<FastMasConfig>(config)
            .ok_or_else(|| wrong_config(self.id(), "FastMas"))?;
        let view = evaluate_fast_mas(rows, config, series)?;
        let fingerprint = config_fingerprint(config);
        let producer_key = producer_series_key(&config.source, series);
        let series = fast_mas_nodes(config)
            .into_iter()
            .zip(view.lines)
            .map(|(node, line)| IndicatorSeries {
                node: node.to_owned(),
                name: line.name,
                display_values: line.values.clone(),
                display_mapping: None,
                values: line.values,
                style: line.style,
                attachment: SlotAttachment::MainOverlay,
                key: series_key_for(node, fingerprint, producer_key),
                pane: None,
            })
            .collect();
        Ok(IndicatorEvaluation {
            series,
            overlay: Ok(Vec::new()),
        })
    }

    fn paint_main_overlay(
        &self,
        painter: &egui::Painter,
        evaluation: &IndicatorEvaluation,
        viewport: &Viewport,
        origin: egui::Pos2,
        base: usize,
        width_px: f32,
        theme: &ThemeConfig,
        _config: &dyn Component,
        style: &IndicatorStyle,
    ) -> Result<(), EguiError> {
        // One polyline per average through the shared seam — the same
        // sampler pair and the same styled emitter as every line-like
        // series, so no second polyline mapping can drift from it.
        paint_series(
            painter, evaluation, viewport, origin, base, width_px, theme, style,
        )
    }
}

fn default_kernel() -> String {
    "sma".to_owned()
}

fn default_kernel_length() -> i64 {
    14
}

fn default_kernel_style() -> String {
    "fast".to_owned()
}

/// Default line style key for a kernel: the oscillator kind key where the
/// kernel is a bounded oscillator, else the role default. Slots keep their
/// own choice afterwards — this only seeds new slots, so existing slots
/// never move.
#[must_use]
/// Default line style key for a kernel: the oscillator kind key where the
/// kernel is a bounded oscillator, else the role default. Slots keep their
/// own choice afterwards — this only seeds new slots, so existing slots
/// never move.
pub fn default_style_for_kernel(kernel: &str) -> &'static str {
    panel_kind_for_kernel(kernel).unwrap_or("fast")
}

fn default_kernel_source() -> String {
    crate::topology::default_source_spelling()
}

fn default_osc_trim() -> f64 {
    0.0
}

fn default_osc_threshold() -> f64 {
    100.0
}

fn default_smooth_type() -> String {
    "na".to_owned()
}

fn default_smooth_period() -> i64 {
    3
}

/// One kernel plugin's configuration: which kernel, its declared parameters,
/// its paint style, and its input source. `length` remains a legacy alias
/// that old preferences migrate through.
#[derive(Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize)]
#[builder(start_fn = new)]
/// One kernel plugin's configuration: which kernel, its declared parameters,
/// its paint style, and its input source. `length` remains a legacy alias
/// that old preferences migrate through.
pub struct KernelIndicatorConfig {
    /// Kernel id resolved against the builtin registry.
    #[builder(default = default_kernel())]
    pub kernel: String,
    /// Legacy length alias; declared `length` wins when the kernel has one.
    #[builder(default = default_kernel_length())]
    #[serde(default = "default_kernel_length")]
    pub length: i64,
    /// Line theme style key: a catalogue pick, never free text.
    #[builder(default = default_kernel_style())]
    pub style: String,
    /// Input series: a price-source catalogue pick or `series:<node>`.
    #[builder(default = default_kernel_source())]
    pub source: String,
    /// Declared parameter values by name, encoded by kind. Missing entries
    /// read the kernel's declared default. Stays `BTreeMap`: already sorted
    /// with replace-on-duplicate, matching `KeyedRegistry` guarantees.
    #[builder(default)]
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    /// Whether display values squash through the frozen catalog. `None`
    /// (including old preferences that predate the dial) follows the kernel:
    /// on for mapped oscillators, off otherwise. Explicit `Some(true)` on an
    /// unmapped kernel refuses.
    #[serde(default)]
    pub normalize: Option<bool>,
    /// Display gain over the catalog squash. Unset follows the kind
    /// default from the Oscillators panel (neutral 1.0); empty clears back
    /// to following.
    #[serde(default)]
    pub osc_gain: Option<f64>,
    /// Post-normalization centering trim. Computation untouched.
    #[builder(default = default_osc_trim())]
    #[serde(default = "default_osc_trim")]
    pub osc_trim: f64,
    /// Display threshold at `±osc_threshold`. Painted rules only.
    #[builder(default = default_osc_threshold())]
    #[serde(default = "default_osc_threshold")]
    pub osc_threshold: f64,
    /// Display offset over the catalog row. Unset follows the row;
    /// explicit values reshape the squash. Empty clears back to unset.
    #[serde(default)]
    pub osc_offset: Option<f64>,
    /// Display steepness over the catalog row. Unset follows the row.
    #[serde(default)]
    pub osc_k: Option<f64>,
    /// Display band half-width over the catalog row. Unset follows the row.
    #[serde(default)]
    pub osc_mult: Option<f64>,
    /// Trailing smoothing family for the display leg (`NA` disables).
    /// Applied to the raw leg before normalization; the published raw
    /// series never sees it.
    #[builder(default = default_smooth_type())]
    #[serde(default = "default_smooth_type")]
    pub smooth_type: String,
    /// Trailing smoothing window in bars. Shares the MA period contract.
    #[builder(default = default_smooth_period())]
    #[serde(default = "default_smooth_period")]
    pub smooth_period: i64,
}

const FIXED_FIELDS: &[&str] = &[
    "kernel",
    "length",
    "style",
    "source",
    "normalize",
    "osc_gain",
    "osc_trim",
    "osc_threshold",
    "osc_offset",
    "osc_k",
    "osc_mult",
    "smooth_type",
    "smooth_period",
];

impl KernelIndicatorConfig {
    /// Effective normalize flag: the explicit row, or the per-kernel default
    /// (on for mapped oscillators, off otherwise) when unset.
    #[must_use]
    pub fn effective_normalize(&self) -> bool {
        self.normalize
            .unwrap_or_else(|| kernel_normalizes(&self.kernel))
    }

    /// Resolve blank dials against one kind's panel defaults: an unset
    /// gain takes the kind gain, and disarmed slot smoothing follows armed
    /// kind smoothing. Everything set wins as-is; the frozen catalog stays
    /// the last resort downstream. The resolved config is what evaluates
    /// and fingerprints, so following slots move exactly when the kind
    /// does.
    #[must_use]
    pub fn resolve_panel_defaults(&self, defaults: &KindDefaults) -> Self {
        let mut out = self.clone();
        if out.osc_gain.is_none() {
            out.osc_gain = Some(defaults.gain);
        }
        // The triple follows only where a catalog row exists to shape:
        // unmapped kernels keep their blanks, so placeholder rows never
        // fabricate a shape.
        if crate::oscillator_panel::panel_kind_is_shaped(&self.kernel) {
            if out.osc_offset.is_none() {
                out.osc_offset = defaults.offset;
            }
            if out.osc_k.is_none() {
                out.osc_k = defaults.k;
            }
            if out.osc_mult.is_none() {
                out.osc_mult = defaults.mult;
            }
        }
        // `NA` is disarmed by definition: the length-taking check alone
        // would count it as armed, so it is pinned off here like the
        // smoothing validation pins it.
        let slot_armed = matches!(
            SmoothingFamily::parse(&out.smooth_type),
            Some(family) if family != SmoothingFamily::Na && family.takes_length()
        );
        let kind_armed = matches!(
            SmoothingFamily::parse(&defaults.smooth_type),
            Some(family) if family != SmoothingFamily::Na && family.takes_length()
        );
        if !slot_armed && kind_armed {
            out.smooth_type.clone_from(&defaults.smooth_type);
            out.smooth_period = defaults.smooth_period;
        }
        out
    }

    /// The registered kernel's declared parameter and output schema.
    pub fn schema(
        &self,
    ) -> Result<
        (
            &'static [protopyne_kernels::indicator::IndicatorParam],
            &'static [protopyne_kernels::indicator::OutputSpec],
        ),
        EguiError,
    > {
        let registry = shared_registry().map_err(|error| EguiError::Dependency {
            reason: error.to_string(),
        })?;
        let indicator = registry
            .get(&self.kernel)
            .ok_or_else(|| EguiError::InvalidParam {
                what: "kernel".to_owned(),
                reason: format!("kernel {:?} is not in the kernel catalog", self.kernel),
            })?;
        let spec = indicator.spec();
        Ok((spec.params(), spec.outputs()))
    }

    /// The effective parameter set: declared stored values, legacy length
    /// migration, and declared defaults.
    pub fn effective_params(&self) -> Result<protopyne_kernels::ParamSet, EguiError> {
        use protopyne_kernels::indicator::{ParamKind, ParamValue};
        let (declared, _) = self.schema()?;
        for name in self.params.keys() {
            if !declared.iter().any(|param| param.name == name) {
                return Err(EguiError::InvalidParam {
                    what: "params".to_owned(),
                    reason: format!("kernel {:?} declares no parameter {name:?}", self.kernel),
                });
            }
        }
        let mut params = protopyne_kernels::ParamSet::new();
        for param in declared {
            let stored = self.params.get(param.name);
            let value = match (param.kind, stored) {
                (ParamKind::Int, Some(text)) => {
                    let value = text.parse::<i64>().map_err(|_| EguiError::InvalidParam {
                        what: param.name.to_owned(),
                        reason: format!("expected an integer, got {text:?}"),
                    })?;
                    ParamValue::Int(value)
                }
                (ParamKind::Float, Some(text)) => {
                    let value = text.parse::<f64>().map_err(|_| EguiError::InvalidParam {
                        what: param.name.to_owned(),
                        reason: format!("expected a number, got {text:?}"),
                    })?;
                    ParamValue::Float(value)
                }
                (ParamKind::Bool, Some(text)) => {
                    ParamValue::Bool(text.parse::<bool>().map_err(|_| EguiError::InvalidParam {
                        what: param.name.to_owned(),
                        reason: format!("expected true/false, got {text:?}"),
                    })?)
                }
                (_, None) if param.name == "length" => ParamValue::Int(self.length),
                (_, None) => param.default,
            };
            let numeric = match value {
                ParamValue::Int(value) => Some(value as f64),
                ParamValue::Float(value) => Some(value),
                ParamValue::Bool(_) => None,
            };
            if let Some(value) = numeric {
                if !value.is_finite()
                    || param.min.is_some_and(|min| value < min)
                    || param.max.is_some_and(|max| value > max)
                {
                    return Err(EguiError::InvalidParam {
                        what: param.name.to_owned(),
                        reason: format!(
                            "{value} is outside [{}, {}]",
                            param.min.unwrap_or(f64::NEG_INFINITY),
                            param.max.unwrap_or(f64::INFINITY)
                        ),
                    });
                }
            }
            params = match value {
                ParamValue::Int(value) => params.with_int(param.name, value),
                ParamValue::Float(value) => params.with_float(param.name, value),
                ParamValue::Bool(value) => params.with_bool(param.name, value),
            };
        }
        if declared.iter().any(|param| param.name == "length")
            && !self.params.contains_key("length")
        {
            params = params.with_int("length", self.length);
        }
        Ok(params)
    }

    /// The display triple in force for one catalog kind: the explicit dials
    /// over the frozen row, or `None` when the kind refuses (only the
    /// refused row lacks constants, and no mapped output names it).
    #[must_use]
    pub fn effective_osc_params_for(&self, kind: &OscillatorKind) -> Option<OscParams> {
        let catalog = kind.params()?;
        Some(OscParams {
            offset: self.osc_offset.unwrap_or(catalog.offset),
            k: self.osc_k.unwrap_or(catalog.k),
            multiplier: self.osc_mult.unwrap_or(catalog.multiplier),
        })
    }

    /// Fail-closed self-consistency check: a registered kernel, a positive
    /// legacy length, a non-empty style key, and a valid declared schema.
    pub fn validate(&self) -> Result<(), EguiError> {
        if self.kernel.trim().is_empty() {
            return Err(EguiError::InvalidParam {
                what: "kernel".to_owned(),
                reason: "kernel id must not be empty".to_owned(),
            });
        }
        self.schema()?;
        if self.length <= 0 {
            return Err(EguiError::InvalidParam {
                what: "length".to_owned(),
                reason: "kernel length must be positive".to_owned(),
            });
        }
        if self.style.trim().is_empty() {
            return Err(EguiError::InvalidParam {
                what: "style".to_owned(),
                reason: "style key must not be empty".to_owned(),
            });
        }
        if SeriesSource::parse(&self.source).is_none() {
            return Err(EguiError::InvalidParam {
                what: "source".to_owned(),
                reason: format!(
                    "unknown input source {:?}: expected a price source (close, hl2, ...) or series:<node>",
                    self.source
                ),
            });
        }
        self.effective_params()?;
        if self
            .osc_gain
            .is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            return Err(EguiError::InvalidParam {
                what: "osc_gain".to_owned(),
                reason: format!(
                    "osc_gain must be a positive finite number, got {:?}",
                    self.osc_gain
                ),
            });
        }
        if !self.osc_trim.is_finite() {
            return Err(EguiError::InvalidParam {
                what: "osc_trim".to_owned(),
                reason: format!("osc_trim must be finite, got {}", self.osc_trim),
            });
        }
        if !self.osc_threshold.is_finite() || self.osc_threshold <= 0.0 {
            return Err(EguiError::InvalidParam {
                what: "osc_threshold".to_owned(),
                reason: format!(
                    "osc_threshold must be a positive finite number, got {}",
                    self.osc_threshold
                ),
            });
        }
        if self.osc_offset.is_some_and(|value| !value.is_finite()) {
            return Err(EguiError::InvalidParam {
                what: "osc_offset".to_owned(),
                reason: format!("osc_offset must be finite, got {:?}", self.osc_offset),
            });
        }
        if self
            .osc_k
            .is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            return Err(EguiError::InvalidParam {
                what: "osc_k".to_owned(),
                reason: format!(
                    "osc_k must be a positive finite number, got {:?}",
                    self.osc_k
                ),
            });
        }
        if self
            .osc_mult
            .is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            return Err(EguiError::InvalidParam {
                what: "osc_mult".to_owned(),
                reason: format!(
                    "osc_mult must be a positive finite number, got {:?}",
                    self.osc_mult
                ),
            });
        }
        let armed_family = match SmoothingFamily::parse(&self.smooth_type) {
            Some(SmoothingFamily::Na) => None,
            Some(family) if family.takes_length() => Some(family),
            _ => {
                return Err(EguiError::InvalidParam {
                    what: "smooth_type".to_owned(),
                    reason: format!(
                        "smoothing {:?} is not a length-taking smoothing family",
                        self.smooth_type
                    ),
                });
            }
        };
        if let Some(family) = armed_family {
            let kernel = family.kernel_id().ok_or_else(|| EguiError::InvalidParam {
                what: "smooth_type".to_owned(),
                reason: format!("smoothing {:?} has no kernel to evaluate", self.smooth_type),
            })?;
            if shared_registry()
                .map_err(|error| EguiError::Dependency {
                    reason: error.to_string(),
                })?
                .get(kernel)
                .is_none()
            {
                return Err(EguiError::InvalidParam {
                    what: "smooth_type".to_owned(),
                    reason: format!("smoothing kernel {kernel:?} is not registered"),
                });
            }
            if self.smooth_period < MA_PERIOD_MIN || self.smooth_period > MA_PERIOD_MAX {
                return Err(EguiError::InvalidParam {
                    what: "smooth_period".to_owned(),
                    reason: format!(
                        "smoothing window must sit within [{MA_PERIOD_MIN}, {MA_PERIOD_MAX}], got {}",
                        self.smooth_period
                    ),
                });
            }
        }
        if self.effective_normalize() && !kernel_normalizes(&self.kernel) {
            return Err(EguiError::InvalidParam {
                what: "normalize".to_owned(),
                reason: format!(
                    "kernel {:?} has no normalizable oscillator mapping, so normalize refuses",
                    self.kernel
                ),
            });
        }
        Ok(())
    }

    fn default_param_text(param: &protopyne_kernels::indicator::IndicatorParam) -> String {
        use protopyne_kernels::indicator::ParamValue;
        match param.default {
            ParamValue::Int(value) => value.to_string(),
            ParamValue::Float(value) => value.to_string(),
            ParamValue::Bool(value) => value.to_string(),
        }
    }

    /// A default config for one kernel with every declared default filled.
    #[must_use]
    pub fn defaults_for(kernel: &str) -> Self {
        let mut config = Self::new().kernel(kernel.to_owned()).build();
        config.normalize = Some(kernel_normalizes(kernel));
        default_style_for_kernel(kernel).clone_into(&mut config.style);
        if let Ok((declared, _)) = config.schema() {
            for param in declared {
                config
                    .params
                    .insert(param.name.to_owned(), Self::default_param_text(param));
            }
        }
        config
    }
}

impl Default for KernelIndicatorConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

/// Parse an optional display dial: blank restores the catalog default, any
/// other text must parse as a number. Mirrors the style component's
/// empty-clears convention, so unsetting a dial needs no magic value.
fn parse_optional_number(value: &str) -> Result<Option<f64>, FieldError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed
        .parse()
        .map(Some)
        .map_err(|_| FieldError::Parse(value.to_owned()))
}

impl FieldAccess for KernelIndicatorConfig {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        match name {
            "kernel" => {
                if value.trim().is_empty() {
                    return Err(FieldError::Parse(value.to_owned()));
                }
                value.clone_into(&mut self.kernel);
                self.params.clear();
                self.normalize = Some(kernel_normalizes(&self.kernel));
                if let Ok((declared, _)) = self.schema() {
                    for param in declared {
                        self.params
                            .insert(param.name.to_owned(), Self::default_param_text(param));
                    }
                }
                Ok(())
            }
            "length" => {
                self.length = value
                    .parse()
                    .map_err(|_| FieldError::Parse(value.to_owned()))?;
                if self
                    .schema()
                    .is_ok_and(|(declared, _)| declared.iter().any(|p| p.name == "length"))
                {
                    self.params.insert("length".to_owned(), value.to_owned());
                }
                Ok(())
            }
            "style" => {
                value.clone_into(&mut self.style);
                Ok(())
            }
            "source" => {
                value.clone_into(&mut self.source);
                Ok(())
            }
            "normalize" => {
                self.normalize = Some(
                    value
                        .parse()
                        .map_err(|_| FieldError::Parse(value.to_owned()))?,
                );
                Ok(())
            }
            "osc_gain" => {
                self.osc_gain = parse_optional_number(value)?;
                Ok(())
            }
            "osc_trim" => {
                self.osc_trim = value
                    .parse()
                    .map_err(|_| FieldError::Parse(value.to_owned()))?;
                Ok(())
            }
            "osc_threshold" => {
                self.osc_threshold = value
                    .parse()
                    .map_err(|_| FieldError::Parse(value.to_owned()))?;
                Ok(())
            }
            "osc_offset" => {
                self.osc_offset = parse_optional_number(value)?;
                Ok(())
            }
            "osc_k" => {
                self.osc_k = parse_optional_number(value)?;
                Ok(())
            }
            "osc_mult" => {
                self.osc_mult = parse_optional_number(value)?;
                Ok(())
            }
            "smooth_type" => {
                value.clone_into(&mut self.smooth_type);
                Ok(())
            }
            "smooth_period" => {
                self.smooth_period = value
                    .parse()
                    .map_err(|_| FieldError::Parse(value.to_owned()))?;
                Ok(())
            }
            other => {
                let (declared, _) = self
                    .schema()
                    .map_err(|error| FieldError::NotFound(format!("{other}: {error}")))?;
                let Some(param) = declared.iter().find(|param| param.name == other) else {
                    return Err(FieldError::NotFound(other.to_owned()));
                };
                use protopyne_kernels::indicator::ParamKind;
                let valid = match param.kind {
                    ParamKind::Int => value.parse::<i64>().is_ok(),
                    ParamKind::Float => value.parse::<f64>().is_ok(),
                    ParamKind::Bool => value.parse::<bool>().is_ok(),
                };
                if !valid {
                    return Err(FieldError::Parse(value.to_owned()));
                }
                self.params.insert(other.to_owned(), value.to_owned());
                Ok(())
            }
        }
    }

    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        match name {
            "kernel" => Ok(self.kernel.clone()),
            // A declared length reads the schema value, like `set_field`
            // writes it; the legacy alias only shows when the kernel
            // declares no length of its own.
            "length" => {
                if self
                    .schema()
                    .is_ok_and(|(declared, _)| declared.iter().any(|p| p.name == "length"))
                {
                    Ok(self
                        .params
                        .get("length")
                        .cloned()
                        .unwrap_or_else(|| self.length.to_string()))
                } else {
                    Ok(self.length.to_string())
                }
            }
            "style" => Ok(self.style.clone()),
            "source" => Ok(self.source.clone()),
            "normalize" => Ok(self.effective_normalize().to_string()),
            "osc_gain" => Ok(self
                .osc_gain
                .map(|value| value.to_string())
                .unwrap_or_default()),
            "osc_trim" => Ok(self.osc_trim.to_string()),
            "osc_threshold" => Ok(self.osc_threshold.to_string()),
            "osc_offset" => Ok(self
                .osc_offset
                .map(|value| value.to_string())
                .unwrap_or_default()),
            "osc_k" => Ok(self
                .osc_k
                .map(|value| value.to_string())
                .unwrap_or_default()),
            "osc_mult" => Ok(self
                .osc_mult
                .map(|value| value.to_string())
                .unwrap_or_default()),
            "smooth_type" => Ok(self.smooth_type.clone()),
            "smooth_period" => Ok(self.smooth_period.to_string()),
            other => {
                let (declared, _) = self
                    .schema()
                    .map_err(|error| FieldError::NotFound(format!("{other}: {error}")))?;
                let param = declared
                    .iter()
                    .find(|param| param.name == other)
                    .ok_or_else(|| FieldError::NotFound(other.to_owned()))?;
                Ok(self
                    .params
                    .get(other)
                    .cloned()
                    .unwrap_or_else(|| Self::default_param_text(param)))
            }
        }
    }

    fn field_names(&self) -> &'static [&'static str] {
        FIXED_FIELDS
    }
}

impl Describable for KernelIndicatorConfig {
    fn describe(&self) -> serde_json::Value {
        use protopyne_kernels::indicator::ParamKind;
        let mut properties = serde_json::Map::new();
        properties.insert(
            "style".to_owned(),
            serde_json::json!({
                "type": "string",
                "description": "Line theme style key",
                "enum": fluent_egui::ThemeConfig::style_keys(),
            }),
        );
        properties.insert(
            "source".to_owned(),
            serde_json::json!({
                "type": "string",
                "description": "Input series (price source or series:<node>)",
                "enum": crate::topology::price_source_ids(),
            }),
        );
        // The normalization dials render only where they can move the
        // picture: slots whose kernel never normalizes omit all seven, so
        // moving averages stop offering inert controls. Smoothing stays —
        // smoothed passthrough is functional on any leg — as do style,
        // source, and the kernel's own params. Validation still refuses a
        // forced normalize on such kernels; the schema simply stops
        // suggesting it.
        if kernel_normalizes(&self.kernel) {
            properties.insert(
                "normalize".to_owned(),
                serde_json::json!({
                    "type": "boolean",
                    "description": "Squash display through the frozen oscillator catalog",
                    "default": true,
                    "x-tab": "style",
                }),
            );
            properties.insert(
                "osc_gain".to_owned(),
                serde_json::json!({
                    "type": "number",
                    "description": "Display gain over the catalog squash",
                    "default": 1.0,
                    "exclusiveMinimum": 0.0,
                    "x-tab": "style",
                }),
            );
            properties.insert(
                "osc_trim".to_owned(),
                serde_json::json!({
                    "type": "number",
                    "description": "Post-normalization centering trim",
                    "default": 0.0,
                    "x-tab": "style",
                }),
            );
            properties.insert(
                "osc_threshold".to_owned(),
                serde_json::json!({
                    "type": "number",
                    "description": "Display threshold at plus/minus this value",
                    "default": 100.0,
                    "exclusiveMinimum": 0.0,
                }),
            );
            properties.insert(
                "osc_offset".to_owned(),
                serde_json::json!({
                    "type": "number",
                    "description": "Display offset over the catalog row (blank follows the catalog)",
                    "x-tab": "style",
                }),
            );
            properties.insert(
                "osc_k".to_owned(),
                serde_json::json!({
                    "type": "number",
                    "description": "Display steepness over the catalog row, positive (blank follows the catalog)",
                    "exclusiveMinimum": 0.0,
                    "x-tab": "style",
                }),
            );
            properties.insert(
                "osc_mult".to_owned(),
                serde_json::json!({
                    "type": "number",
                    "description": "Display band half-width over the catalog row, positive (blank follows the catalog)",
                    "exclusiveMinimum": 0.0,
                    "x-tab": "style",
                }),
            );
        }
        properties.insert(
            "smooth_type".to_owned(),
            serde_json::json!({
                "type": "string",
                "description": "Trailing smoothing for the display leg (NA disables)",
                "enum": protopyne_kernels::smoothing::length_smoothing_ids(),
                "default": "na",
                "x-tab": "style",
            }),
        );
        properties.insert(
            "smooth_period".to_owned(),
            serde_json::json!({
                "type": "integer",
                "description": "Trailing smoothing window in bars",
                "default": 3,
                "minimum": 2,
                "maximum": 200,
                "x-tab": "style",
            }),
        );
        if let Ok((declared, _)) = self.schema() {
            for param in declared {
                let mut hint = serde_json::json!({
                    "description": param.description,
                });
                hint["type"] = serde_json::json!(match param.kind {
                    ParamKind::Int => "integer",
                    ParamKind::Float => "number",
                    ParamKind::Bool => "boolean",
                });
                if let Some(min) = param.min {
                    hint["minimum"] = serde_json::json!(min);
                }
                if let Some(max) = param.max {
                    hint["maximum"] = serde_json::json!(max);
                }
                if param.step > 0.0 {
                    hint["multipleOf"] = serde_json::json!(param.step);
                }
                properties.insert(param.name.to_owned(), hint);
            }
        }
        serde_json::Value::Object(serde_json::Map::from_iter([(
            "properties".to_owned(),
            serde_json::Value::Object(properties),
        )]))
    }
}

impl Validate for KernelIndicatorConfig {
    type Err = EguiError;
    fn validate(&self) -> Result<(), Self::Err> {
        KernelIndicatorConfig::validate(self)
    }
}

impl WorkUnit for KernelIndicatorConfig {
    fn name(&self) -> &str {
        "kernel_indicator_config"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        check_for_execute(self, "kernel indicator config invalid")?;
        Ok(valid_output(format!(
            "kernel indicator ready ({})",
            self.kernel
        )))
    }
}

impl_component!(KernelIndicatorConfig);

/// Every registered kernel id, sorted: the kernel picker. Empty only when
/// the registry itself fails to build (which also refuses instantiation).
#[must_use]
/// Every registered kernel id, sorted: the kernel picker. Empty only when
/// the registry itself fails to build (which also refuses instantiation).
pub fn kernel_ids() -> Vec<String> {
    shared_registry()
        .map(protopyne_kernels::IndicatorRegistry::ids)
        .unwrap_or_default()
}

/// Display title for a kernel id: the kernel's own spec name, falling back
/// to the id when the registry does not hold it.
#[must_use]
/// Display title for a kernel id: the kernel's own spec name, falling back
/// to the id when the registry does not hold it.
pub fn kernel_title(kernel: &str) -> String {
    shared_registry()
        .ok()
        .and_then(|registry| {
            registry
                .get(kernel)
                .map(|indicator| indicator.spec().display_name.clone())
        })
        .unwrap_or_else(|| kernel.to_owned())
}

/// Whether every declared output leg of a kernel maps into the shared
/// oscillator catalog: the single predicate behind the merge gate, the
/// normalize default, and shared-pane derivation. Unknown kernels and
/// partially mapped families refuse — merging half-raw content into a
/// symmetric normalized pane is structurally meaningless, and the pane
/// derivation must agree with the gate.
#[must_use]
/// Whether every declared output leg of a kernel maps into the shared
/// oscillator catalog: the single predicate behind the merge gate, the
/// normalize default, and shared-pane derivation. Unknown kernels and
/// partially mapped families refuse — merging half-raw content into a
/// symmetric normalized pane is structurally meaningless, and the pane
/// derivation must agree with the gate.
pub fn kernel_normalizes(kernel: &str) -> bool {
    let Ok(registry) = shared_registry() else {
        return false;
    };
    let Some(indicator) = registry.get(kernel) else {
        return false;
    };
    let outputs = indicator.spec().outputs();
    !outputs.is_empty()
        && outputs
            .iter()
            .all(|output| oscillator_mapping_for_output(kernel, output.name).is_some())
}

/// Pane a kernel plugin paints in: length-taking smoothings are one-arm
/// overlays through the shared kernel path; every other kernel stays in the
/// sub-pane. Cache keys and plan edges carry no attachment, so routing moves
/// paint only.
#[must_use]
/// Pane a kernel plugin paints in: length-taking smoothings are one-arm
/// overlays through the shared kernel path; every other kernel stays in the
/// sub-pane. Cache keys and plan edges carry no attachment, so routing moves
/// paint only.
pub fn kernel_attachment(kernel: &str) -> SlotAttachment {
    if SmoothingFamily::parse(kernel).is_some_and(SmoothingFamily::takes_length) {
        SlotAttachment::MainOverlay
    } else {
        SlotAttachment::SubPane
    }
}

/// The kernel plugin: hosts any builtin kernel (or a domain monitor bound to
/// one) behind the shared plugin contract.
pub struct KernelIndicator {
    id: String,
    title: String,
    kernel: String,
    attachment: SlotAttachment,
}

impl KernelIndicator {
    /// Create a plugin bound to `kernel`; refuses an unregistered kernel.
    /// Titles come from the kernel spec and single smoothings paint as
    /// overlays; monitors keep their explicit sub-pane.
    pub fn new(kernel: &str) -> Result<Self, EguiError> {
        let title = kernel_title(kernel);
        let attachment = kernel_attachment(kernel);
        Self::bind(kernel, &title, kernel, attachment)
    }

    /// Create a named monitor plugin bound to `kernel`.
    fn monitor(id: &str, title: &str, kernel: &str) -> Result<Self, EguiError> {
        Self::bind(id, title, kernel, SlotAttachment::SubPane)
    }

    /// Bind a plugin to a kernel, refusing one the registry does not hold.
    fn bind(
        id: &str,
        title: &str,
        kernel: &str,
        attachment: SlotAttachment,
    ) -> Result<Self, EguiError> {
        let registry = shared_registry().map_err(|error| EguiError::Dependency {
            reason: error.to_string(),
        })?;
        if registry.get(kernel).is_none() {
            return Err(EguiError::InvalidParam {
                what: "kernel".to_owned(),
                reason: format!(
                    "kernel {kernel:?} is not in the kernel catalog (free-text names outside the catalog no longer validate)"
                ),
            });
        }
        Ok(Self {
            id: id.to_owned(),
            title: title.to_owned(),
            kernel: kernel.to_owned(),
            attachment,
        })
    }
}

impl ChartIndicator for KernelIndicator {
    fn id(&self) -> &str {
        &self.id
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn attachment(&self) -> SlotAttachment {
        self.attachment
    }

    fn default_config(&self) -> Box<dyn Component> {
        Box::new(KernelIndicatorConfig::defaults_for(&self.kernel))
    }

    fn plan_nodes(&self, config: &dyn Component) -> Result<Vec<String>, EguiError> {
        let config = component_downcast_ref::<KernelIndicatorConfig>(config)
            .ok_or_else(|| wrong_config(self.id(), "KernelIndicator"))?;
        let (_, outputs) = config.schema()?;
        let mut nodes = Vec::with_capacity(outputs.len().max(1));
        if outputs.is_empty() {
            nodes.push(format!("kernel:{}", self.id));
        } else {
            for (index, output) in outputs.iter().enumerate() {
                nodes.push(if index == 0 {
                    format!("kernel:{}", self.id)
                } else {
                    format!("kernel:{}:{}", self.id, output.name)
                });
            }
        }
        Ok(nodes)
    }

    fn config_key(&self, config: &dyn Component) -> Result<u64, EguiError> {
        let config = component_downcast_ref::<KernelIndicatorConfig>(config)
            .ok_or_else(|| wrong_config(self.id(), "KernelIndicator"))?;
        Ok(config_fingerprint(config))
    }

    fn plan_deps(&self, config: &dyn Component) -> Result<Vec<String>, EguiError> {
        let config = component_downcast_ref::<KernelIndicatorConfig>(config)
            .ok_or_else(|| wrong_config(self.id(), "KernelIndicator"))?;
        Ok(source_edge(&config.source))
    }

    fn evaluate(
        &self,
        rows: &[ChartRow],
        config: &dyn Component,
        series: &SeriesCatalog,
    ) -> Result<IndicatorEvaluation, EguiError> {
        let config = component_downcast_ref::<KernelIndicatorConfig>(config)
            .ok_or_else(|| wrong_config(self.id(), "KernelIndicator"))?;
        config.validate()?;
        let source =
            SeriesSource::parse(&config.source).ok_or_else(|| EguiError::InvalidParam {
                what: "source".to_owned(),
                reason: format!("unknown input source {:?}", config.source),
            })?;
        let (column, producer) = resolve_input_column(&source, rows, series)?;
        // The reference arm ignores the source series and reads the bars
        // directly (high/low legs); the port mirrors that, so the slot
        // evaluates converted bars instead of the picked column. Only
        // aroon needs this today; range kernels in general still evaluate
        // their picked column (recorded follow-up).
        let frame = if config.kernel == "aroon" {
            crate::topology::kernel_preview_frame(rows)
        } else {
            series_frame(rows, &column)?
        };
        let params = config.effective_params()?;
        let result = kernel_result(&config.kernel, &params, &frame)?;
        let (_, outputs) = config.schema()?;
        let fingerprint = config_fingerprint(config);
        let producer_key = producer_series_key(&config.source, series);
        let holes = if producer > 0 {
            let own = kernel_warmup(&config.kernel, &params)?;
            producer.saturating_add(own).min(result.len())
        } else {
            0
        };
        let primary_node = format!("kernel:{}", self.id);
        let mut series_out = Vec::with_capacity(outputs.len().max(1));
        if outputs.is_empty() {
            let mut values = result.values.clone();
            mask_leading(&mut values, holes);
            let display_source = smooth_raw_for_display(
                &values,
                rows,
                &config.smooth_type,
                config.smooth_period,
                holes,
                "smooth_type",
            )?;
            let (display_values, display_mapping) =
                display_for_kernel(&display_source, config, "value");
            series_out.push(IndicatorSeries {
                node: primary_node.clone(),
                name: config.kernel.clone(),
                display_values,
                display_mapping,
                values,
                style: config.style.clone(),
                attachment: self.attachment,
                key: series_key_for(&primary_node, fingerprint, producer_key),
                pane: None,
            });
        } else {
            for (index, output) in outputs.iter().enumerate() {
                let node = if index == 0 {
                    primary_node.clone()
                } else {
                    format!("kernel:{}:{}", self.id, output.name)
                };
                let mut values = if index == 0 {
                    result.values.clone()
                } else {
                    result
                        .secondary(output.name)
                        .ok_or_else(|| EguiError::Dependency {
                            reason: format!(
                                "kernel {} declared output {:?} but produced none",
                                config.kernel, output.name
                            ),
                        })?
                        .to_vec()
                };
                mask_leading(&mut values, holes);
                let display_source = smooth_raw_for_display(
                    &values,
                    rows,
                    &config.smooth_type,
                    config.smooth_period,
                    holes,
                    "smooth_type",
                )?;
                let (display_values, display_mapping) =
                    display_for_kernel(&display_source, config, output.name);
                series_out.push(IndicatorSeries {
                    node: node.clone(),
                    name: if index == 0 {
                        config.kernel.clone()
                    } else {
                        format!("{} {}", config.kernel, output.name)
                    },
                    display_values,
                    display_mapping,
                    values,
                    style: config.style.clone(),
                    attachment: self.attachment,
                    key: series_key_for(&node, fingerprint, producer_key),
                    // Main-overlays paint on the chart and own no pane: a
                    // pane identity here would derive an empty strip below
                    // it. Normalized oscillators resolve to the shared
                    // sub-pane natively, so they never derive a custom pane
                    // either — only kernels that are neither chart nor
                    // normalized-oscillator content name one.
                    pane: if self.attachment == SlotAttachment::MainOverlay
                        || kernel_normalizes(&config.kernel)
                    {
                        None
                    } else {
                        Some(format!("kernel:{}", self.id))
                    },
                });
            }
        }
        Ok(IndicatorEvaluation {
            series: series_out,
            overlay: Ok(Vec::new()),
        })
    }
}

fn mask_leading(values: &mut [Option<f64>], holes: usize) {
    for value in values.iter_mut().take(holes) {
        *value = None;
    }
}

/// Smooth one raw leg for display when the slot arms a trailing smoothing
/// family (`NA` passes through). The smoothing kernel runs over the
/// single-column frame built from the masked raw leg, and its own warmup
/// masks on top of the leg's holes — holes stay holes, and no value is
/// ever invented past the kernels' own warmups. An unevaluable arm refuses
/// naming the field. The published raw series never sees this: smoothing
/// feeds normalization only.
/// Smooth one raw leg for display through a trailing smoothing family
/// (`NA` passes through). The smoothing kernel runs over the
/// single-column frame built from the masked raw leg, and its own warmup
/// masks on top of the leg's holes: holes stay holes, and no value is ever
/// invented past the kernels' own warmups. `field` names the dial refused
/// reports carry. Callers resolve blanks to effective values before this
/// runs, so every dial reads concrete here.
pub(crate) fn smooth_raw_for_display(
    raw: &[Option<f64>],
    rows: &[ChartRow],
    smooth_type: &str,
    smooth_period: i64,
    holes: usize,
    field: &str,
) -> Result<Vec<Option<f64>>, EguiError> {
    let family = match SmoothingFamily::parse(smooth_type) {
        Some(SmoothingFamily::Na) => return Ok(raw.to_vec()),
        Some(family) if family.takes_length() => family,
        _ => {
            return Err(EguiError::InvalidParam {
                what: field.to_owned(),
                reason: format!(
                    "smoothing {smooth_type:?} is not a length-taking smoothing family"
                ),
            });
        }
    };
    let kernel = family.kernel_id().ok_or_else(|| EguiError::InvalidParam {
        what: field.to_owned(),
        reason: format!("smoothing {smooth_type:?} has no kernel to evaluate"),
    })?;
    if shared_registry()
        .map_err(|error| EguiError::Dependency {
            reason: error.to_string(),
        })?
        .get(kernel)
        .is_none()
    {
        return Err(EguiError::InvalidParam {
            what: field.to_owned(),
            reason: format!("smoothing kernel {kernel:?} is not registered"),
        });
    }
    let frame = series_frame(rows, raw)?;
    let smoothed = family
        .evaluate(&frame, &SmoothingArgs::length(smooth_period))
        .map_err(|error| describe_kernel(&error))?;
    let warmup = family
        .warmup_for_length(smooth_period)
        .map_err(|error| describe_kernel(&error))?;
    let mut out = smoothed;
    let mask = holes.saturating_add(warmup).min(out.len());
    mask_leading(&mut out, mask);
    Ok(out)
}

/// Display values plus provenance for one raw leg: the dialed transform
/// `band(gain * catalog(raw) + trim)` when normalizing is on and the output
/// maps, else a raw passthrough. Holes stay holes; a refused kind cannot
/// occur here because validation already refused `normalize` on unmapped
/// kernels.
fn display_for_kernel(
    raw: &[Option<f64>],
    config: &KernelIndicatorConfig,
    output: &str,
) -> (Vec<Option<f64>>, Option<(OscillatorKind, WhichLeg)>) {
    if !config.effective_normalize() {
        return (raw.to_vec(), None);
    }
    let Some(mapping) = oscillator_mapping_for_output(&config.kernel, output) else {
        return (raw.to_vec(), None);
    };
    let (kind, _) = mapping;
    let Some(params) = config.effective_osc_params_for(&kind) else {
        return (raw.to_vec(), None);
    };
    // Aroon legs arrive as window fractions; the catalog row is calibrated
    // for the Pine 0..100 arm, so the exact bridge rescales first. The
    // length is the slot's own (validated before display runs): panel edits
    // template new slots, and every slot carries what it evaluates.
    let shaped: Vec<Option<f64>> = if kind == OscillatorKind::Aroon {
        let length = config
            .params
            .get("length")
            .and_then(|text| text.parse::<i64>().ok())
            .unwrap_or(config.length);
        crate::oscillators::rescale_aroon_output(raw, length)
    } else {
        raw.to_vec()
    };
    (
        display_mapped_series(
            &shaped,
            &params,
            config.osc_gain.unwrap_or(1.0),
            config.osc_trim,
        ),
        Some(mapping),
    )
}

/// Build the indicator catalog: the two scripted indicators under My Scripts,
/// every builtin kernel under Technical, and the domain monitors under
/// Financial.
pub fn build_indicator_catalog() -> Result<CatalogModal<IndicatorCategory>, EguiError> {
    build_indicator_catalog_with_manifest(&[])
}

/// Build the indicator catalog over manifest rows: discovered Pine scripts
/// register under My Scripts with their manifest titles (never id-as-title),
/// prefixed with 🌲 so scripts read as scripts in the picker. The manifest
/// is built at boot/CLI time; the widget only renders its rows and never
/// reads the directory. A non-empty manifest also installs the Pine
/// namespace checkbox beside Favorites; an empty one leaves the catalog
/// exactly as before.
pub fn build_indicator_catalog_with_manifest(
    rows: &[IndicatorManifestEntry],
) -> Result<CatalogModal<IndicatorCategory>, EguiError> {
    let mut catalog = CatalogModal::new("Indicators");
    catalog.register(
        INTEGRATION_LITE_ID,
        "Integration Lite",
        IndicatorCategory::MyScripts,
    )?;
    catalog.register(FAST_MAS_ID, "Fast MAs", IndicatorCategory::MyScripts)?;
    catalog.register(
        DELTA_VOLUME_PROFILE_ID,
        "Delta Volume Profile",
        IndicatorCategory::MyScripts,
    )?;
    for row in rows {
        catalog.register(
            &row.id,
            &format!("🌲 {}", row.title),
            IndicatorCategory::MyScripts,
        )?;
    }
    if !rows.is_empty() {
        catalog.set_tag_filter("Pine", "pine:");
    }
    let registry = shared_registry().map_err(|error| EguiError::Dependency {
        reason: error.to_string(),
    })?;
    for id in registry.ids() {
        catalog.register(&id, &kernel_title(&id), IndicatorCategory::Technical)?;
    }
    catalog.register(
        REGIME_ID,
        "Market Regime Monitor",
        IndicatorCategory::Financial,
    )?;
    catalog.register(HEALTH_ID, "MA Health Monitor", IndicatorCategory::Financial)?;
    Ok(catalog)
}

/// Instantiate a plugin by catalog id: the two scripted indicators, the two
/// domain monitors, or any builtin kernel. Unknown ids refuse.
pub fn instantiate_indicator(id: &str) -> Result<Box<dyn ChartIndicator>, EguiError> {
    instantiate_indicator_with_manifest(id, &[])
}

/// Manifest-backed scripts that resolve to a shipped native plugin: the
/// script's parity was proven against this plugin's output, so the manifest
/// id rides the same path instead of forking a second executor.
fn manifest_backing(id: &str) -> Option<&'static str> {
    match id {
        "pine:_IntegrationLite" => Some(INTEGRATION_LITE_ID),
        "pine:_FastMAs" => Some(FAST_MAS_ID),
        "pine:_DeltaVolumeProfileProtopyne" => Some(DELTA_VOLUME_PROFILE_ID),
        _ => None,
    }
}

/// Instantiate a plugin with manifest rows in scope: shipped and kernel ids
/// resolve as before; `pine:<stem>` ids resolve through their backing plugin
/// when verified, and refuse with the reason when unverified, unbacked, or
/// unlisted. Red entries are listed-but-never-runnable; the refusal carries
/// the manifest reason instead of hiding the script.
pub fn instantiate_indicator_with_manifest(
    id: &str,
    rows: &[IndicatorManifestEntry],
) -> Result<Box<dyn ChartIndicator>, EguiError> {
    if let Some(stem) = id.strip_prefix("pine:") {
        let entry =
            rows.iter()
                .find(|row| row.id == id)
                .ok_or_else(|| EguiError::InvalidParam {
                    what: "indicator".to_owned(),
                    reason: format!("unknown indicator {id:?}: no manifest row lists it"),
                })?;
        if let ManifestParity::Unverified { reason } = &entry.parity {
            return Err(EguiError::InvalidParam {
                what: "indicator".to_owned(),
                reason: format!("{id:?} refuses: {reason}"),
            });
        }
        let backing = manifest_backing(id).ok_or_else(|| EguiError::InvalidParam {
            what: "indicator".to_owned(),
            reason: format!(
                "{stem:?} has no native plugin: pine-backed execution is not wired yet"
            ),
        })?;
        return instantiate_indicator(backing);
    }
    match id {
        INTEGRATION_LITE_ID => Ok(Box::new(IntegrationLiteIndicator::new())),
        FAST_MAS_ID => Ok(Box::new(FastMasIndicator::new())),
        DELTA_VOLUME_PROFILE_ID => Ok(Box::new(DeltaVolumeProfileIndicator::new())),
        REGIME_ID => Ok(Box::new(KernelIndicator::monitor(
            REGIME_ID,
            "Market Regime Monitor",
            "eit",
        )?)),
        HEALTH_ID => Ok(Box::new(KernelIndicator::monitor(
            HEALTH_ID,
            "MA Health Monitor",
            "vwma",
        )?)),
        other => {
            let registry = shared_registry().map_err(|error| EguiError::Dependency {
                reason: error.to_string(),
            })?;
            if registry.get(other).is_some() {
                Ok(Box::new(KernelIndicator::new(other)?))
            } else {
                Err(EguiError::InvalidParam {
                    what: "indicator".to_owned(),
                    reason: format!("unknown indicator {other:?}"),
                })
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/indicators.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/pickers.rs"]
mod picker_tests;

#[cfg(test)]
#[path = "../tests/indicator_manifest.rs"]
mod indicator_manifest_tests;

#[cfg(test)]
#[path = "../tests/kernel_plugins.rs"]
mod kernel_plugin_tests;

#[cfg(test)]
#[path = "../tests/settings_dials.rs"]
mod settings_dials_tests;