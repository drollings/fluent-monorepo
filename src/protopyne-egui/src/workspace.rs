//! Central chart workspace: the terminal's state store and full canvas.
//!
//! [`ChartWorkspace`] owns the active ticker (interned in a scoped
//! [`SymbolIndex`]), the timeframe, range, and chart-type selectors, the
//! indicator hot-slot stack, the pane plan, the pane cache, and the 2D
//! interaction machine. It is the one place the terminal's state lives, so the
//! top bar, the symbol and indicator modals, the hot-slot legend, and the
//! multi-pane canvas all read and write the same truth.
//!
//! History arrives through the neutral [`TerminalGateway`] (`chart_bars_for`),
//! never by reaching into a trading crate; a gateway-less workspace (the demo
//! shell) can be seeded with converted rows directly. Every display number
//! comes from the shared kernels through the [`ChartIndicator`] plugins, so no
//! calculation is re-implemented here.

use std::collections::HashMap;

use fluent_egui::{
    CatalogAction, CatalogModal, DeltaView, EguiError, HotSlotAction, HotSlotStack, InteractionState,
    PaneCache, PanePlan, PointerInput, RibbonView, SettingsTab, SlotAttachment, ThemeConfig,
    TintedCandle, TintedDeltaBar, Viewport, candle_geoms, delta_bar_geoms, opacity_from_transparency,
    paint_candles_tinted, paint_catalog_modal, paint_delta_bars_tinted, paint_hot_slot_stack,
    paint_oscillator, paint_ribbon_fill, paint_slot_settings_modal, resolve_color, ribbon_polygons,
    translate_candles, translate_delta_bars, translate_points, translate_polygons,
};
use fluent_wvr::{FieldAccess, component_downcast_ref};
use protopyne_chart_model::{ChartRow, ChartType, DateRange};
use protopyne_chart_scale::PriceScaleMode;
use protopyne_types::{SymbolIndex, SymKey};
use protopyne_view::{ChartBars, TerminalGateway};

use crate::chart::{ChartConfig, DEFAULT_SYMBOL, TimeframeKey, window_for_range};
use crate::indicators::{
    ChartIndicator, IndicatorCategory, IndicatorEvaluation, IndicatorSeries, build_indicator_catalog,
    instantiate_indicator,
};
use crate::integration_lite::{
    IntegrationLiteConfig, IntegrationLiteSubPane, MomentumPalette, MomentumTint,
    candle_momentum_keys, candle_momentum_series, integration_lite_sub_pane, momentum_tints,
    ribbon_fill_transparency,
};
use crate::widgets::{TopBarAction, paint_top_bar};

/// Stable id of the IntegrationLite slot whose richer sub-pane geometry
/// (delta histogram) the canvas paints alongside the plugin's series.
const INTEGRATION_LITE_ID: &str = "integration_lite";

/// Pixels per bar used when no configured spacing applies.
const DEFAULT_BAR_SPACING: f32 = 8.0;
/// Main chart area height.
const PRICE_PANE_HEIGHT: f32 = 320.0;
/// Sub-pane area height.
const SUB_PANE_HEIGHT: f32 = 140.0;
/// Price scale base value for the main chart area.
const PRICE_BASE: f64 = 100.0;
/// Sub-pane oscillator band: the bounded `[-100, +100]` legs plus headroom.
const SUB_MIN: f64 = -110.0;
/// Sub-pane upper bound.
const SUB_MAX: f64 = 110.0;
/// Horizontal pane width the interaction envelope is bound to.
const PANE_WIDTH_PX: f32 = 640.0;
/// EIT adaptive smoothing used for the candle tint when no IntegrationLite
/// slot supplies one.
const DEFAULT_EIT_ALPHA: f64 = 0.14;

/// One asset class tab in the Symbol Selection Modal.
///
/// `All` is a selector, not an entry category, so
/// [`entry`](Self::entry) maps it to `None` (the generic modal renders its own
/// `All` tab).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AssetClass {
    /// Every instrument, regardless of class.
    All,
    /// Equities and equity ETFs.
    Stocks,
    /// Funds and commodity trusts.
    Funds,
    /// Futures contracts.
    Futures,
    /// Digital assets.
    Crypto,
}

impl AssetClass {
    /// Every tab, in render order.
    #[must_use]
    pub fn all() -> [Self; 5] {
        [Self::All, Self::Stocks, Self::Funds, Self::Futures, Self::Crypto]
    }

    /// The tab label the modal renders.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Stocks => "Stocks",
            Self::Funds => "Funds",
            Self::Futures => "Futures",
            Self::Crypto => "Crypto",
        }
    }

    /// The entry category this tab selects, if it is a real category.
    #[must_use]
    pub fn entry(self) -> Option<Self> {
        match self {
            Self::All => None,
            other => Some(other),
        }
    }
}

