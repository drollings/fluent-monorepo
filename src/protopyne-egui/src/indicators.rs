//! Indicator plugin system: the searchable indicator catalog and the
//! [`ChartIndicator`] contract every hot-slotted indicator implements.
//!
//! One catalog serves the Indicator Search & Management modal (My Scripts,
//! Technical, Financial) over the shared [`CatalogModal`]. One plugin trait
//! serves every indicator kind — the two scripted modules
//! ([`IntegrationLiteIndicator`], [`FastMasIndicator`]) and
//! [`KernelIndicator`], which hosts any kernel from the shared
//! [`builtin_registry`]. A plugin is stateless behavior: it owns no
//! configuration, so the hot-slot stack stays the single settings authority
//! and an edit and its recompute can never diverge. Every plugin reads its
//! configuration through [`ChartIndicator::evaluate`], routing display legs
//! through the shared fenced kernel frame in `topology`.
//!
//! Indicator isolation is structural: each plugin exposes its own
//! [`default_config`](ChartIndicator::default_config) schema and its own
//! pane-plan node namespace, so two plugins never share a field, a
//! fingerprint, or a cache key.

use fluent_wvr::prelude::*;
use fluent_wvr::component_downcast_ref;
use protopyne_chart_model::ChartRow;
use protopyne_kernels::{ParamSet, builtin_registry};
use protopyne_types::{Validate, check_for_execute, valid_output};

use fluent_egui::{
    CatalogModal, EguiError, IndicatorStyle, ReadoutValue, SlotAttachment, ThemeConfig, Viewport,
    cache_key, needs_column_aggregation, paint_oscillator_styled,
    proportional_line_points, sampled_line_points, translate_points, visible_rows,
};

use crate::fast_mas::{ALMA_OFFSET, ALMA_SIGMA, FastMasConfig, evaluate_fast_mas};
use crate::delta_volume_profile::{DELTA_VOLUME_PROFILE_ID, DeltaVolumeProfileIndicator};
use crate::integration_lite::{IntegrationLiteConfig, integration_lite_sub_pane};
use crate::topology::{kernel_preview_frame, kernel_series};

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
        [Self::All, Self::Favorites, Self::MyScripts, Self::Technical, Self::Financial]
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
pub struct IndicatorSeries {
    /// Pane-plan node this series computes (cache-key scope).
    pub node: String,
    /// Display name shown in readouts and legends.
    pub name: String,
    /// Per-bar display values, holes preserved.
    pub values: Vec<Option<f64>>,
    /// Theme style key resolved against the theme at paint time.
    pub style: String,
    /// Where this series paints.
    pub attachment: SlotAttachment,
    /// Cache key binding the node to its parameter fingerprint.
    pub key: u64,
}

impl IndicatorSeries {
    /// The most recent resolved value, holes skipped.
    #[must_use]
    pub fn last_value(&self) -> Option<f64> {
        self.values.iter().rev().find_map(|slot| *slot)
    }
}