/// The shipped symbol catalog: `(symbol, name, class)`, the default ticker
/// first.
const SYMBOL_CATALOG: &[(&str, &str, AssetClass)] = &[
    ("GLD", "SPDR Gold Shares", AssetClass::Funds),
    ("SPY", "SPDR S&P 500 ETF Trust", AssetClass::Stocks),
    ("QQQ", "Invesco QQQ Trust", AssetClass::Stocks),
    ("BTC-USD", "Bitcoin", AssetClass::Crypto),
    ("ES", "E-mini S&P 500", AssetClass::Futures),
];

/// Build the Symbol Selection Modal catalog, spanning every asset class.
pub fn build_symbol_catalog() -> Result<CatalogModal<AssetClass>, EguiError> {
    let mut catalog = CatalogModal::new("Symbols");
    for (symbol, name, class) in SYMBOL_CATALOG {
        catalog.register(symbol, name, *class)?;
    }
    Ok(catalog)
}

/// The real asset-class tabs (everything but the `All` selector).
fn symbol_categories() -> Vec<(AssetClass, &'static str)> {
    AssetClass::all()
        .into_iter()
        .filter_map(|class| class.entry().map(|category| (category, category.label())))
        .collect()
}

/// The indicator-catalog tabs that are real categories.
fn indicator_categories() -> Vec<(IndicatorCategory, &'static str)> {
    [
        IndicatorCategory::MyScripts,
        IndicatorCategory::Technical,
        IndicatorCategory::Financial,
    ]
    .into_iter()
    .map(|category| (category, category.label()))
    .collect()
}

/// Convert gateway chart bars into display rows (same shape, no `Decimal`
/// leg — the bars are already display-native).
fn chart_rows_of(bars: &ChartBars) -> Vec<ChartRow> {
    bars.rows
        .iter()
        .map(|bar| ChartRow {
            time: bar.time,
            open: bar.open,
            high: bar.high,
            low: bar.low,
            close: bar.close,
            volume: bar.volume,
        })
        .collect()
}

/// The padded price extent of a row window; a degenerate window falls back to
/// a unit range so a viewport always constructs.
fn price_extent(rows: &[ChartRow]) -> (f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for row in rows {
        lo = lo.min(row.low);
        hi = hi.max(row.high);
    }
    if !lo.is_finite() || !hi.is_finite() || lo >= hi {
        return (0.0, 1.0);
    }
    let pad = (hi - lo) * 0.05;
    (lo - pad, hi + pad)
}

/// Aggregate display rows upward by `group` (trailing-aligned, like exchange
/// resampling): open from the first row, close/time from the last, high/low as
/// extremes, volume summed. A leading partial chunk is dropped so every output
/// row covers exactly `group` inputs; `group <= 1` returns the rows unchanged.
fn aggregate_rows(rows: &[ChartRow], group: usize) -> Vec<ChartRow> {
    if group <= 1 {
        return rows.to_vec();
    }
    let skip = rows.len() % group;
    rows[skip..]
        .chunks(group)
        .filter(|chunk| chunk.len() == group)
        .map(|chunk| {
            let first = &chunk[0];
            let last = &chunk[group - 1];
            let mut high = first.high;
            let mut low = first.low;
            let mut volume = 0_u64;
            for row in chunk {
                high = high.max(row.high);
                low = low.min(row.low);
                volume = volume.saturating_add(row.volume);
            }
            ChartRow { time: last.time, open: first.open, high, low, close: last.close, volume }
        })
        .collect()
}

/// Window a trend ribbon to `[start, start + len)` for pane-local painting.
///
/// Ribbon legs line up with the full history; the price pane paints the
/// trailing range window, so its ribbon must be sliced to the same bars
/// (candle geometry and ribbon polygons then share one index origin). An
/// out-of-range slice yields an empty band rather than a misaligned one.
fn windowed_ribbon(ribbon: &RibbonView, start: usize, len: usize) -> RibbonView {
    let slice = |values: &[Option<f64>]| {
        values.get(start..start + len).map_or_else(Vec::new, <[_]>::to_vec)
    };
    RibbonView {
        upper: slice(&ribbon.upper),
        lower: slice(&ribbon.lower),
        bullish: ribbon.bullish.get(start..start + len).map_or_else(Vec::new, <[_]>::to_vec),
        alpha: ribbon.alpha,
    }
}

/// Window an indicator evaluation to `[start, start + len)` for pane-local
/// painting: the plugins evaluate over the full history, but the canvas paints
/// the trailing range window, so every series is sliced to the same bars the
/// candles map (one index origin across the price pane and the sub-pane).
fn windowed_evaluation(
    evaluation: &IndicatorEvaluation,
    start: usize,
    len: usize,
) -> IndicatorEvaluation {
    IndicatorEvaluation {
        series: evaluation
            .series
            .iter()
            .map(|series| IndicatorSeries {
                values: series.values.get(start..start + len).map_or_else(Vec::new, <[_]>::to_vec),
                ..series.clone()
            })
            .collect(),
    }
}

/// Window a delta histogram to `[start, start + len)` (see
/// [`windowed_evaluation`]).
fn windowed_delta(delta: &DeltaView, start: usize, len: usize) -> DeltaView {
    let slice = |values: &[Option<f64>]| {
        values.get(start..start + len).map_or_else(Vec::new, <[_]>::to_vec)
    };
    DeltaView { buy_top: slice(&delta.buy_top), sell_bottom: slice(&delta.sell_bottom) }
}

/// Resolve a ported momentum gradient tint to its paint color at the tint's
/// own Pine transparency.
fn tint_color(tint: MomentumTint) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        tint.red,
        tint.green,
        tint.blue,
        opacity_from_transparency(tint.transparency),
    )
}

/// The terminal's central state store: active symbol, selectors, hot-slot
/// stack, pane plan, pane cache, and the 2D interaction machine.
pub struct ChartWorkspace {
    symbol_index: SymbolIndex,
    active_symbol: SymKey,
    timeframe: TimeframeKey,
    range: DateRange,
    chart_type: ChartType,
    config: ChartConfig,
    theme: ThemeConfig,
    plan: PanePlan,
    cache: PaneCache,
    stack: HotSlotStack,
    plugins: Vec<Box<dyn ChartIndicator>>,
    evaluations: HashMap<String, IndicatorEvaluation>,
    sub_pane: Option<IntegrationLiteSubPane>,
    momentum_keys: Vec<&'static str>,
    tints: Vec<Option<MomentumTint>>,
    rows: Vec<ChartRow>,
    histories: HashMap<TimeframeKey, Vec<ChartRow>>,
    interaction: Option<InteractionState>,
    main: Option<Viewport>,
    sub: Option<Viewport>,
    symbol_modal: Option<CatalogModal<AssetClass>>,
    indicator_modal: Option<CatalogModal<IndicatorCategory>>,
}

impl ChartWorkspace {
    /// Boot the terminal: the default ticker (`GLD`), the default timeframe,
    /// and the two scripted indicators hot-slotted. No history yet — pull it
    /// with [`with_gateway`](Self::with_gateway) or seed it with
    /// [`with_rows`](Self::with_rows).
    pub fn new() -> Result<Self, EguiError> {
        let mut symbol_index = SymbolIndex::new();
        let active_symbol = symbol_index.intern(DEFAULT_SYMBOL).map_err(|error| {
            EguiError::InvalidParam { what: "symbol".to_owned(), reason: error.to_string() }
        })?;
        let mut workspace = Self {
            symbol_index,
            active_symbol,
            timeframe: TimeframeKey::default(),
            range: DateRange::All,
            chart_type: ChartType::Candles,
            config: ChartConfig::default(),
            theme: ThemeConfig::default(),
            plan: PanePlan::new(),
            cache: PaneCache::new(),
            stack: HotSlotStack::new(),
            plugins: Vec::new(),
            evaluations: HashMap::new(),
            sub_pane: None,
            momentum_keys: Vec::new(),
            tints: Vec::new(),
            rows: Vec::new(),
            histories: HashMap::new(),
            interaction: None,
            main: None,
            sub: None,
            symbol_modal: None,
            indicator_modal: None,
        };
        workspace.slot_indicator(INTEGRATION_LITE_ID)?;
        workspace.slot_indicator("fast_mas")?;
        Ok(workspace)
    }

    /// Boot and pull the first history through `gateway`.
    pub fn with_gateway(gateway: &dyn TerminalGateway) -> Result<Self, EguiError> {
        let mut workspace = Self::new()?;
        workspace.refresh(gateway)?;
        Ok(workspace)
    }

    /// Boot and seed a history directly (the demo's CSV path), keyed to the
    /// default timeframe.
    pub fn with_rows(rows: Vec<ChartRow>) -> Result<Self, EguiError> {
        let mut workspace = Self::new()?;
        workspace.set_rows(rows)?;
        Ok(workspace)
    }

    /// Seed one timeframe's history into the workspace store and, when it is
    /// the active timeframe, select it. Coarser timeframes (`1H`/`4H`) are
    /// resampled from the seeded `15M` slot by [`history_for`](Self::history_for),
    /// so a caller only needs to seed the native slots it has.
    pub fn seed_history(
        &mut self,
        timeframe: TimeframeKey,
        rows: Vec<ChartRow>,
    ) -> Result<(), EguiError> {
        self.histories.insert(timeframe, rows);
        if self.timeframe == timeframe {
            self.select_timeframe(timeframe)?;
        }
        Ok(())
    }

    /// The active ticker's spelling.
    #[must_use]
    pub fn active_symbol(&self) -> &str {
        self.symbol_index.resolve(self.active_symbol).unwrap_or(DEFAULT_SYMBOL)
    }

    /// The selected timeframe key.
    #[must_use]
    pub fn timeframe(&self) -> TimeframeKey {
        self.timeframe
    }

    /// The selected range preset.
    #[must_use]
    pub fn range(&self) -> DateRange {
        self.range
    }