/// One plugin evaluation: the series produced, in plan-node order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct IndicatorEvaluation {
    /// Series produced by the evaluation.
    pub series: Vec<IndicatorSeries>,
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

    /// Evaluate `config` over `rows`, one series per plan node.
    fn evaluate(
        &self,
        rows: &[ChartRow],
        config: &dyn Component,
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
        paint_series(painter, evaluation, viewport, origin, base, width_px, theme, style)
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
        paint_series(painter, evaluation, viewport, origin, base, width_px, theme, style)
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
fn paint_series(
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
        let len = series.values.len().saturating_sub(base);
        let window = visible_rows(&series.values, base, len);
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
fn wrong_config(id: &str, expected: &str) -> EguiError {
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
        Ok(INTEGRATION_LITE_NODES.iter().map(|node| (*node).to_owned()).collect())
    }

    fn evaluate(
        &self,
        rows: &[ChartRow],
        config: &dyn Component,
    ) -> Result<IndicatorEvaluation, EguiError> {
        let config = component_downcast_ref::<IntegrationLiteConfig>(config)
            .ok_or_else(|| wrong_config(self.id(), "IntegrationLite"))?;
        let pane = integration_lite_sub_pane(rows, config)?;
        let fingerprint = config_fingerprint(config);
        let legs = [
            ("Fast", pane.fast_bounded, "fast"),
            ("Slow", pane.slow_bounded, "slow"),
            ("Ribbon Health", pane.ribbon_health_bounded, "fast"),
            ("MA Health", pane.ma_health_bounded, "slow"),
        ];
        let series = INTEGRATION_LITE_NODES
            .iter()
            .zip(legs)
            .map(|(node, (name, values, style))| IndicatorSeries {
                node: (*node).to_owned(),
                name: name.to_owned(),
                values,
                style: style.to_owned(),
                attachment: SlotAttachment::SubPane,
                key: cache_key(node, fingerprint),
            })
            .collect();
        Ok(IndicatorEvaluation { series })
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
        Ok(fast_mas_nodes(config).into_iter().map(str::to_owned).collect())
    }

    fn evaluate(
        &self,
        rows: &[ChartRow],
        config: &dyn Component,
    ) -> Result<IndicatorEvaluation, EguiError> {
        let config = component_downcast_ref::<FastMasConfig>(config)
            .ok_or_else(|| wrong_config(self.id(), "FastMas"))?;
        let view = evaluate_fast_mas(rows, config)?;
        let fingerprint = config_fingerprint(config);
        let series = fast_mas_nodes(config)
            .into_iter()
            .zip(view.lines)
            .map(|(node, line)| IndicatorSeries {
                node: node.to_owned(),
                name: line.name,
                values: line.values,
                style: line.style,
                attachment: SlotAttachment::MainOverlay,
                key: cache_key(node, fingerprint),
            })
            .collect();
        Ok(IndicatorEvaluation { series })
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
        paint_series(painter, evaluation, viewport, origin, base, width_px, theme, style)
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

/// One kernel plugin's configuration: which kernel, its length, and its
/// paint style.
#[derive(
    Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize, FieldAccess, Describable
)]
#[builder(start_fn = new)]
pub struct KernelIndicatorConfig {
    /// Kernel id resolved against the builtin registry.
    #[field(desc = "Kernel id")]
    #[builder(default = default_kernel())]
    pub kernel: String,
    /// Kernel length parameter.
    #[field(desc = "Kernel length")]
    #[builder(default = default_kernel_length())]
    pub length: i64,
    /// Line theme style key.
    #[field(desc = "Line theme style key")]
    #[builder(default = default_kernel_style())]
    pub style: String,
}

impl KernelIndicatorConfig {
    /// Fail-closed self-consistency check: a registered kernel, a positive
    /// length, and a non-empty style key.
    pub fn validate(&self) -> Result<(), EguiError> {
        if self.kernel.trim().is_empty() {
            return Err(EguiError::InvalidParam {
                what: "kernel".to_owned(),
                reason: "kernel id must not be empty".to_owned(),
            });
        }
        let registry =
            builtin_registry().map_err(|error| EguiError::Dependency { reason: error.to_string() })?;
        if registry.get(&self.kernel).is_none() {
            return Err(EguiError::InvalidParam {
                what: "kernel".to_owned(),
                reason: format!("kernel not registered: {}", self.kernel),
            });
        }
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
        Ok(())
    }
}

impl Default for KernelIndicatorConfig {
    fn default() -> Self {
        Self::new().build()
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
        Ok(valid_output(format!("kernel indicator ready ({})", self.kernel)))
    }
}

impl_component!(KernelIndicatorConfig);

/// The kernel plugin: hosts any builtin kernel (or a domain monitor bound to
/// one) behind the shared plugin contract.
pub struct KernelIndicator {
    id: String,
    title: String,
    kernel: String,
}

impl KernelIndicator {
    /// Create a plugin bound to `kernel`; refuses an unregistered kernel.
    pub fn new(kernel: &str) -> Result<Self, EguiError> {
        Self::bind(kernel, kernel, kernel)
    }

    /// Create a named monitor plugin bound to `kernel`.
    fn monitor(id: &str, title: &str, kernel: &str) -> Result<Self, EguiError> {
        Self::bind(id, title, kernel)
    }

    /// Bind a plugin to a kernel, refusing one the registry does not hold.
    fn bind(id: &str, title: &str, kernel: &str) -> Result<Self, EguiError> {
        let registry =
            builtin_registry().map_err(|error| EguiError::Dependency { reason: error.to_string() })?;
        if registry.get(kernel).is_none() {
            return Err(EguiError::InvalidParam {
                what: "kernel".to_owned(),
                reason: format!("kernel not registered: {kernel}"),
            });
        }
        Ok(Self { id: id.to_owned(), title: title.to_owned(), kernel: kernel.to_owned() })
    }
}

/// The parameter set for one kernel: length for every kernel, plus the ALMA
/// reference offset and sigma when the kernel is `alma`.
fn kernel_params(kernel: &str, length: i64) -> ParamSet {
    let params = ParamSet::new().with_int("length", length);
    if kernel == "alma" {
        params.with_float("offset", ALMA_OFFSET).with_float("sigma", ALMA_SIGMA)
    } else {
        params
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
        SlotAttachment::SubPane
    }

    fn default_config(&self) -> Box<dyn Component> {
        Box::new(KernelIndicatorConfig::new().kernel(self.kernel.clone()).build())
    }

    fn plan_nodes(&self, _config: &dyn Component) -> Result<Vec<String>, EguiError> {
        Ok(vec![format!("kernel:{}", self.id)])
    }

    fn evaluate(
        &self,
        rows: &[ChartRow],
        config: &dyn Component,
    ) -> Result<IndicatorEvaluation, EguiError> {
        let config = component_downcast_ref::<KernelIndicatorConfig>(config)
            .ok_or_else(|| wrong_config(self.id(), "KernelIndicator"))?;
        config.validate()?;
        let frame = kernel_preview_frame(rows);
        let values = kernel_series(&config.kernel, &kernel_params(&config.kernel, config.length), &frame)?;
        let node = format!("kernel:{}", self.id);
        let key = cache_key(&node, config_fingerprint(config));
        Ok(IndicatorEvaluation {
            series: vec![IndicatorSeries {
                node,
                name: config.kernel.clone(),
                values,
                style: config.style.clone(),
                attachment: SlotAttachment::SubPane,
                key,
            }],
        })
    }
}

/// Build the indicator catalog: the two scripted indicators under My Scripts,
/// every builtin kernel under Technical, and the domain monitors under
/// Financial.
pub fn build_indicator_catalog() -> Result<CatalogModal<IndicatorCategory>, EguiError> {
    let mut catalog = CatalogModal::new("Indicators");
    catalog.register(INTEGRATION_LITE_ID, "Integration Lite", IndicatorCategory::MyScripts)?;
    catalog.register(FAST_MAS_ID, "Fast MAs", IndicatorCategory::MyScripts)?;
    catalog.register(
        DELTA_VOLUME_PROFILE_ID,
        "Delta Volume Profile",
        IndicatorCategory::MyScripts,
    )?;
    let registry =
        builtin_registry().map_err(|error| EguiError::Dependency { reason: error.to_string() })?;
    for id in registry.ids() {
        catalog.register(&id, &id, IndicatorCategory::Technical)?;
    }
    catalog.register(REGIME_ID, "Market Regime Monitor", IndicatorCategory::Financial)?;
    catalog.register(HEALTH_ID, "MA Health Monitor", IndicatorCategory::Financial)?;
    Ok(catalog)
}

/// Instantiate a plugin by catalog id: the two scripted indicators, the two
/// domain monitors, or any builtin kernel. Unknown ids refuse.
pub fn instantiate_indicator(id: &str) -> Result<Box<dyn ChartIndicator>, EguiError> {
    match id {
        INTEGRATION_LITE_ID => Ok(Box::new(IntegrationLiteIndicator::new())),
        FAST_MAS_ID => Ok(Box::new(FastMasIndicator::new())),
        DELTA_VOLUME_PROFILE_ID => Ok(Box::new(DeltaVolumeProfileIndicator::new())),
        REGIME_ID => {
            Ok(Box::new(KernelIndicator::monitor(REGIME_ID, "Market Regime Monitor", "eit")?))
        }
        HEALTH_ID => Ok(Box::new(KernelIndicator::monitor(HEALTH_ID, "MA Health Monitor", "vwma")?)),
        other => {
            let registry = builtin_registry()
                .map_err(|error| EguiError::Dependency { reason: error.to_string() })?;
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