    /// The selected chart type.
    #[must_use]
    pub fn chart_type(&self) -> ChartType {
        self.chart_type
    }

    /// The current history rows.
    #[must_use]
    pub fn rows(&self) -> &[ChartRow] {
        &self.rows
    }

    /// The main chart area viewport, if bound.
    #[must_use]
    pub fn main_viewport(&self) -> Option<Viewport> {
        self.main
    }

    /// The sub-pane viewport, if bound.
    #[must_use]
    pub fn sub_viewport(&self) -> Option<Viewport> {
        self.sub
    }

    /// The slotted indicator ids, in insertion order.
    #[must_use]
    pub fn slot_ids(&self) -> Vec<String> {
        self.stack.slot_ids()
    }

    /// The number of evaluated series published into the pane cache.
    #[must_use]
    pub fn cached_series(&self) -> usize {
        self.cache.len()
    }

    /// The DAG plan's execution order (every registered node).
    pub fn plan_order(&self) -> Result<Vec<String>, EguiError> {
        self.plan.order()
    }

    /// Whether a slot is currently visible (Eye Icon state).
    #[must_use]
    pub fn slot_visible(&self, id: &str) -> Option<bool> {
        self.stack.visible(id)
    }

    /// Toggle a slot's visibility (the Eye Icon action).
    pub fn toggle_visibility(&mut self, id: &str) -> Result<(), EguiError> {
        self.stack.toggle_visibility(id)
    }

    /// Open one slot's isolated settings pane (the Six-Sided Bolt action).
    pub fn open_settings(&mut self, id: &str) -> Result<(), EguiError> {
        self.stack.open_settings(id)
    }

    /// Select the settings tab for one slot.
    pub fn set_settings_tab(&mut self, id: &str, tab: SettingsTab) -> Result<(), EguiError> {
        self.stack.set_settings_tab(id, tab)
    }

    /// Write one settings field on a slot through its own `FieldAccess`
    /// contract, then recompute so the edit is live.
    pub fn set_slot_field(&mut self, id: &str, name: &str, value: &str) -> Result<(), EguiError> {
        self.stack.set_slot_field(id, name, value)?;
        self.recompute()
    }

    /// Remove an indicator: close its pane handle, unslot it, drop its
    /// evaluation, and refresh the derived pane/tint state.
    pub fn remove_indicator(&mut self, id: &str) -> Result<(), EguiError> {
        self.stack.remove(id)?;
        self.plugins.retain(|plugin| plugin.id() != id);
        self.evaluations.remove(id);
        if id == INTEGRATION_LITE_ID {
            self.sub_pane = None;
            self.momentum_keys.clear();
            self.tints.clear();
        }
        if self.rows.is_empty() {
            return Ok(());
        }
        self.sub_pane = self.compute_sub_pane()?;
        self.recompute_tint()
    }

    /// Pull fresh bars for the active symbol and timeframe through `gateway`,
    /// rebind the viewports, and recompute every active indicator.
    pub fn refresh(&mut self, gateway: &dyn TerminalGateway) -> Result<(), EguiError> {
        let bars = gateway
            .chart_bars_for(self.active_symbol(), self.timeframe.label())
            .map_err(|error| EguiError::Dependency { reason: error.to_string() })?;
        self.rows = chart_rows_of(&bars);
        self.histories.insert(self.timeframe, self.rows.clone());
        self.rebind_viewports()?;
        self.recompute()
    }

    /// Seed a history for the active timeframe directly and recompute (no
    /// gateway). Distinct timeframes keep their own histories; the switch
    /// path re-reads the store.
    pub fn set_rows(&mut self, rows: Vec<ChartRow>) -> Result<(), EguiError> {
        self.seed_history(self.timeframe, rows)
    }

    /// Switch the active ticker and pull its history through `gateway`.
    pub fn switch_symbol(
        &mut self,
        symbol: &str,
        gateway: &dyn TerminalGateway,
    ) -> Result<(), EguiError> {
        self.set_symbol(symbol)?;
        self.refresh(gateway)
    }

    /// Switch the active timeframe and pull its history through `gateway`.
    pub fn switch_timeframe(
        &mut self,
        timeframe: TimeframeKey,
        gateway: &dyn TerminalGateway,
    ) -> Result<(), EguiError> {
        self.timeframe = timeframe;
        self.refresh(gateway)
    }

    /// Select a timeframe from the seeded history store without a gateway,
    /// resampling coarser keys from the `15M` slot; the active candles and
    /// every indicator recompute from the switched history.
    pub fn select_timeframe(&mut self, timeframe: TimeframeKey) -> Result<(), EguiError> {
        let rows = self.history_for(timeframe)?;
        self.rows = rows;
        self.timeframe = timeframe;
        self.rebind_viewports()?;
        self.recompute()
    }

    /// The seeded history for `timeframe`, resampling coarser keys from the
    /// `15M` slot by whole groups. `1M`/`5M`/`15M` read their own slots; a
    /// missing slot refuses by name rather than serving another timeframe.
    fn history_for(&self, timeframe: TimeframeKey) -> Result<Vec<ChartRow>, EguiError> {
        if let Some(rows) = self.histories.get(&timeframe) {
            return Ok(rows.clone());
        }
        if timeframe.m15_group() > 1 {
            if let Some(base) = self.histories.get(&TimeframeKey::M15) {
                return Ok(aggregate_rows(base, timeframe.m15_group()));
            }
        }
        Err(EguiError::InvalidParam {
            what: "timeframe".to_owned(),
            reason: format!("no loaded history for {}", timeframe.label()),
        })
    }

    /// Step the 2D interaction machine and lock the sub-pane time axis to the
    /// main chart area's.
    pub fn step_interaction(&mut self, dt: f64, events: &[PointerInput]) {
        let Some(main) = self.main else {
            return;
        };
        let Some(machine) = self.interaction.as_mut() else {
            return;
        };
        let next = machine.step(dt, &main, events);
        self.main = Some(next);
        if let Some(sub) = self.sub {
            if let Ok(synced) = sub.sync_time_axis(&next) {
                self.sub = Some(synced);
            }
        }
    }

    /// Intern a symbol and set it active without pulling history.
    fn set_symbol(&mut self, symbol: &str) -> Result<(), EguiError> {
        let key = self.symbol_index.intern(symbol).map_err(|error| EguiError::InvalidParam {
            what: "symbol".to_owned(),
            reason: error.to_string(),
        })?;
        self.active_symbol = key;
        self.config.set_field("symbol", symbol).map_err(|error| EguiError::InvalidParam {
            what: "symbol".to_owned(),
            reason: error.to_string(),
        })?;
        Ok(())
    }

    /// Slot one indicator plugin by catalog id; a duplicate is a no-op.
    fn slot_indicator(&mut self, id: &str) -> Result<(), EguiError> {
        if self.stack.contains(id) {
            return Ok(());
        }
        let plugin = instantiate_indicator(id)?;
        let config = plugin.default_config();
        let nodes = plugin.plan_nodes(&*config)?;
        let node_refs: Vec<&str> = nodes.iter().map(String::as_str).collect();
        let fingerprint = common_core::hash::fnv1a64(id.as_bytes());
        self.stack.insert(
            plugin.id(),
            plugin.title(),
            plugin.attachment(),
            config,
            &node_refs,
            &[],
            fingerprint,
            &mut self.plan,
        )?;
        self.plugins.push(plugin);
        Ok(())
    }

    /// Rebind the main and sub-pane viewports (and the interaction bounds) to
    /// the current rows and range.
    fn rebind_viewports(&mut self) -> Result<(), EguiError> {
        let window = window_for_range(&self.rows, self.range, self.timeframe.seconds());
        let (lo, hi) = price_extent(window);
        let spacing = if self.config.bar_spacing > 0.0 {
            self.config.bar_spacing as f32
        } else {
            DEFAULT_BAR_SPACING
        };
        let main = Viewport::new(
            PriceScaleMode::Normal,
            lo,
            hi,
            PRICE_PANE_HEIGHT,
            spacing,
            PRICE_BASE,
            false,
        )?;
        let sub = Viewport::new(
            PriceScaleMode::Normal,
            SUB_MIN,
            SUB_MAX,
            SUB_PANE_HEIGHT,
            spacing,
            0.0,
            false,
        )?;
        self.main = Some(main);
        self.sub = Some(sub);
        self.interaction = if self.rows.is_empty() {
            None
        } else {
            Some(
                InteractionState::new(lo, hi)?
                    .with_horizontal_bounds(self.rows.len(), PANE_WIDTH_PX)?,
            )
        };
        Ok(())
    }

    /// Evaluate every active indicator into the pane cache and readouts, then
    /// rebuild the IntegrationLite delta histogram and the candle/delta tint
    /// for the canvas.
    fn recompute(&mut self) -> Result<(), EguiError> {
        if self.rows.is_empty() {
            self.evaluations.clear();
            self.sub_pane = None;
            self.momentum_keys.clear();
            self.tints.clear();
            return Ok(());
        }
        for index in 0..self.plugins.len() {
            let id = self.plugins[index].id().to_owned();
            let evaluation = {
                let config = self.stack.config(&id).ok_or_else(|| EguiError::InvalidParam {
                    what: id.clone(),
                    reason: "slot owns no configuration".to_owned(),
                })?;
                self.plugins[index].evaluate(&self.rows, config)?
            };
            for series in &evaluation.series {
                self.cache.publish(series.key, series.values.clone());
            }
            let readouts = self.plugins[index].readouts(&evaluation);
            self.stack.set_readouts(&id, readouts)?;
            self.evaluations.insert(id, evaluation);
        }
        self.sub_pane = self.compute_sub_pane()?;
        self.recompute_tint()?;
        Ok(())
    }

    /// Rebuild the momentum keys and per-bar candle/delta tint from the pane's
    /// EIT legs and volume conviction. The keys always resolve (falling back
    /// to candle direction), while the gradient tints are absent when the
    /// theme's four momentum hues cannot resolve — the canvas then paints the
    /// discrete keys instead of a substituted color.
    fn recompute_tint(&mut self) -> Result<(), EguiError> {
        let Some(pane) = &self.sub_pane else {
            self.momentum_keys.clear();
            self.tints.clear();
            return Ok(());
        };
        self.momentum_keys = candle_momentum_keys(&self.rows, &pane.ribbon.bullish)?;
        let eit_alpha = self
            .stack
            .config(INTEGRATION_LITE_ID)
            .and_then(|config| component_downcast_ref::<IntegrationLiteConfig>(config))
            .map_or(DEFAULT_EIT_ALPHA, |config| config.eit_alpha);
        self.tints = match MomentumPalette::from_theme(&self.theme) {
            Ok(palette) => {
                let momentum = candle_momentum_series(&self.rows, eit_alpha)?;
                momentum_tints(&self.rows, &momentum, eit_alpha, &palette)?
            }
            Err(_) => Vec::new(),
        };
        Ok(())
    }

    /// The resolved paint color for one bar: the momentum gradient tint when
    /// available, else the discrete trend key resolved against the theme.
    fn bar_color(&self, index: usize, key: &str, theme: &ThemeConfig) -> Result<egui::Color32, EguiError> {
        match self.tints.get(index).copied().flatten() {
            Some(tint) => Ok(tint_color(tint)),
            None => resolve_color(theme, key),
        }
    }

    /// Rebuild the IntegrationLite sub-pane (delta histogram) when that slot
    /// is present and its config downcasts.
    fn compute_sub_pane(&self) -> Result<Option<IntegrationLiteSubPane>, EguiError> {
        if !self.stack.contains(INTEGRATION_LITE_ID) {
            return Ok(None);
        }
        let Some(config) = self.stack.config(INTEGRATION_LITE_ID) else {
            return Ok(None);
        };
        let Some(config) = component_downcast_ref::<IntegrationLiteConfig>(config) else {
            return Ok(None);
        };
        Ok(Some(integration_lite_sub_pane(&self.rows, config)?))
    }

    /// The one settings pane currently open, if any.
    fn open_settings_id(&self) -> Option<String> {
        self.stack.slot_ids().into_iter().find(|id| self.stack.settings_open(id))
    }

    /// Dispatch one top-bar action.
    fn apply_top_bar_action(
        &mut self,
        action: TopBarAction,
        gateway: Option<&dyn TerminalGateway>,
    ) -> Result<(), EguiError> {
        match action {
            TopBarAction::OpenSymbolSearch => self.symbol_modal = Some(build_symbol_catalog()?),
            TopBarAction::OpenIndicatorSearch => {
                self.indicator_modal = Some(build_indicator_catalog()?);
            }
            TopBarAction::PickedSymbol(symbol) => self.apply_symbol_pick(&symbol, gateway)?,
            TopBarAction::PickedTimeframe(timeframe) => match gateway {
                Some(gateway) => self.switch_timeframe(timeframe, gateway)?,
                None => self.select_timeframe(timeframe)?,
            },
            TopBarAction::PickedRange(range) => {
                self.range = range;
                self.rebind_viewports()?;
            }
            TopBarAction::PickedChartType(chart_type) => self.chart_type = chart_type,
        }
        Ok(())
    }

    /// Apply a symbol pick with or without a gateway to pull through.
    fn apply_symbol_pick(
        &mut self,
        symbol: &str,
        gateway: Option<&dyn TerminalGateway>,
    ) -> Result<(), EguiError> {
        match gateway {
            Some(gateway) => self.switch_symbol(symbol, gateway),
            None => self.set_symbol(symbol),
        }
    }

    /// Apply one hot-slot action. The painter already flipped visibility and
    /// opened settings directly on the stack; the shell owns removal.
    fn apply_slot_action(&mut self, action: HotSlotAction) {
        match action {
            HotSlotAction::ClosedSettings(id) => {
                let _ = self.stack.close_settings(&id);
            }
            HotSlotAction::Removed(id) => {
                let _ = self.remove_indicator(&id);
            }
            HotSlotAction::ToggledVisibility(_)
            | HotSlotAction::OpenedSettings(_)
            | HotSlotAction::SelectedCard(_) => {}
        }
    }

    /// Paint the main chart area and the sub-pane, stepping the interaction
    /// machine from this frame's pointer gestures first.
    fn paint_canvas(&mut self, ui: &mut egui::Ui) -> Result<(), EguiError> {
        let width = ui.available_width().max(PANE_WIDTH_PX);
        let (main_rect, response) =
            ui.allocate_exact_size(egui::vec2(width, PRICE_PANE_HEIGHT), egui::Sense::drag());
        self.step_from_response(ui, &response, main_rect);

        let theme = self.theme.clone();
        let background = resolve_color(&theme, "background")?;
        let body_width = (self.config.bar_spacing as f32 * 0.7).max(1.0);
        // One trailing range window for every layer (candles, overlays, and
        // the sub-pane), so the price pane and the oscillator share a time
        // axis instead of one showing the newest and the other the oldest bars.
        let window = window_for_range(&self.rows, self.range, self.timeframe.seconds());
        let window_start = self.rows.len().saturating_sub(window.len());

        let painter = ui.painter_at(main_rect);
        painter.rect_filled(main_rect, 0.0, background);
        if let Some(main) = self.main {
            // The trend ribbon (IntegrationLite's main-pane leg) and the
            // candle tint both read the same windowed trend keys.
            let ribbon = self
                .sub_pane
                .as_ref()
                .map(|pane| windowed_ribbon(&pane.ribbon, window_start, window.len()));
            let trend: Vec<bool> = ribbon
                .as_ref()
                .map_or_else(|| vec![false; window.len()], |band| band.bullish.clone());
            if let Some(band) = &ribbon {
                if self.sub_pane.as_ref().is_some_and(|pane| pane.show_ribbon)
                    && self.stack.visible(INTEGRATION_LITE_ID) == Some(true)
                {
                    let placed = translate_polygons(
                        ribbon_polygons(band, &main),
                        (main_rect.left(), main_rect.top()),
                    );
                    let alpha = f32::from(opacity_from_transparency(ribbon_fill_transparency(
                        band.alpha,
                    ))) / 255.0;
                    paint_ribbon_fill(&painter, &placed, &theme, alpha)?;
                }
            }
            if self.chart_type == ChartType::Candles {
                let geoms = candle_geoms(window, &trend, &main)?;
                let placed = translate_candles(geoms, (main_rect.left(), main_rect.top()));
                let keys = candle_momentum_keys(window, &trend)?;
                let mut candles = Vec::with_capacity(placed.len());
                for geom in &placed {
                    let key = keys.get(geom.index).copied().unwrap_or("bull");
                    let color = self.bar_color(window_start + geom.index, key, &theme)?;
                    candles.push(TintedCandle { geom: *geom, color });
                }
                paint_candles_tinted(&painter, &candles, body_width);
            } else {
                // Line/Area (and any other display-row picture) render the
                // close series; Heikin smoothing needs the core bar pipeline.
                let points: Vec<Option<(f32, f32)>> = window
                    .iter()
                    .enumerate()
                    .map(|(index, row)| {
                        main.price_to_y(row.close).map(|y| (main.index_to_x(index), y))
                    })
                    .collect();
                let placed = translate_points(points, (main_rect.left(), main_rect.top()));
                paint_oscillator(&painter, &placed, "fast", &theme, 1.0)?;
            }
            for plugin in &self.plugins {
                if plugin.attachment() != SlotAttachment::MainOverlay
                    || self.stack.visible(plugin.id()) != Some(true)
                {
                    continue;
                }
                if let Some(evaluation) = self.evaluations.get(plugin.id()) {
                    let windowed = windowed_evaluation(evaluation, window_start, window.len());
                    plugin.paint_main_overlay(
                        &painter,
                        &windowed,
                        &main,
                        main_rect.left_top(),
                        &theme,
                    )?;
                }
            }
        }

        let (sub_rect, _) =
            ui.allocate_exact_size(egui::vec2(width, SUB_PANE_HEIGHT), egui::Sense::hover());
        let sub_painter = ui.painter_at(sub_rect);
        sub_painter.rect_filled(sub_rect, 0.0, background);
        if let Some(sub) = self.sub {
            for plugin in &self.plugins {
                if plugin.attachment() != SlotAttachment::SubPane
                    || self.stack.visible(plugin.id()) != Some(true)
                {
                    continue;
                }
                if let Some(evaluation) = self.evaluations.get(plugin.id()) {
                    let windowed = windowed_evaluation(evaluation, window_start, window.len());
                    plugin.paint_sub_pane(&sub_painter, &windowed, &sub, sub_rect.left_top(), &theme)?;
                }
            }
            if let Some(pane) = &self.sub_pane {
                let delta = windowed_delta(&pane.delta_volume, window_start, window.len());
                let bars = delta_bar_geoms(&delta, &sub)?;
                let placed = translate_delta_bars(bars, (sub_rect.left(), sub_rect.top()));
                let mut tinted = Vec::with_capacity(placed.len());
                for geom in &placed {
                    let global = window_start + geom.index;
                    let key = self.momentum_keys.get(global).copied().unwrap_or("bull");
                    let color = self.bar_color(global, key, &theme)?;
                    tinted.push(TintedDeltaBar { geom: *geom, color });
                }
                paint_delta_bars_tinted(
                    &sub_painter,
                    &tinted,
                    (self.config.bar_spacing as f32 * 0.35).max(0.5),
                );
            }
            // The zero line makes the centered histogram legible.
            if let Some(zero_y) = sub.windowed_y(0.0) {
                let y = sub_rect.top() + zero_y;
                let stroke = egui::Stroke::new(1.0, resolve_color(&theme, "grid")?);
                sub_painter.line_segment(
                    [egui::Pos2::new(sub_rect.left(), y), egui::Pos2::new(sub_rect.right(), y)],
                    stroke,
                );
            }
        }
        Ok(())
    }

    /// Translate this frame's pointer gestures into typed interaction inputs.
    fn step_from_response(&mut self, ui: &egui::Ui, response: &egui::Response, rect: egui::Rect) {
        let dt = f64::from(ui.input(|input| input.stable_dt));
        let mut events = Vec::new();
        let drag = response.drag_delta();
        if drag.x != 0.0 || drag.y != 0.0 {
            events.push(PointerInput::Pan2D { dx_px: drag.x, dy_px: drag.y });
        }
        if response.hovered() {
            let (scroll, modifiers) =
                ui.input(|input| (input.smooth_scroll_delta, input.modifiers));
            if let Some(hover) = response.hover_pos() {
                if scroll.x != 0.0 {
                    events.push(PointerInput::WheelTime {
                        delta_px: scroll.x,
                        anchor_x: hover.x - rect.left(),
                    });
                }
                if modifiers.ctrl && scroll.y != 0.0 {
                    events.push(PointerInput::Wheel {
                        delta_px: scroll.y,
                        anchor_y: hover.y - rect.top(),
                    });
                }
            }
        }
        self.step_interaction(dt, &events);
    }

    /// Paint the symbol and indicator modals, dispatching their picks.
    fn paint_modals(
        &mut self,
        ui: &mut egui::Ui,
        gateway: Option<&dyn TerminalGateway>,
    ) -> Result<(), EguiError> {
        let theme = self.theme.clone();
        // A dismissable modal: Escape clears whichever catalog is open, and
        // the window close button clears it through `open` (an untouched
        // frame keeps both as they are).
        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.symbol_modal = None;
            self.indicator_modal = None;
        }
        let mut picked_symbol = None;
        if let Some(modal) = self.symbol_modal.as_mut() {
            let mut open = true;
            egui::Window::new("Symbols")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    match paint_catalog_modal(ui, modal, &symbol_categories(), &theme) {
                        Ok(actions) => {
                            for action in actions {
                                if let CatalogAction::Picked(id) = action {
                                    picked_symbol = Some(id);
                                }
                            }
                        }
                        Err(error) => {
                            ui.colored_label(egui::Color32::RED, error.to_string());
                        }
                    }
                });
            if !open {
                self.symbol_modal = None;
            }
        }
        if let Some(symbol) = picked_symbol {
            self.symbol_modal = None;
            self.apply_symbol_pick(&symbol, gateway)?;
        }

        let mut picked_indicator = None;
        if let Some(modal) = self.indicator_modal.as_mut() {
            let mut open = true;
            egui::Window::new("Indicators")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    match paint_catalog_modal(ui, modal, &indicator_categories(), &theme) {
                        Ok(actions) => {
                            for action in actions {
                                if let CatalogAction::Picked(id) = action {
                                    picked_indicator = Some(id);
                                }
                            }
                        }
                        Err(error) => {
                            ui.colored_label(egui::Color32::RED, error.to_string());
                        }
                    }
                });
            if !open {
                self.indicator_modal = None;
            }
        }
        if let Some(id) = picked_indicator {
            self.indicator_modal = None;
            self.slot_indicator(&id)?;
            self.recompute()?;
        }
        Ok(())
    }
}

impl Default for ChartWorkspace {
    fn default() -> Self {
        Self::new().expect("the default workspace boots with the shipped plugins")
    }
}

/// Paint the full terminal: top bar, multi-pane canvas, hot-slot legend,
/// settings pane, and the symbol and indicator modals.
///
/// The gateway is optional: a workspace seeded with rows (the demo shell)
/// paints without one, and a symbol or timeframe pick can only pull fresh
/// history when a gateway is supplied.
pub fn paint_workspace(
    ui: &mut egui::Ui,
    workspace: &mut ChartWorkspace,
    gateway: Option<&dyn TerminalGateway>,
) -> Result<(), EguiError> {
    if let Some(action) = paint_top_bar(
        ui,
        workspace.active_symbol(),
        workspace.timeframe,
        workspace.range,
        workspace.chart_type,
    ) {
        workspace.apply_top_bar_action(action, gateway)?;
    }
    ui.separator();
    workspace.paint_canvas(ui)?;

    let slot_actions = paint_hot_slot_stack(ui, &mut workspace.stack, &workspace.theme)?;
    for action in slot_actions {
        workspace.apply_slot_action(action);
    }

    if let Some(id) = workspace.open_settings_id() {
        let theme = workspace.theme.clone();
        let _ = paint_slot_settings_modal(ui, &mut workspace.stack, &id, &theme);
    }

    workspace.paint_modals(ui, gateway)?;
    Ok(())
}

#[cfg(test)]
#[path = "../tests/workspace.rs"]
mod tests;
