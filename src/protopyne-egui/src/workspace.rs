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

use std::collections::{BTreeMap, BTreeSet, HashMap};

use common_core::hash::{fingerprint_framed, fnv1a64};
use fluent_egui::{
    anchor_window, annotate_at, auto_height_range, axis_ticks, candle_body_width,
    column_candle_geoms, column_delta_geoms, crosshair_data, crosshair_lines, marker_dots,
    needs_column_aggregation, opacity_from_transparency, paint_candles_tinted, paint_catalog_modal,
    paint_crosshair, paint_delta_bars_styled, paint_delta_bars_tinted, paint_hot_slot_stack,
    paint_markers, paint_oscillator, paint_ribbon, paint_ribbon_fill, paint_slot_settings_modal,
    paint_thresholds, project_style_fields, proportional_ribbon_polygons, resolve_color,
    sampled_line_points, sampled_ribbon_bands, schema_fields, translate_bands, translate_candles,
    translate_delta_bars, translate_points, translate_polygons, visible_rows, wheel_zoom_factor,
    zoom_range_around, AnnotationSet, CatalogAction, CatalogModal, CrosshairLines, EguiError,
    HotSlotAction, HotSlotStack, InteractionState, PaneCache, PanePlan, PaneZones, PointerInput,
    SchemaField, SettingsCommit, SettingsTab, SlotAttachment, ThemeConfig, TintedCandle,
    TintedDeltaBar, Viewport, WheelTarget, WheelZones,
};
use fluent_wvr::{
    component_downcast_mut, component_downcast_ref, Component, Describable, FieldAccess,
};
use protopyne_chart_model::{heikin_rows_of, Annotation, ChartRow, ChartType, DateRange};
use protopyne_chart_scale::{PriceRange, PriceScaleMode};
use protopyne_types::{BucketAccum, Money, SymKey, SymbolIndex, Timeframe};
use protopyne_view::{
    pick_default_symbol, symbol_catalog, AssetClass, ReplayDecisionView, ReplayStartParams,
    ReplayStateView, StructureKindHint, SymbolInfoView, TerminalGateway, TicketDraft, TicketSide,
};

use crate::chart::{
    fetch_for_span, line_values, window_for_range, ChartConfig, TimeframeKey, DEFAULT_RANGE,
    DEFAULT_SYMBOL,
};
use crate::indicators::{
    build_indicator_catalog_with_manifest, instantiate_indicator, ChartIndicator,
    IndicatorCategory, IndicatorEvaluation, IndicatorSeries,
};
use crate::integration_lite::{
    candle_momentum_keys, candle_momentum_series, integration_lite_sub_pane, momentum_key_for,
    momentum_tints, ribbon_fill_transparency, IntegrationLiteConfig, IntegrationLiteSubPane,
    MomentumPalette, MomentumTint, CALIBRATED_PIVOT_THRESHOLD, VOLUME_TRANSPARENCY,
};
use crate::oscillator_panel::{panel_kind_for_kernel, OscillatorPanelState, PANEL_KINDS};
use crate::overlay::{BACKTEST_EQUITY_ID, BACKTEST_OVERLAY_ID};
use crate::prefs::{BookmarkEntry, Favorites, SlotPrefs, TerminalPrefs, TickerLayout};
use crate::session::{fmt_boundary_time, time_grid_lines, time_major_unit, SessionBreakCache};
use crate::topology::SeriesCatalog;
use crate::views::{
    load_bundle, paint_broker_panel, BrokerModel, TicketAction, TicketModel, ViewsBundle,
};
use crate::widgets::{
    bar_text, paint_calendar, paint_mtf_table, paint_range_dialog, paint_top_bar, CalendarState,
    MtfRow, RangeChoice, ReplaySpeed, TopBarAction, TopBarMenuState, CHART_LONG_PRESS_SECS,
};
use crate::{paint_backtest_panel, BacktestFormState, BacktestModel};

/// Stable id of the IntegrationLite slot whose richer sub-pane geometry
/// (delta histogram) the canvas paints alongside the plugin's series.
const INTEGRATION_LITE_ID: &str = "integration_lite";

/// Pixels per bar used when no configured spacing applies.
const DEFAULT_BAR_SPACING: f32 = 8.0;
/// Smallest trailing-bar count one chart read asks for. A **cost bound** — the
/// pane paints a trailing window, so this floor keeps a short range on a cheap
/// read while a larger preset raises the bound to its calibrated need. A
/// gateway with no durable tape ignores it; one that plans a store read
/// narrows to it.
pub(crate) const CHART_WINDOW_BARS: usize = 5_000;
/// Indicator headroom ahead of the trailing window, in bars: covers the
/// longest shipped default lookback, so the window's leading bars carry
/// valid indicator values. Longer custom lengths rely on load-more.
const FETCH_WARMUP_BARS: usize = 200;
/// Calendar month in seconds for the load-more window math: the loaded span
/// grows one month at a time. The unit itself lives in
/// [`protopyne_chart_model::MONTH_SECONDS`]; this alias keeps the load-more
/// arithmetic legible at its call sites.
const MONTH_SECONDS: i64 = protopyne_chart_model::MONTH_SECONDS;
/// Share of the chart canvas height the price pane takes; the oscillator
/// takes the rest. The default gives the price pane three quarters of the
/// available height.
const DEFAULT_PANE_SHARE: f32 = 0.75;
/// Narrowest share either pane may take through the divider drag: both panes
/// stay legible, and neither collapses to zero height.
const MIN_PANE_SHARE: f32 = 0.15;
/// Widest share the price pane may take through the divider drag; the mirror
/// of the minimum, keeping the oscillator legible.
const MAX_PANE_SHARE: f32 = 0.85;
/// Divider strip height in pixels: the visible seam plus its grab area. The
/// strip floats over the seam (both panes still tile the canvas exactly), so
/// dragging it never steals pixels from either pane.
const DIVIDER_HEIGHT: f32 = 6.0;
/// Nominal canvas height for viewport binds before the first frame measures
/// the window: proportions already follow the pane share, and the first
/// painted frame resyncs both heights to the measured plots.
const NOMINAL_CANVAS_HEIGHT: f32 = 460.0;
/// Smallest canvas height the frame lays out: below this the price plot (its
/// pane minus the time strip) would collapse at a narrow share.
const MIN_CANVAS_HEIGHT: f32 = 160.0;
/// Price scale base value for the main chart area.
const PRICE_BASE: f64 = 100.0;
/// Sub-pane oscillator band: the bounded `[-100, +100]` legs plus headroom.
const SUB_MIN: f64 = -110.0;
/// Sub-pane upper bound.
const SUB_MAX: f64 = 110.0;
/// Index of the price pane in the wheel-zone list.
const PRICE_PANE_ZONE: usize = 0;
/// Index of the oscillator pane in the wheel-zone list (after the price
/// pane), naming which pane a price-axis zoom belongs to.
const SUB_PANE_ZONE: usize = 1;
/// Share of a candle body a delta histogram bar occupies: the histogram is an
/// overlay inside the oscillator pane, so it stays visibly slimmer than the
/// candles it sits beside while keeping enough pixels to read as bars.
const DELTA_BAR_BODY_SHARE: f32 = 0.7;
/// Narrowest oscillator span a wheel zoom may leave, as a fraction of the
/// pane's base band: below this the series collapse into a smear.
const SUB_MIN_SPAN_FRACTION: f64 = 0.02;
/// Widest oscillator span a wheel zoom may leave, as a fraction of the pane's
/// base band: the default band itself, so a zoom-out never pushes the labels
/// past the range the pane was built for.
const SUB_MAX_SPAN_FRACTION: f64 = 1.0;
/// Right price-axis strip width, TradingView-style.
const AXIS_WIDTH: f32 = 68.0;
/// Bottom time-axis strip height on the price pane.
const TIME_STRIP: f32 = 18.0;
/// Horizontal pane width the interaction envelope is bound to.
const PANE_WIDTH_PX: f32 = 640.0;
/// Minimum pixels between adjacent time-axis labels.
const MIN_TIME_LABEL_GAP: f32 = 120.0;
/// Width of the crosshair time tag on the time strip: room for `YYYY-MM-DD HH:MM`.
const TIME_TAG_WIDTH: f32 = 112.0;
/// Axis-label font size.
const AXIS_FONT: f32 = 11.0;
/// Background gridline stroke width: thinner than a candle wick so the grid
/// reads as a reference, never as data.
const GRID_STROKE_WIDTH: f32 = 0.5;
/// Background gridline dimming; the rules sit below the candles' brightness.
const GRID_DIM: f32 = 0.55;
/// EIT adaptive smoothing used for the candle tint when no IntegrationLite
/// slot supplies one.
const DEFAULT_EIT_ALPHA: f64 = 0.14;
/// Calibrated AutoHeight padding (task value: legibility — wicks visible, no
/// dead space; it claims nothing about whether the window is tradeable).
/// Calibrated in `tests/auto_height.rs`: the clipped-wick control needs tip
/// clearance a 0.02 pad cannot give, the dead-space control needs occupancy
/// a 0.25 pad cannot give, and 0.05 lands both with margin while matching
/// the shipped full-view pad — so [`price_extent`] shares it instead of
/// carrying a second magic number.
pub const AUTO_HEIGHT_PAD_FRACTION: f64 = 0.05;

/// Build the Symbol Selection Modal from gateway rows. The catalog is a
/// derived view: the terminal renders whatever the gateway reports, and an
/// empty row list refuses (a modal with nothing to select is a bug, not a
/// state to paper over with the shipped list).
pub fn build_symbol_catalog(
    rows: &[SymbolInfoView],
) -> Result<CatalogModal<AssetClass>, EguiError> {
    if rows.is_empty() {
        return Err(EguiError::InvalidParam {
            what: "symbol catalog".to_owned(),
            reason: "the gateway served an empty symbol list".to_owned(),
        });
    }
    let mut catalog = CatalogModal::new("Symbols");
    for row in rows {
        let class = AssetClass::parse(&row.asset_class).unwrap_or(AssetClass::Unknown);
        catalog.register(&row.symbol, &row.name, class)?;
    }
    Ok(catalog)
}

/// The Symbol Selection Modal for a live gateway (or the shipped offline
/// list for a gateway-less workspace, the demo's CSV path).
pub fn symbol_modal_for(
    gateway: Option<&dyn TerminalGateway>,
) -> Result<CatalogModal<AssetClass>, EguiError> {
    let rows = match gateway {
        Some(gateway) => gateway.symbols().map_err(|error| EguiError::Dependency {
            reason: format!("symbol catalog unavailable: {error}"),
        })?,
        None => symbol_catalog(),
    };
    build_symbol_catalog(&rows)
}

/// The asset-class tabs (the modal renders its own `All` tab).
fn symbol_categories() -> Vec<(AssetClass, &'static str)> {
    AssetClass::all()
        .into_iter()
        .map(|class| (class, class.label()))
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

/// The padded price extent of a row window; a degenerate window falls back to
/// a unit range so a viewport always constructs. The lower edge never drops
/// below zero: prices have an absolute floor, so the pad clamps there instead
/// of opening a negative pane.
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
    let pad = (hi - lo) * AUTO_HEIGHT_PAD_FRACTION;
    ((lo - pad).max(0.0), hi + pad)
}

/// Fold display rows upward to `timeframe` over the epoch-anchored bucket
/// grid (see [`Timeframe::bucket_start_ms`]): open from the first row, close
/// from the last, high/low as extremes, volume summed, stamped at the bucket
/// start — the same anchor and the same complete-only rule as the store fold
/// (see [`BucketAccum`]), so the seeded demo's resampled slots render
/// identically to the Parquet demo's seeded frames. Only whole buckets emit:
/// a short edge, a torn bucket, or a lone row is omitted, never fabricated
/// into a partial bar. Display floats stay display floats throughout: this
/// never converts back into decision values. An empty input stays empty.
fn aggregate_rows(rows: &[ChartRow], timeframe: Timeframe) -> Vec<ChartRow> {
    let step_ms = timeframe.duration_ms();
    // Day-and-coarser buckets are session-shaped: a trading day cannot tile a
    // 24-hour UTC bucket, so a complete-only fold would emit nothing and the
    // daily chart would go dark. Emit one partial bar per bucket that holds
    // data instead — how a native daily series reads. Sub-daily buckets keep
    // the complete-only rule, so a torn intraday hour is still omitted.
    let complete_only = step_ms < Timeframe::D1.duration_ms();
    let mut folded = Vec::new();
    let mut open: Option<(i64, ChartAccum)> = None;
    for row in rows {
        let start_ms = timeframe.bucket_start_ms(row.time.saturating_mul(1_000));
        let start = start_ms / 1_000;
        match open.as_mut() {
            Some((held, accum)) if *held == start => accum.absorb_row(row),
            _ => {
                if let Some((held, accum)) = open.take() {
                    if let Some(folded_row) = accum.finish(held, step_ms, complete_only) {
                        folded.push(folded_row);
                    }
                }
                open = Some((start, ChartAccum::first(start_ms, row)));
            }
        }
    }
    if let Some((held, accum)) = open.take() {
        if let Some(folded_row) = accum.finish(held, step_ms, complete_only) {
            folded.push(folded_row);
        }
    }
    folded
}

/// One display bucket under construction: the running first/last/extremes/
/// sum in display floats plus the [`BucketAccum`] completeness state over
/// the same stamps. Completeness is O(1) per row with no per-bucket
/// allocation; the arithmetic keeps f64 throughout and never converts back
/// into decision values.
struct ChartAccum {
    complete: BucketAccum,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: u64,
}

impl ChartAccum {
    fn first(start_ms: i64, row: &ChartRow) -> Self {
        let mut complete = BucketAccum::new(start_ms);
        complete.absorb(row.time.saturating_mul(1_000));
        Self {
            complete,
            open: row.open,
            high: row.high,
            low: row.low,
            close: row.close,
            volume: row.volume,
        }
    }

    fn absorb_row(&mut self, row: &ChartRow) {
        self.complete.absorb(row.time.saturating_mul(1_000));
        self.high = self.high.max(row.high);
        self.low = self.low.min(row.low);
        self.close = row.close;
        self.volume = self.volume.saturating_add(row.volume);
    }

    /// Emit the folded row stamped at `start_sec` when the absorbed rows tile
    /// the `step_ms` bucket whole — or, when `complete_only` is false
    /// (day-and-coarser buckets), whenever the bucket holds at least one row,
    /// so a session day emits a daily bar instead of nothing.
    fn finish(self, start_sec: i64, step_ms: i64, complete_only: bool) -> Option<ChartRow> {
        if complete_only && !self.complete.is_complete(step_ms) {
            return None;
        }
        Some(ChartRow {
            time: start_sec,
            open: self.open,
            high: self.high,
            low: self.low,
            close: self.close,
            volume: self.volume,
        })
    }
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

/// One resolved delta volume color at the script's delta transparency: the
/// dialed hex through the shared hex reader, dimmed exactly like the
/// script's `color.new(hex, 60)`. A malformed dial refuses naming its
/// field instead of tinting from silent gray — validation already refuses
/// it, so paint can only see this on a config that changed mid-frame.
fn volume_color(hex: &str, slot: &str) -> Result<egui::Color32, EguiError> {
    let base = fluent_egui::parse_hex_color(slot, hex)?;
    Ok(egui::Color32::from_rgba_unmultiplied(
        base.r(),
        base.g(),
        base.b(),
        opacity_from_transparency(VOLUME_TRANSPARENCY),
    ))
}

/// Where a merged slot paints: its computation stays where it was, only
/// the presentation moves. The default keeps the plugin's own pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PaneTarget {
    /// The plugin's own pane (no override row stored).
    #[default]
    Default,
    /// Over the price pane.
    Main,
    /// In the shared oscillator pane.
    SharedOscillator,
}

/// One sub-pane's identity and viewport.
#[derive(Debug, Clone)]
struct SubPaneView {
    id: String,
    viewport: Viewport,
}

/// One pane's layout: the data plot, the right price-axis strip, and the
/// optional bottom time strip.
#[derive(Debug, Clone, Copy)]
struct PaneLayout {
    /// Data area: grid, candles, overlays, and the crosshair draw here.
    plot: egui::Rect,
    /// Right strip that carries price tick labels and the last-price tag.
    axis: egui::Rect,
    /// Bottom strip that carries time labels; only the price pane reserves one.
    time: Option<egui::Rect>,
}

impl PaneLayout {
    /// Split a pane rect into a plot, a right price axis, and (when
    /// `time_strip`) a bottom time strip. Both panes share a left-aligned time
    /// origin, so their bars and vertical gridlines stay in lockstep.
    fn split(rect: egui::Rect, time_strip: bool) -> Self {
        let plot_right = (rect.right() - AXIS_WIDTH).max(rect.left());
        let plot_bottom = if time_strip {
            (rect.bottom() - TIME_STRIP).max(rect.top())
        } else {
            rect.bottom()
        };
        Self {
            plot: egui::Rect::from_min_max(rect.min, egui::pos2(plot_right, plot_bottom)),
            axis: egui::Rect::from_min_max(
                egui::pos2(plot_right, rect.top()),
                egui::pos2(rect.right(), plot_bottom),
            ),
            time: time_strip
                .then(|| egui::Rect::from_min_max(egui::pos2(rect.left(), plot_bottom), rect.max)),
        }
    }
}

/// Split the chart canvas rect into the price pane and the sub-pane beneath
/// it at `share` of the canvas height, in the zone order [`WheelZones`]
/// indexes.
///
/// The canvas is allocated as **one** rect covering both panes, so a single
/// pointer response senses the whole chart area. That is the whole point: a
/// second allocation would only sense its own rect, and a gesture over the
/// oscillator pane or its price labels would then never reach the router —
/// the pane would look dead to the wheel. One sensing surface plus region
/// routing keeps "where is the pointer" and "what does the gesture act on"
/// separate questions, and the two rects still tile the canvas exactly, so no
/// pixel is sensed twice or left unsensed. Out-of-range shares clamp to the
/// legibility rails instead of collapsing a pane.
fn split_canvas(canvas: egui::Rect, share: f32) -> [egui::Rect; 2] {
    let share = share.clamp(MIN_PANE_SHARE, MAX_PANE_SHARE);
    let split_y = (canvas.top() + canvas.height() * share).clamp(canvas.top(), canvas.bottom());
    [
        egui::Rect::from_min_max(canvas.min, egui::pos2(canvas.right(), split_y)),
        egui::Rect::from_min_max(egui::pos2(canvas.left(), split_y), canvas.max),
    ]
}

/// Split the canvas into the price pane and `pane_count` equal sub-panes.
fn split_canvas_many(canvas: egui::Rect, share: f32, pane_count: usize) -> Vec<egui::Rect> {
    if pane_count <= 1 {
        return split_canvas(canvas, share).to_vec();
    }
    let share = share.clamp(MIN_PANE_SHARE, MAX_PANE_SHARE);
    let split_y = (canvas.top() + canvas.height() * share).clamp(canvas.top(), canvas.bottom());
    let mut rects = Vec::with_capacity(pane_count + 1);
    rects.push(egui::Rect::from_min_max(
        canvas.min,
        egui::pos2(canvas.right(), split_y),
    ));
    let sub_height = (canvas.bottom() - split_y) / pane_count as f32;
    for index in 0..pane_count {
        let top = split_y + sub_height * index as f32;
        let bottom = if index + 1 == pane_count {
            canvas.bottom()
        } else {
            top + sub_height
        };
        rects.push(egui::Rect::from_min_max(
            egui::pos2(canvas.left(), top),
            egui::pos2(canvas.right(), bottom),
        ));
    }
    rects
}

/// The draggable strip over the pane seam: centered on the split at `share`,
/// spanning the canvas width and at most [`DIVIDER_HEIGHT`] tall. Floating
/// over the seam (rather than reserving its own row) keeps the two panes
/// tiling the canvas exactly — the strip is a grab handle, never layout.
fn divider_rect(canvas: egui::Rect, share: f32) -> egui::Rect {
    let [main, _] = split_canvas(canvas, share);
    let half = DIVIDER_HEIGHT / 2.0;
    egui::Rect::from_min_max(
        egui::pos2(canvas.left(), (main.bottom() - half).max(canvas.top())),
        egui::pos2(canvas.right(), (main.bottom() + half).min(canvas.bottom())),
    )
}

/// Rebuild a viewport onto a new plot height, keeping its price window, bar
/// spacing, time offset, mode, base value, and inversion. The frame measures
/// the plots every pass (the canvas fills the window), so the mapping the
/// painters read always agrees with the rect they paint into. `None` on a
/// non-positive or non-finite height — the caller then holds its viewport.
fn viewport_with_plot_height(viewport: Viewport, height: f32) -> Option<Viewport> {
    if !height.is_finite() || height <= 0.0 {
        return None;
    }
    let scale = viewport.scale();
    Viewport::new(
        scale.mode(),
        scale.range().min,
        scale.range().max,
        height,
        scale.bar_spacing(),
        scale.base_value(),
        scale.inverted(),
    )
    .and_then(|view| view.with_x_offset(viewport.x_offset_px()))
    .ok()
}

/// Where the floating indicator legend sits over a chart pane: the upper
/// left, clear of the left edge, with a small inset. TradingView places its
/// legend top-left; the origin is set before gestures are routed so
/// wheel-over-legend hits the legend this frame, not last frame's position.
/// Material: 8dp inset, cards stack vertically without covering price action
/// on the right axis.
fn legend_origin(chart: egui::Rect, _card_width: f32) -> (f32, f32) {
    (chart.left() + 8.0, chart.top() + 8.0)
}

/// Format an epoch timestamp for the time axis: clock time on a sub-daily
/// timeframe, month-day otherwise.
/// Every declared field of a component as a name→string map, read through its
/// own `FieldAccess` contract; a field that refuses is skipped, never
/// substituted. The one reader behind [`ChartWorkspace::prefs`].
fn fields_of(component: Option<&dyn Component>) -> BTreeMap<String, String> {
    let Some(component) = component else {
        return BTreeMap::new();
    };
    component
        .field_names()
        .iter()
        .filter_map(|name| {
            component
                .get_field(name)
                .ok()
                .map(|value| ((*name).to_owned(), value))
        })
        .collect()
}

/// Format a custom-start epoch as the Beginning draft's `YYYY-MM-DD` text;
/// an unrepresentable stamp yields an empty draft rather than a guess.
fn format_date_text(epoch: i64) -> String {
    chrono::DateTime::from_timestamp(epoch, 0)
        .map(|moment| moment.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// Format an epoch timestamp for the crosshair time tag: always calendar
/// date and clock time (`2026-03-02 09:30`), so the hovered readout names
/// the bar exactly even when the strip labels read clock time alone.
fn fmt_crosshair_time(epoch: i64) -> String {
    match chrono::DateTime::from_timestamp(epoch, 0) {
        Some(moment) => moment.format("%Y-%m-%d %H:%M").to_string(),
        None => String::new(),
    }
}

/// Decimals behind a price readout: two by default (cents), widening one
/// step per leading zero below a dollar so sub-dollar moves stay legible.
/// Non-finite prices fall back to the default rather than refusing the tag.
fn price_decimals(price: f64) -> usize {
    if !price.is_finite() {
        return 2;
    }
    let magnitude = price.abs();
    if magnitude == 0.0 || magnitude >= 1.0 {
        2
    } else if magnitude >= 0.1 {
        3
    } else if magnitude >= 0.01 {
        4
    } else if magnitude >= 0.001 {
        5
    } else {
        6
    }
}

/// Format a price readout at its relevant precision (see [`price_decimals`]).
fn fmt_price(price: f64) -> String {
    format!("{price:.dec$}", dec = price_decimals(price))
}

/// One visible time-grid mark: its pane-local x and the bar time behind the
/// label.
struct TimeMark {
    x: f32,
    time: i64,
}

/// Time-grid marks culled to the viewport's visible window: the shared
/// [`time_grid_lines`] boundaries filtered to the culled bar range (with its
/// overscan, so edge lines never pop), each carrying its bar time. The unit
/// ladder is unchanged — a `1M` chart still grids hourly (60 bars), a `1H`
/// chart daily (24 bars) — so a 5–60 bar view holds a fitting handful of
/// dividers instead of the whole trailing window's worth.
fn visible_time_marks(
    viewport: Viewport,
    rows: &[ChartRow],
    bar_seconds: u64,
    width: f32,
) -> Vec<TimeMark> {
    let (start, end) = viewport.culled_bar_range(width, rows.len());
    time_grid_lines(&viewport, rows, bar_seconds)
        .into_iter()
        .filter(|line| line.index >= start && line.index < end)
        .filter_map(|line| {
            rows.get(line.index).map(|row| TimeMark {
                x: line.x,
                time: row.time,
            })
        })
        .collect()
}

/// The dimmed, thin stroke every background gridline draws with, so the
/// horizontal and vertical rules stay visually subordinate to the candles.
fn grid_stroke(theme: &ThemeConfig) -> Result<egui::Stroke, EguiError> {
    Ok(egui::Stroke::new(
        GRID_STROKE_WIDTH,
        resolve_color(theme, "grid")?.gamma_multiply(GRID_DIM),
    ))
}

/// Paint horizontal gridlines at the window's round price ticks.
fn paint_price_grid(
    painter: &egui::Painter,
    viewport: Viewport,
    layout: PaneLayout,
    theme: &ThemeConfig,
) -> Result<(), EguiError> {
    let stroke = grid_stroke(theme)?;
    let window = viewport.visible_range();
    for tick in axis_ticks(window.min, window.max, 8, 8) {
        if let Some(y) = viewport.price_to_y(tick.value) {
            let y = layout.plot.top() + y;
            painter.line_segment(
                [
                    egui::pos2(layout.plot.left(), y),
                    egui::pos2(layout.plot.right(), y),
                ],
                stroke,
            );
        }
    }
    Ok(())
}

/// Paint vertical gridlines at the window's major time boundaries.
fn paint_time_grid(
    painter: &egui::Painter,
    viewport: Viewport,
    rows: &[ChartRow],
    bar_seconds: u64,
    layout: PaneLayout,
    theme: &ThemeConfig,
) -> Result<(), EguiError> {
    let stroke = grid_stroke(theme)?;
    for mark in visible_time_marks(viewport, rows, bar_seconds, layout.plot.width()) {
        let x = layout.plot.left() + mark.x;
        painter.line_segment(
            [
                egui::pos2(x, layout.plot.top()),
                egui::pos2(x, layout.plot.bottom()),
            ],
            stroke,
        );
    }
    Ok(())
}

/// Paint the right price axis: round tick labels plus the last-price tag.
fn paint_price_axis(
    painter: &egui::Painter,
    viewport: Viewport,
    layout: PaneLayout,
    last_price: Option<(f64, bool)>,
    theme: &ThemeConfig,
) -> Result<(), EguiError> {
    let ink = resolve_color(theme, "grid")?;
    let window = viewport.visible_range();
    for tick in axis_ticks(window.min, window.max, 8, 8) {
        if let Some(y) = viewport.price_to_y(tick.value) {
            painter.text(
                egui::pos2(layout.axis.right() - 4.0, layout.axis.top() + y),
                egui::Align2::RIGHT_CENTER,
                &tick.label,
                egui::FontId::proportional(AXIS_FONT),
                ink,
            );
        }
    }
    if let Some((price, up)) = last_price {
        if let Some(y) = viewport.price_to_y(price) {
            let y = layout.axis.top() + y;
            let tag = resolve_color(theme, if up { "bull" } else { "bear" })?;
            let rect = egui::Rect::from_min_max(
                egui::pos2(layout.axis.left() + 2.0, y - 9.0),
                egui::pos2(layout.axis.right() - 2.0, y + 9.0),
            );
            painter.rect_filled(rect, 3.0, tag);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                fmt_price(price),
                egui::FontId::proportional(AXIS_FONT),
                egui::Color32::WHITE,
            );
        }
    }
    Ok(())
}

/// Paint thinned time labels along the strip that separates the price and
/// oscillator panes, one per major boundary, centered on the boundary's
/// vertical gridline. Only major-unit boundaries are labeled (hour, day,
/// week, month), and each label reads at the detail of the largest unit
/// that has passed since the previous boundary ([`fmt_boundary_time`]): a
/// day change shows `MM/DD HH:MM`, an hour change `HH:MM`. Labels closer
/// than [`MIN_TIME_LABEL_GAP`] pixels are thinned so the strip never crowds.
fn paint_time_axis(
    painter: &egui::Painter,
    viewport: Viewport,
    rows: &[ChartRow],
    bar_seconds: u64,
    layout: PaneLayout,
    theme: &ThemeConfig,
) -> Result<(), EguiError> {
    let Some(strip) = layout.time else {
        return Ok(());
    };
    let ink = resolve_color(theme, "grid")?;
    let unit = time_major_unit(bar_seconds);
    let mut last_x = f32::NEG_INFINITY;
    for mark in visible_time_marks(viewport, rows, bar_seconds, layout.plot.width()) {
        let x = layout.plot.left() + mark.x;
        if x < strip.left() + TIME_TAG_WIDTH / 2.0
            || x > strip.right() - TIME_TAG_WIDTH / 2.0
            || x - last_x < MIN_TIME_LABEL_GAP
        {
            continue;
        }
        last_x = x;
        painter.text(
            egui::pos2(x, strip.center().y),
            egui::Align2::CENTER_CENTER,
            fmt_boundary_time(mark.time, unit),
            egui::FontId::proportional(AXIS_FONT),
            ink,
        );
    }
    Ok(())
}

/// Paint one vertical crosshair band through `x`: the oscillator-side half
/// of the readout (and the main-pane half when the pointer sits over the
/// oscillator). Same 1px `"grid"` stroke as [`paint_crosshair`], so the band
/// reads as one readout across both panes.
fn paint_crosshair_band(
    painter: &egui::Painter,
    x: f32,
    top: f32,
    bottom: f32,
    theme: &ThemeConfig,
) -> Result<(), EguiError> {
    painter.line_segment(
        [egui::pos2(x, top), egui::pos2(x, bottom)],
        egui::Stroke::new(1.0, resolve_color(theme, "grid")?),
    );
    Ok(())
}

/// Paint the crosshair time tag on the strip: date and clock time
/// ([`fmt_crosshair_time`]) centered on the snapped bar, clamped into the
/// plot so the tag never hangs off the strip's edge.
fn paint_time_tag(
    chrome_painter: &egui::Painter,
    layout: PaneLayout,
    x: f32,
    time: i64,
    theme: &ThemeConfig,
) -> Result<(), EguiError> {
    let Some(strip) = layout.time else {
        return Ok(());
    };
    let tag = resolve_color(theme, "grid")?;
    let half = TIME_TAG_WIDTH / 2.0;
    let tag_x = x.clamp(layout.plot.left() + half, layout.plot.right() - half);
    let time_rect = egui::Rect::from_center_size(
        egui::pos2(tag_x, strip.center().y),
        egui::vec2(TIME_TAG_WIDTH, strip.height() - 4.0),
    );
    chrome_painter.rect_filled(time_rect, 3.0, tag);
    chrome_painter.text(
        time_rect.center(),
        egui::Align2::CENTER_CENTER,
        fmt_crosshair_time(time),
        egui::FontId::proportional(AXIS_FONT),
        egui::Color32::WHITE,
    );
    Ok(())
}

/// Paint the hover crosshair over the price pane: snapped
/// vertical/horizontal lines plus the price tag on the axis and the time tag
/// on the strip. Answers the snapped bar's screen x plus its bar index for
/// the oscillator band (and the replay's start-at-crosshair control), or
/// `None` when the pointer names no bar. The time tag
/// always reads date and clock time ([`fmt_crosshair_time`]) and the price
/// tag reads at its relevant precision ([`fmt_price`]), so the hovered
/// readout never depends on the strip's thinned granularity.
fn paint_crosshair_overlay(
    plot_painter: &egui::Painter,
    chrome_painter: &egui::Painter,
    viewport: Viewport,
    rows: &[ChartRow],
    layout: PaneLayout,
    hover: egui::Pos2,
    theme: &ThemeConfig,
) -> Result<Option<(f32, usize)>, EguiError> {
    // NOT-A-LADDER: hover-absence reads as none, not a candidate walk.
    let local = (hover.x - layout.plot.left(), hover.y - layout.plot.top());
    let Some(data) = crosshair_data(local, &viewport, rows.len()) else {
        return Ok(None);
    };
    let Some(span) = crosshair_lines(&data, &viewport, layout.plot.width(), layout.plot.height())
    else {
        return Ok(None);
    };
    let span = CrosshairLines {
        x: span.x + layout.plot.left(),
        y: span.y + layout.plot.top(),
        left: layout.plot.left(),
        right: layout.plot.right(),
        top: layout.plot.top(),
        bottom: layout.plot.bottom(),
    };
    paint_crosshair(plot_painter, &span, theme)?;
    let tag = resolve_color(theme, "grid")?;
    let price_rect = egui::Rect::from_min_max(
        egui::pos2(layout.axis.left() + 2.0, span.y - 9.0),
        egui::pos2(layout.axis.right() - 2.0, span.y + 9.0),
    );
    chrome_painter.rect_filled(price_rect, 3.0, tag);
    chrome_painter.text(
        price_rect.center(),
        egui::Align2::CENTER_CENTER,
        fmt_price(data.price),
        egui::FontId::proportional(AXIS_FONT),
        egui::Color32::WHITE,
    );
    if let Some(row) = rows.get(data.index) {
        paint_time_tag(chrome_painter, layout, span.x, row.time, theme)?;
    }
    Ok(Some((span.x, data.index)))
}

/// One canvas context-menu row, raised near the pointer on right-click or
/// long-press: Auto Zoom refits the Y axis to the displayed candles,
/// Bookmark captures the trigger candle for the active ticker, and Set
/// Replay Boundary arms the replay button's window at the trigger candle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChartContextAction {
    /// Refit Y to the displayed candles, preserving X (Refresh's Y-fit).
    AutoZoom,
    /// Capture the trigger candle plus timeframe into this ticker's list.
    Bookmark,
    /// Set a replay boundary at the trigger candle (beginning by default,
    /// the end when a beginning already precedes it).
    SetReplayBoundary,
    /// Drop an operator note on the trigger candle.
    Annotate,
    /// Remove the most recently added operator note.
    UndoAnnotation,
    /// Move the custom pane under the pointer onto the price pane.
    MergeToChart,
    /// Move the custom pane under the pointer into the shared oscillator.
    MergeToOscillator,
}

/// Which pane the canvas menu opened over: the price pane, the shared
/// oscillator, or one custom pane by id.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ContextPane {
    /// The price pane.
    Main,
    /// The shared oscillator pane.
    Shared,
    /// One custom pane by id.
    Custom(String),
}

/// An interactive replay run driven from the dock: the gateway owns the
/// session (decisions, fills, journal), this owns the controls and the
/// cached state the panes render. Chart and indicators truncate to the
/// cursor through [`ChartWorkspace::replay_prefix_len`]; the full history
/// stays loaded, so exiting returns to the live chart without a re-pull.
/// Display-only: every decide and fill happens core-side behind the gateway.
struct LiveReplay {
    /// Registry slot opened core-side.
    session_id: String,
    /// Last state the gateway served (cursor, verdicts, fills, journal).
    /// The cursor runs in the server history's coordinates.
    state: ReplayStateView,
    /// Whether the play loop steps while the section paints.
    playing: bool,
    /// Last control refusal, cleared on the next success.
    error: Option<String>,
    /// Exclusive end of the replay window in server coordinates: steps clamp
    /// here and play stops on arrival. `None` replays to the last server bar.
    server_end: Option<u64>,
    /// Server history timestamps by bar, in order: maps the server cursor to
    /// the display prefix (bars with an earlier time are decided, the rest
    /// stay hidden). Empty means the legacy identity mapping — the server
    /// cursor is read as a display index — kept for gateways that serve no
    /// replay time axis.
    server_times: Vec<i64>,
}

impl LiveReplay {
    fn new(session_id: String, state: ReplayStateView) -> Self {
        Self {
            session_id,
            state,
            playing: false,
            error: None,
            server_end: None,
            server_times: Vec::new(),
        }
    }

    /// Exclusive end of the window this run may decide, in server
    /// coordinates, defaulting to the session's own bar count when no
    /// explicit end was picked.
    fn end(&self) -> u64 {
        self.server_end.unwrap_or(self.state.total_bars)
    }
}

/// The terminal's central state store: active symbol, selectors, hot-slot
/// stack, pane plan, pane cache, the 2D interaction machine, and the Material
/// dock (left navigation drawer) with its cached gateway views.
///
/// One place owns all terminal state so the Material top bar, the symbol and
/// indicator modals, the upper-left hot-slot legend, the multi-pane canvas,
/// and the dock's collapsible subpanes read and write the same truth.
#[allow(clippy::struct_excessive_bools)]
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
    /// Published indicator outputs by stable node id: the series catalog a
    /// source picker lists. Rebuilt every recompute in plan order, so a
    /// consumer never reads a stale producer.
    series_catalog: SeriesCatalog,
    stack: HotSlotStack,
    plugins: Vec<Box<dyn ChartIndicator>>,
    // Catalog stays `HashMap`: consumers read `values()` order-insensitively,
    // replace-on-duplicate matches `KeyedRegistry` but sorting would add
    // churn with no reader benefit.
    evaluations: HashMap<String, IndicatorEvaluation>,
    sub_pane: Option<IntegrationLiteSubPane>,
    momentum_keys: Vec<&'static str>,
    tints: Vec<Option<MomentumTint>>,
    /// Derived-pane builds actually executed: incremented only when the
    /// published series revisions (or the rows shape) moved and the
    /// sub-pane, momentum keys, and tints were rebuilt. Identical
    /// republishes backdate every revision and skip the rebuild, so this
    /// counts work done, never frames seen.
    derived_builds: u64,
    /// Last built series revisions, as sorted `(key, revision)` pairs.
    last_series_revisions: Vec<(u64, u64)>,
    /// Last built plan fingerprint, over the live slot configs. Together with
    /// the rows shape this is the skip-gate key: both must match exactly.
    last_plan_fingerprint: u64,
    /// Last built rows shape: length plus first/last bar times.
    last_rows_shape: (usize, Option<i64>, Option<i64>),
    /// Content hash of the evaluated prefix at the last build.
    last_rows_content: u64,
    rows: Vec<ChartRow>,
    histories: HashMap<TimeframeKey, Vec<ChartRow>>,
    /// Full-history Heikin-Ashi rows, transformed once per rows shape at
    /// `recompute` and windowed by paint like any other full-history
    /// series. The recurrence seeds from row zero, so a per-window
    /// transform would shift every open past the seam.
    heikin: Vec<ChartRow>,
    /// Share of the canvas height the price pane takes; the oscillator takes
    /// the rest. Owned here so the divider drag, the canvas split, and the
    /// viewport binds all read one truth.
    pane_share: f32,
    interaction: Option<InteractionState>,
    main: Option<Viewport>,
    sub: Option<Viewport>,
    /// Custom sub-panes after the default shared `sub` pane.
    sub_panes: Vec<SubPaneView>,
    /// Presentation-only routing overrides by slot id: merging moves paint,
    /// never plan nodes, cache keys, or fingerprints. Empty by default, so
    /// routing is identical until a row is written. Removal deletes the row;
    /// a target that no longer resolves reads as default.
    pane_routing: HashMap<String, PaneTarget>,
    symbol_modal: Option<CatalogModal<AssetClass>>,
    indicator_modal: Option<CatalogModal<IndicatorCategory>>,
    /// Bookmark picker over the active ticker's entries, rebuilt on every
    /// open; `None` is a closed picker. Painted as a titlebar-less popup
    /// below the menu-bar button, like the ticker and indicator pickers.
    bookmark_modal: Option<CatalogModal<TimeframeKey>>,
    /// Menu-bar button rects the titlebar-less pickers hang below: the popup
    /// anchors at the stored rect, so the ticker, indicator, bookmark, and
    /// jump UIs open centered below their buttons above the dock or chart.
    /// Set on every opener click beside the modal state they anchor.
    symbol_anchor: Option<egui::Rect>,
    /// See [`symbol_anchor`](Self::symbol_anchor).
    indicator_anchor: Option<egui::Rect>,
    /// See [`symbol_anchor`](Self::symbol_anchor).
    bookmark_anchor: Option<egui::Rect>,
    /// See [`symbol_anchor`](Self::symbol_anchor).
    jump_anchor: Option<egui::Rect>,
    /// Frame rect the open settings pane painted last frame, for wheel
    /// routing: a wheel over it scrolls the pane, never the canvas below.
    /// `None` while no settings pane is open.
    settings_rect: Option<egui::Rect>,
    /// Whether the Jump To Time popup (menu-bar magnifier) is open.
    jump_open: bool,
    /// Month the Jump calendar is showing; picking a day sets the jump
    /// text's date part and preserves its clock part.
    jump_calendar: CalendarState,
    /// Jump refusal, when the last Go failed. Shown inside the popup;
    /// another Go overwrites it.
    jump_error: Option<String>,
    /// Bookmark refusal, when the last bookmark jump failed. Shown inside
    /// the bookmark picker; another pick overwrites it.
    bookmark_error: Option<String>,
    /// Whether the canvas context menu (right-click / long-press) is open.
    context_open: bool,
    /// Pointer position the context menu opened at, in global pixels.
    context_at: egui::Pos2,
    /// Pointer position relative to the price plot's origin, for the
    /// annotate action's hit test. Resolved at trigger time with the bar
    /// time above, so later pans cannot move the note.
    context_local: Option<(f32, f32)>,
    /// Bar time the context menu opened over, resolved through the main
    /// viewport at trigger time so later pans cannot move the actions.
    context_time: Option<i64>,
    /// Which pane the context menu opened over, resolved from the layout
    /// rects at trigger time so later pans cannot move the actions.
    context_pane: Option<ContextPane>,
    /// Long-press tracking on the canvas: pointer position and UI seconds
    /// when the current primary hold started, if the pointer is down on it.
    context_press: Option<(f32, f32, f32)>,
    /// Whether the current hold already opened the context menu: the release
    /// is then consumed instead of starting a second menu.
    context_fired: bool,
    /// Starred ticker symbols, owned here (not in the on-demand modal) so the
    /// Favorites filter survives the modal closing and serializes to
    /// `protopyne.json`.
    favorite_symbols: BTreeSet<String>,
    /// Starred indicator ids; the indicator-catalog twin of
    /// [`favorite_symbols`](Self::favorite_symbols).
    favorite_indicators: BTreeSet<String>,
    /// Pine indicator manifest rows for the catalog picker, empty until boot
    /// loads `<data-root>/indicators`. Without rows the catalog offers no
    /// Pine scripts and no Pine checkbox.
    pine_manifest: Vec<protopyne_kernels::IndicatorManifestEntry>,
    /// Pine sources behind the manifest rows, for linking at slot time:
    /// indicator files and library files as loaded.
    pine_indicators: Vec<protopyne_kernels::ManifestFile>,
    /// Pine library files backing `import` resolution at link time.
    pine_libraries: Vec<protopyne_kernels::ManifestFile>,
    /// Starred bookmark ids (`{symbol}|{timeframe}|{time}`); the bookmark
    /// picker's Favorites filter keys on this set, like the other catalogs.
    favorite_bookmarks: BTreeSet<String>,
    /// Bookmarked candles by ticker symbol, in capture order. Set from the
    /// canvas context menu; picked from the menu-bar bookmark picker, which
    /// jumps the chart to the bookmarked candle.
    bookmarks: BTreeMap<String, Vec<BookmarkEntry>>,
    /// Saved per-ticker layouts, stashed on every ticker switch and applied
    /// to the incoming ticker: the Save Layout button overwrites exactly the
    /// active ticker's entry, and [`prefs`](Self::prefs) merges the stash
    /// with the live layout. Never written except through an explicit save.
    saved_layouts: BTreeMap<String, TickerLayout>,
    /// Session-only layout stash, separate from the saved map: switching
    /// tickers parks the live layout here so an unsaved session keeps its
    /// edits while roaming, without overwriting any ticker's saved entry.
    /// A saved entry always wins over a stashed one on the way in.
    stashed_layouts: BTreeMap<String, TickerLayout>,
    /// Save requested by the menu-bar Save Layout button and not yet
    /// consumed: the composition root writes `protopyne.json` only when
    /// [`take_save_request`](Self::take_save_request) answers true.
    save_requested: bool,
    /// Session-break memo over the window's bar times. The loader refreshes
    /// it when the window's contents move (see
    /// [`refresh_session_breaks`](Self::refresh_session_breaks)); the paint
    /// path reads it through
    /// [`session_break_lines`](Self::session_break_lines) without rescanning.
    /// No session overlay paints until a provider is installed — the memo
    /// holds no provider of its own.
    session_breaks: SessionBreakCache,
    /// Material dock (left navigation drawer) open state. Toggled by the far
    /// left menu-bar button into a floating window along the left boundary
    /// below the app bar; the chart canvas keeps the full window when the
    /// drawer is closed or overlaid. Defaults closed so first paint is
    /// full-bleed. Persists across frames in memory.
    dock_open: bool,
    /// Top-bar menu drafts (Beginning + Jump texts) behind the dock's range
    /// section. Owned here so the section's pickers share one truth.
    top_menu: TopBarMenuState,
    /// Draft preset in the dock range section; `None` when a Beginning is drafted.
    range_draft: Option<DateRange>,
    /// Oscillator kind defaults behind the dock Oscillators section: the
    /// live defaults unset slot dials follow. Persists in prefs.
    oscillator_panel: OscillatorPanelState,
    /// Month the Beginning calendar is showing. Owned here (not in the
    /// reflection-backed menu component) so the picker navigates months
    /// without a schema field per cursor.
    calendar: CalendarState,
    /// Range section refusal, when the last apply/jump failed. Shown inline
    /// with a dismiss control; applying again overwrites it.
    range_error: Option<String>,
    /// Custom window start (epoch seconds) from the Beginning picker. `None`
    /// means the preset range rules; `Some(t)` filters the trailing window
    /// to bars at or after `t`. Clamped to loaded history; a fetch is
    /// attempted when the gateway can serve more.
    custom_start: Option<i64>,
    /// Deferred window fit: set by timeframe switches, consumed by the next
    /// `paint_canvas` once rows are loaded and the plot width is measured.
    /// Never fitted on a proxy width or an empty store.
    pending_auto_height: bool,
    /// Last measured chart-plot width in pixels. Updated every `paint_canvas`
    /// after the resync; view buttons (refresh, the rewind/forward pan
    /// cluster, jump) read this instead of a proxy so they never act on an
    /// unset X dimension.
    last_plot_width: f32,
    /// Cached gateway views bundle for the dock (health/portfolio/proposals/
    /// journal). Refreshed on `refresh` and on dock refresh; per-frame reads
    /// would re-serialize the journal, so the frame reads this cache.
    dock_bundle: Option<ViewsBundle>,
    /// Dock bundle refusal, when the last pull failed.
    dock_bundle_error: Option<String>,
    /// Cached broker panel pull; loaded on first open, then held.
    broker_model: Option<BrokerModel>,
    /// Journal lines pre-rendered in provenance order at refresh time.
    dock_journal_lines: Vec<String>,
    /// Served backtest schema, kept so a re-render never re-asks: the form
    /// owns the working document, this owns the descriptor it was built from.
    backtest_schema: Option<serde_json::Value>,
    /// Backtest authoring form over the served schema: drafts, submit, and
    /// the last outcome. Owned here so the dock's backtest subpane and the
    /// terminal example share one truth.
    backtest_form: BacktestFormState,
    /// Cached backtest read: metrics, trades, curve, and the final status
    /// row. Reads happen on click only; the painter never re-reads.
    backtest: Option<BacktestModel>,
    /// Operator annotations on the chart: click-to-annotate notes with undo.
    /// Painted as markers over the main canvas; the run's own markers stay
    /// with the backtest overlay, so the two never double-paint a bar.
    annotations: AnnotationSet,
    /// Backtest refusal, when the last run control failed. Shown inline in
    /// the backtest section; running again overwrites it.
    backtest_error: Option<String>,
    /// Replay refusal, when the last replay control failed. Shown inline in
    /// the replay section; applying again or opening overwrites it.
    replay_error: Option<String>,
    /// Interactive replay run (proposals + paper fills) when one is live:
    /// the controls and the cached core-side state the chart and panes
    /// truncate to. `None` is the live chart.
    live_replay: Option<LiveReplay>,
    /// Transport speed in candles at the selected timeframe, shared by the
    /// menu-bar play button and its long-press popup: Slow shows one candle
    /// every three seconds, Normal paces one candle per second, Fast steps
    /// five candles per second, and Full advances as fast as the core
    /// calculates. Applies to the next opened run and live-adjusts a running
    /// one.
    replay_speed: ReplaySpeed,
    /// Paced-play clock, in seconds on the pump's own time base: advanced by
    /// each pump frame's elapsed `dt`, so headless tests drive the same
    /// cadence with explicit durations. Reset on open and stop.
    replay_clock: f64,
    /// UI-clock time the next paced candle is due, on [`replay_clock`](Self::replay_clock)'s
    /// base: `None` until the first pump after play starts, which waits one
    /// full interval so a fresh press never bursts. Cleared on pause, open,
    /// and stop, so resuming restarts the cadence instead of dumping a
    /// banked backlog.
    replay_next_due: Option<f64>,
    /// Applied replay window as epoch seconds: `(start, end)` with
    /// `start <= end`, captured from the chart's own visible window at the
    /// moment play is pressed. Times are fixed — panning the display moves
    /// the markers with the time axis, never the window itself. `None` is
    /// the live chart over the full loaded history; the transport captures
    /// a fresh window on every play from no run, so a replay never starts
    /// without an explicit window.
    replay_range: Option<(i64, i64)>,
    /// Manual-ticket issue drafts: the operator's exact ticket fields. The
    /// terminal only collects them into a [`TicketDraft`]; the core mints the
    /// ticket identity and validates every value with its own parser.
    ticket_underlying: String,
    /// Ticket side draft.
    ticket_side: TicketSide,
    /// Ticket quantity draft (whole contracts, parsed only when the issue
    /// click builds the typed draft).
    ticket_quantity: String,
    /// Ticket limit-price draft (decimal text, never parsed here).
    ticket_limit: String,
    /// Ticket refusal, when the last ticket action failed.
    ticket_error: Option<String>,
}

/// One live chart update handed to [`ChartWorkspace`].
#[derive(Debug, Clone, PartialEq)]
pub enum LiveChartUpdate {
    /// A still-forming bar: display only.
    Forming(ChartRow),
    /// A final bar: append/replace and recompute.
    Closed(ChartRow),
}

/// Parse one rehearsal feed frame into the live update it carries. Only
/// closed-bar frames exist today; anything else refuses by name so a future
/// frame kind cannot silently paint as a bar.
pub fn live_update_of_frame(frame: &str) -> Result<LiveChartUpdate, EguiError> {
    let refused = |reason: String| EguiError::InvalidParam {
        what: "frame".to_owned(),
        reason,
    };
    let value: serde_json::Value = serde_json::from_str(frame)
        .map_err(|error| refused(format!("live frame is not JSON: {error}")))?;
    if value.get("type").and_then(serde_json::Value::as_str) != Some("bar") {
        let kind = value
            .get("type")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        return Err(refused(format!("live frame type {kind} is not a bar")));
    }
    let number = |name: &str| {
        value
            .get(name)
            .and_then(serde_json::Value::as_f64)
            .ok_or_else(|| refused(format!("live bar frame has no numeric {name:?}")))
    };
    let time = value
        .get("time")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| refused("live bar frame has no integer \"time\"".to_owned()))?;
    let volume = value
        .get("volume")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| refused("live bar frame has no integer \"volume\"".to_owned()))?;
    Ok(LiveChartUpdate::Closed(ChartRow {
        time,
        open: number("open")?,
        high: number("high")?,
        low: number("low")?,
        close: number("close")?,
        volume,
    }))
}

/// Content hash of the evaluated prefix: time plus each OHLCV field as raw
/// bits, so a revised forming bar or an identical-shape symbol switch moves
/// the skip key while a pure re-render holds it.
fn rows_content_of(rows: &[ChartRow]) -> u64 {
    let mut bytes = Vec::with_capacity(rows.len() * 44);
    for row in rows {
        bytes.extend_from_slice(&row.time.to_le_bytes());
        bytes.extend_from_slice(&row.open.to_bits().to_le_bytes());
        bytes.extend_from_slice(&row.high.to_bits().to_le_bytes());
        bytes.extend_from_slice(&row.low.to_bits().to_le_bytes());
        bytes.extend_from_slice(&row.close.to_bits().to_le_bytes());
        bytes.extend_from_slice(&row.volume.to_le_bytes());
    }
    fnv1a64(&bytes)
}

/// A slot config ready to evaluate or fingerprint: the stored config, or
/// a kind-resolved clone when the slot follows panel defaults. Borrowed
/// storage and owned resolutions share one call shape, so the plan
/// fingerprint and the evaluator cannot disagree on what a slot means.
enum EvalConfig<'a> {
    /// The slot's stored config, untouched.
    Stored(&'a dyn Component),
    /// Blanks resolved against the panel kind rows (boxed: resolved configs
    /// dwarf a fat pointer, and the lint is right to say so).
    Resolved(Box<dyn Component>),
}

impl EvalConfig<'_> {
    /// The config to evaluate and fingerprint.
    fn as_component(&self) -> &dyn Component {
        match self {
            Self::Stored(config) => *config,
            Self::Resolved(config) => config.as_ref(),
        }
    }
}

impl ChartWorkspace {
    /// Boot the terminal: the default ticker (`SPY`), the five-minute default
    /// timeframe, a two-month default window, and the three scripted
    /// indicators hot-slotted. No history yet — pull it with
    /// [`with_gateway`](Self::with_gateway) or seed it with
    /// [`with_rows`](Self::with_rows).
    pub fn new() -> Result<Self, EguiError> {
        let mut symbol_index = SymbolIndex::new();
        let active_symbol =
            symbol_index
                .intern(DEFAULT_SYMBOL)
                .map_err(|error| EguiError::InvalidParam {
                    what: "symbol".to_owned(),
                    reason: error.to_string(),
                })?;
        let mut workspace = Self {
            symbol_index,
            active_symbol,
            timeframe: TimeframeKey::default(),
            range: DEFAULT_RANGE,
            chart_type: ChartType::Candles,
            config: ChartConfig::default(),
            theme: ThemeConfig::default(),
            plan: PanePlan::new(),
            cache: PaneCache::new(),
            series_catalog: SeriesCatalog::empty(),
            stack: HotSlotStack::new(),
            plugins: Vec::new(),
            evaluations: HashMap::new(),
            sub_pane: None,
            momentum_keys: Vec::new(),
            tints: Vec::new(),
            derived_builds: 0,
            last_series_revisions: Vec::new(),
            last_plan_fingerprint: 0,
            last_rows_shape: (0, None, None),
            last_rows_content: 0,
            rows: Vec::new(),
            histories: HashMap::new(),
            heikin: Vec::new(),
            pane_share: DEFAULT_PANE_SHARE,
            interaction: None,
            main: None,
            sub: None,
            sub_panes: Vec::new(),
            pane_routing: HashMap::new(),
            symbol_modal: None,
            indicator_modal: None,
            bookmark_modal: None,
            symbol_anchor: None,
            indicator_anchor: None,
            bookmark_anchor: None,
            jump_anchor: None,
            settings_rect: None,
            jump_open: false,
            jump_calendar: CalendarState::default(),
            jump_error: None,
            bookmark_error: None,
            context_open: false,
            context_at: egui::Pos2::ZERO,
            context_local: None,
            context_time: None,
            context_pane: None,
            context_press: None,
            context_fired: false,
            favorite_symbols: BTreeSet::new(),
            favorite_indicators: BTreeSet::new(),
            favorite_bookmarks: BTreeSet::new(),
            bookmarks: BTreeMap::new(),
            saved_layouts: BTreeMap::new(),
            stashed_layouts: BTreeMap::new(),
            save_requested: false,
            session_breaks: SessionBreakCache::new(),
            dock_open: false,
            top_menu: TopBarMenuState::new().build(),
            range_draft: None,
            oscillator_panel: OscillatorPanelState::default(),
            calendar: CalendarState::default(),
            range_error: None,
            custom_start: None,
            pending_auto_height: false,
            last_plot_width: PANE_WIDTH_PX,
            dock_bundle: None,
            dock_bundle_error: None,
            broker_model: None,
            dock_journal_lines: Vec::new(),
            backtest_schema: None,
            backtest_form: BacktestFormState::default(),
            backtest: None,
            backtest_error: None,
            pine_manifest: Vec::new(),
            pine_indicators: Vec::new(),
            pine_libraries: Vec::new(),
            annotations: AnnotationSet::new(),
            replay_error: None,
            live_replay: None,
            replay_speed: ReplaySpeed::default(),
            replay_clock: 0.0,
            replay_next_due: None,
            replay_range: None,
            ticket_underlying: "SPY".to_owned(),
            ticket_side: TicketSide::Buy,
            ticket_quantity: "1".to_owned(),
            ticket_limit: String::new(),
            ticket_error: None,
        };
        workspace.slot_indicator(INTEGRATION_LITE_ID)?;
        workspace.slot_indicator("fast_mas")?;
        workspace.slot_indicator(crate::delta_volume_profile::DELTA_VOLUME_PROFILE_ID)?;
        Ok(workspace)
    }

    /// Boot and pull the first history through `gateway`.
    ///
    /// The active symbol is the default rule, not a constant: the gateway's
    /// catalog is consulted and the preferred symbol is taken when present,
    /// else the first in sorted order; a gateway that serves no catalog
    /// leaves the preference seed standing (it will fail loudly on refresh).
    ///
    /// Boot is chart-only: the dock bundle (journal/portfolio) and the replay
    /// window load when the dock opens, never on the critical first frame, so
    /// startup pays for the visible chart alone.
    pub fn with_gateway(gateway: &dyn TerminalGateway) -> Result<Self, EguiError> {
        let mut workspace = Self::new()?;
        if let Ok(catalog) = gateway.symbols() {
            if !catalog.is_empty() {
                let symbols: Vec<&str> = catalog.iter().map(|row| row.symbol.as_str()).collect();
                if let Some(symbol) = pick_default_symbol(DEFAULT_SYMBOL, symbols) {
                    workspace.set_symbol(&symbol)?;
                }
            }
        }
        workspace.refresh_chart(gateway, None)?;
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
        self.symbol_index
            .resolve(self.active_symbol)
            .unwrap_or(DEFAULT_SYMBOL)
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

    /// The preset highlighted in the range menu, or `None` when a custom
    /// Beginning start is active: a custom window is not any preset, so
    /// highlighting the `All` it happens to sit on would misreport it.
    #[must_use]
    pub fn active_range(&self) -> Option<DateRange> {
        self.custom_start.is_none().then_some(self.range)
    }

    /// The range button's label: the Beginning date when one is active, else
    /// the preset's short label, so the button always names the loaded window
    /// rather than the preset a custom pick replaced.
    #[must_use]
    pub fn range_label(&self) -> String {
        match self.custom_start {
            Some(start) => chrono::DateTime::from_timestamp(start, 0).map_or_else(
                || self.range.label().to_owned(),
                |dt| dt.format("%Y-%m-%d").to_string(),
            ),
            None => self.range.label().to_owned(),
        }
    }

    /// Whole months the loaded window spans: derived from the active range,
    /// or from the Beginning start when one is set. There is no stored width
    /// to drift — one derivation serves the load-more window, the range
    /// default, and any reader.
    #[must_use]
    pub fn visible_months(&self) -> u32 {
        match self.custom_start {
            Some(_) => {
                let end = self.rows.last().map(|row| row.time);
                self.window_months(end)
            }
            None => self.range.lookback_months().unwrap_or(u32::MAX),
        }
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

    /// Share of the canvas height the price pane takes; the oscillator takes
    /// the rest. Defaults to three quarters.
    #[must_use]
    pub fn pane_share(&self) -> f32 {
        self.pane_share
    }

    /// Move the price/oscillator seam to `share` of the canvas height,
    /// clamped to the legibility rails; a non-finite input holds the seam
    /// instead of poisoning the layout.
    pub fn set_pane_share(&mut self, share: f32) {
        if share.is_finite() {
            self.pane_share = share.clamp(MIN_PANE_SHARE, MAX_PANE_SHARE);
        }
    }

    /// The timeframes holding a seeded or pulled history, in toolbar order.
    /// A frame the shell can resample on demand (coarser than a seeded base)
    /// is not listed until it is selected; this names what is loaded, not
    /// what is reachable.
    #[must_use]
    pub fn loaded_timeframes(&self) -> Vec<TimeframeKey> {
        TimeframeKey::all()
            .into_iter()
            .filter(|key| self.histories.contains_key(key))
            .collect()
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

    /// Refresh the session-break memo from the window's bar times: the
    /// recompute seam for a session-break paint path. Identical content
    /// keeps the revision and skips the scan; changed content rescans once
    /// no matter how many frames read it. The caller owns the provider (the
    /// workspace installs none); the memo never outlives a provider swap
    /// silently — a new provider name rekeys the fingerprint and rescans.
    pub fn refresh_session_breaks(
        &mut self,
        times: &[chrono::DateTime<chrono::Utc>],
        provider: &dyn protopyne_chart_model::SessionProvider,
    ) -> u64 {
        self.session_breaks.refresh_times(times, provider)
    }

    /// Session-break gridlines from the memo for `viewport`: the paint read
    /// twin of [`refresh_session_breaks`](Self::refresh_session_breaks).
    /// No scan runs here — a window whose contents never moved paints the
    /// same lines every frame for free.
    #[must_use]
    pub fn session_break_lines(
        &self,
        viewport: &fluent_egui::Viewport,
    ) -> Vec<fluent_egui::BreakLine> {
        self.session_breaks.lines(viewport)
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

    /// Derived-pane builds actually executed (sub-pane, momentum keys,
    /// tints). Identical republishes skip the rebuild, so repeated frames
    /// over unchanged content leave this untouched.
    #[must_use]
    pub fn derived_builds(&self) -> u64 {
        self.derived_builds
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
    /// contract, then recompute so the edit is live. Like every
    /// indicator-side recompute the view is preserved: X stays put and Y
    /// refits to the visible window.
    pub fn set_slot_field(&mut self, id: &str, name: &str, value: &str) -> Result<(), EguiError> {
        self.stack.set_slot_field(id, name, value)?;
        self.recompute_preserving_view()
    }

    /// Write one Style field (a color override or the line width) on a slot
    /// through its own style component, without recomputing anything.
    ///
    /// A style write changes how a series paints, never what a series is, so
    /// it deliberately skips the recompute that
    /// [`set_slot_field`](Self::set_slot_field) triggers — republishing every
    /// series to the pane cache would be pure cost for a pure repaint. The
    /// field's own contract refuses, naming itself.
    pub fn set_slot_style_field(
        &mut self,
        id: &str,
        name: &str,
        value: &str,
    ) -> Result<(), EguiError> {
        self.stack.set_style_field(id, name, value)
    }

    /// Read one Style field back off a slot's style component.
    pub fn slot_style_field(&self, id: &str, name: &str) -> Result<String, EguiError> {
        self.stack.get_style_field(id, name)
    }

    /// Commit one slot's staged settings edits through its live config and
    /// recompute so the chart reflects them. Per-field refusals are returned
    /// (never silently swallowed) while every accepted field applies.
    pub fn commit_slot_settings(&mut self, id: &str) -> Result<Vec<String>, EguiError> {
        let refused = self.stack.commit_settings(id)?;
        self.recompute_preserving_view()?;
        Ok(refused)
    }

    /// Recompute after an indicator-side change (slot add, settings commit,
    /// single-field write): the recompute rebinds every viewport from the
    /// configured spacing onto the trailing edge, so the whole time axis —
    /// position *and* zoom — is restored afterwards and the Y axis refits
    /// to the visible window in place. That is the same visible-window
    /// refit the Refresh button owns, never a jump to the newest bar. A
    /// pre-paint call has no measured width yet, so the Y fit waits for
    /// the first frame instead of fitting on a proxy.
    fn recompute_preserving_view(&mut self) -> Result<(), EguiError> {
        let saved_main = self.main;
        self.recompute()?;
        if let (Some(saved), Some(fresh)) = (saved_main, self.main) {
            // The rebind rebuilt every viewport from the configured spacing;
            // adopt the saved time axis (zoom and position) onto the fresh
            // price range, then lock every pane back to it. Rows never move
            // under an indicator-side change, so no prepend compensation
            // applies — a direct adopt is exact.
            if let Ok(restored) = fresh.sync_time_axis(&saved) {
                self.main = Some(restored);
                if let Some(sub) = self.sub {
                    if let Ok(synced) = sub.sync_time_axis(&restored) {
                        self.sub = Some(synced);
                    }
                }
                for pane in &mut self.sub_panes {
                    if let Ok(synced) = pane.viewport.sync_time_axis(&restored) {
                        pane.viewport = synced;
                    }
                }
            }
        }
        self.apply_auto_height(self.last_plot_width);
        Ok(())
    }

    /// Remove an indicator: close its pane handle, unslot it, drop its
    /// evaluation, and refresh the derived pane/tint state.
    pub fn remove_indicator(&mut self, id: &str) -> Result<(), EguiError> {
        self.stack.remove(id)?;
        self.plugins.retain(|plugin| plugin.id() != id);
        self.pane_routing.remove(id);
        // The plan and the series catalog are derived per recompute; rebuild
        // the plan now so a re-slot never trips over the removed nodes, and
        // drop the catalog so no later reader meets a stale producer.
        self.rebuild_series_plan()?;
        self.series_catalog.clear();
        // Prune the removed evaluation's keys from the revision snapshot so
        // the next recompute compares against what is actually cached — the
        // direct rebuild below counts, like every derived build.
        if let Some(evaluation) = self.evaluations.remove(id) {
            for series in &evaluation.series {
                self.last_series_revisions
                    .retain(|(key, _)| *key != series.key);
            }
        }
        if self.rows.is_empty() {
            return Ok(());
        }
        self.sub_pane = self.compute_sub_pane()?;
        self.recompute_tint()?;
        self.rebuild_derived_panes()?;
        self.derived_builds += 1;
        // The direct rebuild above counts as the build for the new plan, so
        // the next no-op recompute holds the gate instead of re-evaluating.
        self.last_plan_fingerprint = self.plan_fingerprint()?;
        Ok(())
    }

    /// Fast chart-only reload: fetch trailing bars through `gateway` (capped
    /// at `limit`, defaulting to [`target_fetch`](Self::target_fetch)),
    /// rebind the viewports, and recompute every active indicator. The dock
    /// bundle and the replay window are untouched, so timeframe/symbol
    /// switches never pay for journal serialization — that cost belongs to
    /// the explicit Refresh button. The fetch lands before any assignment, so
    /// a refused fetch leaves the `(symbol, timeframe, rows)` triple
    /// untouched.
    fn refresh_chart(
        &mut self,
        gateway: &dyn TerminalGateway,
        limit: Option<usize>,
    ) -> Result<(), EguiError> {
        // A bounded read: a store-backed gateway plans a trailing window
        // through the durable tape, a store-less one serves its own history
        // and ignores the cap. The call is the same on both, so switching a
        // symbol or a timeframe replays through the identical read. An
        // unspecified bound means the active range's calibrated need (one
        // month by default), never a fixed three-month floor: the read
        // matches the window the operator is looking at, so a small window
        // never pays for a larger one and a preset grows it on demand.
        let symbol = self.active_symbol().to_owned();
        let timeframe = self.timeframe;
        let size = limit.unwrap_or_else(|| self.target_fetch(timeframe));
        let rows = Self::fetch_rows(gateway, &symbol, timeframe, size)?;
        self.apply_fetched(&symbol, timeframe, rows)
    }

    /// Trailing-bar target for the active range at `timeframe`: the
    /// calibrated need for the selected window plus indicator headroom,
    /// floored at [`CHART_WINDOW_BARS`]. A custom Beginning start sizes the
    /// span from that date to `end` instead of a fixed preset, so the window
    /// and the read are always one span. The one home for the fetch bound
    /// every chart read plans against.
    fn trailing_keep(&self, timeframe: TimeframeKey, end: Option<i64>) -> usize {
        fetch_for_span(
            self.window_span_seconds(end),
            timeframe.timeframe().total_seconds(),
            FETCH_WARMUP_BARS,
        )
    }

    /// The loaded window's calendar span in seconds: the active preset's span,
    /// or — when a Beginning start is set — the span from that date to `end`
    /// (the latest loaded bar, else now). One derivation behind the fetch
    /// bound, the load-more window, and [`visible_months`](Self::visible_months).
    fn window_span_seconds(&self, end: Option<i64>) -> u64 {
        match self.custom_start {
            Some(start) => {
                let end = end.unwrap_or_else(|| chrono::Utc::now().timestamp());
                (end - start).max(0) as u64
            }
            None => self.range.span_seconds(),
        }
    }

    /// Whole months the current window spans (rounded up): the load-more
    /// cursor and the derived [`visible_months`](Self::visible_months).
    fn window_months(&self, end: Option<i64>) -> u32 {
        let span = self.window_span_seconds(end);
        let months = span.div_ceil(MONTH_SECONDS.max(1) as u64);
        u32::try_from(months).unwrap_or(u32::MAX)
    }

    /// Trailing-bar target for the active symbol's loaded rows.
    fn target_fetch(&self, timeframe: TimeframeKey) -> usize {
        self.trailing_keep(timeframe, self.rows.last().map(|row| row.time))
    }

    /// Trailing slice of `full` holding `keep` bars (all when fewer). The one
    /// windowing step that turns a full held frame into the loaded window, so
    /// a seed never binds more bars than the selected range needs.
    fn trailing_slice(full: &[ChartRow], keep: usize) -> Vec<ChartRow> {
        let keep = keep.min(full.len());
        full[full.len() - keep..].to_vec()
    }

    /// Re-bound the loaded rows from the held frame for the active
    /// timeframe: the gateway-less growth path (a preset or Beginning pick
    /// pulls earlier bars already held instead of re-reading a store). Only
    /// the trailing window is copied — the held frame is borrowed, so an
    /// unload of a large held frame is O(window), never O(history).
    fn rebound_from_history(&mut self) {
        let Some(full) = self.histories.get(&self.timeframe) else {
            return;
        };
        let keep = self.trailing_keep(self.timeframe, full.last().map(|row| row.time));
        let rows = Self::trailing_slice(full, keep);
        self.rows = rows;
    }

    /// Trailing read for `symbol`/`timeframe` without touching state: the
    /// label, the rows, and the history slot land together in
    /// [`apply_fetched`](Self::apply_fetched) only after this succeeds.
    fn fetch_rows(
        gateway: &dyn TerminalGateway,
        symbol: &str,
        timeframe: TimeframeKey,
        size: usize,
    ) -> Result<Vec<ChartRow>, EguiError> {
        gateway
            .chart_bars_for_limit(symbol, timeframe.label(), Some(size))
            .map(|bars| bars.rows)
            .map_err(|error| EguiError::Dependency {
                reason: error.to_string(),
            })
    }

    /// Publish a fetched history: the label, the rows, and the history slot
    /// land atomically after a successful fetch, then the viewports rebind
    /// and every indicator recomputes. Every switch path funnels through
    /// here, so a refused fetch cannot strand a new label on stale rows.
    fn apply_fetched(
        &mut self,
        symbol: &str,
        timeframe: TimeframeKey,
        rows: Vec<ChartRow>,
    ) -> Result<(), EguiError> {
        self.set_symbol(symbol)?;
        self.timeframe = timeframe;
        // The full frame stays in `histories` (so a later preset can grow or
        // prune over held bars); the loaded window is the trailing slice the
        // selected range needs, never the whole store.
        let keep = self.trailing_keep(timeframe, rows.last().map(|row| row.time));
        self.rows = Self::trailing_slice(&rows, keep);
        self.histories.insert(timeframe, rows);
        self.rebind_viewports()?;
        self.recompute()
    }

    /// One live chart update. Forming bars repaint only (no indicator
    /// recompute); closed bars upsert the row and recompute through the
    /// existing pane plan/cache. Neither path reaches strategy or risk.
    ///
    /// Fail-closed on time: a bar stamped at the last bar's time revises it,
    /// a bar stamped later appends it, and a bar stamped earlier refuses —
    /// a wrapped or reordered feed (for example the rehearsal pump cycling
    /// back to its first month) never splices non-consecutive history onto
    /// the tail. The refusal names the offending stamp so the caller can
    /// break the drain instead of painting January after March.
    #[allow(clippy::needless_pass_by_value)]
    pub fn apply_live_update(&mut self, update: LiveChartUpdate) -> Result<(), EguiError> {
        fn refuse_older(what: &str, time: i64, last: i64) -> EguiError {
            EguiError::InvalidParam {
                what: what.to_owned(),
                reason: format!("live bar at {time} is older than the last loaded bar at {last}"),
            }
        }
        match update {
            LiveChartUpdate::Forming(row) => {
                match self.rows.last_mut() {
                    Some(last) if last.time == row.time => *last = row,
                    Some(last) if row.time > last.time => self.rows.push(row),
                    Some(last) => return Err(refuse_older("forming", row.time, last.time)),
                    None => self.rows.push(row),
                }
                Ok(())
            }
            LiveChartUpdate::Closed(row) => {
                match self.rows.last_mut() {
                    Some(last) if last.time == row.time => *last = row,
                    Some(last) if row.time > last.time => self.rows.push(row),
                    Some(last) => return Err(refuse_older("closed", row.time, last.time)),
                    None => self.rows.push(row),
                }
                self.recompute()
            }
        }
    }

    /// Pull fresh bars for the active symbol and timeframe through `gateway`,
    /// rebind the viewports, recompute every active indicator, and refresh
    /// the dock's cached gateway views (health/portfolio/proposals/journal).
    /// This is the full refresh a **live feed** drives (new bars keep
    /// arriving, so the dock moves too); with no live feed the menu-bar
    /// button calls [`verify_chart_range`](Self::verify_chart_range) instead,
    /// which touches the chart alone.
    pub fn refresh(&mut self, gateway: &dyn TerminalGateway) -> Result<(), EguiError> {
        self.refresh_chart(gateway, None)?;
        self.refresh_dock_bundle(gateway);
        Ok(())
    }

    /// Verify the chart's selected range is loaded — the menu-bar Refresh
    /// action when no live feed is active. It confirms the loaded window
    /// covers the active range and reads the missing trailing delta through
    /// `gateway` when it does not; an already-covered range only rebinds and
    /// recomputes. Either way the indicators redraw and the visible window
    /// refits its Y axis in place: the X position and zoom the operator is
    /// looking at never move (trailing to the newest bar is the
    /// forward-to-end button's job, not refresh's). The dock bundle and
    /// replay are left untouched: with no live feed they cannot have
    /// changed, so their serialization cost is not paid by a chart verify
    /// (the dock still loads on first open).
    pub fn verify_chart_range(&mut self, gateway: &dyn TerminalGateway) -> Result<(), EguiError> {
        // Snapshot the X axis before any rebind: rebinding rebuilds the
        // viewports onto the trailing edge, so the offsets are restored
        // afterwards, shifted by the bars the read prepended.
        let saved_offset = self.main.map(Viewport::x_offset_px);
        let saved_first = self.rows.first().map(|row| row.time);
        let needed = self.trailing_keep(self.timeframe, None);
        if needed > self.rows.len() {
            let symbol = self.active_symbol().to_owned();
            let timeframe = self.timeframe;
            let rows = Self::fetch_rows(gateway, &symbol, timeframe, needed)?;
            self.apply_fetched(&symbol, timeframe, rows)?;
        } else {
            self.rebind_viewports()?;
            self.recompute()?;
        }
        if let (Some(offset), Some(first)) = (saved_offset, saved_first) {
            self.restore_x_offset(offset, first);
        }
        self.apply_auto_height(self.last_plot_width);
        Ok(())
    }

    /// Restore the X position saved before a rebind: bars prepended by the
    /// read shift every old index right, so the offset moves left by the
    /// shift to keep the same bars under the same pixels. Tail appends keep
    /// old indices, so they need no compensation and the same math covers a
    /// pure tail advance (zero shift). A non-positive or non-finite spacing,
    /// or an empty store, holds the rebound viewport instead of refusing the
    /// whole refresh.
    fn restore_x_offset(&mut self, offset_px: f32, old_first_time: i64) {
        let Some(main) = self.main else {
            return;
        };
        let spacing = main.scale().bar_spacing();
        if !spacing.is_finite() || spacing <= 0.0 || self.rows.is_empty() {
            return;
        }
        let shift = self.rows.partition_point(|row| row.time < old_first_time) as f32;
        let next = offset_px - shift * spacing;
        if !next.is_finite() {
            return;
        }
        if let Ok(view) = main.with_x_offset(next) {
            self.main = Some(view);
            if let Some(sub) = self.sub {
                if let Ok(synced) = sub.sync_time_axis(&view) {
                    self.sub = Some(synced);
                }
            }
            for pane in &mut self.sub_panes {
                if let Ok(synced) = pane.viewport.sync_time_axis(&view) {
                    pane.viewport = synced;
                }
            }
        }
    }

    /// Build the issue-click intent document from the ticket drafts. Drafts
    /// stay plain strings; the core alone parses them, so a bad draft
    /// refuses here with the field named and never reaches the gateway.
    fn ticket_draft(&self) -> Result<TicketDraft, String> {
        // The typed fields are all the terminal contributes: the quantity text
        // has to become a `u32` to fit the draft (a text field cannot hold an
        // unparseable number), and everything else — the symbol, the price
        // text, the side, the positive-quantity rule, the ticket identity —
        // is the core's to validate and mint.
        let quantity: u32 = self
            .ticket_quantity
            .trim()
            .parse()
            .map_err(|_| "ticket quantity must be whole contracts".to_owned())?;
        Ok(TicketDraft {
            side: self.ticket_side,
            quantity,
            limit_price: self.ticket_limit.trim().to_owned(),
            symbol: self.ticket_underlying.trim().to_owned(),
        })
    }

    /// Refresh the dock's cached bundle (health/portfolio/proposals/journal)
    /// without touching the chart rows. Uncached by design elsewhere, cached
    /// here so the frame reads lines instead of re-serializing the journal.
    pub fn refresh_dock_bundle(&mut self, gateway: &dyn TerminalGateway) {
        match load_bundle(gateway) {
            Ok(bundle) => {
                let by_seq: std::collections::HashMap<i64, &protopyne_view::JournalRowView> =
                    bundle.journal.iter().map(|row| (row.seq, row)).collect();
                self.dock_journal_lines = bundle
                    .provenance_order
                    .iter()
                    .filter_map(|seq| by_seq.get(seq))
                    .map(|row| format!("#{} {} {} {}", row.seq, row.client_id, row.kind, row.at_ms))
                    .collect();
                self.dock_bundle = Some(bundle);
                self.dock_bundle_error = None;
            }
            Err(error) => {
                self.dock_bundle = None;
                self.dock_bundle_error = Some(error.to_string());
                self.dock_journal_lines.clear();
            }
        }
    }

    /// Bars the chart and indicators may read while a replay is live: the
    /// decided prefix of the loaded history. `None` is the live chart (the
    /// full store). A prefix never renumbers — indices ride untouched — so
    /// every viewport mapping and slot space stays valid.
    ///
    /// The server cursor runs in server-history coordinates; the display
    /// prefix counts loaded bars whose window is fully decided — a display
    /// bar appears only once the server has decided through its bucket, so
    /// no painter ever reads a bar the run has only half decided. Without a
    /// mapped server time axis the cursor is read as a display index, the
    /// legacy contract for gateways that serve no replay window.
    fn replay_prefix_len(&self) -> Option<usize> {
        let live = self.live_replay.as_ref()?;
        if live.server_times.is_empty() {
            return Some((live.state.cursor as usize).min(self.rows.len()));
        }
        let cursor = live.state.cursor as usize;
        if cursor >= live.server_times.len() {
            return Some(self.rows.len());
        }
        let time = live.server_times[cursor];
        // Bars stamped at or before the bar under the cursor are decided;
        // the leading bar itself never counts, so a run opened at the head
        // (or an empty store) still reads an empty prefix.
        Some(
            self.rows
                .partition_point(|row| row.time <= time)
                .saturating_sub(1),
        )
    }

    /// Truncate a full-store row slice to the replay view. The store is
    /// never mutated, so exiting returns to the live chart without a
    /// re-pull; paint and recompute pass every row slice through here.
    fn replay_view_rows<'rows>(&self, rows: &'rows [ChartRow]) -> &'rows [ChartRow] {
        match self.replay_prefix_len() {
            Some(prefix) => &rows[..prefix.min(rows.len())],
            None => rows,
        }
    }

    /// One display row per executed spread leg on a filled verdict, in fill
    /// order; empty for singles and refusals. Pure formatting over the
    /// decision's own rows — display only, no new authority.
    pub fn replay_leg_lines(decision: &ReplayDecisionView) -> Vec<String> {
        decision
            .legs
            .iter()
            .map(|leg| {
                format!(
                    "{} {} {} @ {} (fee {})",
                    leg.contract, leg.side, leg.quantity, leg.price, leg.fee
                )
            })
            .collect()
    }

    /// Open (or replace) the interactive replay session over an epoch window,
    /// then land the chart on it: indicators recompute over the decided
    /// prefix and the cursor jumps into view with the Y fit to the prefix.
    /// `start_epoch` is the first bar the run decides; `end_excl_epoch` the
    /// first bar it never decides (`None` replays to the last loaded bar).
    /// Rehearsal economics match the preview's own booking; an explicit grant
    /// document replays under that grant instead (parsed core-side, never
    /// armed). `structure` is the picker tag (`"Single"`/`"Vertical"`); the
    /// core alone maps it onto strategy and grant scope. Opening never
    /// auto-plays: the transport starts the loop explicitly, so a fresh open
    /// lands paused on its first bar.
    ///
    /// The window is resolved against the loaded chart data, which is
    /// in-filled first: when fewer than the indicators' warmup bars precede
    /// the start, earlier history is pulled through the gateway so the start
    /// carries valid indicator values; a refused pull refuses the open
    /// fail-closed rather than replaying over a gapped context. The session
    /// itself runs over the server history, so display indices are translated
    /// to server indices by timestamp (the loaded rows are the server's own
    /// trailing window); a gateway that serves no replay time axis falls back
    /// to the legacy identity mapping.
    #[allow(clippy::too_many_arguments)]
    fn open_live_replay(
        &mut self,
        gateway: &dyn TerminalGateway,
        start_epoch: i64,
        end_excl_epoch: Option<i64>,
        seed: u64,
        grant: Option<serde_json::Value>,
        structure: String,
    ) {
        if self.rows.is_empty() {
            self.replay_error = Some("replay needs loaded bars".to_owned());
            return;
        }
        // A fresh open clears the last refusal, like every other replay
        // control path: a stale reason never outlives the retry it names.
        self.replay_error = None;
        // The core declares the economics; the terminal sends them. A refused
        // read, an unknown shape, or unparseable money is a visible error, not
        // a fallback booking of our own.
        let underlying = self.active_symbol().to_owned();
        let Some(hint) = StructureKindHint::from_tag(&structure) else {
            self.replay_error = Some(format!("unknown replay structure '{structure}'"));
            return;
        };
        let economics = match gateway.rehearsal_economics(&underlying, hint) {
            Ok(economics) => economics,
            Err(error) => {
                self.replay_error = Some(error.to_string());
                return;
            }
        };
        let (max_loss, notional) = match (
            economics.max_loss.parse::<Money>(),
            economics.notional.parse::<Money>(),
        ) {
            (Ok(max_loss), Ok(notional)) => (max_loss, notional),
            (Err(error), _) | (_, Err(error)) => {
                self.replay_error = Some(format!("rehearsal economics malformed: {error}"));
                return;
            }
        };
        let mut start = self.rows.partition_point(|row| row.time < start_epoch);
        let mut end = match end_excl_epoch {
            Some(time) => self.rows.partition_point(|row| row.time < time),
            None => self.rows.len(),
        };
        if start > end {
            self.replay_error = Some(format!("replay start {start_epoch} is past the replay end"));
            return;
        }
        // In-fill the warmup context before resolving the window: the fetch
        // prepends earlier bars, so resolve again from the stable epochs.
        if let Err(error) = self.ensure_replay_coverage(start, gateway) {
            self.replay_error = Some(error.to_string());
            return;
        }
        start = self.rows.partition_point(|row| row.time < start_epoch);
        end = match end_excl_epoch {
            Some(time) => self.rows.partition_point(|row| row.time < time),
            None => self.rows.len(),
        }
        .min(self.rows.len());
        start = start.min(end);
        if start >= end {
            self.replay_error = Some("replay window is empty".to_owned());
            return;
        }
        let (server_start, server_end, server_times) =
            self.map_window_to_server(gateway, start, end);
        let params = ReplayStartParams {
            session_id: "terminal".to_owned(),
            underlying,
            strategy_name: economics.strategy,
            max_loss,
            notional,
            dte_days: economics.dte_days,
            warmup_bars: 0,
            seed,
            start_bar: server_start,
            structure,
            grant,
        };
        match gateway.replay_start(params) {
            Ok(state) => {
                let mut live = LiveReplay::new("terminal".to_owned(), state);
                live.server_end = Some(server_end);
                live.server_times = server_times;
                // Opening never auto-plays: the transport (menu-bar play or
                // the dock's play toggle) starts the loop explicitly, so a
                // fresh open lands paused on its first bar.
                live.playing = false;
                self.live_replay = Some(live);
                self.replay_clock = 0.0;
                self.replay_next_due = None;
                let _ = self.recompute();
                self.jump_to_replay_cursor();
                self.apply_auto_height_now();
            }
            Err(error) => {
                self.replay_error = Some(error.to_string());
            }
        }
    }

    /// Warmup bars the active indicators need before the replay start: the
    /// deepest leading-hole run across the current evaluations (each kernel's
    /// own warmup for its live config), floored at the fetch headroom that
    /// covers the longest shipped default lookback. Zero when nothing is
    /// evaluated yet — the open then pads with the floor alone.
    fn replay_warmup_need(&self) -> usize {
        let mut need = FETCH_WARMUP_BARS;
        for evaluation in self.evaluations.values() {
            for series in &evaluation.series {
                let holes = series
                    .values
                    .iter()
                    .take_while(|value| value.is_none())
                    .count();
                need = need.max(holes);
            }
        }
        need
    }

    /// In-fill the loaded history so the replay start carries valid
    /// indicators: when fewer than [`replay_warmup_need`](Self::replay_warmup_need)
    /// bars precede display index `start`, pull a wider trailing window
    /// through the gateway and rebind over it. The full store stays loaded,
    /// so exiting the replay returns without a re-pull; a refused pull
    /// propagates, and the caller refuses the open rather than replaying over
    /// a gapped context. Without a gateway there is nothing to pull through,
    /// so the loaded rows stand as they are.
    fn ensure_replay_coverage(
        &mut self,
        start: usize,
        gateway: &dyn TerminalGateway,
    ) -> Result<(), EguiError> {
        let pad = self.replay_warmup_need();
        if start >= pad {
            return Ok(());
        }
        let symbol = self.active_symbol().to_owned();
        let timeframe = self.timeframe;
        let size = self.rows.len() + (pad - start);
        let rows = Self::fetch_rows(gateway, &symbol, timeframe, size)?;
        self.apply_fetched(&symbol, timeframe, rows)
    }

    /// Translate a display window to server coordinates by timestamp: the
    /// first server bar at or after the display start opens the session, the
    /// first server bar at or after the display end bounds it, and the full
    /// server time axis rides along so later cursors map back to display
    /// prefixes. Display times are the server's own trailing window stamped
    /// through the same fold, so every display stamp names a server bar. A
    /// gateway that serves no (or no parseable) replay time axis falls back
    /// to the identity mapping — the legacy contract, correct whenever the
    /// loaded rows already are the session history.
    fn map_window_to_server(
        &self,
        gateway: &dyn TerminalGateway,
        start: usize,
        end: usize,
    ) -> (u64, u64, Vec<i64>) {
        let identity = || (start as u64, end as u64, Vec::new());
        let Ok(view) = gateway.replay() else {
            return identity();
        };
        if view.steps.is_empty() {
            return identity();
        }
        let mut times = Vec::with_capacity(view.steps.len());
        for step in &view.steps {
            let Ok(stamp) = chrono::DateTime::parse_from_rfc3339(&step.ts) else {
                return identity();
            };
            times.push(stamp.timestamp());
        }
        let Some(first) = self.rows.get(start).map(|row| row.time) else {
            return identity();
        };
        let server_start = times.partition_point(|time| *time < first) as u64;
        let server_end = match self.rows.get(end).map(|row| row.time) {
            Some(first_excluded) => times.partition_point(|time| *time < first_excluded) as u64,
            None => times.len() as u64,
        };
        (server_start, server_end, times)
    }

    /// Window the transport replays, in display epochs: the visible chart
    /// window captured at the moment play was pressed, defaulting to the
    /// full loaded history. The end rides `None` when it is the last loaded
    /// bar, so a later in-fill still ends at the newest bar.
    fn replay_epochs(&self) -> (i64, Option<i64>) {
        match self.replay_range {
            Some((start_epoch, end_epoch)) => {
                let end = self.rows.partition_point(|row| row.time <= end_epoch);
                let end_excl = self.rows.get(end).map(|row| row.time);
                (start_epoch, end_excl)
            }
            None => (self.rows.first().map_or(0, |row| row.time), None),
        }
    }

    /// Label for the dock replay section: the captured `start→end` dates, or
    /// `Full` when no window is captured (the transport then replays the
    /// whole loaded history).
    fn replay_range_label(&self) -> String {
        match self.replay_range {
            Some((start, end)) => format!("{}→{}", format_date_text(start), format_date_text(end)),
            None => "Full".to_owned(),
        }
    }

    /// Whether the play loop is currently stepping.
    fn replay_playing(&self) -> bool {
        self.live_replay.as_ref().is_some_and(|live| live.playing)
    }

    /// Capture the chart's own visible window as the replay window: the
    /// displayed beginning and end at the moment play is pressed. Stored as
    /// epochs, so panning the display afterwards moves the markers with the
    /// time axis without moving the window itself. An empty visible window
    /// (nothing on screen) refuses with a reason instead of opening; with
    /// no bound viewport yet, the window stays full history.
    fn capture_visible_replay_window(&mut self) -> Result<(), EguiError> {
        let Some(main) = self.main else {
            self.replay_range = None;
            return Ok(());
        };
        let width = self.last_plot_width;
        if !width.is_finite() || width <= 0.0 || self.rows.is_empty() {
            self.replay_range = None;
            return Ok(());
        }
        let (start, end) = main.visible_bar_range(width, self.rows.len());
        if start >= end {
            return Err(EguiError::InvalidParam {
                what: "replay window".to_owned(),
                reason: "nothing visible to replay".to_owned(),
            });
        }
        let start_epoch = self.rows[start].time;
        let end_epoch = self.rows[end.saturating_sub(1).max(start)].time;
        self.replay_range = Some((start_epoch, end_epoch));
        Ok(())
    }

    /// Display indices of the captured replay window's edges, for the pane
    /// divider markers: the first visible-window bar and the last one. Times
    /// are fixed at capture; indices re-resolve every frame so panning moves
    /// the markers with the time axis.
    fn replay_marker_indices(&self) -> Option<(usize, usize)> {
        let (start_epoch, end_epoch) = self.replay_range?;
        if self.rows.is_empty() {
            return None;
        }
        let start = self.rows.partition_point(|row| row.time < start_epoch);
        let end = self.rows.partition_point(|row| row.time <= end_epoch);
        if start >= self.rows.len() || end == 0 || start >= end {
            return None;
        }
        Some((start, end - 1))
    }

    /// Transport play/pause: pause when playing, resume when paused, and open
    /// then play when no run is live. Opening replays the boundary window
    /// when one is set (context-menu boundaries or a previous capture), else
    /// captures the chart's own visible window as the replay area — the
    /// displayed beginning and end at the moment play is pressed — so play
    /// without a visible window refuses instead of guessing. A finished run
    /// re-opens fresh on the next play instead of resuming past its end
    /// (the core-side session is single-cursor; rewinding the display alone
    /// would desync it). With no gateway the chart stays live and the
    /// refusal names the missing gateway.
    fn toggle_replay_play(&mut self, gateway: Option<&dyn TerminalGateway>) {
        let finished = self
            .live_replay
            .as_ref()
            .is_some_and(|live| !live.playing && live.state.cursor >= live.end());
        if self.live_replay.is_some() && !finished {
            if let Some(live) = self.live_replay.as_mut() {
                live.playing = !live.playing;
            }
            // Pausing or resuming restarts the cadence from the press: a
            // resumed run waits one full interval instead of dumping whatever
            // banked while paused.
            self.replay_next_due = None;
            return;
        }
        // No run, or a finished one: open over the boundary window when one
        // is set, else capture the visible window first. Stopping first
        // closes the spent core-side session. A refusal to capture (nothing
        // visible) names itself and opens nothing.
        if self.replay_range.is_none() {
            if let Err(error) = self.capture_visible_replay_window() {
                self.replay_error = Some(error.to_string());
                return;
            }
        }
        if let Some(gateway) = gateway {
            if self.live_replay.is_some() {
                self.stop_live_replay(gateway);
            }
        }
        let Some(gateway) = gateway else {
            self.replay_error = Some("replay needs a gateway".to_owned());
            return;
        };
        self.open_captured_replay(gateway);
        if let Some(live) = self.live_replay.as_mut() {
            live.playing = true;
        }
    }

    /// Open a replay run over the boundary window (full history when none is
    /// set, e.g. no viewport bound yet). The run always rehearses: fixed
    /// deterministic ids, and the economics the core declares for this
    /// underlying and structure — read through the gateway, never authored
    /// here, so the terminal cannot book numbers its own authority would
    /// refuse. Structure and grant choice stay core-side (the core builds the
    /// replay strategy and grant scope from the `"Single"` shape and the
    /// rehearsal grant); the dock offers no run-configuration knobs beyond the
    /// window and the transport.
    fn open_captured_replay(&mut self, gateway: &dyn TerminalGateway) {
        let (start_epoch, end_excl_epoch) = self.replay_epochs();
        self.open_live_replay(
            gateway,
            start_epoch,
            end_excl_epoch,
            7,
            None,
            "Single".to_owned(),
        );
    }

    /// Display index one past the last candle the run may decide: the loaded
    /// candles whose window the captured end covers. Identity-mapped runs
    /// read the server end straight as a display index.
    fn replay_display_end(&self) -> Option<usize> {
        let live = self.live_replay.as_ref()?;
        let end = live.end() as usize;
        if live.server_times.is_empty() {
            return Some(end.min(self.rows.len()));
        }
        if end >= live.server_times.len() {
            return Some(self.rows.len());
        }
        // Bars whose window closes at or before the end bar: the bar
        // straddling the end stays hidden with it. (Index 0 never counts
        // itself, mirroring the prefix rule.)
        let edge = live.server_times[end];
        Some(
            self.rows
                .partition_point(|row| row.time <= edge)
                .saturating_sub(1),
        )
    }

    /// Server cursor whose decision first reveals display candle `target`:
    /// the first server bar at or after that candle's own stamp, so deciding
    /// it completes the candle's window. Without a mapped time axis the
    /// target already addresses the server.
    fn server_cursor_for_display(&self, target: usize) -> Option<u64> {
        let live = self.live_replay.as_ref()?;
        if live.server_times.is_empty() {
            return Some((target as u64).min(live.end()));
        }
        if target >= self.rows.len() {
            return Some(live.end());
        }
        if target == 0 {
            return Some(live.state.cursor.min(live.end()));
        }
        let stamp = self.rows[target].time;
        Some((live.server_times.partition_point(|time| *time < stamp) as u64).min(live.end()))
    }

    /// Advance the run by up to `display_candles` chart candles at the
    /// selected timeframe, mapping through the server time axis, and bound
    /// the core work to `max_server_bars` per call. Returns the display
    /// candles actually decided. Reaching the captured end stops the loop
    /// like a zero step does.
    fn step_display_bars(
        &mut self,
        gateway: &dyn TerminalGateway,
        display_bars: u64,
        max_server_bars: u64,
    ) -> u64 {
        let Some(prefix) = self.replay_prefix_len() else {
            return 0;
        };
        let Some(display_end) = self.replay_display_end() else {
            return 0;
        };
        let target = prefix
            .saturating_add(usize::try_from(display_bars).unwrap_or(usize::MAX))
            .min(display_end);
        if target <= prefix {
            if let Some(live) = self.live_replay.as_mut() {
                live.playing = false;
            }
            return 0;
        }
        let Some(mut server_target) = self.server_cursor_for_display(target) else {
            return 0;
        };
        server_target =
            server_target.min(self.live_replay.as_ref().map_or(u64::MAX, LiveReplay::end));
        let mut remaining = max_server_bars;
        for _ in 0..4096 {
            let cursor = self
                .live_replay
                .as_ref()
                .map_or(server_target, |live| live.state.cursor);
            if cursor >= server_target {
                break;
            }
            let step = (server_target - cursor).min(256).min(remaining);
            if step == 0 {
                break;
            }
            self.step_live_replay(gateway, step);
            remaining = remaining.saturating_sub(step);
            let advanced = self
                .live_replay
                .as_ref()
                .map_or(cursor, |live| live.state.cursor);
            if advanced <= cursor || remaining == 0 {
                break;
            }
        }
        self.replay_prefix_len()
            .unwrap_or(prefix)
            .saturating_sub(prefix) as u64
    }

    /// Step the live session, then recompute indicators over the new prefix,
    /// follow the cursor when it leaves the view, and refit the Y axis to
    /// the decided window so every newly revealed candle paints in full. A
    /// refused step stops the play loop with the reason on the section; the
    /// session stays live so the operator can retry. Steps clamp to the run's
    /// server end (the captured window's end, or the last server bar):
    /// arriving stops play, so a backtest never steps past what the operator
    /// asked for.
    fn step_live_replay(&mut self, gateway: &dyn TerminalGateway, bars: u64) {
        let end = self.live_replay.as_ref().map(LiveReplay::end);
        let bars = match end {
            Some(end) => {
                let cursor = self
                    .live_replay
                    .as_ref()
                    .map_or(0, |live| live.state.cursor);
                bars.min(end.saturating_sub(cursor))
            }
            None => bars,
        };
        if bars == 0 {
            if let Some(live) = self.live_replay.as_mut() {
                live.playing = false;
            }
            return;
        }
        let Some(live) = self.live_replay.as_mut() else {
            return;
        };
        match gateway.replay_step(&live.session_id, bars) {
            Ok(state) => {
                live.state = state;
                live.error = None;
                if live.state.cursor >= live.end() || live.state.cursor >= live.state.total_bars {
                    live.playing = false;
                }
            }
            Err(error) => {
                live.error = Some(error.to_string());
                live.playing = false;
                return;
            }
        }
        let _ = self.recompute();
        self.follow_replay_cursor();
        // Refit the visible window's Y axis to the newly revealed bars, so
        // the full candles stay painted as the run advances through them.
        self.apply_auto_height(self.last_plot_width);
    }

    /// Close the live session core-side and return to the live chart: the
    /// full history was never unloaded, so this recomputes over it without
    /// a re-pull. A refused close still clears the local run — the operator
    /// asked to exit — with the desync named on the section.
    fn stop_live_replay(&mut self, gateway: &dyn TerminalGateway) {
        if let Some(live) = self.live_replay.as_ref() {
            if let Err(error) = gateway.replay_stop(&live.session_id) {
                self.replay_error = Some(error.to_string());
            }
        }
        self.live_replay = None;
        self.replay_clock = 0.0;
        self.replay_next_due = None;
        let _ = self.recompute();
    }

    /// Play pump: pace steps to the selected speed — counted in display
    /// candles at the selected timeframe, so 1 candle/s advances one chart
    /// candle per second on any timeframe — and keep frames coming until
    /// paused or the head is reached. Frame-level (not section-level), so
    /// menu-bar play advances with the drawer closed; the run's end clamp
    /// still bounds every step.
    ///
    /// Steady, never bursty: each paced candle has a wall-clock deadline one
    /// interval after the previous (`1/rate`: 0.2 s at Fast), and a frame
    /// steps at most one candle — the one whose deadline crossed — no matter
    /// how much time banked up. A stall longer than one interval drops the
    /// missed candles instead of dumping them as a burst, and pausing or
    /// reopening restarts the cadence from the press. The wake-up is
    /// scheduled for the next deadline (capped at 100 ms), so at Fast frames
    /// land one candle apart instead of idling. Full speed steps the gateway
    /// maximum every frame instead.
    fn pump_replay_play(
        &mut self,
        gateway: Option<&dyn TerminalGateway>,
        ui: &mut egui::Ui,
        dt: std::time::Duration,
    ) {
        if !self.live_replay.as_ref().is_some_and(|live| live.playing) {
            return;
        }
        self.replay_clock += dt.as_secs_f64();
        let clock = self.replay_clock;
        // The frame started one `dt` ago: a deadline initialized now counts
        // from the press, not from after the first frame's time.
        let frame_start = clock - dt.as_secs_f64();
        // Deadline slop: frame deltas like 0.1 s are not exact in binary, so
        // ten of them can land a rounding step short of the tenth deadline.
        // A nanosecond of grace keeps the cadence without ever stepping two.
        const DEADLINE_EPS: f64 = 1e-9;
        // How long to sleep before the next frame: paced play wakes for its
        // next deadline, Full speed polls at the idle cadence.
        let mut wait = std::time::Duration::from_millis(100);
        match self.replay_speed.candles_per_sec() {
            Some(rate) => {
                let interval = 1.0 / rate;
                let mut due_at = if let Some(due_at) = self.replay_next_due {
                    due_at
                } else {
                    // The first pump after a press waits one full interval:
                    // a fresh press never opens with a burst.
                    let due_at = frame_start + interval;
                    self.replay_next_due = Some(due_at);
                    due_at
                };
                if due_at <= clock + DEADLINE_EPS {
                    if let Some(gateway) = gateway {
                        // At most one candle per frame: the deadline that
                        // crossed. A zero step means the end clamp stopped
                        // the run, so there is nothing more to schedule.
                        if self.step_display_bars(gateway, 1, 256) == 0 {
                            self.replay_next_due = None;
                            return;
                        }
                    }
                    due_at += interval;
                }
                // A stall banked more than one interval: drop the backlog
                // instead of dumping it — steadiness over exactness.
                if due_at <= clock + DEADLINE_EPS {
                    due_at = clock + interval;
                }
                self.replay_next_due = Some(due_at);
                wait = std::time::Duration::from_secs_f64((due_at - clock).clamp(0.0, 0.1));
            }
            // Uncapped: as fast as the core calculates, one gateway maximum
            // per frame.
            None => {
                if let Some(gateway) = gateway {
                    self.step_display_bars(gateway, u64::MAX, 256);
                }
            }
        }
        if self.live_replay.as_ref().is_some_and(|live| live.playing) {
            ui.ctx().request_repaint_after(wait);
        }
    }

    /// Land the replay head at the pane's right edge, preserving zoom and
    /// the price window — the same placement a jump-to-current performs,
    /// aimed at the last decided bar instead of the last loaded one.
    fn jump_to_replay_cursor(&mut self) {
        let Some(prefix) = self.replay_prefix_len() else {
            return;
        };
        let head = prefix.saturating_sub(1);
        let width = self.last_plot_width;
        if !width.is_finite() || width <= 0.0 {
            return;
        }
        let _ = self.place_bar_at_right(head, width);
    }

    /// Follow the replay head while playing: when the last decided bar
    /// leaves the culled view, place it back at the right edge. A visible
    /// head never moves the chart under the operator.
    fn follow_replay_cursor(&mut self) {
        let Some(prefix) = self.replay_prefix_len() else {
            return;
        };
        if prefix == 0 {
            return;
        }
        let Some(main) = self.main else {
            return;
        };
        let width = self.last_plot_width;
        if !width.is_finite() || width <= 0.0 {
            return;
        }
        let (start, end) = main.culled_bar_range(width, prefix);
        let head = prefix - 1;
        if head >= start && head < end {
            return;
        }
        let _ = self.place_bar_at_right(head, width);
    }

    /// Set the backtest schema once when the served descriptor is available
    /// (the terminal composition root owns the descriptor; this crate never
    /// names the trading path). Until set, the backtest subpane offers the
    /// run params over an empty document and the server names any missing
    /// field on submit.
    pub fn set_backtest_schema(&mut self, schema: &serde_json::Value) {
        self.backtest_schema = Some(schema.clone());
        // A stored run configuration survives the schema arriving late: only
        // the document and the leaf set refresh, never the operator's text.
        let seed = self.backtest_form.seed_text().to_owned();
        let initial_equity = self.backtest_form.initial_equity_text().to_owned();
        let start = self.backtest_form.start_text().to_owned();
        let end = self.backtest_form.end_text().to_owned();
        self.backtest_form = BacktestFormState::from_schema(schema);
        self.backtest_form.set_seed_text(seed);
        self.backtest_form.set_initial_equity_text(initial_equity);
        self.backtest_form.set_start_text(start);
        self.backtest_form.set_end_text(end);
    }

    /// Install Pine indicator sources discovered at boot: manifest rows for
    /// the catalog picker plus the file texts linking needs at slot time.
    /// Empty inputs clear both, leaving the catalog exactly as before.
    pub fn set_pine_sources(
        &mut self,
        indicators: Vec<protopyne_kernels::ManifestFile>,
        libraries: Vec<protopyne_kernels::ManifestFile>,
    ) {
        self.pine_manifest = protopyne_kernels::build_manifest(&indicators, &libraries);
        self.pine_indicators = indicators;
        self.pine_libraries = libraries;
    }

    /// One multi-timeframe row per loaded history: the window direction
    /// over that timeframe's loaded rows plus the signed percent move on
    /// the display band. A flat window reads flat; an unmeasurable window
    /// is skipped, never zero-filled.
    fn mtf_window_rows(&self) -> Vec<MtfRow> {
        let mut keys: Vec<TimeframeKey> = self.histories.keys().copied().collect();
        keys.sort();
        let mut rows = Vec::new();
        for key in keys {
            let Some(history) = self.histories.get(&key) else {
                continue;
            };
            let (Some(first), Some(last)) = (history.first(), history.last()) else {
                continue;
            };
            if !first.close.is_finite() || !last.close.is_finite() || first.close == 0.0 {
                continue;
            }
            let moved = (last.close - first.close) / first.close.abs() * 100.0;
            if !moved.is_finite() {
                continue;
            }
            let bias = if moved > 0.0 {
                "bull"
            } else if moved < 0.0 {
                "bear"
            } else {
                "flat"
            };
            rows.push(MtfRow {
                timeframe: key.label().to_owned(),
                bias: bias.to_owned(),
                strength: moved.clamp(-100.0, 100.0),
            });
        }
        rows
    }

    /// Show one stored run on the chart: the marker/resting-line overlay
    /// plus the equity pane, replacing any previous run's slots. The
    /// plugins carry the run's points (immutable evaluation input); the
    /// slot configs carry the run id that keys the plan fingerprint, so a
    /// new run always recomputes. Reads the cached model only — never the
    /// gateway. Called after the backtest panel paints, so the overlay
    /// follows the model with no extra click.
    pub fn sync_backtest_overlay(&mut self) {
        let want = self
            .backtest
            .as_ref()
            .and_then(|model| model.view())
            .map(|view| view.run_id.clone());
        if want == self.backtest_overlay_run_id() {
            return;
        }
        if self.stack.contains(BACKTEST_OVERLAY_ID) {
            let _ = self.remove_indicator(BACKTEST_OVERLAY_ID);
        }
        if self.stack.contains(BACKTEST_EQUITY_ID) {
            let _ = self.remove_indicator(BACKTEST_EQUITY_ID);
        }
        let Some(run_id) = want else {
            return;
        };
        let view = self
            .backtest
            .as_ref()
            .and_then(|model| model.view())
            .cloned();
        let Some(view) = view else {
            return;
        };
        let (overlay, overlay_config) = crate::overlay::BacktestOverlay::with_run(&run_id, &view);
        let (equity, equity_config) = crate::overlay::EquityPane::with_run(&run_id, &view);
        let _ = self.slot_backtest_plugin(
            Box::new(overlay),
            Box::new(overlay_config),
            BACKTEST_OVERLAY_ID,
            "Backtest Overlay",
            fluent_egui::SlotAttachment::MainOverlay,
        );
        let _ = self.slot_backtest_plugin(
            Box::new(equity),
            Box::new(equity_config),
            BACKTEST_EQUITY_ID,
            "Backtest Equity",
            fluent_egui::SlotAttachment::SubPane,
        );
    }

    /// The run id the overlay slots were built for, when both are slotted
    /// and agree. Anything else reads as unslotted, so the next sync
    /// rebuilds rather than painting a half-run overlay.
    fn backtest_overlay_run_id(&self) -> Option<String> {
        let overlay = self.stack.config(BACKTEST_OVERLAY_ID)?;
        let equity = self.stack.config(BACKTEST_EQUITY_ID)?;
        let overlay_id =
            fluent_wvr::component_downcast_ref::<crate::overlay::BacktestOverlayConfig>(overlay)?
                .run_id
                .clone();
        let equity_id =
            fluent_wvr::component_downcast_ref::<crate::overlay::BacktestOverlayConfig>(equity)?
                .run_id
                .clone();
        (overlay_id == equity_id).then_some(overlay_id)
    }

    /// Slot one prebuilt backtest plugin: the catalog path builds from an
    /// id, but a run overlay arrives with its points, so the plugin and its
    /// run-id configuration enter together.
    fn slot_backtest_plugin(
        &mut self,
        plugin: Box<dyn crate::indicators::ChartIndicator>,
        config: Box<dyn fluent_wvr::Component>,
        id: &str,
        title: &str,
        attachment: fluent_egui::SlotAttachment,
    ) -> Result<(), EguiError> {
        let nodes = plugin.plan_nodes(&*config)?;
        let node_refs: Vec<&str> = nodes.iter().map(String::as_str).collect();
        let deps = plugin.plan_deps(&*config)?;
        let dep_refs: Vec<&str> = deps.iter().map(String::as_str).collect();
        let fingerprint = common_core::hash::fnv1a64(id.as_bytes());
        self.stack.insert(
            plugin.id(),
            title,
            attachment,
            config,
            &node_refs,
            &dep_refs,
            fingerprint,
            &mut self.plan,
        )?;
        self.plugins.push(plugin);
        Ok(())
    }

    /// Whether the Material dock is open.
    #[must_use]
    pub fn dock_open(&self) -> bool {
        self.dock_open
    }

    /// Flip the Material dock.
    pub fn toggle_dock(&mut self) {
        self.dock_open = !self.dock_open;
    }

    /// Effective trailing window for the active range plus the custom
    /// Beginning filter: the preset window first, then bars at or after
    /// `custom_start` when set. Beginning replaces the preset's left edge
    /// (picking a preset clears it), so the two never fight.
    #[must_use]
    pub fn effective_window(&self) -> &[ChartRow] {
        let window = window_for_range(
            &self.rows,
            self.range,
            self.timeframe.timeframe().total_seconds(),
        );
        match self.custom_start {
            Some(start) => {
                let skip = window.iter().take_while(|row| row.time < start).count();
                &window[skip.min(window.len())..]
            }
            None => window,
        }
    }

    /// Parse `YYYY-MM-DD` and set it as the custom window start (Beginning).
    /// Clears to the default two-month window on empty input. Refuses
    /// unparseable dates and dates past the last bar; a date before the first
    /// bar clamps to the first bar. When a gateway can serve more, the read
    /// grows to cover the picked date to the latest bar (the chosen window),
    /// then the view trails to the newest bar; the dock bundle is untouched.
    pub fn set_beginning(
        &mut self,
        text: &str,
        gateway: Option<&dyn TerminalGateway>,
    ) -> Result<(), EguiError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            self.custom_start = None;
            self.range = DEFAULT_RANGE;
            self.rebind_viewports()?;
            return Ok(());
        }
        let date = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d").map_err(|_| {
            EguiError::InvalidParam {
                what: "beginning".to_owned(),
                reason: format!("{trimmed:?} is not YYYY-MM-DD"),
            }
        })?;
        let start = date
            .and_hms_opt(0, 0, 0)
            .map_or(0, |dt| dt.and_utc().timestamp());
        if let Some(last) = self.rows.last() {
            if start > last.time {
                return Err(EguiError::InvalidParam {
                    what: "beginning".to_owned(),
                    reason: "beginning is past the last bar".to_owned(),
                });
            }
        }
        // Land the window first so the chart-only read below sizes itself to
        // the picked span (Beginning replaces the preset's left edge). The
        // visible width derives from `custom_start`, never a stored copy.
        self.custom_start = Some(start);
        self.range = DateRange::All;
        // A store-backed gateway may hold more history than the current
        // trailing window; a seeded workspace grows over bars already held.
        // Either way the read/rebind is chart-only, so the picker never pays
        // for journal serialization.
        if let Some(gateway) = gateway {
            let _ = self.refresh_chart(gateway, None);
        } else {
            self.rebound_from_history();
            // Rows changed under the held frame: re-evaluate so the
            // panes track the widened window, never the old tape.
            self.recompute()?;
        }
        self.rebind_viewports()?;
        self.jump_to_current_plot(self.last_plot_width)?;
        Ok(())
    }

    /// Select a chart-range preset from the range menu: set the loaded window
    /// to the selected range — relative to the most recent bars — loading or
    /// unloading the held history as needed. A range whose need exceeds what
    /// is held grows: from the held full frame when it reaches back far
    /// enough (a seeded workspace, or a previously wider read), else from a
    /// bounded gateway read. A smaller range unloads: the loaded window is
    /// pruned to the range's trailing bars with no read, trailed to the most
    /// recent bar, redrawn, and refit. A larger range restores the previous
    /// rightmost bar when it is still in view, else trails.
    pub fn select_range(
        &mut self,
        range: DateRange,
        gateway: Option<&dyn TerminalGateway>,
    ) -> Result<(), EguiError> {
        if self.range == range && self.custom_start.is_none() {
            return Ok(());
        }
        let anchor = self.rightmost_time(self.last_plot_width);
        // Shrinking means the new trailing need fits inside what is already
        // loaded: the window re-anchors to the most recent bars.
        let shrinking = {
            let needed = {
                let saved_range = self.range;
                let saved_start = self.custom_start;
                self.range = range;
                self.custom_start = None;
                let needed = self.trailing_keep(self.timeframe, None);
                self.range = saved_range;
                self.custom_start = saved_start;
                needed
            };
            needed < self.rows.len()
        };
        self.range = range;
        self.custom_start = None;
        let needed = self.trailing_keep(self.timeframe, None);
        // The held full frame is the first source: when it already reaches
        // back the whole range, re-bound from it (this both grows and unloads)
        // without touching the gateway.
        let held = self.histories.get(&self.timeframe).map_or(0, Vec::len);
        if held >= needed {
            self.rebound_from_history();
        } else if let Some(gateway) = gateway {
            let symbol = self.active_symbol().to_owned();
            let timeframe = self.timeframe;
            let rows = Self::fetch_rows(gateway, &symbol, timeframe, needed)?;
            // `apply_fetched` rebinds and recomputes; the anchor step below
            // finishes the change.
            self.apply_fetched(&symbol, timeframe, rows)?;
            return self.finish_range_change(anchor, shrinking);
        } else {
            // A short held frame: show everything it holds.
            self.rebound_from_history();
        }
        self.rebind_viewports()?;
        self.recompute()?;
        self.finish_range_change(anchor, shrinking)
    }

    /// Finish a range change over freshly bound rows: a shrinking window
    /// trails to the most recent bar, a growing one restores the previous
    /// rightmost bar when still in view. Both redraws already landed via the
    /// caller's rebind/recompute; both refit the visible window's Y axis.
    fn finish_range_change(
        &mut self,
        anchor: Option<i64>,
        shrinking: bool,
    ) -> Result<(), EguiError> {
        if shrinking {
            // Best-effort trail: an unmeasured width holds the rebound window
            // rather than refusing the whole range pick.
            let _ = self.jump_to_current_plot(self.last_plot_width);
        } else {
            self.restore_or_trail(anchor, self.last_plot_width)?;
        }
        self.apply_auto_height(self.last_plot_width);
        Ok(())
    }

    /// Jump the time axis to an absolute `YYYY-MM-DD HH:MM` (or `YYYY-MM-DD`)
    /// target: the nearest bar at or before the target becomes the rightmost
    /// visible bar, preserving zoom and the price window. A target earlier
    /// than the loaded window loads the additional range necessary to include
    /// it (widening to a Beginning start at the target, chart-only); a target
    /// past the latest bar refreshes once, then clamps to the newest bar. A
    /// target inside the loaded window moves with no read.
    pub fn jump_to_time(
        &mut self,
        text: &str,
        plot_width: f32,
        gateway: Option<&dyn TerminalGateway>,
    ) -> Result<(), EguiError> {
        let trimmed = text.trim();
        let target = chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M")
            .map(|dt| dt.and_utc().timestamp())
            .or_else(|_| {
                chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d").map(|date| {
                    date.and_hms_opt(0, 0, 0)
                        .map_or(0, |dt| dt.and_utc().timestamp())
                })
            })
            .map_err(|_| EguiError::InvalidParam {
                what: "jump".to_owned(),
                reason: format!("{trimmed:?} is not YYYY-MM-DD HH:MM"),
            })?;
        if self.rows.is_empty() {
            return Err(EguiError::InvalidParam {
                what: "jump".to_owned(),
                reason: "no bars loaded".to_owned(),
            });
        }
        let first = self.rows.first().map_or(target, |row| row.time);
        let last = self.rows.last().map_or(target, |row| row.time);
        if target < first {
            // Widen the loaded range back to the target so the jump index
            // exists: a Beginning start at the target, chart-only, mirroring
            // the load-more widen path. A refused read holds the current rows
            // and the place below clamps to the earliest held bar.
            self.custom_start = Some(target);
            self.range = DateRange::All;
            if let Some(gateway) = gateway {
                let _ = self.refresh_chart(gateway, None);
                self.rebind_viewports()?;
            } else {
                self.rebound_from_history();
                self.rebind_viewports()?;
                let _ = self.recompute();
            }
        } else if target > last {
            // The tape may have advanced past the loaded tail: one chart-only
            // read, then clamp to whatever is newest.
            if let Some(gateway) = gateway {
                let _ = self.refresh_chart(gateway, None);
            }
        }
        if self.rows.is_empty() {
            return Err(EguiError::InvalidParam {
                what: "jump".to_owned(),
                reason: "no bars loaded".to_owned(),
            });
        }
        // Nearest bar at or before the target; clamp to ends.
        let pos = self.rows.partition_point(|row| row.time <= target);
        let index = pos.saturating_sub(1).min(self.rows.len() - 1);
        self.place_bar_at_right(index, plot_width)?;
        Ok(())
    }

    /// Set the Jump text's date part from a calendar pick, preserving the
    /// clock part: a picked day never clobbers a typed `HH:MM`.
    fn apply_jump_date(&mut self, date: &str) {
        let time = self
            .top_menu
            .jump_text
            .split_whitespace()
            .nth(1)
            .unwrap_or("")
            .to_owned();
        self.top_menu.jump_text = if time.is_empty() {
            date.to_owned()
        } else {
            format!("{date} {time}")
        };
    }

    /// Whether the chart is panned to the earliest loaded bar: the culled
    /// window starts at bar zero over the last measured plot width. Empty
    /// stores and unbound viewports answer false so the affordance never
    /// shows before the chart is loaded.
    fn at_left_edge(&self) -> bool {
        if self.rows.is_empty() {
            return false;
        }
        let Some(main) = self.main else {
            return false;
        };
        let width = self.last_plot_width;
        if !width.is_finite() || width <= 0.0 {
            return false;
        }
        let (start, end) = main.culled_bar_range(width, self.rows.len());
        start == 0 && end > start
    }

    /// Load and prepend the previous month of chart data, widening the
    /// visible window one month. Keeps the current rightmost bar stable so
    /// the eyes stay on the data. Returns `Ok(true)` when the window grew,
    /// `Ok(false)` when there is nothing more to load (empty store, or the
    /// gateway served no earlier bars).
    pub fn load_previous_month(
        &mut self,
        gateway: Option<&dyn TerminalGateway>,
    ) -> Result<bool, EguiError> {
        if self.rows.is_empty() {
            return Ok(false);
        }
        let anchor = self.rightmost_time(self.last_plot_width);
        // Widen the visible window one month over the (possibly grown) rows.
        // The current span comes from the one derivation (preset, or the
        // existing Beginning start); one month is added. A start past the
        // earliest available bar simply shows every held row: the read/rebind
        // clamps to what exists, never to an empty window.
        let last = self.rows.last().map_or(0, |row| row.time);
        let span = self.window_span_seconds(Some(last));
        let cutoff = last.saturating_sub(i64::try_from(span).unwrap_or(i64::MAX) + MONTH_SECONDS);
        let before = self.rows.len();
        // Land the window first so the chart-only read below sizes itself to
        // the widened span (Beginning semantics: closest earlier bar to now).
        self.custom_start = Some(cutoff);
        self.range = DateRange::All;
        // A store-backed gateway serves earlier bars for the wider span; a
        // seeded workspace widens over bars already held. A refused read still
        // leaves the widened window over the held rows, so the caller sees no
        // growth rather than a dead button.
        if let Some(gateway) = gateway {
            if self.refresh_chart(gateway, None).is_err() {
                self.rebind_viewports()?;
                self.restore_anchor(anchor, self.last_plot_width)?;
                return Ok(false);
            }
        } else {
            self.rebound_from_history();
            // Rows changed under the held frame: re-evaluate so the
            // panes track the widened window, never the old tape.
            self.recompute()?;
        }
        self.rebind_viewports()?;
        // Keep the same rightmost bar so the new month appears to the left
        // instead of yanking the view; keep the price window (no refit).
        self.restore_anchor(anchor, self.last_plot_width)?;
        Ok(gateway.is_none() || self.rows.len() > before)
    }

    /// Rightmost visible bar time for `plot_width`, or the last bar when no
    /// viewport is bound. Used to anchor timeframe switches.
    fn rightmost_time(&self, plot_width: f32) -> Option<i64> {
        let main = self.main?;
        let (start, end) = main.culled_bar_range(plot_width, self.rows.len());
        if start < end {
            self.rows.get(end - 1).map(|row| row.time)
        } else {
            self.rows.last().map(|row| row.time)
        }
    }

    /// Place `index` at the right edge for `plot_width`, preserving spacing
    /// and the price window (time-only move). Used by Jump actions and by the
    /// timeframe anchor restore.
    fn place_bar_at_right(&mut self, index: usize, plot_width: f32) -> Result<(), EguiError> {
        let main = self.main.ok_or(EguiError::InvalidParam {
            what: "viewport".to_owned(),
            reason: "jump needs a bound price viewport".to_owned(),
        })?;
        let spacing = main.scale().bar_spacing();
        if !plot_width.is_finite() || plot_width <= 0.0 {
            return Err(EguiError::InvalidParam {
                what: "plot_width".to_owned(),
                reason: "plot width must be positive".to_owned(),
            });
        }
        // index_to_x(index) + offset = plot_width  =>  offset = width - index*spacing - spacing
        // (the bar's slot right edge lands at the pane's right edge).
        let offset = plot_width - (index as f32 + 1.0) * spacing;
        if !offset.is_finite() {
            return Err(EguiError::InvalidParam {
                what: "x_offset".to_owned(),
                reason: "jump offset is not finite".to_owned(),
            });
        }
        let next = main.with_x_offset(offset)?;
        self.main = Some(next);
        if let Some(sub) = self.sub {
            if let Ok(synced) = sub.sync_time_axis(&next) {
                self.sub = Some(synced);
            }
        }
        Ok(())
    }

    /// Scroll so the latest bar sits at the right edge, preserving zoom and
    /// the price window. No refit.
    pub fn jump_to_current_plot(&mut self, plot_width: f32) -> Result<(), EguiError> {
        if self.rows.is_empty() {
            return Err(EguiError::InvalidParam {
                what: "rows".to_owned(),
                reason: "no bars to jump to".to_owned(),
            });
        }
        // A live replay's "current" is the replay head, not the last loaded
        // bar: the jump lands on the last decided bar.
        let total = self.replay_prefix_len().unwrap_or(self.rows.len());
        let last = total.saturating_sub(1).min(self.rows.len() - 1);
        self.place_bar_at_right(last, plot_width)
    }

    /// The rewind/forward step in bars for the active timeframe: the calendar
    /// pan unit (see [`crate::session::pan_unit_seconds`]) expressed on the
    /// bar grid, never below one bar. A 1M/5M chart steps a day, a 15M/1H
    /// chart a week, so one click moves a couple hundred candles.
    fn pan_step_bars(&self) -> i64 {
        let seconds = self.timeframe.timeframe().total_seconds().max(1) as i64;
        (crate::session::pan_unit_seconds(self.timeframe.timeframe().total_seconds()) / seconds)
            .max(1)
    }

    /// Step the time axis by `delta_bars` (negative moves earlier) on
    /// `plot_width`, preserving zoom and the price window, then refit
    /// AutoHeight to the window that lands. The rightmost visible bar is the
    /// anchor, so repeated clicks pan smoothly rather than re-centering.
    fn pan_by_bars(&mut self, delta_bars: i64, plot_width: f32) -> Result<(), EguiError> {
        if self.rows.is_empty() {
            return Err(EguiError::InvalidParam {
                what: "rows".to_owned(),
                reason: "no bars to pan".to_owned(),
            });
        }
        let main = self.main.ok_or(EguiError::InvalidParam {
            what: "viewport".to_owned(),
            reason: "pan needs a bound price viewport".to_owned(),
        })?;
        let (start, end) = main.visible_bar_range(plot_width, self.rows.len());
        let rightmost = end.saturating_sub(1).max(start);
        let last = self.rows.len() as i64 - 1;
        let target = (rightmost as i64 + delta_bars).clamp(0, last) as usize;
        self.place_bar_at_right(target, plot_width)?;
        // Every rewind/forward lands a fit, so the newly visible window fills
        // the pane instead of opening on a stale price range.
        self.apply_auto_height(plot_width);
        Ok(())
    }

    /// Rewind one pan unit toward the beginning of the loaded data, then
    /// refit AutoHeight.
    pub fn rewind_view(&mut self) -> Result<(), EguiError> {
        let step = self.pan_step_bars();
        self.pan_by_bars(-step, self.last_plot_width)
    }

    /// Advance one pan unit toward the end of the loaded data, then refit
    /// AutoHeight.
    pub fn forward_view(&mut self) -> Result<(), EguiError> {
        let step = self.pan_step_bars();
        self.pan_by_bars(step, self.last_plot_width)
    }

    /// Jump to the beginning of the loaded data: the earliest bar sits at the
    /// pane's left edge, preserving zoom and the price window; then refit
    /// AutoHeight.
    pub fn rewind_to_beginning_view(&mut self) -> Result<(), EguiError> {
        if self.rows.is_empty() {
            return Err(EguiError::InvalidParam {
                what: "rows".to_owned(),
                reason: "no bars to jump to".to_owned(),
            });
        }
        let width = self.last_plot_width;
        if !width.is_finite() || width <= 0.0 {
            return Err(EguiError::InvalidParam {
                what: "plot_width".to_owned(),
                reason: "chart not measured yet".to_owned(),
            });
        }
        let main = self.main.ok_or(EguiError::InvalidParam {
            what: "viewport".to_owned(),
            reason: "jump needs a bound price viewport".to_owned(),
        })?;
        // Offset zero maps bar zero to the pane's left edge, so the window
        // opens on the earliest loaded bars.
        let next = main.with_x_offset(0.0)?;
        self.main = Some(next);
        if let Some(sub) = self.sub {
            if let Ok(synced) = sub.sync_time_axis(&next) {
                self.sub = Some(synced);
            }
        }
        self.apply_auto_height(width);
        Ok(())
    }

    /// Jump to the end of the loaded data: the latest bar sits at the pane's
    /// right edge (the mirror of
    /// [`rewind_to_beginning_view`](Self::rewind_to_beginning_view)),
    /// preserving zoom and the price window; then refit AutoHeight.
    pub fn forward_to_end_view(&mut self) -> Result<(), EguiError> {
        let width = self.last_plot_width;
        if !width.is_finite() || width <= 0.0 {
            return Err(EguiError::InvalidParam {
                what: "plot_width".to_owned(),
                reason: "chart not measured yet".to_owned(),
            });
        }
        self.jump_to_current_plot(width)?;
        self.apply_auto_height(width);
        Ok(())
    }

    /// Seed a history for the active timeframe directly and recompute (no
    /// gateway). Distinct timeframes keep their own histories; the switch
    /// path re-reads the store.
    pub fn set_rows(&mut self, rows: Vec<ChartRow>) -> Result<(), EguiError> {
        self.seed_history(self.timeframe, rows)
    }

    /// Switch the active ticker and pull its history through `gateway`.
    /// Chart-only: the dock bundle and replay stay out (Refresh owns them).
    /// The window and range carry over (the new ticker loads on demand for the
    /// range already selected). The fetch lands before any assignment, so a
    /// refused fetch leaves the `(symbol, timeframe, rows)` triple untouched.
    /// A live replay run never survives the switch: its window epochs and
    /// server coordinates belong to the previous ticker, so the run closes
    /// and the captured window clears rather than deciding one history while
    /// painting another. (Timeframe switches keep both: epochs re-resolve
    /// and the timestamp mapping is timeframe-agnostic.)
    pub fn switch_symbol(
        &mut self,
        symbol: &str,
        gateway: &dyn TerminalGateway,
    ) -> Result<(), EguiError> {
        self.stash_current_layout();
        let timeframe = self.timeframe;
        let size = self.target_fetch(timeframe);
        let rows = Self::fetch_rows(gateway, symbol, timeframe, size)?;
        self.apply_fetched(symbol, timeframe, rows)?;
        self.clear_replay_for_symbol_switch(Some(gateway));
        // The incoming ticker restores its own layout: a saved entry wins,
        // else the session's stashed one keeps unsaved edits while roaming.
        let layout = self
            .saved_layouts
            .get(symbol)
            .or_else(|| self.stashed_layouts.get(symbol))
            .cloned();
        if let Some(layout) = layout {
            self.apply_ticker_layout(&layout, Some(gateway))?;
        }
        Ok(())
    }

    /// Switch the active timeframe and pull its history through `gateway`.
    /// Fast path: chart-only fetch (no dock/replay serialization), anchor the
    /// rightmost time on the last measured width, and defer the AutoHeight
    /// fit to the next painted frame — never on a proxy width or an empty
    /// store. The fetch lands before any assignment, so a refused fetch
    /// leaves the `(symbol, timeframe, rows)` triple untouched.
    pub fn switch_timeframe(
        &mut self,
        timeframe: TimeframeKey,
        gateway: &dyn TerminalGateway,
    ) -> Result<(), EguiError> {
        let anchor = self.rightmost_time(self.last_plot_width);
        let symbol = self.active_symbol().to_owned();
        let size = self.target_fetch(timeframe);
        let rows = Self::fetch_rows(gateway, &symbol, timeframe, size)?;
        if rows.is_empty() {
            // The gateway's base cannot fold this grid whole — a session day
            // tears every daily UTC bucket, so it serves an empty window.
            // Fall back to the display fold: resample a held finer slot, or
            // read the 15M slot once and resample from it, instead of leaving
            // the chart dark.
            if self.history_for(timeframe).is_ok() {
                return self.select_timeframe(timeframe);
            }
            if timeframe.timeframe().duration_ms() > Timeframe::M15.duration_ms() {
                let base_size = self.target_fetch(TimeframeKey::M15);
                let base = Self::fetch_rows(gateway, &symbol, TimeframeKey::M15, base_size)?;
                if !base.is_empty() {
                    self.histories.insert(TimeframeKey::M15, base);
                    return self.select_timeframe(timeframe);
                }
            }
        }
        self.apply_fetched(&symbol, timeframe, rows)?;
        self.restore_or_trail(anchor, self.last_plot_width)?;
        self.request_auto_height();
        Ok(())
    }

    /// Select a timeframe from the seeded history store without a gateway,
    /// resampling coarser keys from the `15M` slot; the active candles and
    /// every indicator recompute from the switched history. Same deferred
    /// AutoHeight contract as [`switch_timeframe`](Self::switch_timeframe).
    /// The resample resolves before any assignment, so a refused resample
    /// leaves the `(symbol, timeframe, rows)` triple untouched.
    pub fn select_timeframe(&mut self, timeframe: TimeframeKey) -> Result<(), EguiError> {
        let anchor = self.rightmost_time(self.last_plot_width);
        let rows = self.history_for(timeframe)?;
        let symbol = self.active_symbol().to_owned();
        self.apply_fetched(&symbol, timeframe, rows)?;
        self.restore_or_trail(anchor, self.last_plot_width)?;
        self.request_auto_height();
        Ok(())
    }

    /// Restore a saved rightmost time after fresh rows land: an anchor older
    /// than the painted window's first bar names a bar the window no longer
    /// shows, so the view trails to the newest bar instead of opening on
    /// stale leading bars. In-window anchors restore exactly, preserving the
    /// operator's pan across the switch.
    fn restore_or_trail(&mut self, anchor: Option<i64>, plot_width: f32) -> Result<(), EguiError> {
        let stale = match (anchor, self.effective_window().first()) {
            (Some(time), Some(first)) => time < first.time,
            _ => false,
        };
        if stale {
            self.jump_to_current_plot(plot_width)
        } else {
            self.restore_anchor(anchor, plot_width)
        }
    }

    /// Restore a saved rightmost time index after a timeframe switch: the
    /// nearest bar at or before `anchor` becomes the rightmost visible bar
    /// (clamped to ends). `None` jumps to the latest bar.
    fn restore_anchor(&mut self, anchor: Option<i64>, plot_width: f32) -> Result<(), EguiError> {
        if self.rows.is_empty() {
            return Ok(());
        }
        let Some(time) = anchor else {
            return self.jump_to_current_plot(plot_width);
        };
        let mut index = 0;
        for (i, row) in self.rows.iter().enumerate() {
            if row.time <= time {
                index = i;
            } else {
                break;
            }
        }
        self.place_bar_at_right(index, plot_width)
    }

    /// Fit the visible window's extremes (AutoHeight) for `plot_width`,
    /// holding the viewport when nothing is visible to fit. Guards the
    /// regression: refuses empty stores, unbound viewports, and non-positive
    /// widths instead of fitting before the chart is loaded or the X
    /// dimension is set.
    fn apply_auto_height(&mut self, plot_width: f32) {
        if self.rows.is_empty() || self.main.is_none() {
            return;
        }
        if !plot_width.is_finite() || plot_width <= 0.0 {
            return;
        }
        if let Ok(event) = self.auto_height_event(plot_width) {
            self.step_interaction(0.0, &[event]);
        }
    }

    /// Defer an AutoHeight fit to the next painted frame: marks the fit
    /// pending when there is something to fit (non-empty rows and a bound
    /// viewport), otherwise clears it. The canvas consumes it with the
    /// measured plot width.
    fn request_auto_height(&mut self) {
        if self.rows.is_empty() || self.main.is_none() {
            self.pending_auto_height = false;
            return;
        }
        self.pending_auto_height = true;
    }

    /// Drop any live replay run and its captured window on a ticker switch:
    /// the window's epochs and the session's server coordinates belong to
    /// the previous ticker, so carrying them on would decide one history
    /// while painting another. A refused close still clears the local run;
    /// the reason rides the replay section, and the chart keeps the freshly
    /// loaded rows either way.
    fn clear_replay_for_symbol_switch(&mut self, gateway: Option<&dyn TerminalGateway>) {
        if self.live_replay.is_none() && self.replay_range.is_none() {
            return;
        }
        if let Some(gateway) = gateway {
            if self.live_replay.is_some() {
                self.stop_live_replay(gateway);
            }
        } else if self.live_replay.is_some() {
            self.live_replay = None;
            let _ = self.recompute();
        }
        self.replay_range = None;
        self.replay_error = Some("replay closed: the symbol switched".to_owned());
    }

    /// Switch the active ticker over caller-seeded histories, without a
    /// gateway: records `symbol`, replaces every timeframe slot with
    /// `frames`, and binds the active timeframe's rows. The demo shell owns
    /// this path — one ticker's months load on demand and seed here, so a
    /// symbol pick never paints another ticker's rows. Slots the caller
    /// omits resample from the `15M` slot on demand as usual; pass an
    /// explicit empty set to paint the picked symbol with no rows (fail
    /// closed on an unloadable ticker, never stale rows under a fresh
    /// label).
    pub fn switch_symbol_seeded(
        &mut self,
        symbol: &str,
        frames: Vec<(TimeframeKey, Vec<ChartRow>)>,
    ) -> Result<(), EguiError> {
        self.stash_current_layout();
        self.set_symbol(symbol)?;
        self.histories.clear();
        for (timeframe, rows) in frames {
            self.histories.insert(timeframe, rows);
        }
        // The select cannot fail when the caller seeds the active key (the
        // demo always seeds every key); if it does, clear the rows rather
        // than painting the previous ticker under the fresh label.
        if let Err(error) = self.select_timeframe(self.timeframe) {
            self.rows.clear();
            self.rebind_viewports()?;
            self.recompute()?;
            return Err(error);
        }
        self.clear_replay_for_symbol_switch(None);
        // Best-effort seeded restore: a held frame missing the saved
        // timeframe keeps the live rows instead of failing the switch.
        let layout = self
            .saved_layouts
            .get(symbol)
            .or_else(|| self.stashed_layouts.get(symbol))
            .cloned();
        if let Some(layout) = layout {
            let _ = self.apply_ticker_layout(&layout, None);
        }
        Ok(())
    }

    /// The seeded history for `timeframe`, resampling coarser keys from the
    /// `15M` slot over whole buckets. `1M`/`5M`/`15M` read their own slots; a
    /// missing slot refuses by name rather than serving another timeframe.
    /// A finer key never resamples downward: folding fifteen-minute rows
    /// into one-minute buckets would only omit every bucket.
    fn history_for(&self, timeframe: TimeframeKey) -> Result<Vec<ChartRow>, EguiError> {
        if let Some(rows) = self.histories.get(&timeframe) {
            return Ok(rows.clone());
        }
        let target = timeframe.timeframe();
        if target.duration_ms() > Timeframe::M15.duration_ms() {
            if let Some(base) = self.histories.get(&TimeframeKey::M15) {
                return Ok(aggregate_rows(base, target));
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
        for pane in &mut self.sub_panes {
            if let Ok(synced) = pane.viewport.sync_time_axis(&next) {
                pane.viewport = synced;
            }
        }
    }

    /// Build the AutoHeight event for the current culled window: fit the
    /// visible bars' extremes with the calibrated pad. Refuses without a
    /// bound viewport or with nothing visible to fit (an off-window viewport
    /// holds instead of jumping); the caller owns the viewport. The fitted
    /// minimum clamps at zero, the price pane's absolute floor.
    pub fn auto_height_event(&self, plot_width: f32) -> Result<PointerInput, EguiError> {
        let main = self.main.as_ref().ok_or(EguiError::InvalidParam {
            what: "viewport".to_owned(),
            reason: "auto-height needs a bound price viewport".to_owned(),
        })?;
        // A live replay fits the decided prefix: future bars never set the
        // Y range, so the fit tracks the replay head like a rewind landing.
        let total = self
            .replay_prefix_len()
            .unwrap_or(self.rows.len())
            .min(self.rows.len());
        let (start, end) = main.culled_bar_range(plot_width, total);
        let fitted = auto_height_range(
            &self.rows[..total],
            (start, end.min(total)),
            AUTO_HEIGHT_PAD_FRACTION,
        )?;
        let min = fitted.min.max(0.0);
        if min >= fitted.max {
            return Err(EguiError::InvalidParam {
                what: "window".to_owned(),
                reason: "auto-height fit collides with the zero floor".to_owned(),
            });
        }
        Ok(PointerInput::AutoHeight {
            min,
            max: fitted.max,
        })
    }

    /// Fit the current view to the visible price action: guarded — refuses
    /// empty stores, unbound viewports, and unset widths instead of fitting
    /// before the chart is loaded. Shared by refresh and the rewind/forward
    /// pan cluster, so every landing window fills the pane's Y axis the same
    /// way.
    pub fn apply_auto_height_now(&mut self) {
        let width = self.last_plot_width;
        self.apply_auto_height(width);
    }

    /// Scroll the latest bar to the right edge on the last measured width:
    /// guarded like AutoHeight, preserving zoom and the price window.
    pub fn jump_to_current_now(&mut self) -> Result<(), EguiError> {
        let width = self.last_plot_width;
        if !width.is_finite() || width <= 0.0 {
            return Err(EguiError::InvalidParam {
                what: "plot_width".to_owned(),
                reason: "chart not measured yet".to_owned(),
            });
        }
        self.jump_to_current_plot(width)
    }

    /// Intern a symbol and set it active without pulling history.
    fn set_symbol(&mut self, symbol: &str) -> Result<(), EguiError> {
        let key = self
            .symbol_index
            .intern(symbol)
            .map_err(|error| EguiError::InvalidParam {
                what: "symbol".to_owned(),
                reason: error.to_string(),
            })?;
        self.active_symbol = key;
        self.config
            .set_field("symbol", symbol)
            .map_err(|error| EguiError::InvalidParam {
                what: "symbol".to_owned(),
                reason: error.to_string(),
            })?;
        Ok(())
    }

    /// Link a catalogued Pine script into an interpreted plugin. The script
    /// text comes from the boot-discovered sources; libraries resolve by
    /// filename stem as the manifest does. Anything missing or unparseable
    /// refuses naming the stem, never a half-linked plugin.
    fn instantiate_pine(&self, id: &str, stem: &str) -> Result<Box<dyn ChartIndicator>, EguiError> {
        let script = self
            .pine_indicators
            .iter()
            .find(|file| crate::pine_indicator::pine_stem_of(&file.relpath) == stem)
            .ok_or_else(|| EguiError::InvalidParam {
                what: "indicator".to_owned(),
                reason: format!("unknown pine script {stem:?}: no boot source lists it"),
            })?;
        let title = self
            .pine_manifest
            .iter()
            .find(|row| row.id == id)
            .map_or_else(|| stem.to_owned(), |row| row.title.clone());
        let libraries: std::collections::HashMap<String, String> = self
            .pine_libraries
            .iter()
            .map(|file| (file.relpath.clone(), file.text.clone()))
            .collect();
        Ok(Box::new(crate::pine_indicator::PineIndicatorPlugin::link(
            id,
            &title,
            &script.text,
            &libraries,
        )?))
    }

    /// Slot one indicator plugin by catalog id; a duplicate is a no-op.
    /// A `pine:` id prefers its shipped native backing when one exists
    /// (parity-proven and identical to before); otherwise the workspace's
    /// boot-discovered sources link through the embedded interpreter.
    /// Anything else resolves through the native catalog exactly as before.
    fn slot_indicator(&mut self, id: &str) -> Result<(), EguiError> {
        if self.stack.contains(id) {
            return Ok(());
        }
        let plugin = match instantiate_indicator(id) {
            Ok(plugin) => plugin,
            Err(_) if id.starts_with("pine:") => {
                let stem = id.strip_prefix("pine:").unwrap_or(id);
                self.instantiate_pine(id, stem)?
            }
            Err(error) => return Err(error),
        };
        let mut config = plugin.default_config();
        // New oscillator slots template the panel period: the kind default
        // is the length a fresh slot opens with (the registry default it
        // replaces matches, so a neutral panel changes nothing).
        if let Some(kernel_config) =
            component_downcast_mut::<crate::indicators::KernelIndicatorConfig>(&mut *config)
        {
            if let Some(kind) = panel_kind_for_kernel(&kernel_config.kernel) {
                if let (Ok(defaults), Some(length)) = (
                    self.oscillator_panel.entry(kind),
                    kernel_config.params.get_mut("length"),
                ) {
                    *length = defaults.period.to_string();
                }
            }
        }
        let nodes = plugin.plan_nodes(&*config)?;
        let node_refs: Vec<&str> = nodes.iter().map(String::as_str).collect();
        let deps = plugin.plan_deps(&*config)?;
        let dep_refs: Vec<&str> = deps.iter().map(String::as_str).collect();
        let fingerprint = common_core::hash::fnv1a64(id.as_bytes());
        self.stack.insert(
            plugin.id(),
            plugin.title(),
            plugin.attachment(),
            config,
            &node_refs,
            &dep_refs,
            fingerprint,
            &mut self.plan,
        )?;
        self.plugins.push(plugin);
        Ok(())
    }

    /// Rebuild the series plan from the live slot configs: every plugin's
    /// current nodes with its current source edge. Config edits move edges,
    /// so the plan is derived per recompute (and per removal), never trusted
    /// from slot time. A node depending on itself refuses naming the cycle
    /// (the graph skips self-loops by design, so this check owns them).
    fn rebuild_series_plan(&mut self) -> Result<(), EguiError> {
        let mut plan = PanePlan::new();
        for plugin in &self.plugins {
            let config = self
                .stack
                .config(plugin.id())
                .ok_or_else(|| EguiError::InvalidParam {
                    what: plugin.id().to_owned(),
                    reason: "slot owns no configuration".to_owned(),
                })?;
            let nodes = plugin.plan_nodes(config)?;
            let deps = plugin.plan_deps(config)?;
            for node in &nodes {
                if deps.iter().any(|dep| dep == node) {
                    return Err(EguiError::InvalidParam {
                        what: "series".to_owned(),
                        reason: format!("series cycle: {node} depends on itself"),
                    });
                }
                let dep_refs: Vec<&str> = deps.iter().map(String::as_str).collect();
                plan.register_node(node, &dep_refs, common_core::hash::fnv1a64(node.as_bytes()))?;
            }
        }
        self.plan = plan;
        Ok(())
    }

    /// Slot indices in evaluation order: plan order mapped back to plugins
    /// (a plugin runs at its earliest node's position), stable by slot order
    /// so edge-free panes evaluate exactly as slotted. Plan-derived, not a
    /// residual sorter.
    fn eval_order(&self) -> Result<Vec<usize>, EguiError> {
        let node_order = self.plan.order()?;
        let position: HashMap<&str, usize> = node_order
            .iter()
            .enumerate()
            .map(|(index, node)| (node.as_str(), index))
            .collect();
        let mut indices: Vec<usize> = (0..self.plugins.len()).collect();
        indices.sort_by_key(|&index| {
            let nodes = self
                .stack
                .config(self.plugins[index].id())
                .and_then(|config| self.plugins[index].plan_nodes(config).ok());
            match nodes {
                Some(nodes) => nodes
                    .iter()
                    .filter_map(|node| position.get(node.as_str()).copied())
                    .min()
                    .unwrap_or(usize::MAX),
                None => usize::MAX,
            }
        });
        Ok(indices)
    }

    /// Custom pane ids published by multi-output kernel plugins, sorted.
    /// Merged slots publish no custom pane: routing is presentation-only,
    /// so a merged slot's id disappears from the derivation until unmerged.
    fn custom_pane_ids(&self) -> Vec<String> {
        let mut ids = BTreeSet::new();
        for (slot_id, evaluation) in &self.evaluations {
            if self.pane_target(slot_id) != PaneTarget::Default {
                continue;
            }
            for series in &evaluation.series {
                if let Some(pane) = &series.pane {
                    if pane != "sub" {
                        ids.insert(pane.clone());
                    }
                }
            }
        }
        ids.into_iter().collect()
    }

    /// The routing target for a slot: the stored row, or the default when
    /// no row exists or the slot is unknown. A dangling target never errors.
    fn pane_target(&self, slot_id: &str) -> PaneTarget {
        if !self.stack.contains(slot_id) {
            return PaneTarget::Default;
        }
        self.pane_routing
            .get(slot_id)
            .copied()
            .unwrap_or(PaneTarget::Default)
    }

    /// Whether a slot may merge into the shared oscillator pane: only slots
    /// whose every declared output leg normalizes qualify. A partially
    /// mapped kernel would paint raw legs into a symmetric normalized
    /// pane, which is structurally meaningless, not low confidence.
    fn is_mergeable_to_shared(&self, slot_id: &str) -> bool {
        if slot_id == INTEGRATION_LITE_ID {
            return true;
        }
        let Some(config) = self.stack.config(slot_id) else {
            return false;
        };
        let Some(config) =
            fluent_wvr::component_downcast_ref::<crate::indicators::KernelIndicatorConfig>(config)
        else {
            return false;
        };
        crate::indicators::kernel_normalizes(&config.kernel)
    }

    /// Merge a slot to a target pane: presentation-only, so plan nodes,
    /// cache keys, and catalog entries never move. Merging to the shared
    /// pane refuses with the plugin named when no normalizable mapping
    /// exists. Setting the default deletes the row.
    pub fn set_pane_target(&mut self, slot_id: &str, target: PaneTarget) -> Result<(), EguiError> {
        if !self.stack.contains(slot_id) {
            return Err(EguiError::InvalidParam {
                what: slot_id.to_owned(),
                reason: "unknown slot id".to_owned(),
            });
        }
        if target == PaneTarget::SharedOscillator && !self.is_mergeable_to_shared(slot_id) {
            return Err(EguiError::InvalidParam {
                what: slot_id.to_owned(),
                reason: format!(
                    "{slot_id} has no normalizable oscillator mapping, so it cannot merge into the shared oscillator"
                ),
            });
        }
        if target == PaneTarget::Default {
            self.pane_routing.remove(slot_id);
        } else {
            self.pane_routing.insert(slot_id.to_owned(), target);
        }
        Ok(())
    }

    /// The pane a series paints in after routing: merged slots resolve to
    /// their target, everything else to the series' own pane.
    fn resolved_series_pane(&self, slot_id: &str, series: &IndicatorSeries) -> String {
        match self.pane_target(slot_id) {
            PaneTarget::SharedOscillator => "sub".to_owned(),
            PaneTarget::Main => "main".to_owned(),
            PaneTarget::Default => series.pane_id("sub").to_owned(),
        }
    }

    /// One slot's evaluation slice for a pane after routing: plan nodes,
    /// cache keys, and catalog entries stay untouched, only the paint slice
    /// moves.
    fn evaluation_for_pane(
        &self,
        slot_id: &str,
        evaluation: &IndicatorEvaluation,
        pane: &str,
    ) -> IndicatorEvaluation {
        let series: Vec<IndicatorSeries> = evaluation
            .series
            .iter()
            .filter(|series| self.resolved_series_pane(slot_id, series) == pane)
            .cloned()
            .collect();
        // The overlay rides with the pane that keeps the slot's series: a
        // pane without series paints no overlay, so drawings never duplicate
        // across the main and shared panes.
        let overlay = if series.is_empty() {
            Ok(Vec::new())
        } else {
            evaluation.overlay.clone()
        };
        IndicatorEvaluation { series, overlay }
    }

    /// Declared bounded policy union for one custom pane, when every
    /// declared output is bounded.
    fn declared_pane_bounds(&self, pane_id: &str) -> Option<(f64, f64)> {
        use protopyne_kernels::indicator::ScalePolicy;
        let plugin_id = pane_id.strip_prefix("kernel:")?;
        let config = self.stack.config(plugin_id)?;
        let config = component_downcast_ref::<crate::indicators::KernelIndicatorConfig>(config)?;
        let (_, outputs) = config.schema().ok()?;
        let mut bounds: Option<(f64, f64)> = None;
        for output in outputs {
            match output.policy {
                ScalePolicy::Bounded { min, max } => {
                    bounds = Some(match bounds {
                        Some((lo, hi)) => (lo.min(min), hi.max(max)),
                        None => (min, max),
                    });
                }
                ScalePolicy::Auto => return None,
            }
        }
        bounds
    }

    /// The display range for one custom pane: declared bounds plus a small
    /// pad, or the visible values' extent. Values are never clamped.
    /// Ranges read display values, the same contract painters read, so a
    /// normalized pane contains its normalized content. Declared bounds
    /// describe raw outputs, so they join the union only when no series in
    /// the pane normalizes — mixing a raw-space band into a display-space
    /// range would misplace both.
    fn custom_pane_range(&self, pane_id: &str) -> (f64, f64) {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        let mut mapped = false;
        for evaluation in self.evaluations.values() {
            for series in &evaluation.series {
                if series.pane_id("sub") != pane_id {
                    continue;
                }
                if series.display_mapping.is_some() {
                    mapped = true;
                }
                for value in series.display_values.iter().flatten() {
                    if value.is_finite() {
                        lo = lo.min(*value);
                        hi = hi.max(*value);
                    }
                }
            }
        }
        if !mapped {
            if let Some((declared_lo, declared_hi)) = self.declared_pane_bounds(pane_id) {
                lo = lo.min(declared_lo);
                hi = hi.max(declared_hi);
            }
        }
        if !lo.is_finite() || !hi.is_finite() || lo >= hi {
            return (SUB_MIN, SUB_MAX);
        }
        let pad = (hi - lo) * AUTO_HEIGHT_PAD_FRACTION;
        (lo - pad, hi + pad)
    }

    /// The style keys one slot's series paint with, in first-painted order.
    /// The settings Style tab projects the generic override block down to
    /// these keys, so the tab shows that indicator's display values. An
    /// unevaluated slot projects to nothing, so the tab offers no colors
    /// rather than every color of every indicator.
    pub fn painted_style_keys(&self, slot_id: &str) -> Vec<String> {
        let Some(evaluation) = self.evaluations.get(slot_id) else {
            return Vec::new();
        };
        let mut keys = Vec::new();
        for series in &evaluation.series {
            if !keys.iter().any(|key: &String| key == &series.style) {
                keys.push(series.style.clone());
            }
        }
        if slot_id == INTEGRATION_LITE_ID {
            // The slot paints the trend ribbon beside its legs, so the
            // Style tab offers the ribbon overrides with the leg keys.
            for key in ["ribbon_bull", "ribbon_bear"] {
                if !keys.iter().any(|held: &String| held == key) {
                    keys.push(key.to_owned());
                }
            }
        }
        keys
    }

    /// The lifted settings schemas for one slot: the config schema plus the
    /// style schema, assembled where the domain facts live.
    ///
    /// The config's `source` dropdown gains the live `series:<node>`
    /// producer options beside the schema's static price sources, so the
    /// dropdown is the single source picker — no bespoke button row beside
    /// it. The style schema projects to the stroke width plus the keys the
    /// slot's own series paint. Unknown slots lift nothing.
    pub fn settings_fields(&self, slot_id: &str) -> Option<(Vec<SchemaField>, Vec<SchemaField>)> {
        let config = self.stack.config(slot_id)?;
        let mut config_fields = schema_fields(&config.describe());
        // Pine slots inherit the chart scope (see `resolved_slot_config`):
        // the symbol/period fields are redundant with the chart settings,
        // so the slot UI hides them instead of offering a second copy.
        if slot_id.starts_with("pine:") {
            config_fields
                .retain(|field| field.name != "chart_symbol" && field.name != "chart_period");
        }
        if let Some(source) = config_fields
            .iter_mut()
            .find(|field| field.name == "source" && field.kind == fluent_egui::FieldKind::Select)
        {
            let mut nodes: Vec<String> = self
                .series_catalog
                .node_names()
                .into_iter()
                .map(|node| format!("series:{node}"))
                .collect();
            nodes.sort();
            for node in nodes {
                if !source.options.contains(&node) {
                    source.options.push(node);
                }
            }
        }
        let painted = self.painted_style_keys(slot_id);
        let key_refs: Vec<&str> = painted.iter().map(String::as_str).collect();
        let style = self.stack.style(slot_id)?;
        let style_fields = project_style_fields(&schema_fields(&style.describe()), &key_refs);
        Some((config_fields, style_fields))
    }

    /// Whether one slot paints into the shared oscillator pane: any of its
    /// series resolving to `sub` after routing.
    fn paints_in_shared(&self, slot_id: &str, evaluation: &IndicatorEvaluation) -> bool {
        evaluation
            .series
            .iter()
            .any(|series| self.resolved_series_pane(slot_id, series) == "sub")
    }

    /// Threshold rules the shared pane shows: one entry per contributing
    /// slot, deduplicated. Kernel slots read their display threshold; the
    /// composite slot reads the frozen pivot threshold. An empty shared pane
    /// carries no rules.
    fn shared_osc_thresholds(&self) -> Vec<f64> {
        let mut thresholds = Vec::new();
        for (slot_id, evaluation) in &self.evaluations {
            if !self.paints_in_shared(slot_id, evaluation) {
                continue;
            }
            if slot_id == INTEGRATION_LITE_ID {
                let show = self
                    .stack
                    .config(INTEGRATION_LITE_ID)
                    .and_then(component_downcast_ref::<IntegrationLiteConfig>)
                    .is_none_or(|config| config.pivots_show);
                if show {
                    thresholds.push(CALIBRATED_PIVOT_THRESHOLD);
                }
                continue;
            }
            if let Some(config) = self
                .stack
                .config(slot_id)
                .and_then(component_downcast_ref::<crate::indicators::KernelIndicatorConfig>)
            {
                thresholds.push(config.osc_threshold);
            }
        }
        thresholds.sort_by(f64::total_cmp);
        thresholds.dedup();
        thresholds
    }

    /// The shared oscillator display range: symmetric auto-range over the
    /// normalized content painting there, padded, always containing every
    /// slot threshold and zero. With no normalized content the fixed band
    /// stays the honest fallback.
    fn shared_osc_range(&self) -> (f64, f64) {
        let mut half = 0.0_f64;
        let mut found = false;
        for (slot_id, evaluation) in &self.evaluations {
            for series in &evaluation.series {
                if self.resolved_series_pane(slot_id, series) != "sub" {
                    continue;
                }
                if series.display_mapping.is_none() {
                    continue;
                }
                for value in series.display_values.iter().flatten() {
                    if value.is_finite() {
                        half = half.max(value.abs());
                        found = true;
                    }
                }
            }
        }
        for threshold in self.shared_osc_thresholds() {
            if threshold.is_finite() {
                half = half.max(threshold.abs());
                found = true;
            }
        }
        if !found || !half.is_finite() || half <= 0.0 {
            return (SUB_MIN, SUB_MAX);
        }
        let pad = (2.0 * half) * AUTO_HEIGHT_PAD_FRACTION;
        (-half - pad, half + pad)
    }

    /// Nominal plot heights for the current pane share: the price plot (minus
    /// its time strip) and one sub-pane's share for `pane_count` sub-panes.
    /// The frame resyncs both to the measured window; these only bind
    /// viewports before first paint. Shared by the full and soft rebinds so
    /// geometry cannot drift between them.
    fn pane_heights(&self, pane_count: usize) -> (f32, f32) {
        let share = self.pane_share.clamp(MIN_PANE_SHARE, MAX_PANE_SHARE);
        let price_plot = NOMINAL_CANVAS_HEIGHT * share - TIME_STRIP;
        let sub_plot = NOMINAL_CANVAS_HEIGHT * (1.0 - share);
        (price_plot, sub_plot / pane_count.max(1) as f32)
    }

    /// Rebuild the derived oscillator viewports from the published
    /// evaluations: the shared pane plus one viewport per custom pane id.
    /// Pane membership is derived state — the ids come from the evaluations,
    /// never from slot order — so removal prunes through this one path and
    /// recompute rebinds through it. Display ranges come from the existing
    /// range helper; no new math lives here. Time axes sync to the main
    /// viewport when one is bound, so a removal keeps the chart's position.
    fn rebuild_derived_panes(&mut self) -> Result<(), EguiError> {
        let custom_ids = self.custom_pane_ids();
        let (_, pane_height) = self.pane_heights(custom_ids.len() + 1);
        let spacing = if self.config.bar_spacing > 0.0 {
            self.config.bar_spacing as f32
        } else {
            DEFAULT_BAR_SPACING
        };
        let (sub_lo, sub_hi) = self.shared_osc_range();
        let mut sub = Viewport::new(
            PriceScaleMode::Normal,
            sub_lo,
            sub_hi,
            pane_height,
            spacing,
            0.0,
            false,
        )?;
        let mut sub_panes = Vec::with_capacity(custom_ids.len());
        for id in custom_ids {
            let (pane_lo, pane_hi) = self.custom_pane_range(&id);
            sub_panes.push(SubPaneView {
                id,
                viewport: Viewport::new(
                    PriceScaleMode::Normal,
                    pane_lo,
                    pane_hi,
                    pane_height,
                    spacing,
                    0.0,
                    false,
                )?,
            });
        }
        if let Some(main) = self.main {
            if let Ok(synced) = sub.sync_time_axis(&main) {
                sub = synced;
            }
            for pane in &mut sub_panes {
                if let Ok(synced) = pane.viewport.sync_time_axis(&main) {
                    pane.viewport = synced;
                }
            }
        }
        self.sub = Some(sub);
        self.sub_panes = sub_panes;
        Ok(())
    }

    /// Rebind the main and sub-pane viewports (and the interaction bounds) to
    /// the current rows and range. Heights follow the pane share over the
    /// nominal canvas (the price plot minus its time strip); the first
    /// painted frame resyncs both to the measured window. The price extent
    /// follows the effective window (preset plus Beginning filter), so a
    /// custom start refits exactly what it shows.
    fn rebind_viewports(&mut self) -> Result<(), EguiError> {
        // One window source: the helper owns the preset-plus-Beginning
        // filter, so the fit and the painted window cannot drift apart.
        let (lo, hi, window_len) = {
            let window = self.effective_window();
            let (lo, hi) = price_extent(window);
            (lo, hi, window.len())
        };
        let spacing = if self.config.bar_spacing > 0.0 {
            self.config.bar_spacing as f32
        } else {
            DEFAULT_BAR_SPACING
        };
        let custom_ids = self.custom_pane_ids();
        let (price_plot, _) = self.pane_heights(custom_ids.len() + 1);
        let main = Viewport::new(
            PriceScaleMode::Normal,
            lo,
            hi,
            price_plot,
            spacing,
            PRICE_BASE,
            false,
        )?;
        self.main = Some(main);
        self.rebuild_derived_panes()?;
        // Trailing-edge anchor when history overflows the window: a zero
        // offset would open on the oldest bars while every anchor read
        // addresses the full history. When the window covers the whole
        // history the zero origin already frames the oldest bars, so it
        // stays put and short histories paint exactly as before.
        let window_start = self.rows.len().saturating_sub(window_len);
        if window_start > 0 {
            let width = if self.last_plot_width.is_finite() && self.last_plot_width > 0.0 {
                self.last_plot_width
            } else {
                PANE_WIDTH_PX
            };
            let offset = width - self.rows.len() as f32 * spacing;
            if let Some(main) = self.main.and_then(|view| view.with_x_offset(offset).ok()) {
                self.main = Some(main);
                if let Some(sub) = self.sub {
                    if let Ok(synced) = sub.sync_time_axis(&main) {
                        self.sub = Some(synced);
                    }
                }
                for pane in &mut self.sub_panes {
                    if let Ok(synced) = pane.viewport.sync_time_axis(&main) {
                        pane.viewport = synced;
                    }
                }
            }
        }
        self.interaction = if self.rows.is_empty() {
            None
        } else {
            Some(
                InteractionState::new(lo, hi)?
                    .with_horizontal_bounds(self.rows.len(), PANE_WIDTH_PX)?
                    // The Y axis roams free above: the data extent bound
                    // `lo, hi` frames the initial fit only, never a ceiling.
                    // Zero stays the absolute floor for every zoom and pan.
                    .without_vertical_bounds()
                    .with_value_floor(0.0)?,
            )
        };
        Ok(())
    }

    /// Rebind viewports without moving the chart: rebuilds the mappings at
    /// the current heights and spacing, but keeps every pane's X offset and
    /// price range. Used by recompute while a replay run is live, so
    /// advancing a bar never alters the X position or zoom behind the
    /// transport's back — the cursor jump (open) and the head follow (steps)
    /// own placement, and the per-step auto-height owns the Y fit. Falls back
    /// to the full rebind when no viewport is bound yet. The interaction
    /// envelope still re-keys to the loaded length, so an in-fill growth at
    /// open widens the pan bounds without moving the view.
    fn soft_rebind_viewports(&mut self) -> Result<(), EguiError> {
        let (Some(main), Some(sub)) = (self.main, self.sub) else {
            return self.rebind_viewports();
        };
        let offset = main.x_offset_px();
        let main_range: PriceRange = main.scale().range();
        let sub_range: PriceRange = sub.scale().range();
        let old_subs: Vec<(String, PriceRange)> = self
            .sub_panes
            .iter()
            .map(|pane| (pane.id.clone(), pane.viewport.scale().range()))
            .collect();
        let spacing = if self.config.bar_spacing > 0.0 {
            self.config.bar_spacing as f32
        } else {
            DEFAULT_BAR_SPACING
        };
        let custom_ids = self.custom_pane_ids();
        let (price_plot, pane_height) = self.pane_heights(custom_ids.len() + 1);
        let next = Viewport::new(
            PriceScaleMode::Normal,
            main_range.min,
            main_range.max,
            price_plot,
            spacing,
            PRICE_BASE,
            false,
        )?
        .with_x_offset(offset)?;
        self.main = Some(next);
        if let Ok(synced) = Viewport::new(
            PriceScaleMode::Normal,
            sub_range.min,
            sub_range.max,
            pane_height,
            spacing,
            0.0,
            false,
        )?
        .sync_time_axis(&next)
        {
            self.sub = Some(synced);
        }
        let mut sub_panes = Vec::with_capacity(custom_ids.len());
        for id in custom_ids {
            let (pane_lo, pane_hi) = match old_subs.iter().find(|(held, _)| held == &id) {
                Some((_, range)) => (range.min, range.max),
                None => self.custom_pane_range(&id),
            };
            let viewport = Viewport::new(
                PriceScaleMode::Normal,
                pane_lo,
                pane_hi,
                pane_height,
                spacing,
                0.0,
                false,
            )?;
            sub_panes.push(SubPaneView {
                id,
                viewport: viewport.sync_time_axis(&next).unwrap_or(viewport),
            });
        }
        self.sub_panes = sub_panes;
        let (lo, hi) = (main_range.min, main_range.max);
        self.interaction = if self.rows.is_empty() {
            None
        } else {
            Some(
                InteractionState::new(lo, hi)?
                    .with_horizontal_bounds(self.rows.len(), PANE_WIDTH_PX)?
                    .without_vertical_bounds()
                    .with_value_floor(0.0)?,
            )
        };
        Ok(())
    }

    /// Exact-equality gate key over the live slot configs: one `(slot id,
    /// config hash)` pair per plugin through the shared length-framed
    /// builder (names sort, hashes are fixed-width words). Each hash is the
    /// same fingerprint `evaluate` folds into its series keys, so the gate
    /// moves exactly when the evaluations would. This is not a confidence
    /// threshold — every skip needs a byte-exact match on this, the rows
    /// shape, and the evaluated prefix's content hash. Style edits ride a
    /// separate component and never touch this key.
    fn plan_fingerprint(&self) -> Result<u64, EguiError> {
        let mut hashes = Vec::with_capacity(self.plugins.len());
        for plugin in &self.plugins {
            let resolved = self.resolved_slot_config(plugin.id())?;
            hashes.push((
                plugin.id().to_owned(),
                plugin.config_key(resolved.as_component())?.to_le_bytes(),
            ));
        }
        let pairs: Vec<(&str, &[u8])> = hashes
            .iter()
            .map(|(id, hash)| (id.as_str(), &hash[..]))
            .collect();
        Ok(fingerprint_framed(&pairs))
    }

    /// The slot config with panel kind-defaults resolved in: kernel slots
    /// bound to a panel kind evaluate their blanks (unset gain, disarmed
    /// smoothing) against the kind row; everything else evaluates its
    /// stored config untouched. Both the plan fingerprint and evaluation
    /// read through this, so following slots rebuild exactly when the
    /// kind does and overriding slots never move.
    ///
    /// Pine slots additionally inherit the chart scope: `chart_symbol` and
    /// `chart_period` come from the workspace's active symbol and timeframe
    /// (the chart owns both), never from the slot's own stored copy — the
    /// slot settings UI hides those fields, so a Pine script's
    /// `syminfo.tickerid`/`timeframe.period` always name the chart in view.
    fn resolved_slot_config(&self, plugin_id: &str) -> Result<EvalConfig<'_>, EguiError> {
        let config = self
            .stack
            .config(plugin_id)
            .ok_or_else(|| EguiError::InvalidParam {
                what: plugin_id.to_owned(),
                reason: "slot owns no configuration".to_owned(),
            })?;
        if let Some(kernel_config) =
            component_downcast_ref::<crate::indicators::KernelIndicatorConfig>(config)
        {
            if let Some(kind) = panel_kind_for_kernel(&kernel_config.kernel) {
                if let Ok(defaults) = self.oscillator_panel.entry(kind) {
                    return Ok(EvalConfig::Resolved(Box::new(
                        kernel_config.resolve_panel_defaults(defaults),
                    )));
                }
            }
            return Ok(EvalConfig::Stored(config));
        }
        if let Some(lite_config) =
            component_downcast_ref::<crate::integration_lite::IntegrationLiteConfig>(config)
        {
            return Ok(EvalConfig::Resolved(Box::new(
                lite_config.resolve_panel_defaults(&self.oscillator_panel),
            )));
        }
        if let Some(pine_config) =
            component_downcast_ref::<protopyne_kernels::PineRuntimeConfig>(config)
        {
            let mut stamped = pine_config.clone();
            self.active_symbol().clone_into(&mut stamped.chart_symbol);
            self.timeframe
                .pine_period()
                .clone_into(&mut stamped.chart_period);
            // The chart window always evaluates: raise a stored cap below
            // the loaded rows up to the window (a larger stored cap stands).
            // The engine refuses past `max_bars` instead of truncating, so
            // without this every Pine slot on a large window would degrade
            // to an empty evaluation while native slots paint. The window
            // itself stays bounded by the trailing keep, so this never
            // opens unbounded runs — it only stops the cap from fighting
            // the window the operator is looking at.
            stamped.max_bars = stamped.max_bars.max(self.rows.len() as u64);
            return Ok(EvalConfig::Resolved(Box::new(stamped)));
        }
        Ok(EvalConfig::Stored(config))
    }

    /// Evaluate every active indicator into the pane cache and readouts, then
    /// rebuild the IntegrationLite delta histogram and the candle/delta tint
    /// for the canvas — unless the exact-equality gate holds. The gate key
    /// is `(rows_shape, rows_content, plan_fingerprint)` over the evaluated
    /// prefix; a match returns before any plugin `evaluate`, and the stored
    /// evaluations (which the key proves `evaluate` would reproduce
    /// deterministically) stay live, so the legend never goes stale on a
    /// skip. A miss rebuilds everything and the revision backdate still
    /// guards the derived vectors.
    /// Evaluate one slot and publish its series: pad past the cursor with
    /// holes (painters cull the full slot space, and a hole paints as a
    /// gap — never a value the replay has not decided yet), publish to the
    /// cache and catalog, answer readouts from the unpadded evaluation, and
    /// store the padded evaluation. Any failure refuses with the slot's
    /// reason; the caller isolates it so one refusing slot never blanks
    /// the chart.
    fn evaluate_slot(
        &mut self,
        index: usize,
        id: &str,
        eval_rows: &[ChartRow],
        full_len: usize,
        revisions: &mut Vec<(u64, u64)>,
    ) -> Result<(), EguiError> {
        let evaluation = {
            let resolved = self.resolved_slot_config(id)?;
            self.plugins[index].evaluate(
                eval_rows,
                resolved.as_component(),
                &self.series_catalog,
            )?
        };
        for series in &evaluation.series {
            // Pad past the cursor with holes: painters cull the full
            // slot space, and a hole paints as a gap — never a value the
            // replay has not decided yet.
            let mut values = series.values.clone();
            values.resize(full_len, None);
            revisions.push((series.key, self.cache.publish(series.key, values.clone())));
            self.series_catalog
                .publish(&series.node, series.key, values);
        }
        let readouts = self.plugins[index].readouts(&evaluation);
        self.stack.set_readouts(id, readouts)?;
        // Store the padded evaluation: painters read this through the
        // global slot space, so stored series keep full length with
        // holes past the cursor. Readouts stay prefix-native (answered
        // above, from the unpadded evaluation).
        let mut stored = evaluation;
        for series in &mut stored.series {
            series.values.resize(full_len, None);
            series.display_values.resize(full_len, None);
        }
        self.evaluations.insert(id.to_owned(), stored);
        Ok(())
    }

    fn recompute(&mut self) -> Result<(), EguiError> {
        self.rebuild_series_plan()?;
        // A live replay evaluates over the decided prefix only: bars past
        // the cursor never enter an indicator, so the values at the cursor
        // carry no lookahead. Warmup holes are the kernels' own (evaluated,
        // never repaired); plugin series pad back to full length with holes
        // past the cursor so every painter's slot space stays valid.
        let full_len = self.rows.len();
        let prefix = self.replay_prefix_len().unwrap_or(full_len).min(full_len);
        let eval_rows = &self.rows[..prefix];
        if eval_rows.is_empty() {
            self.evaluations.clear();
            self.series_catalog.clear();
            self.sub_pane = None;
            self.momentum_keys.clear();
            self.tints.clear();
            self.heikin.clear();
            self.last_series_revisions.clear();
            self.last_plan_fingerprint = 0;
            self.last_rows_shape = (0, None, None);
            self.last_rows_content = 0;
            return Ok(());
        }
        let rows_shape = (
            eval_rows.len(),
            eval_rows.first().map(|row| row.time),
            eval_rows.last().map(|row| row.time),
        );
        let rows_content = rows_content_of(eval_rows);
        let plan_fingerprint = self.plan_fingerprint()?;
        if rows_shape == self.last_rows_shape
            && rows_content == self.last_rows_content
            && plan_fingerprint == self.last_plan_fingerprint
        {
            return Ok(());
        }
        if rows_shape != self.last_rows_shape || rows_content != self.last_rows_content {
            self.heikin = heikin_rows_of(&self.rows);
        }
        // Owned prefix rows: per-slot evaluation borrows the workspace
        // mutably, so the slice cannot outlive this statement.
        let eval_rows: Vec<ChartRow> = eval_rows.to_vec();
        let eval_rows = eval_rows.as_slice();
        let order = self.eval_order()?;
        let mut revisions = Vec::new();
        self.series_catalog.clear();
        for index in order {
            let id = self.plugins[index].id().to_owned();
            // Per-slot isolation: one refusing indicator must never blank
            // the chart. A slot that fails to resolve or evaluate keeps an
            // empty evaluation (no series, no readouts — the same shape as
            // an unevaluated slot), while every other slot still paints.
            // Refusals stay loud at slot time (`slot_indicator` names them);
            // recompute-time failures are data-dependent, so they degrade
            // to empty instead of aborting the frame — but the reason goes
            // to stderr, so an empty pane is diagnosable instead of silent.
            if let Err(error) = self.evaluate_slot(index, &id, eval_rows, full_len, &mut revisions)
            {
                eprintln!("protopyne workspace: slot {id} refused to evaluate: {error}");
                self.evaluations.insert(
                    id.clone(),
                    IndicatorEvaluation {
                        series: Vec::new(),
                        overlay: Ok(Vec::new()),
                    },
                );
                let _ = self.stack.set_readouts(&id, Vec::new());
            }
        }
        // Picker-independent canonicalization for change detection, not pane ordering.
        revisions.sort_unstable();
        if revisions == self.last_series_revisions
            && rows_shape == self.last_rows_shape
            && rows_content == self.last_rows_content
        {
            self.last_plan_fingerprint = plan_fingerprint;
            return Ok(());
        }
        self.last_series_revisions = revisions;
        self.last_rows_shape = rows_shape;
        self.last_rows_content = rows_content;
        self.last_plan_fingerprint = plan_fingerprint;
        self.sub_pane = self.compute_sub_pane()?;
        self.recompute_tint()?;
        self.derived_builds += 1;
        // While a replay run is live the transport owns placement: a soft
        // rebind keeps the X offset and zoom, so advancing a bar never moves
        // the chart behind the cursor jump and head follow. The Y fit still
        // lands through the per-step auto-height.
        if self.live_replay.is_some() {
            self.soft_rebind_viewports()?;
        } else {
            self.rebind_viewports()?;
        }
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
        // Prefix-consistent with the sub-pane: both read the replay view, so
        // the length contract holds with or without a live replay.
        let rows = self.replay_view_rows(&self.rows);
        self.momentum_keys = candle_momentum_keys(rows, &pane.ribbon.bullish)?;
        let eit_alpha = self
            .stack
            .config(INTEGRATION_LITE_ID)
            .and_then(|config| component_downcast_ref::<IntegrationLiteConfig>(config))
            .map_or(DEFAULT_EIT_ALPHA, |config| config.eit_alpha);
        // The color-code switch is display-only: off clears the gradient so
        // bars fall back to the discrete trend keys, on tints as today.
        let color_code = self
            .stack
            .config(INTEGRATION_LITE_ID)
            .and_then(|config| component_downcast_ref::<IntegrationLiteConfig>(config))
            .is_none_or(|config| config.color_code);
        if !color_code {
            self.tints = Vec::new();
            return Ok(());
        }
        self.tints = match MomentumPalette::from_theme(&self.theme) {
            Ok(palette) => {
                let momentum = candle_momentum_series(rows, eit_alpha)?;
                momentum_tints(rows, &momentum, eit_alpha, &palette)?
            }
            Err(_) => Vec::new(),
        };
        Ok(())
    }

    /// The resolved paint color for one bar: the momentum gradient tint when
    /// available, else the discrete trend key resolved against the theme.
    fn bar_color(
        &self,
        index: usize,
        key: &str,
        theme: &ThemeConfig,
    ) -> Result<egui::Color32, EguiError> {
        match self.tints.get(index).copied().flatten() {
            Some(tint) => Ok(tint_color(tint)),
            None => resolve_color(theme, key),
        }
    }

    /// Rebuild the IntegrationLite sub-pane (delta histogram) when that slot
    /// is present and its config downcasts. Reads the replay view, so a live
    /// replay folds volume over decided bars only.
    fn compute_sub_pane(&self) -> Result<Option<IntegrationLiteSubPane>, EguiError> {
        // NOT-A-LADDER: missing slot/config reads as none, not a walk.
        if !self.stack.contains(INTEGRATION_LITE_ID) {
            return Ok(None);
        }
        let Some(config) = self.stack.config(INTEGRATION_LITE_ID) else {
            return Ok(None);
        };
        let Some(config) = component_downcast_ref::<IntegrationLiteConfig>(config) else {
            return Ok(None);
        };
        let resolved = config.resolve_panel_defaults(&self.oscillator_panel);
        Ok(Some(integration_lite_sub_pane(
            self.replay_view_rows(&self.rows),
            &resolved,
        )?))
    }

    /// The one settings pane currently open, if any.
    fn open_settings_id(&self) -> Option<String> {
        self.stack
            .slot_ids()
            .into_iter()
            .find(|id| self.stack.settings_open(id))
    }

    /// Dispatch one top-bar action. Timeframe picks funnel through
    /// [`switch_timeframe`](Self::switch_timeframe) (gateway) or
    /// [`select_timeframe`](Self::select_timeframe) (seeded): both keep the
    /// rightmost-time anchor and defer the window fit to the next painted
    /// frame, so the price action in view fills the pane's full Y axis once
    /// the chart is loaded and measured — never before.
    fn apply_top_bar_action(
        &mut self,
        action: TopBarAction,
        gateway: Option<&dyn TerminalGateway>,
    ) -> Result<(), EguiError> {
        match action {
            TopBarAction::ToggleDock => {
                // Instant flip: dock sections load lazily inside their own
                // collapsing bodies on first open, so the toggle never pays
                // for bundle serialization or the replay graph. Boot paid for
                // the chart alone.
                self.toggle_dock();
            }
            TopBarAction::OpenSymbolSearch => {
                // Toggled expansion below the button: a second click while
                // open closes instead of rebuilding (keeping the query).
                if self.symbol_modal.is_some() {
                    self.symbol_modal = None;
                    self.symbol_anchor = None;
                } else {
                    self.symbol_modal = Some(symbol_modal_for(gateway)?);
                    self.seed_favorites();
                }
            }
            TopBarAction::OpenIndicatorSearch => {
                if self.indicator_modal.is_some() {
                    self.indicator_modal = None;
                    self.indicator_anchor = None;
                } else {
                    self.indicator_modal =
                        Some(build_indicator_catalog_with_manifest(&self.pine_manifest)?);
                    self.seed_favorites();
                }
            }
            TopBarAction::OpenBookmarkSearch => {
                if self.bookmark_modal.is_some() {
                    self.bookmark_modal = None;
                    self.bookmark_anchor = None;
                } else {
                    self.bookmark_modal = Some(self.build_bookmark_catalog());
                    self.seed_favorites();
                }
            }
            TopBarAction::OpenJumpSearch => {
                // Toggled expansion like the other pickers; the anchor was
                // stored by the button click beside this action.
                self.jump_open = !self.jump_open;
                if !self.jump_open {
                    self.jump_anchor = None;
                    self.jump_error = None;
                }
            }
            TopBarAction::SaveLayout => {
                // The only writer: the composition root persists on this
                // flag. Saving snapshots the active ticker's chart and
                // indicator settings, overwriting that ticker's entry.
                self.save_requested = true;
            }
            TopBarAction::PickedSymbol(symbol) => self.apply_symbol_pick(&symbol, gateway)?,
            TopBarAction::PickedTimeframe(timeframe) => match gateway {
                Some(gateway) => self.switch_timeframe(timeframe, gateway)?,
                None => self.select_timeframe(timeframe)?,
            },
            TopBarAction::PickedChartType(chart_type) => self.chart_type = chart_type,
            TopBarAction::PickedReplaySpeed(speed) => self.replay_speed = speed,
            TopBarAction::Refresh => {
                // No live feed is wired, so refresh verifies the chart range
                // only: the dock bundle and replay cannot have changed and are
                // not re-serialized here. Either way the indicators redraw and
                // the visible window refits its Y axis in place — the X
                // position never trails to the newest bar (that is the
                // forward-to-end button's job).
                if let Some(gateway) = gateway {
                    self.verify_chart_range(gateway)?;
                } else {
                    let saved_offset = self.main.map(Viewport::x_offset_px);
                    let saved_first = self.rows.first().map(|row| row.time);
                    self.rebind_viewports()?;
                    self.recompute()?;
                    if let (Some(offset), Some(first)) = (saved_offset, saved_first) {
                        self.restore_x_offset(offset, first);
                    }
                    self.apply_auto_height(self.last_plot_width);
                }
            }
            TopBarAction::RewindToBeginning => {
                self.rewind_to_beginning_view()?;
            }
            TopBarAction::Rewind => {
                self.rewind_view()?;
            }
            TopBarAction::ToggleReplayPlay => {
                self.toggle_replay_play(gateway);
            }
            TopBarAction::Forward => {
                self.forward_view()?;
            }
            TopBarAction::ForwardToEnd => {
                self.forward_to_end_view()?;
            }
        }
        Ok(())
    }

    /// Apply one dock range-section choice: a preset or Beginning pick grows,
    /// prunes, or unloads the loaded history with indicator warmup, and
    /// only the picked range stays displayed; anything else leaves the chart
    /// alone. A refusal is stored on [`range_error`](Self::range_error) for
    /// the section to echo, never a frame-stopping error.
    fn apply_range_choice(&mut self, choice: RangeChoice, gateway: Option<&dyn TerminalGateway>) {
        self.range_error = None;
        let outcome = match choice {
            RangeChoice::Apply => {
                if self.top_menu.beginning_text.trim().is_empty() {
                    match self.range_draft {
                        Some(range) => self.select_range(range, gateway).map(|()| {
                            self.range_draft = None;
                        }),
                        None => Ok(()),
                    }
                } else {
                    let text = self.top_menu.beginning_text.clone();
                    self.set_beginning(&text, gateway)
                }
            }
            RangeChoice::Cancel => {
                // Discard the drafts: reseed from the live window so the next
                // paint shows the applied state, not the abandoned edits.
                self.range_draft = self.active_range();
                self.top_menu.beginning_text =
                    self.custom_start.map(format_date_text).unwrap_or_default();
                Ok(())
            }
        };
        if let Err(error) = outcome {
            self.range_error = Some(error.to_string());
        }
    }

    /// Seed the dock range drafts from the live window when they carry no
    /// edits: the highlighted preset, or a Beginning text carrying the custom
    /// start. Typed drafts are never overwritten, so seeding only fills what
    /// the operator has not touched.
    fn seed_range_drafts(&mut self) {
        if self.range_draft.is_none() && self.top_menu.beginning_text.trim().is_empty() {
            self.range_draft = self.active_range();
            if self.custom_start.is_some() {
                self.top_menu.beginning_text =
                    self.custom_start.map(format_date_text).unwrap_or_default();
            }
        }
    }

    /// Apply a symbol pick with or without a gateway to pull through. The
    /// incoming ticker restores its own layout: a saved entry wins, else the
    /// session's stashed one keeps unsaved edits while roaming.
    fn apply_symbol_pick(
        &mut self,
        symbol: &str,
        gateway: Option<&dyn TerminalGateway>,
    ) -> Result<(), EguiError> {
        if let Some(gateway) = gateway {
            return self.switch_symbol(symbol, gateway);
        }
        self.stash_current_layout();
        self.set_symbol(symbol)?;
        let layout = self
            .saved_layouts
            .get(symbol)
            .or_else(|| self.stashed_layouts.get(symbol))
            .cloned();
        if let Some(layout) = layout {
            let _ = self.apply_ticker_layout(&layout, None);
        }
        Ok(())
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
    /// machine from this frame's pointer gestures first. The canvas fills
    /// the window: full available width and full remaining height, split
    /// between the panes at the divider share.
    fn paint_canvas(
        &mut self,
        ui: &mut egui::Ui,
        gateway: Option<&dyn TerminalGateway>,
    ) -> Result<(), EguiError> {
        // The canvas fills the window — full available width (bounded below
        // by the interaction envelope) and full remaining height (bounded
        // below by the smallest layout that keeps both plots positive). The
        // legend floats over the chart and the modals are overlay windows, so
        // nothing below competes for this height.
        let available = ui.available_size();
        let width = available.x.max(PANE_WIDTH_PX);
        let height = available.y.max(MIN_CANVAS_HEIGHT);
        // One allocation spans both panes: the pointer response then senses the
        // price pane, the oscillator pane, and both price strips, so a wheel
        // anywhere over the chart reaches `step_from_response` and region
        // routing decides what it acts on.
        let (canvas, response) =
            ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::drag());
        let mut share = self.pane_share;
        let mut pane_count = self.sub_panes.len() + 1;
        let mut panes = split_canvas_many(canvas, share, pane_count);
        // The divider floats over the seam with its own drag sense: a grab
        // resizes the panes' share of the canvas. While it is dragged the
        // canvas pan stays out, so one gesture never pans and resizes at once.
        let divider_id = ui.id().with("pane_divider");
        let mut divider = divider_rect(canvas, share);
        let divider_response = ui.interact(divider, divider_id, egui::Sense::drag());
        if divider_response.hovered() || divider_response.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        let divider_dy = divider_response.drag_delta().y;
        if divider_dy != 0.0 && canvas.height() > 0.0 {
            self.set_pane_share(share + divider_dy / canvas.height());
            share = self.pane_share;
            pane_count = self.sub_panes.len() + 1;
            panes = split_canvas_many(canvas, share, pane_count);
            divider = divider_rect(canvas, share);
        }
        let main_rect = panes[0];
        let sub_rects: Vec<egui::Rect> = panes[1..].to_vec();
        let layout = PaneLayout::split(main_rect, true);
        let sub_layouts: Vec<PaneLayout> = sub_rects
            .iter()
            .map(|rect| PaneLayout::split(*rect, false))
            .collect();
        let sub_layout = sub_layouts[0];
        let mut layouts = Vec::with_capacity(1 + sub_layouts.len());
        layouts.push(layout);
        layouts.extend(sub_layouts.iter().copied());
        // Resync both mappings to the measured plots, so grid, candles, and
        // axis labels agree with the rects they paint into at any window size.
        if let Some(main) = self.main {
            if let Some(resized) = viewport_with_plot_height(main, layout.plot.height()) {
                self.main = Some(resized);
            }
        }
        if let Some(sub) = self.sub {
            if let Some(resized) = viewport_with_plot_height(sub, sub_layout.plot.height()) {
                self.sub = Some(resized);
            }
        }
        for (pane, pane_layout) in self.sub_panes.iter_mut().zip(sub_layouts.iter().skip(1)) {
            if let Some(resized) =
                viewport_with_plot_height(pane.viewport, pane_layout.plot.height())
            {
                pane.viewport = resized;
            }
        }
        // Record the measured plot width for view buttons (refresh, the
        // rewind/forward pan cluster, jump): they read this, never a proxy,
        // so they cannot act before the X dimension is set.
        if layout.plot.width().is_finite() && layout.plot.width() > 0.0 {
            self.last_plot_width = layout.plot.width();
        }
        // Consume a deferred window fit with the measured width — only when
        // rows are loaded and a viewport is bound. A switch before first
        // paint only marks the fit pending; this is where it lands.
        if self.pending_auto_height {
            self.pending_auto_height = false;
            self.apply_auto_height(self.last_plot_width);
        }
        // Float the legend over the chart's upper-right before routing, so a
        // wheel already sitting over it scrolls the list this frame.
        let legend = legend_origin(main_rect, self.stack.card_width());
        self.stack.set_origin(legend);
        self.step_from_response(ui, &response, &layouts, divider_response.dragged());
        // Right-click or long-press on either pane arms the canvas context
        // menu at the pointer, resolved against the decided view.
        self.poll_chart_context_menu(ui, &response, &layouts);
        let bar_seconds = self.timeframe.timeframe().total_seconds();

        let theme = self.theme.clone();
        let background = resolve_color(&theme, "background")?;
        // Slots are global across the window: the window is a view, never a
        // renumbering. Every layer below culls the full history through the
        // shared samplers, so bar indices ride the viewport mapping untouched
        // and painters that keep full-history series need no base shift.
        // Heikin-Ashi reads the `recompute` cache (seeded once from row
        // zero), sharing the candle geometry path with candles.
        // A live replay paints the decided prefix only: the full history
        // stays loaded (indices ride untouched), but rows past the cursor
        // never reach a painter. Heikin is causal, so its full-store rows
        // truncate identically to the prefix they were seeded from.
        let view_rows = self.replay_view_rows(&self.rows);
        let view_heikin = self.replay_view_rows(&self.heikin);
        let candle_rows: &[ChartRow] = if self.chart_type == ChartType::Heikin {
            view_heikin
        } else {
            view_rows
        };

        let chrome = ui.painter_at(main_rect);
        chrome.rect_filled(main_rect, 0.0, background);
        let plot = ui.painter_at(layout.plot);
        // The hovered bar's screen x, shared with the oscillator band below:
        // set by whichever pane the pointer sits over, read when the
        // oscillator paints, so the vertical band spans both panes.
        let mut cross_x: Option<f32> = None;
        if let Some(main) = self.main {
            paint_price_grid(&plot, main, layout, &theme)?;
            paint_time_grid(&plot, main, view_rows, bar_seconds, layout, &theme)?;
            // The trend ribbon (IntegrationLite's main-pane leg) and the
            // candle tint both read the full-history legs: the sampler culls
            // the visible tail itself, so legs and candles share one slot
            // space with no per-frame window copy.
            let ribbon_full = self.sub_pane.as_ref().map(|pane| {
                (
                    pane.ribbon.upper.as_slice(),
                    pane.ribbon.lower.as_slice(),
                    pane.ribbon.bullish.as_slice(),
                    pane.ribbon.alpha,
                    pane.show_ribbon,
                )
            });
            // NOT-A-LADDER (M5): default allocation when no ribbon exists,
            // not an ordered first-Ok candidate walk.
            // Candles need a trend key per bar; the ribbon supplies it, and
            // a ribbon-less workspace reads every bar as bearish. The
            // fallback allocates only when no ribbon exists (the default
            // workspace always carries one, so the common frame borrows).
            let fallback_trend = if ribbon_full.is_none() {
                vec![false; view_rows.len()]
            } else {
                Vec::new()
            };
            let trend: &[bool] = match &ribbon_full {
                Some((_, _, bullish, _, _)) => bullish,
                None => &fallback_trend,
            };
            if let Some((upper, lower, bullish, alpha, show)) = &ribbon_full {
                if *show
                    && self.sub_pane.as_ref().is_some_and(|pane| pane.show_ribbon)
                    && self.stack.visible(INTEGRATION_LITE_ID) == Some(true)
                {
                    let paint_alpha =
                        f32::from(opacity_from_transparency(ribbon_fill_transparency(*alpha)))
                            / 255.0;
                    let ribbon_style = self.stack.indicator_style(INTEGRATION_LITE_ID);
                    if needs_column_aggregation(main.scale().bar_spacing()) {
                        // One band per painted column through the shared
                        // band sampler (display-only: values untouched,
                        // holes stay holes), blitted as rects through the
                        // same rect emitter as the native strips.
                        let (bands, trends) =
                            sampled_ribbon_bands(upper, lower, bullish, &main, layout.plot.width());
                        let placed =
                            translate_bands(bands, (layout.plot.left(), layout.plot.top()));
                        paint_ribbon(
                            &plot,
                            &placed,
                            0.5,
                            &theme,
                            &ribbon_style,
                            &trends,
                            paint_alpha,
                        )?;
                    } else {
                        // Native ribbon fills over the twice-visible borrow
                        // only (display-only cull through the shared core:
                        // same pixels, fewer polygons).
                        let placed = translate_polygons(
                            proportional_ribbon_polygons(
                                upper,
                                lower,
                                bullish,
                                &main,
                                layout.plot.width(),
                            )?,
                            (layout.plot.left(), layout.plot.top()),
                        );
                        paint_ribbon_fill(&plot, &placed, &theme, &ribbon_style, paint_alpha)?;
                    }
                }
            }
            if self.chart_type == ChartType::Candles || self.chart_type == ChartType::Heikin {
                // Candles + Heikin dispatch by pixel columns: native geometry above
                // one pixel per bar, one folded candle per screen column
                // below it. Rows arrive transformed at native resolution
                // first (bar-backed pictures convert before windowing), so
                // the fold only ever samples display pixels, never feeds a
                // recursive transform.
                let geoms = column_candle_geoms(candle_rows, trend, &main, layout.plot.width())?;
                let placed = translate_candles(geoms, (layout.plot.left(), layout.plot.top()));
                // Per-column momentum keys from the same full-history trend:
                // one single-bar lookup per painted candle, never a fresh
                // window `Vec` — identical keys to the windowed fold
                // through the one single-bar core.
                let mut candles = Vec::with_capacity(placed.len());
                for geom in &placed {
                    let key = match (candle_rows.get(geom.index), trend.get(geom.index)) {
                        (Some(row), Some(bull)) => momentum_key_for(row.open, row.close, *bull),
                        _ => "bull",
                    };
                    let color = self.bar_color(geom.index, key, &theme)?;
                    candles.push(TintedCandle { geom: *geom, color });
                }
                // The body follows the live time-axis zoom, not the configured
                // default spacing: a wheel-zoomed pane draws its candles at
                // the width of the unit of time it is actually showing.
                paint_candles_tinted(
                    &plot,
                    &candles,
                    candle_body_width(main.scale().bar_spacing()),
                );
            } else {
                // Line/Area (and any other display-row picture) render the
                // close series; Heikin smoothing needs the core bar pipeline.
                // Culled over the full history (borrowed, no copy), then one
                // vertex per column through the shared line sampler at
                // sub-pixel zoom (display-only) or the native mapping
                // untouched above it — same pixels the full window would
                // paint, off-pane shapes never built.
                let (cstart, cend) = main.culled_bar_range(layout.plot.width(), view_rows.len());
                let cwindow = visible_rows(view_rows, cstart, cend.saturating_sub(cstart));
                if needs_column_aggregation(main.scale().bar_spacing()) {
                    let closes = line_values(cwindow);
                    let points = sampled_line_points(&closes, &main, cstart);
                    let placed = translate_points(points, (layout.plot.left(), layout.plot.top()));
                    paint_oscillator(&plot, &placed, "fast", &theme, 1.0)?;
                } else {
                    let points: Vec<Option<(f32, f32)>> = cwindow
                        .iter()
                        .enumerate()
                        .map(|(offset, row)| {
                            main.price_to_y(row.close)
                                .map(|y| (main.index_to_x(cstart + offset), y))
                        })
                        .collect();
                    let placed = translate_points(points, (layout.plot.left(), layout.plot.top()));
                    paint_oscillator(&plot, &placed, "fast", &theme, 1.0)?;
                }
            }
            for plugin in &self.plugins {
                if self.stack.visible(plugin.id()) != Some(true) {
                    continue;
                }
                let target = self.pane_target(plugin.id());
                let is_native_main = plugin.attachment() == SlotAttachment::MainOverlay
                    && target == PaneTarget::Default;
                let is_routed_main = target == PaneTarget::Main;
                if !is_native_main && !is_routed_main {
                    continue;
                }
                let Some(config) = self.stack.config(plugin.id()) else {
                    continue;
                };
                if let Some(evaluation) = self.evaluations.get(plugin.id()) {
                    // Full-history evaluations paint through the global slot
                    // space, so the base is zero: each painter culls its own
                    // visible tail through the shared samplers — no per-frame,
                    // per-plugin window copy on this path. Native legs cull to
                    // the twice-visible borrow (display-only).
                    let routed;
                    let evaluation = if is_routed_main {
                        routed = self.evaluation_for_pane(plugin.id(), evaluation, "main");
                        if routed.series.is_empty() {
                            continue;
                        }
                        &routed
                    } else {
                        evaluation
                    };
                    plugin.paint_main_overlay(
                        &plot,
                        evaluation,
                        &main,
                        layout.plot.left_top(),
                        0,
                        layout.plot.width(),
                        &theme,
                        config,
                        &self.stack.indicator_style(plugin.id()),
                    )?;
                }
            }
            // Operator annotations paint as markers over the main canvas:
            // the set stays empty until the context menu adds a note, so the
            // common frame pays no per-note allocation.
            if !self.annotations.is_empty() {
                let notes: Vec<Annotation> = self.annotations.iter().cloned().collect();
                let mut dots = marker_dots(&notes, view_rows, &main);
                for dot in &mut dots {
                    dot.x += layout.plot.left();
                    dot.y += layout.plot.top();
                }
                paint_markers(&plot, &dots, &theme)?;
            }
            let last = view_rows.last();
            paint_price_axis(
                &chrome,
                main,
                layout,
                last.map(|bar| (bar.close, bar.close >= bar.open)),
                &theme,
            )?;
            paint_time_axis(&chrome, main, view_rows, bar_seconds, layout, &theme)?;
            // The crosshair answers over either pane: a hover over the price
            // pane paints the full readout (lines plus both tags) and arms
            // the oscillator band; a hover over the oscillator snaps through
            // the same time mapping and paints the vertical band plus the
            // time tag, so the band reads as one readout across both panes.
            if let Some(hover) = response.hover_pos() {
                if layout.plot.contains(hover) {
                    cross_x = paint_crosshair_overlay(
                        &plot, &chrome, main, view_rows, layout, hover, &theme,
                    )?
                    .map(|(x, _index)| x);
                } else if sub_layout.plot.contains(hover) {
                    // Mid-plot height always maps to a price, so the readout
                    // resolves the bar under the pointer through the exact
                    // same rounding as the price pane; only the band and the
                    // time tag paint, never a price from the wrong pane. The
                    // index is global, like every other slot in this frame.
                    let local = (hover.x - layout.plot.left(), layout.plot.height() / 2.0);
                    if let Some(data) = crosshair_data(local, &main, view_rows.len()) {
                        if let Some(row) = view_rows.get(data.index) {
                            let x = main.index_to_x(data.index) + layout.plot.left();
                            paint_crosshair_band(
                                &plot,
                                x,
                                layout.plot.top(),
                                layout.plot.bottom(),
                                &theme,
                            )?;
                            paint_time_tag(&chrome, layout, x, row.time, &theme)?;
                            cross_x = Some(x);
                        }
                    }
                }
            }
        }

        // The sub-pane rect and layout were allocated at the top of the frame;
        // reuse them so the oscillator paints into the region routing knows.
        let sub_chrome = ui.painter_at(sub_rects[0]);
        sub_chrome.rect_filled(sub_rects[0], 0.0, background);
        let sub_plot = ui.painter_at(sub_layout.plot);
        if let Some(sub) = self.sub {
            paint_price_grid(&sub_plot, sub, sub_layout, &theme)?;
            paint_time_grid(&sub_plot, sub, view_rows, bar_seconds, sub_layout, &theme)?;
            for plugin in &self.plugins {
                if self.stack.visible(plugin.id()) != Some(true) {
                    continue;
                }
                let target = self.pane_target(plugin.id());
                if target == PaneTarget::Main {
                    continue;
                }
                if plugin.attachment() != SlotAttachment::SubPane
                    && target != PaneTarget::SharedOscillator
                {
                    continue;
                }
                if let Some(evaluation) = self.evaluations.get(plugin.id()) {
                    let Some(config) = self.stack.config(plugin.id()) else {
                        continue;
                    };
                    let evaluation = self.evaluation_for_pane(plugin.id(), evaluation, "sub");
                    if evaluation.series.is_empty() {
                        continue;
                    }
                    // Full-history evaluations paint through the global slot
                    // space, so the base is zero: each painter culls its own
                    // visible tail through the shared samplers — no per-frame,
                    // per-plugin window copy on this path. Native legs cull to
                    // the twice-visible borrow (display-only).
                    plugin.paint_sub_pane(
                        &sub_plot,
                        &evaluation,
                        &sub,
                        sub_layout.plot.left_top(),
                        0,
                        sub_layout.plot.width(),
                        &theme,
                        config,
                        &self.stack.indicator_style(plugin.id()),
                    )?;
                }
            }
            if let Some(pane) = &self.sub_pane {
                // A hidden delta leg paints nothing but leaves the shared
                // pane's rules, zero line, and axis untouched below.
                if pane.show_delta {
                    // The delta histogram reads the full-history legs and
                    // dispatches by pixel columns exactly as the candles do:
                    // native bars above one pixel per bar, one folded bar per
                    // screen column below it — display density only, values
                    // untouched, holes staying holes per leg.
                    let bars = column_delta_geoms(
                        &pane.delta_volume.buy_top,
                        &pane.delta_volume.sell_bottom,
                        &sub,
                        sub_layout.plot.width(),
                    )?;
                    let placed =
                        translate_delta_bars(bars, (sub_layout.plot.left(), sub_layout.plot.top()));
                    let half_width =
                        candle_body_width(sub.scale().bar_spacing()) * DELTA_BAR_BODY_SHARE / 2.0;
                    // Script-parity split first: the dialed volume colors
                    // paint the buy/sell legs exactly like the reference
                    // plotcandles. A slot that lost its config mid-frame
                    // falls back to the momentum tint rather than refusing
                    // the whole pane.
                    let styled = self
                        .stack
                        .config(INTEGRATION_LITE_ID)
                        .and_then(component_downcast_ref::<IntegrationLiteConfig>)
                        .map(|config| {
                            Ok::<_, EguiError>((
                                volume_color(&config.delta_bull, "delta_bull")?,
                                volume_color(&config.delta_bear, "delta_bear")?,
                            ))
                        })
                        .transpose()?;
                    if let Some((bull, bear)) = styled {
                        paint_delta_bars_styled(&sub_plot, &placed, half_width, bull, bear);
                    } else {
                        let mut tinted = Vec::with_capacity(placed.len());
                        for geom in &placed {
                            let key = self
                                .momentum_keys
                                .get(geom.index)
                                .copied()
                                .unwrap_or("bull");
                            let color = self.bar_color(geom.index, key, &theme)?;
                            tinted.push(TintedDeltaBar { geom: *geom, color });
                        }
                        // The histogram is a slimmer overlay inside the oscillator
                        // pane, but it tracks the same live zoom as the candles.
                        paint_delta_bars_tinted(&sub_plot, &tinted, half_width);
                    }
                }
            }
            // The crosshair's vertical band spans the oscillator at the same
            // snapped bar the price pane reads: the band is the shared time
            // half of the readout, so it paints over either pane's hover.
            if let Some(x) = cross_x {
                paint_crosshair_band(
                    &sub_plot,
                    x,
                    sub_layout.plot.top(),
                    sub_layout.plot.bottom(),
                    &theme,
                )?;
            }
            // Slot thresholds ride the shared pane as rules through the one
            // threshold emitter; contributing slots each lend their display
            // threshold, so the pane shows the union. Off-window legs draw
            // nothing instead of mapping off-pane.
            for threshold in self.shared_osc_thresholds() {
                paint_thresholds(
                    &sub_plot,
                    sub_layout.plot.left(),
                    sub_layout.plot.right(),
                    threshold,
                    &sub,
                    &theme,
                )?;
            }
            // The zero line makes the centered histogram legible.
            if let Some(zero_y) = sub.windowed_y(0.0) {
                let y = sub_layout.plot.top() + zero_y;
                let stroke = egui::Stroke::new(1.0, resolve_color(&theme, "grid")?);
                sub_plot.line_segment(
                    [
                        egui::Pos2::new(sub_layout.plot.left(), y),
                        egui::Pos2::new(sub_layout.plot.right(), y),
                    ],
                    stroke,
                );
            }
            paint_price_axis(&sub_chrome, sub, sub_layout, None, &theme)?;
        }
        // Custom sub-panes: one region per kernel pane identity, each with its
        // own viewport and scale policy.
        for (index, pane) in self.sub_panes.iter().enumerate() {
            let pane_layout = sub_layouts[index + 1];
            let chrome = ui.painter_at(sub_rects[index + 1]);
            chrome.rect_filled(sub_rects[index + 1], 0.0, background);
            let plot = ui.painter_at(pane_layout.plot);
            paint_price_grid(&plot, pane.viewport, pane_layout, &theme)?;
            paint_time_grid(
                &plot,
                pane.viewport,
                view_rows,
                bar_seconds,
                pane_layout,
                &theme,
            )?;
            for plugin in &self.plugins {
                if self.stack.visible(plugin.id()) != Some(true) {
                    continue;
                }
                if self.pane_target(plugin.id()) != PaneTarget::Default {
                    continue;
                }
                if plugin.attachment() != SlotAttachment::SubPane {
                    continue;
                }
                let Some(evaluation) = self.evaluations.get(plugin.id()) else {
                    continue;
                };
                let Some(config) = self.stack.config(plugin.id()) else {
                    continue;
                };
                let evaluation = self.evaluation_for_pane(plugin.id(), evaluation, &pane.id);
                if evaluation.series.is_empty() {
                    continue;
                }
                plugin.paint_sub_pane(
                    &plot,
                    &evaluation,
                    &pane.viewport,
                    pane_layout.plot.left_top(),
                    0,
                    pane_layout.plot.width(),
                    &theme,
                    config,
                    &self.stack.indicator_style(plugin.id()),
                )?;
            }
            paint_price_axis(&chrome, pane.viewport, pane_layout, None, &theme)?;
        }
        // The divider seam on top: a dimmed strip with a grip line, so the
        // grab handle reads without taking layout pixels from either pane.
        let seam = ui.painter_at(divider);
        seam.rect_filled(divider, 0.0, resolve_color(&theme, "background")?);
        let grip_y = divider.center().y;
        seam.line_segment(
            [
                egui::pos2(divider.left() + 8.0, grip_y),
                egui::pos2(divider.right() - 8.0, grip_y),
            ],
            grid_stroke(&theme)?,
        );
        // Replay window edges ride the divider as two time-fixed ticks: the
        // captured window's first and last candle, re-resolved every frame
        // so panning moves the ticks with the time axis while the window
        // itself stays put. Off-screen edges clip with the seam painter.
        if let (Some(main), Some((first, last))) = (self.main, self.replay_marker_indices()) {
            let marker = egui::Stroke::new(2.0, resolve_color(&theme, "fast")?);
            for index in [first, last] {
                let x = main.index_to_x(index) + layout.plot.left();
                seam.line_segment(
                    [
                        egui::pos2(x, divider.top() + 1.0),
                        egui::pos2(x, divider.bottom() - 1.0),
                    ],
                    marker,
                );
            }
        }
        // Canvas context menu (right-click / long-press): Auto Zoom refits
        // the Y axis to the displayed candles, Bookmark captures the trigger
        // candle, and Set Replay Boundary arms the replay button. Anchored
        // at the trigger point, like a Material context menu; an outside
        // click or Escape closes without acting.
        if self.context_open {
            let mut open = true;
            let mut picked = None;
            let (merge_chart_label, merge_osc_label) = self.merge_labels();
            Self::picker_popup(
                ui,
                "chart-context",
                egui::PopupAnchor::Position(self.context_at),
                &mut open,
                |menu| {
                    if menu.button("Auto Zoom").clicked() {
                        picked = Some(ChartContextAction::AutoZoom);
                    }
                    if menu.button("Bookmark").clicked() {
                        picked = Some(ChartContextAction::Bookmark);
                    }
                    if menu.button("Set Replay Boundary").clicked() {
                        picked = Some(ChartContextAction::SetReplayBoundary);
                    }
                    if menu.button("Annotate").clicked() {
                        picked = Some(ChartContextAction::Annotate);
                    }
                    if !self.annotations.is_empty() && menu.button("Undo annotation").clicked() {
                        picked = Some(ChartContextAction::UndoAnnotation);
                    }
                    if let Some(label) = &merge_chart_label {
                        if menu.button(label).clicked() {
                            picked = Some(ChartContextAction::MergeToChart);
                        }
                    }
                    if let Some(label) = &merge_osc_label {
                        if menu.button(label).clicked() {
                            picked = Some(ChartContextAction::MergeToOscillator);
                        }
                    }
                },
            );
            if !open {
                self.context_open = false;
            }
            if let Some(action) = picked {
                self.context_open = false;
                let _ = self.apply_context_action(action);
            }
        }
        // Load-more affordance: when panned to the earliest loaded bar, fade
        // a button into the chart pane's lower-left corner. Clicking loads
        // and prepends the previous month, keeping the current rightmost bar
        // stable so the eyes stay on the data.
        if self.at_left_edge() {
            let load_rect = egui::Rect::from_min_size(
                egui::pos2(layout.plot.left() + 8.0, layout.plot.bottom() - 32.0),
                egui::vec2(180.0, 24.0),
            );
            // Translucent fill: the button reads as faded-in chrome over the
            // candles, never data.
            let faded = egui::Button::new("📥 Load previous")
                .fill(egui::Color32::from_rgba_unmultiplied(0x2B, 0x2B, 0x2B, 180));
            if ui.put(load_rect, faded).clicked() {
                let _ = self.load_previous_month(gateway);
            }
        }
        Ok(())
    }

    /// Poll the canvas pointer for a context-menu trigger: a secondary
    /// click, or a primary hold past the long-press threshold with the
    /// pointer nearly still (touch long-press). Detection is headless — the
    /// canvas keeps its single `Sense::drag` allocation, so no click-sensing
    /// widget adds drag latency — and strictly canvas-bound: a secondary
    /// click over the dock or a picker never arms the chart menu. The trigger
    /// resolves the bar under the pointer through the main viewport, clamped
    /// into the decided view, so later pans cannot move the armed actions.
    fn poll_chart_context_menu(
        &mut self,
        ui: &mut egui::Ui,
        response: &egui::Response,
        layouts: &[PaneLayout],
    ) {
        if layouts.is_empty() {
            return;
        }
        let now = ui.input(|input| input.time) as f32;
        if ui.input(|input| input.pointer.secondary_clicked()) {
            if let Some(pos) = response
                .hover_pos()
                .filter(|pos| response.rect.contains(*pos))
            {
                self.open_chart_context(layouts, pos);
            }
            self.context_press = None;
            self.context_fired = false;
            return;
        }
        if response.is_pointer_button_down_on() {
            if let Some(pos) = response.hover_pos() {
                match self.context_press {
                    None => self.context_press = Some((pos.x, pos.y, now)),
                    Some((x, y, start)) => {
                        if (pos.x - x).hypot(pos.y - y) >= 8.0 {
                            // A drag is a pan, never a hold: cancel.
                            self.context_press = None;
                        } else if !self.context_fired && now - start >= CHART_LONG_PRESS_SECS as f32
                        {
                            self.context_fired = true;
                            self.open_chart_context(layouts, pos);
                        }
                    }
                }
            }
        } else {
            self.context_press = None;
            self.context_fired = false;
        }
    }

    /// Arm the canvas context menu at `pos` (global pixels), resolving the
    /// bar beneath through the main viewport clamped into the decided view.
    /// A trigger with no resolvable bar still opens the menu — Auto Zoom
    /// needs no candle — with the candle actions refusing on their own.
    /// The pane resolves from the layout rects at open time through the one
    /// shared path, so right-click and long-press agree.
    fn open_chart_context(&mut self, layouts: &[PaneLayout], pos: egui::Pos2) {
        let plot = layouts.first().map(|layout| layout.plot);
        self.context_at = pos;
        self.context_local = plot.map(|plot| (pos.x - plot.left(), pos.y - plot.top()));
        self.context_time = self.main.and_then(|main| {
            let plot = plot?;
            if !plot.width().is_finite() || plot.width() <= 0.0 {
                return None;
            }
            let local = (pos.x - plot.left()).clamp(0.0, plot.width());
            let slot = main.x_to_index(local)?;
            if !slot.is_finite() {
                return None;
            }
            let view_len = self.replay_prefix_len().unwrap_or(self.rows.len());
            if view_len == 0 {
                return None;
            }
            let index = (slot + 0.5).floor().max(0.0) as usize;
            self.rows.get(index.min(view_len - 1)).map(|row| row.time)
        });
        self.context_pane = self.resolve_context_pane(layouts, pos);
        self.context_open = true;
    }

    /// Which pane `pos` sits over: the price plot, the shared oscillator, or
    /// one custom pane by id. The one resolution path both gestures share.
    fn resolve_context_pane(&self, layouts: &[PaneLayout], pos: egui::Pos2) -> Option<ContextPane> {
        let [main, shared, customs @ ..] = layouts else {
            return None;
        };
        if main.plot.contains(pos) {
            return Some(ContextPane::Main);
        }
        if shared.plot.contains(pos) {
            return Some(ContextPane::Shared);
        }
        for (layout, pane) in customs.iter().zip(self.sub_panes.iter()) {
            if layout.plot.contains(pos) {
                return Some(ContextPane::Custom(pane.id.clone()));
            }
        }
        None
    }

    /// The merge rows the open menu offers: the chart merge appears for any
    /// custom-pane target, while the oscillator merge appears only when the
    /// pane's slot would survive the shared-pane gate — offering an action
    /// that refuses on click is the same misfire the pane resolution removed.
    /// Both rows name the pane id. One helper behind the labels and the
    /// paint block, so the two can never disagree about what is offered.
    fn merge_labels(&self) -> (Option<String>, Option<String>) {
        let Some(ContextPane::Custom(id)) = &self.context_pane else {
            return (None, None);
        };
        let chart = Some(format!("Merge {id} to Chart"));
        let oscillator = id
            .strip_prefix("kernel:")
            .filter(|slot| self.is_mergeable_to_shared(slot))
            .map(|_| format!("Merge {id} to Oscillator"));
        (chart, oscillator)
    }

    /// Drop an operator note on the trigger candle: the pointer resolves
    /// through the main viewport against the loaded rows, and a miss paints
    /// nothing rather than a misplaced note. Main-pane only — a sub-pane
    /// pointer has no candle to annotate.
    fn annotate_trigger_candle(&mut self) {
        if self.context_pane != Some(ContextPane::Main) {
            return;
        }
        let note = match (self.context_local, self.main) {
            (Some(local), Some(viewport)) => annotate_at(local, &viewport, &self.rows),
            _ => None,
        };
        if let Some(note) = note {
            crate::annotations::append_new(&mut self.annotations, &[note]);
        }
    }

    /// Every row the open menu offers: the three base rows plus the merges
    /// from the one helper behind the paint block.
    pub fn context_menu_labels(&self) -> Vec<String> {
        let mut labels = vec![
            "Auto Zoom".to_owned(),
            "Bookmark".to_owned(),
            "Set Replay Boundary".to_owned(),
            "Annotate".to_owned(),
        ];
        if !self.annotations.is_empty() {
            labels.push("Undo annotation".to_owned());
        }
        let (chart, oscillator) = self.merge_labels();
        labels.extend(chart);
        labels.extend(oscillator);
        labels
    }

    /// Run one menu row: the base rows keep today's behavior, the merges
    /// write the routing row and prune through the derived rebuild.
    fn apply_context_action(&mut self, action: ChartContextAction) -> Result<(), EguiError> {
        match action {
            ChartContextAction::AutoZoom => {
                self.auto_zoom();
                Ok(())
            }
            ChartContextAction::Bookmark => {
                if let Some(time) = self.context_time {
                    self.add_bookmark(time);
                }
                Ok(())
            }
            ChartContextAction::SetReplayBoundary => {
                if let Some(time) = self.context_time {
                    self.set_replay_boundary(time);
                }
                Ok(())
            }
            ChartContextAction::Annotate => {
                self.annotate_trigger_candle();
                Ok(())
            }
            ChartContextAction::UndoAnnotation => {
                self.annotations.undo();
                Ok(())
            }
            ChartContextAction::MergeToChart => {
                let Some(ContextPane::Custom(pane_id)) = self.context_pane.clone() else {
                    return Err(EguiError::InvalidParam {
                        what: "merge".to_owned(),
                        reason: "no custom pane under the menu".to_owned(),
                    });
                };
                let Some(slot_id) = pane_id.strip_prefix("kernel:") else {
                    return Err(EguiError::InvalidParam {
                        what: pane_id,
                        reason: "only kernel panes merge to the chart".to_owned(),
                    });
                };
                self.set_pane_target(slot_id, PaneTarget::Main)?;
                self.rebuild_derived_panes()?;
                Ok(())
            }
            ChartContextAction::MergeToOscillator => {
                let Some(ContextPane::Custom(pane_id)) = self.context_pane.clone() else {
                    return Err(EguiError::InvalidParam {
                        what: "merge".to_owned(),
                        reason: "no custom pane under the menu".to_owned(),
                    });
                };
                let Some(slot_id) = pane_id.strip_prefix("kernel:") else {
                    return Err(EguiError::InvalidParam {
                        what: pane_id,
                        reason: "only kernel panes merge into the shared oscillator".to_owned(),
                    });
                };
                self.set_pane_target(slot_id, PaneTarget::SharedOscillator)?;
                self.rebuild_derived_panes()?;
                Ok(())
            }
        }
    }

    /// Translate this frame's pointer gestures into typed interaction inputs.
    ///
    /// The wheel acts on the region under the pointer, decided once by
    /// [`WheelZones`] so no call site re-guesses it: the candle and oscillator
    /// plots zoom the shared time axis, each pane's right-hand price strip
    /// zooms that pane's own price axis, the floating indicator legend
    /// scrolls instead of moving the chart, and an open settings pane
    /// consumes the gesture for its own scrolled content. Holding <kbd>Ctrl</kbd> overrides
    /// the plot's time zoom with the price zoom, the one gesture a chart pane
    /// has always answered.
    ///
    /// `response` spans both panes (see [`split_canvas`]), so a gesture over
    /// the oscillator pane or its price labels reaches here exactly like one
    /// over the candles. `layouts` is indexed by the router's own pane index,
    /// so the routing decision and the geometry lookup can never disagree
    /// about which pane a gesture landed on. While the divider is dragged the
    /// pan stays out, so one gesture never pans and resizes at once.
    fn step_from_response(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        layouts: &[PaneLayout],
        divider_dragged: bool,
    ) {
        let dt = f64::from(ui.input(|input| input.stable_dt));
        let mut events = Vec::new();
        if !divider_dragged {
            let drag = response.drag_delta();
            if drag.x != 0.0 || drag.y != 0.0 {
                events.push(PointerInput::Pan2D {
                    dx_px: drag.x,
                    dy_px: drag.y,
                });
            }
        }
        if response.hovered() {
            let (scroll, modifiers) =
                ui.input(|input| (input.smooth_scroll_delta, input.modifiers));
            if scroll.y != 0.0 {
                if let Some(hover) = response.hover_pos() {
                    let mut zones = WheelZones::new(
                        layouts
                            .iter()
                            .map(|layout| PaneZones {
                                plot: layout.plot.into(),
                                axis: layout.axis.into(),
                            })
                            .collect(),
                        self.stack
                            .list_rect(layouts[PRICE_PANE_ZONE].plot.height())
                            .map(Into::into),
                    );
                    if let Some(overlay) = self.settings_rect {
                        zones = zones.with_overlay(overlay.into());
                    }
                    match zones.route(hover.into()) {
                        // An open settings pane scrolls under the pointer and
                        // empty space answers nothing: both leave the chart
                        // and the machine untouched.
                        Some(WheelTarget::Overlay) | None => {}
                        // The legend scrolls under the pointer; the chart and
                        // the machine stay untouched.
                        Some(WheelTarget::List) => {
                            self.stack
                                .scroll_by(scroll.y, layouts[PRICE_PANE_ZONE].plot.height());
                        }
                        Some(WheelTarget::Time { anchor_x, .. }) if !modifiers.ctrl => {
                            events.push(PointerInput::WheelTime {
                                delta_px: scroll.y,
                                anchor_x,
                            });
                        }
                        // The price pane shares the interaction machine's data
                        // envelope, so its price zoom is a machine input.
                        Some(WheelTarget::Price {
                            pane: PRICE_PANE_ZONE,
                            anchor_y,
                        }) => {
                            events.push(PointerInput::Wheel {
                                delta_px: scroll.y,
                                anchor_y,
                            });
                        }
                        // Any other pane scales its own window: the shared time
                        // axis and the price pane's window stay put.
                        Some(WheelTarget::Price { pane, anchor_y }) => {
                            self.zoom_pane_price(pane, scroll.y, anchor_y);
                        }
                        Some(WheelTarget::Time { pane, anchor_x }) => {
                            events.push(PointerInput::WheelTime {
                                delta_px: scroll.y,
                                anchor_x,
                            });
                            events.push(PointerInput::Wheel {
                                delta_px: scroll.y,
                                // Pane-local to the pane the x anchor came
                                // from, so the Ctrl override over the
                                // oscillator pane zooms the oscillator and not
                                // the price pane it shares x with.
                                anchor_y: hover.y - layouts[pane].plot.top(),
                            });
                        }
                    }
                }
            }
        }
        self.step_interaction(dt, &events);
    }

    /// Zoom a non-primary pane's own price axis around `anchor_y` (a pane-local
    /// pixel), leaving the shared time axis and the price pane's window alone.
    ///
    /// The oscillator pane is not the price series, so it does not share the
    /// interaction machine's data envelope; it scales its own window with the
    /// same factor and range rescale the machine uses, clamped to the rail
    /// below. A pane the workspace does not hold, an anchor off the pane, or a
    /// rescale the scale refuses holds the window.
    fn zoom_pane_price(&mut self, pane: usize, delta_px: f32, anchor_y: f32) {
        let viewport = if pane == SUB_PANE_ZONE {
            self.sub
        } else {
            self.sub_panes
                .get(pane.saturating_sub(2))
                .map(|pane| pane.viewport)
        };
        let Some(sub) = viewport else {
            return;
        };
        let Some(anchor) = sub.y_to_price(anchor_y) else {
            return;
        };
        let scale = sub.scale();
        let range = scale.range();
        let Some(next) = zoom_range_around(range, anchor, wheel_zoom_factor(delta_px)) else {
            return;
        };
        let base_span = SUB_MAX - SUB_MIN;
        let span = (next.max - next.min).clamp(
            base_span * SUB_MIN_SPAN_FRACTION,
            base_span * SUB_MAX_SPAN_FRACTION,
        );
        // Keep the price under the cursor at its current pixel fraction while
        // the span is clamped, so a zoom that hits the band rail stays
        // anchored instead of translating the window — scroll up and scroll
        // down behave symmetrically and the pane never drifts.
        let frac = if range.max > range.min {
            ((anchor - range.min) / (range.max - range.min)).clamp(0.0, 1.0)
        } else {
            0.5
        };
        let Some(anchored) = anchor_window(anchor, frac, span) else {
            return;
        };
        let (rebuilt, offset) = (
            Viewport::new(
                scale.mode(),
                anchored.min,
                anchored.max,
                scale.height(),
                scale.bar_spacing(),
                scale.base_value(),
                scale.inverted(),
            ),
            sub.x_offset_px(),
        );
        if let Ok(view) = rebuilt.and_then(|view| view.with_x_offset(offset)) {
            if pane == SUB_PANE_ZONE {
                self.sub = Some(view);
            } else if let Some(slot) = pane
                .checked_sub(2)
                .and_then(|index| self.sub_panes.get_mut(index))
            {
                slot.viewport = view;
            }
        }
    }

    /// Main column below the app bar: canvas with the upper-left hotlist over
    /// it, plus the inline settings pane with series pickers. The canvas is
    /// allocated the full remaining width and height (price pane plus
    /// oscillator pane with their divider); the hotlist floats upper-left
    /// over the chart and the dock — when open — floats as a left-boundary
    /// drawer over this column, never shrinking it.
    fn paint_main_column(
        &mut self,
        ui: &mut egui::Ui,
        gateway: Option<&dyn TerminalGateway>,
    ) -> Result<(), EguiError> {
        self.paint_canvas(ui, gateway)?;
        let slot_actions = paint_hot_slot_stack(ui, &mut self.stack, &self.theme)?;
        for action in slot_actions {
            self.apply_slot_action(action);
        }
        if let Some(id) = self.open_settings_id() {
            let theme = self.theme.clone();
            // The settings schemas assemble here, where the domain facts
            // live: the source dropdown gains the live producer nodes and
            // the style schema projects to the keys the slot paints. The
            // modal stays a domain-blind renderer of the lists it is given.
            let (config_fields, style_fields) = self.settings_fields(&id).unwrap_or_default();
            let outcome = paint_slot_settings_modal(
                ui,
                &mut self.stack,
                &id,
                &theme,
                &config_fields,
                &style_fields,
            )?;
            // The frame rect routes next frame's wheel: a wheel over the
            // pane scrolls it instead of zooming the canvas below.
            self.settings_rect = Some(outcome.rect);
            match outcome.commit {
                SettingsCommit::Apply => {
                    self.commit_slot_settings(&id)?;
                }
                SettingsCommit::Ok => {
                    self.commit_slot_settings(&id)?;
                    let _ = self.stack.close_settings(&id);
                }
                SettingsCommit::Cancel => {
                    let _ = self.stack.discard_settings(&id);
                    let _ = self.stack.close_settings(&id);
                }
                SettingsCommit::None => {}
            }
        } else {
            self.settings_rect = None;
        }
        Ok(())
    }

    /// Material dock contents: collapsible subpanes for health, portfolio,
    /// proposals, journals, arm-grant form, and replay scrub. The header and
    /// toggle are gone (the menu-bar hamburger owns the drawer); the gateway
    /// terminal and chart subpanes are gone (refresh lives on the menu bar,
    /// symbol/timeframe/range live on the menu bar). Gateway sections read the
    /// cached bundle; a gateway-less workspace shows placeholders. Infallible
    /// by design: every gateway read is cached with a refusal line, never a
    /// frame-stopping error.
    /// Ensure the cached dock bundle exists when a gateway can serve it: loads
    /// once, then holds (a refusal needs the section's retry button, never a
    /// per-frame re-read). Called inside a section body, so closed sections
    /// never pay for serialization they do not show.
    fn ensure_dock_bundle(&mut self, gateway: Option<&dyn TerminalGateway>) {
        if self.dock_bundle.is_some() || self.dock_bundle_error.is_some() {
            return;
        }
        if let Some(gateway) = gateway {
            self.refresh_dock_bundle(gateway);
        }
    }

    fn paint_dock_contents(&mut self, ui: &mut egui::Ui, gateway: Option<&dyn TerminalGateway>) {
        // Range: chart-window presets plus the Beginning calendar, applied
        // from the dock instead of a menu dialog. The drawer grows on the Y
        // axis to fit the calendars, so every control displays in full.
        ui.collapsing("range", |body| {
            body.label(format!("window: {}", self.range_label()));
            self.seed_range_drafts();
            let active_range = self.active_range();
            let choice = paint_range_dialog(
                body,
                &mut self.range_draft,
                &mut self.top_menu.beginning_text,
                &mut self.calendar,
                active_range,
            );
            if let Some(choice) = choice {
                self.apply_range_choice(choice, gateway);
            }
            if let Some(error) = self.range_error.clone() {
                body.colored_label(egui::Color32::RED, error);
                if body.button("dismiss").clicked() {
                    self.range_error = None;
                }
            }
        });
        // Oscillators: one input row per bounded oscillator kind — period,
        // smoothing, gain, and the theme color the kind paints through.
        // Plain input values in the shipped script's spirit, never source
        // bindings. Writes land immediately; refusals name the field and
        // leave the row untouched.
        ui.collapsing("Oscillators", |body| {
            for kind in PANEL_KINDS {
                body.collapsing(crate::oscillator_panel::panel_kind_label(kind), |row| {
                    let mut row_refusals = Vec::new();
                    if let Ok(entry) = self.oscillator_panel.entry_mut(kind) {
                        let fields: Vec<fluent_egui::SchemaField> =
                            fluent_egui::schema_fields(&entry.describe())
                                .into_iter()
                                .filter(|field| {
                                    !crate::oscillator_panel::hides_dial(kind, &field.name)
                                })
                                .collect();
                        let edits = fluent_egui::paint_schema_fields(row, &fields, &mut *entry);
                        row_refusals.extend(edits.refusals);
                        if let Err(error) = entry.validate() {
                            row_refusals.push(error.to_string());
                        }
                    } else {
                        row_refusals.push(format!("{kind:?} is not an oscillator panel kind"));
                    }
                    let mut color_target = crate::oscillator_panel::ThemeKindColor {
                        theme: &mut self.theme,
                        kind,
                    };
                    let color_fields = fluent_egui::schema_fields(&color_target.describe());
                    let color_edits =
                        fluent_egui::paint_schema_fields(row, &color_fields, &mut color_target);
                    row_refusals.extend(color_edits.refusals);
                    for refusal in row_refusals {
                        row.colored_label(egui::Color32::RED, refusal);
                    }
                });
            }
        });
        // Portfolio. A live replay serves its own fills; the bundle is the
        // preview/live book.
        ui.collapsing("broker", |body| {
            if let Some(gateway) = gateway {
                if self.broker_model.is_none() {
                    self.broker_model = Some(BrokerModel::load_full(gateway));
                }
                if let Some(model) = &self.broker_model {
                    paint_broker_panel(body, model);
                }
            } else {
                body.label("No gateway");
            }
        });
        // Portfolio. A live replay serves its own fills; the bundle is the
        // preview/live book.
        ui.collapsing("portfolio", |body| {
            if let Some(live) = &self.live_replay {
                let state = &live.state;
                body.label(format!(
                    "replay positions={} filled={} fees={} open={}",
                    state.positions.len(),
                    state.filled_orders,
                    state.total_fees,
                    state
                        .positions
                        .iter()
                        .map(|position| position.quantity)
                        .sum::<i64>(),
                ));
                for position in &state.positions {
                    body.label(format!(
                        "{} qty={} avg={}",
                        position.underlying, position.quantity, position.avg_price
                    ));
                }
                return;
            }
            self.ensure_dock_bundle(gateway);
            if let Some(bundle) = &self.dock_bundle {
                body.label(format!(
                    "source={} positions={} filled={} fees={} open={}",
                    bundle.portfolio.source,
                    bundle.portfolio.positions.len(),
                    bundle.portfolio.filled_orders,
                    bundle.portfolio.total_fees,
                    bundle.portfolio.open_contracts
                ));
                for position in &bundle.portfolio.positions {
                    body.label(format!(
                        "{} qty={} avg={}",
                        position.underlying, position.quantity, position.avg_price
                    ));
                }
            } else if let Some(error) = &self.dock_bundle_error {
                body.colored_label(egui::Color32::RED, error.clone());
                if body.button("retry").clicked() {
                    self.dock_bundle_error = None;
                }
            } else {
                body.label("No portfolio pull yet");
            }
        });
        // Proposals. A live replay serves its own verdicts: proposal-bearing
        // decisions newest first, with the chain-gated spread count beside
        // them so a refusal never reads as silence.
        ui.collapsing("proposals", |body| {
            if let Some(live) = &self.live_replay {
                let state = &live.state;
                let mut proposed: Vec<_> = state
                    .decisions
                    .iter()
                    .filter(|decision| decision.direction.is_some())
                    .collect();
                proposed.reverse();
                body.label(format!(
                    "{} replay proposals (cursor {})",
                    proposed.len(),
                    state.cursor
                ));
                for decision in proposed.iter().take(20) {
                    let mut line = format!(
                        "{} {} {} {} {}",
                        decision.proposal_id,
                        decision.direction.as_deref().unwrap_or("-"),
                        decision.structure.as_deref().unwrap_or("-"),
                        decision.state,
                        decision.ts,
                    );
                    if let Some(skipped) = &decision.skipped {
                        use std::fmt::Write as _;
                        let _ = write!(line, " ({skipped})");
                    }
                    body.label(line);
                    for leg in Self::replay_leg_lines(decision) {
                        body.label(format!("  {leg}"));
                    }
                }
                let gated = state
                    .decisions
                    .iter()
                    .filter(|decision| decision.chain_gated)
                    .count();
                if gated > 0 {
                    body.label(format!(
                        "{gated} spread proposals refused (no option-chain feed)"
                    ));
                }
                return;
            }
            // The approval inbox is retired: proposals are authorized through
            // the gateway API, not the terminal dock. The replay arm above
            // still lists the live replay's own proposals read-only.
            body.label("Proposal approval moved to the gateway API");
        });
        // Manual tickets. The platform never submits these — the operator
        // places the exact ticket by hand; the panel issues it, records the
        // human's fill at the ticket price, and reads drift. Import snapshots
        // ride the route/gateway only (a posted position list, no panel
        // input), through the same read-only seam.
        ui.collapsing("tickets", |body| {
            let Some(gateway) = gateway else {
                body.label("Connect a gateway to ticket");
                return;
            };
            let model = match TicketModel::pull(gateway) {
                Ok(model) => model,
                Err(error) => {
                    body.colored_label(egui::Color32::RED, error.to_string());
                    return;
                }
            };
            body.label(format!(
                "{} waiting · {} drifted",
                model.waiting().len(),
                model.drifts().len()
            ));
            let mut actions = Vec::new();
            for ticket in model.waiting() {
                body.horizontal(|row| {
                    row.label(&ticket.rendered);
                    if row.button("confirm at ticket").clicked() {
                        actions.push(TicketAction::confirm(
                            &ticket.ticket_id,
                            serde_json::json!({
                                "quantity": ticket.quantity,
                                "price": ticket.limit_price,
                            }),
                        ));
                    }
                });
            }
            for drift in model.drifts() {
                body.label(format!(
                    "drift {} {}: {}",
                    drift.ticket_id, drift.kind, drift.detail
                ));
            }
            body.horizontal(|row| {
                row.label("issue");
                row.text_edit_singleline(&mut self.ticket_underlying);
                egui::ComboBox::from_id_salt("ticket-side")
                    .selected_text(match self.ticket_side {
                        TicketSide::Buy => "Buy",
                        TicketSide::Sell => "Sell",
                    })
                    .show_ui(row, |ui| {
                        ui.selectable_value(&mut self.ticket_side, TicketSide::Buy, "Buy");
                        ui.selectable_value(&mut self.ticket_side, TicketSide::Sell, "Sell");
                    });
                row.text_edit_singleline(&mut self.ticket_quantity);
                row.text_edit_singleline(&mut self.ticket_limit);
                if row.button("issue").clicked() {
                    match self.ticket_draft() {
                        Ok(ticket) => actions.push(TicketAction::issue(ticket)),
                        Err(reason) => self.ticket_error = Some(reason),
                    }
                }
            });
            if !actions.is_empty() {
                let mut refused = None;
                for action in &actions {
                    if let Err(error) = action.apply(gateway) {
                        refused = Some(error.to_string());
                    }
                }
                match refused {
                    Some(reason) => {
                        self.ticket_error = Some(format!("ticket refused: {reason}"));
                    }
                    None => self.ticket_error = None,
                }
            }
            if let Some(error) = &self.ticket_error {
                body.colored_label(egui::Color32::RED, error.clone());
                if body.button("dismiss").clicked() {
                    self.ticket_error = None;
                }
            }
        });
        // Journals. A live replay serves its own lifecycle rows; the bundle
        // is the preview/live journal.
        ui.collapsing("journals", |body| {
            if let Some(live) = &self.live_replay {
                if live.state.journal.is_empty() {
                    body.label("No replay journal rows yet");
                } else {
                    for row in live.state.journal.iter().rev().take(50) {
                        body.label(format!(
                            "#{} {} {} {}",
                            row.seq, row.client_id, row.kind, row.at_ms
                        ));
                    }
                }
                return;
            }
            self.ensure_dock_bundle(gateway);
            if let Some(error) = &self.dock_bundle_error {
                body.colored_label(egui::Color32::RED, error.clone());
                if body.button("retry").clicked() {
                    self.dock_bundle_error = None;
                }
            } else if self.dock_journal_lines.is_empty() {
                body.label("No journal rows yet");
            } else {
                for line in self.dock_journal_lines.iter().take(50) {
                    body.label(line);
                }
            }
        });
        // Arm grant pane (retired with the approval inbox: arming runs
        // through the gateway API, not the terminal dock).
        ui.collapsing("arm grant", |body| {
            body.label("Grant arming moved to the gateway API");
        });
        // Backtest pane: author a run from the served schema, run it, and
        // read the stored metrics, trades, curve, and status table. This is
        // the only place a strategy run starts: the replay section below
        // replays the fixed rehearsal composition and never runs a strategy.
        ui.collapsing("backtest", |body| {
            if let Some(reason) = self.backtest_error.clone() {
                body.colored_label(egui::Color32::RED, format!("backtest refused: {reason}"));
            }
            paint_backtest_panel(body, &mut self.backtest_form, &mut self.backtest, gateway);
            self.backtest_error = self.backtest_form.last_error().map(str::to_owned);
            self.sync_backtest_overlay();
        });
        // Timeframes: the loaded histories' window move, one row per
        // timeframe. Display-only (the terminal's own window read, never a
        // strategy signal): direction over the loaded window plus the signed
        // percent move on the display band.
        ui.collapsing("timeframes", |body| {
            paint_mtf_table(body, &self.mtf_window_rows());
        });
        // Replay: the rehearsal composition (fixed long, no exits) stepped
        // interactively over the shared decide path — never a strategy run.
        // The gateway owns the session (decisions, fills, journal); this owns
        // the captured window plus the transport, and the chart and panes
        // truncate to the decided prefix. Singles fill honestly; spreads
        // fill when the window's chains cover the bar and refuse with the
        // feed named otherwise. Future bars stay hidden until the run
        // decides them; stepping reveals them with the Y axis refit to the
        // decided window.
        ui.collapsing("replay", |body| {
            if let Some(reason) = self.replay_error.clone() {
                body.colored_label(egui::Color32::RED, format!("replay refused: {reason}"));
            }
            if let Some(live) = self.live_replay.as_ref() {
                let state = &live.state;
                body.label(format!(
                    "live replay: cursor {}/{} · {} decided · {} filled · fees {}",
                    state.cursor,
                    state.total_bars,
                    state.decisions.len(),
                    state.filled_orders,
                    state.total_fees,
                ));
                let gated = state.decisions.iter().filter(|decision| decision.chain_gated).count();
                if gated > 0 {
                    body.label(format!(
                        "{gated} spread proposals refused (no option-chain feed)"
                    ));
                }
                if let Some(error) = live.error.clone() {
                    body.colored_label(egui::Color32::RED, error);
                }
            } else {
                body.label("No live replay — the chart is live.");
            }
            body.label("source: rehearsal (fixed long) — interactive replay, not a backtest");
            body.label(format!("window: {}", self.replay_range_label()));
            body.label("Play replays the boundary window when set, else captures the chart's own visible window; edges stay marked on the pane divider. Right-click the chart to set boundaries.");
            if self.replay_range.is_some()
                && body.button("clear boundaries").clicked()
            {
                self.replay_range = None;
                self.replay_error = None;
            }
            if self.live_replay.is_some() {
                let playing = self
                    .live_replay
                    .as_ref()
                    .is_some_and(|live| live.playing);
                body.horizontal(|row| {
                    if row.button("step 1").clicked() {
                        if let Some(gateway) = gateway {
                            self.step_display_bars(gateway, 1, u64::MAX);
                        }
                    }
                    if row.button("step 10").clicked() {
                        if let Some(gateway) = gateway {
                            self.step_display_bars(gateway, 10, u64::MAX);
                        }
                    }
                    if row.button(if playing { "pause" } else { "play" }).clicked() {
                        self.toggle_replay_play(gateway);
                    }
                    if row.button("exit to live").clicked() {
                        if let Some(gateway) = gateway {
                            self.stop_live_replay(gateway);
                        } else {
                            self.live_replay = None;
                            let _ = self.recompute();
                        }
                    }
                });
                body.label(format!(
                    "speed: {} (change it from the top-bar speed menu)",
                    self.replay_speed.label()
                ));
                body.label("rehearsal economics (400/2000/30d, single-leg); spreads refuse without an option-chain feed.");
            }
        });
        // Health.
        ui.collapsing("health", |body| {
            self.ensure_dock_bundle(gateway);
            if let Some(bundle) = &self.dock_bundle {
                let health = &bundle.health;
                body.label(format!(
                    "status={} trading={} entries={} exits={}",
                    health.status, health.trading_state, health.allows_entries, health.allows_exits
                ));
                if health.halted() {
                    body.colored_label(egui::Color32::RED, "HALTED");
                }
            } else if let Some(error) = &self.dock_bundle_error {
                body.colored_label(egui::Color32::RED, error.clone());
                if body.button("retry").clicked() {
                    self.dock_bundle_error = None;
                }
            } else if gateway.is_some() {
                body.label("No health pull yet");
            } else {
                body.label("No gateway");
            }
        });
    }

    /// Snapshot the persisted terminal state: the active ticker's layout
    /// merged over the saved per-ticker layouts, the session selection, the
    /// starred entries, and the per-ticker bookmarks. The composition root
    /// writes it to `protopyne.json`, but only when the operator presses the
    /// menu-bar Save Layout button (see [`take_save_request`](Self::take_save_request)).
    #[must_use]
    pub fn prefs(&self) -> TerminalPrefs {
        let mut tickers = self.saved_layouts.clone();
        tickers.insert(self.active_symbol().to_owned(), self.current_layout());
        TerminalPrefs {
            active_symbol: Some(self.active_symbol().to_owned()),
            tickers,
            favorites: Favorites {
                symbols: self.favorite_symbols.iter().cloned().collect(),
                indicators: self.favorite_indicators.iter().cloned().collect(),
                bookmarks: self.favorite_bookmarks.iter().cloned().collect(),
            },
            bookmarks: self.bookmarks.clone(),
            dock_open: Some(self.dock_open),
            theme: Some(self.config.theme.clone()),
            oscillators: self.oscillator_panel.clone(),
            backtest: Some(crate::prefs::BacktestPrefs {
                seed: Some(self.backtest_form.seed_text().to_owned()),
                initial_equity: Some(self.backtest_form.initial_equity_text().to_owned()),
                start: (!self.backtest_form.start_text().is_empty())
                    .then(|| self.backtest_form.start_text().to_owned()),
                end: (!self.backtest_form.end_text().is_empty())
                    .then(|| self.backtest_form.end_text().to_owned()),
            }),
            ..TerminalPrefs::default()
        }
    }

    /// Apply a loaded [`TerminalPrefs`]: restore the session selection, the
    /// global settings, the starred entries, and the bookmarks; stash the
    /// per-ticker layouts and apply the active ticker's own (best-effort,
    /// seeded: a held frame missing the saved timeframe keeps the live rows
    /// instead of failing the boot — the next gateway switch repairs it).
    /// Every field is best-effort — an unknown timeframe or indicator id is
    /// skipped rather than refusing the whole document — so a partial or
    /// older file never strands the terminal.
    pub fn apply_prefs(&mut self, prefs: &TerminalPrefs) -> Result<(), EguiError> {
        if let Some(symbol) = prefs
            .active_symbol
            .as_deref()
            .filter(|symbol| !symbol.trim().is_empty())
            .or_else(|| {
                prefs
                    .symbol
                    .as_deref()
                    .filter(|symbol| !symbol.trim().is_empty())
            })
        {
            self.set_symbol(symbol)?;
        }
        self.saved_layouts = prefs.tickers.clone();
        self.stashed_layouts.clear();
        let symbol = self.active_symbol().to_owned();
        if let Some(layout) = prefs.ticker_layout(&symbol) {
            let _ = self.apply_ticker_layout(&layout, None);
        }
        if let Some(theme) = prefs
            .theme
            .as_deref()
            .filter(|theme| !theme.trim().is_empty())
        {
            self.config.theme.clone_from(&theme.to_owned());
        }
        if let Some(dock) = prefs.dock_open {
            self.dock_open = dock;
        }
        if prefs.oscillators.validate().is_ok() {
            self.oscillator_panel.clone_from(&prefs.oscillators);
        }
        if let Some(backtest) = &prefs.backtest {
            // Best-effort like every other arm: only non-blank texts win,
            // and a blank window bound clears the live one.
            if let Some(seed) = backtest
                .seed
                .as_deref()
                .filter(|text| !text.trim().is_empty())
            {
                self.backtest_form.set_seed_text(seed.to_owned());
            }
            if let Some(equity) = backtest
                .initial_equity
                .as_deref()
                .filter(|text| !text.trim().is_empty())
            {
                self.backtest_form
                    .set_initial_equity_text(equity.to_owned());
            }
            self.backtest_form
                .set_start_text(backtest.start.clone().unwrap_or_default());
            self.backtest_form
                .set_end_text(backtest.end.clone().unwrap_or_default());
        }
        self.favorite_symbols = prefs.favorites.symbols.iter().cloned().collect();
        self.favorite_indicators = prefs.favorites.indicators.iter().cloned().collect();
        self.favorite_bookmarks = prefs.favorites.bookmarks.iter().cloned().collect();
        self.bookmarks = prefs.bookmarks.clone();
        self.rebind_viewports()?;
        self.recompute()
    }

    /// Apply the saved loadout: remove slots the document omits, add the ones
    /// it names (through the indicator registry), then set each slot's config,
    /// style, and visibility. One rebind/recompute follows.
    fn apply_slot_prefs(&mut self, slots: &[SlotPrefs]) -> Result<(), EguiError> {
        if slots.is_empty() {
            return Ok(());
        }
        let keep: BTreeSet<&str> = slots.iter().map(|slot| slot.id.as_str()).collect();
        for id in self.stack.slot_ids() {
            if !keep.contains(id.as_str()) {
                self.remove_indicator(&id)?;
            }
        }
        for slot in slots {
            if !self.stack.contains(&slot.id) && self.slot_indicator(&slot.id).is_err() {
                // An id the registry no longer knows is skipped, never fatal.
                continue;
            }
            for (name, value) in &slot.config {
                let _ = self.stack.set_slot_field(&slot.id, name, value);
            }
            for (name, value) in &slot.style {
                let _ = self.stack.set_style_field(&slot.id, name, value);
            }
            if self.stack.visible(&slot.id) == Some(!slot.visible) {
                self.stack.toggle_visibility(&slot.id)?;
            }
        }
        Ok(())
    }

    /// Seed each open picker's favorite flags from this workspace's starred
    /// sets, so a reopened picker shows the persisted favorites, not a blank
    /// slate.
    fn seed_favorites(&mut self) {
        if let Some(modal) = self.symbol_modal.as_mut() {
            for id in modal.ids() {
                let starred = self.favorite_symbols.contains(&id);
                if modal.is_favorite(&id) != Some(starred) {
                    let _ = modal.toggle_favorite(&id);
                }
            }
        }
        if let Some(modal) = self.indicator_modal.as_mut() {
            for id in modal.ids() {
                let starred = self.favorite_indicators.contains(&id);
                if modal.is_favorite(&id) != Some(starred) {
                    let _ = modal.toggle_favorite(&id);
                }
            }
        }
        if let Some(modal) = self.bookmark_modal.as_mut() {
            for id in modal.ids() {
                let starred = self.favorite_bookmarks.contains(&id);
                if modal.is_favorite(&id) != Some(starred) {
                    let _ = modal.toggle_favorite(&id);
                }
            }
        }
    }

    /// Build the bookmark picker over the active ticker's entries: one row
    /// per bookmarked candle, tabbed by timeframe. Stars seed from the
    /// bookmark favorites set, so search and the Favorites filter behave
    /// like the ticker and indicator pickers.
    fn build_bookmark_catalog(&self) -> CatalogModal<TimeframeKey> {
        let symbol = self.active_symbol().to_owned();
        let mut catalog = CatalogModal::new("Bookmarks");
        let bookmarks = self.bookmarks.get(&symbol);
        for entry in bookmarks.into_iter().flatten() {
            let timeframe = TimeframeKey::all()
                .into_iter()
                .find(|key| key.label() == entry.timeframe)
                .unwrap_or(self.timeframe);
            let _ = catalog.register(&entry.id(&symbol), &entry.title(), timeframe);
        }
        catalog
    }

    /// Timeframe tabs for the bookmark picker, in toolbar order.
    fn bookmark_categories() -> Vec<(TimeframeKey, &'static str)> {
        TimeframeKey::all()
            .into_iter()
            .map(|key| (key, key.label()))
            .collect()
    }

    /// Bookmark the bar at `time` on the active timeframe for the active
    /// ticker. Duplicates (the same candle on the same timeframe) are
    /// ignored, so a double right-click never lists a candle twice.
    fn add_bookmark(&mut self, time: i64) {
        let symbol = self.active_symbol().to_owned();
        let timeframe = self.timeframe.label().to_owned();
        let list = self.bookmarks.entry(symbol).or_default();
        if !list
            .iter()
            .any(|entry| entry.time == time && entry.timeframe == timeframe)
        {
            list.push(BookmarkEntry { time, timeframe });
        }
    }

    /// Set a replay boundary at `time` for the replay button: with no window
    /// yet it becomes the beginning and the latest loaded candle the end; a
    /// candle before the beginning becomes the new beginning (the end stays);
    /// a candle at or after the beginning becomes the new end. Times are
    /// fixed — later pans never move the window.
    fn set_replay_boundary(&mut self, time: i64) {
        let last = self.rows.last().map_or(time, |row| row.time);
        match self.replay_range {
            None => self.replay_range = Some((time.min(last), time.max(last))),
            Some((start, end)) if time < start => self.replay_range = Some((time, end)),
            Some((start, end)) if time == start => self.replay_range = Some((start, end)),
            Some((start, _)) => self.replay_range = Some((start, time)),
        }
    }

    /// Fit the Y axis to the currently displayed candles, preserving the X
    /// position and zoom: the Refresh button's Y-fit step on its own, and the
    /// canvas context menu's Auto Zoom. Guards the unmeasured or empty chart
    /// like every other fit.
    fn auto_zoom(&mut self) {
        self.apply_auto_height(self.last_plot_width);
    }

    /// Consume a pending Save Layout request: true once per menu-bar Save
    /// click. The composition root writes `protopyne.json` only on true.
    pub fn take_save_request(&mut self) -> bool {
        std::mem::take(&mut self.save_requested)
    }

    /// Snapshot the live chart and indicator settings as this ticker's
    /// layout: the Save Layout button writes exactly this entry.
    fn current_layout(&self) -> TickerLayout {
        let slots = self
            .stack
            .slot_ids()
            .into_iter()
            .map(|id| SlotPrefs {
                visible: self.stack.visible(&id).unwrap_or(true),
                config: fields_of(self.stack.config(&id)),
                style: fields_of(self.stack.style(&id)),
                id,
            })
            .collect();
        TickerLayout {
            timeframe: Some(self.timeframe.label().to_owned()),
            range: self
                .custom_start
                .is_none()
                .then(|| self.range.label().to_owned()),
            custom_start: self.custom_start,
            chart_type: Some(self.chart_type.name().to_owned()),
            pane_share: Some(self.pane_share),
            slots,
        }
    }

    /// Stash the live layout under the active ticker for session continuity
    /// across ticker switches. The stash never reaches disk on its own —
    /// only an explicit Save writes — so visiting a ticker never overwrites
    /// its saved entry.
    fn stash_current_layout(&mut self) {
        let symbol = self.active_symbol().to_owned();
        let layout = self.current_layout();
        self.stashed_layouts.insert(symbol, layout);
    }

    /// Apply a ticker's saved layout: timeframe (with a gateway fetch, or a
    /// best-effort seeded reselect), the range or Beginning window (loaded
    /// with indicator warmup like every range change), the chart type, the
    /// pane share, and the indicator loadout. Unknown labels and ids are
    /// skipped like every other prefs apply, never fatal.
    fn apply_ticker_layout(
        &mut self,
        layout: &TickerLayout,
        gateway: Option<&dyn TerminalGateway>,
    ) -> Result<(), EguiError> {
        if let Some(label) = layout.timeframe.as_deref() {
            if let Some(key) = TimeframeKey::all()
                .into_iter()
                .find(|key| key.label() == label)
            {
                if key != self.timeframe {
                    match gateway {
                        Some(gateway) => self.switch_timeframe(key, gateway)?,
                        None => {
                            let _ = self.select_timeframe(key);
                        }
                    }
                }
            }
        }
        if let Some(start) = layout.custom_start {
            self.custom_start = Some(start);
            self.range = DateRange::All;
            if let Some(gateway) = gateway {
                let _ = self.refresh_chart(gateway, None);
            } else {
                self.rebound_from_history();
                // Rows changed under the held frame: re-evaluate so a
                // restored layout paints its own window, never stale panes.
                let _ = self.recompute();
            }
            self.rebind_viewports()?;
            let _ = self.jump_to_current_plot(self.last_plot_width);
        } else if let Some(label) = layout.range.as_deref() {
            if let Some(range) = DateRange::from_label(label) {
                if range != self.range || self.custom_start.is_some() {
                    self.select_range(range, gateway)?;
                }
            }
        }
        if let Some(name) = layout.chart_type.as_deref() {
            if let Some(kind) = ChartType::all()
                .iter()
                .copied()
                .find(|kind| kind.name() == name)
            {
                self.chart_type = kind;
            }
        }
        if let Some(share) = layout.pane_share {
            self.set_pane_share(share);
        }
        self.apply_slot_prefs(&layout.slots)?;
        self.rebind_viewports()?;
        self.recompute()
    }

    /// Jump the chart to a bookmarked candle: the nearest loaded bar at or
    /// before the bookmark becomes the rightmost visible bar, loading more
    /// history first when the bookmark predates the window. Unknown ids
    /// refuse by name instead of moving the chart.
    fn jump_to_bookmark(
        &mut self,
        id: &str,
        gateway: Option<&dyn TerminalGateway>,
    ) -> Result<(), EguiError> {
        let symbol = self.active_symbol().to_owned();
        let time = self
            .bookmarks
            .get(&symbol)
            .into_iter()
            .flatten()
            .find(|entry| entry.id(&symbol) == id)
            .map(|entry| entry.time)
            .ok_or_else(|| EguiError::InvalidParam {
                what: "bookmark".to_owned(),
                reason: format!("{id:?} is not a bookmark of {symbol}"),
            })?;
        let text = chrono::DateTime::from_timestamp(time, 0)
            .map(|moment| moment.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        self.jump_to_time(&text, self.last_plot_width, gateway)
    }

    /// Paint one titlebar-less picker popup below its menu-bar button: no
    /// window chrome and no X — an outside click or Escape closes through
    /// `open`, like the Candles pulldown it hangs beside.
    fn picker_popup(
        ui: &mut egui::Ui,
        id: &str,
        anchor: impl Into<egui::PopupAnchor>,
        open: &mut bool,
        contents: impl FnOnce(&mut egui::Ui),
    ) {
        egui::Popup::new(
            egui::Id::new(id),
            ui.ctx().clone(),
            anchor,
            egui::LayerId::new(egui::Order::Foreground, egui::Id::new(id)),
        )
        .open_bool(open)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(contents);
    }

    /// Paint the symbol, indicator, and bookmark pickers plus the Jump To
    /// Time popup, each as a titlebar-less popup anchored below its menu-bar
    /// button — never a window, so no picker carries a titlebar or an X
    /// close button. Picks dispatch inline (symbol jumps tickers, indicator
    /// slots loadout, bookmark jumps its candle, Go jumps the typed time);
    /// star toggles write back to the workspace sets so the Favorites filter
    /// survives the popup closing.
    fn paint_modals(
        &mut self,
        ui: &mut egui::Ui,
        gateway: Option<&dyn TerminalGateway>,
    ) -> Result<(), EguiError> {
        let theme = self.theme.clone();
        // Dismissable pickers: Escape clears every open popup (an untouched
        // frame keeps them as they are).
        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.symbol_modal = None;
            self.symbol_anchor = None;
            self.indicator_modal = None;
            self.indicator_anchor = None;
            self.bookmark_modal = None;
            self.bookmark_anchor = None;
            self.bookmark_error = None;
            self.jump_open = false;
            self.jump_anchor = None;
            self.jump_error = None;
        }
        let mut picked_symbol = None;
        if self.symbol_modal.is_some() {
            let mut open = true;
            match self.symbol_anchor {
                Some(anchor) => {
                    if let Some(modal) = self.symbol_modal.as_mut() {
                        Self::picker_popup(ui, "symbol-picker", anchor, &mut open, |picker| {
                            match paint_catalog_modal(picker, modal, &symbol_categories(), &theme) {
                                Ok(actions) => {
                                    for action in actions {
                                        match action {
                                            CatalogAction::Picked(id) => picked_symbol = Some(id),
                                            CatalogAction::ToggledFavorite(id) => {
                                                let _ = modal.toggle_favorite(&id);
                                            }
                                        }
                                    }
                                }
                                Err(error) => {
                                    picker.colored_label(egui::Color32::RED, error.to_string());
                                }
                            }
                        });
                    }
                }
                // An opener always stores its anchor beside the modal; a
                // missing one closes fail-closed instead of stranding a
                // rect-less popup.
                None => open = false,
            }
            if !open {
                self.symbol_modal = None;
                self.symbol_anchor = None;
            }
        }
        if let Some(modal) = self.symbol_modal.as_ref() {
            self.favorite_symbols = modal
                .ids()
                .into_iter()
                .filter(|id| modal.is_favorite(id) == Some(true))
                .collect();
        }
        if let Some(symbol) = picked_symbol {
            self.symbol_modal = None;
            self.symbol_anchor = None;
            self.apply_symbol_pick(&symbol, gateway)?;
        }

        let mut picked_indicator = None;
        if self.indicator_modal.is_some() {
            let mut open = true;
            match self.indicator_anchor {
                Some(anchor) => {
                    if let Some(modal) = self.indicator_modal.as_mut() {
                        Self::picker_popup(ui, "indicator-picker", anchor, &mut open, |picker| {
                            match paint_catalog_modal(
                                picker,
                                modal,
                                &indicator_categories(),
                                &theme,
                            ) {
                                Ok(actions) => {
                                    for action in actions {
                                        match action {
                                            CatalogAction::Picked(id) => {
                                                picked_indicator = Some(id);
                                            }
                                            CatalogAction::ToggledFavorite(id) => {
                                                let _ = modal.toggle_favorite(&id);
                                            }
                                        }
                                    }
                                }
                                Err(error) => {
                                    picker.colored_label(egui::Color32::RED, error.to_string());
                                }
                            }
                        });
                    }
                }
                None => open = false,
            }
            if !open {
                self.indicator_modal = None;
                self.indicator_anchor = None;
            }
        }
        if let Some(modal) = self.indicator_modal.as_ref() {
            self.favorite_indicators = modal
                .ids()
                .into_iter()
                .filter(|id| modal.is_favorite(id) == Some(true))
                .collect();
        }
        if let Some(id) = picked_indicator {
            self.indicator_modal = None;
            self.indicator_anchor = None;
            self.slot_indicator(&id)?;
            self.recompute_preserving_view()?;
        }

        let mut picked_bookmark = None;
        if self.bookmark_modal.is_some() {
            let mut open = true;
            match self.bookmark_anchor {
                Some(anchor) => {
                    if let Some(modal) = self.bookmark_modal.as_mut() {
                        let bookmark_error = self.bookmark_error.clone();
                        Self::picker_popup(ui, "bookmark-picker", anchor, &mut open, |picker| {
                            if modal.is_empty() {
                                picker.label("No bookmarks available");
                            } else {
                                match paint_catalog_modal(
                                    picker,
                                    modal,
                                    &Self::bookmark_categories(),
                                    &theme,
                                ) {
                                    Ok(actions) => {
                                        for action in actions {
                                            match action {
                                                CatalogAction::Picked(id) => {
                                                    picked_bookmark = Some(id);
                                                }
                                                CatalogAction::ToggledFavorite(id) => {
                                                    let _ = modal.toggle_favorite(&id);
                                                }
                                            }
                                        }
                                    }
                                    Err(error) => {
                                        picker.colored_label(egui::Color32::RED, error.to_string());
                                    }
                                }
                            }
                            if let Some(error) = bookmark_error {
                                picker.colored_label(egui::Color32::RED, error);
                            }
                        });
                    }
                }
                None => open = false,
            }
            if !open {
                self.bookmark_modal = None;
                self.bookmark_anchor = None;
                self.bookmark_error = None;
            }
        }
        if let Some(modal) = self.bookmark_modal.as_ref() {
            self.favorite_bookmarks = modal
                .ids()
                .into_iter()
                .filter(|id| modal.is_favorite(id) == Some(true))
                .collect();
        }
        if let Some(id) = picked_bookmark {
            match self.jump_to_bookmark(&id, gateway) {
                Ok(()) => {
                    self.bookmark_modal = None;
                    self.bookmark_anchor = None;
                    self.bookmark_error = None;
                }
                Err(error) => self.bookmark_error = Some(error.to_string()),
            }
        }

        if self.jump_open {
            let mut open = true;
            match self.jump_anchor {
                Some(anchor) => {
                    let text = &mut self.top_menu.jump_text;
                    let calendar = &mut self.jump_calendar;
                    let jump_error = self.jump_error.clone();
                    let mut go = false;
                    let mut picked_date = None;
                    Self::picker_popup(ui, "jump-picker", anchor, &mut open, |jump_ui| {
                        jump_ui.label(bar_text("Jump To Time"));
                        jump_ui.add(
                            egui::TextEdit::singleline(text)
                                .hint_text("2026-03-02 09:30")
                                .desired_width(180.0),
                        );
                        if jump_ui.button(bar_text("Go")).clicked() {
                            go = true;
                        }
                        if let Some(error) = jump_error {
                            jump_ui.colored_label(egui::Color32::RED, error);
                        }
                        jump_ui.separator();
                        // The calendar sets the date part and preserves the
                        // clock part: a picked day never clobbers `HH:MM`.
                        let selected = text.split_whitespace().next().and_then(|date| {
                            chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()
                        });
                        if let Some(date) = paint_calendar(jump_ui, "jump", calendar, selected) {
                            picked_date = Some(date);
                        }
                    });
                    if let Some(date) = picked_date {
                        self.apply_jump_date(&date);
                    }
                    if go {
                        let target = self.top_menu.jump_text.clone();
                        match self.jump_to_time(&target, self.last_plot_width, gateway) {
                            Ok(()) => {
                                self.jump_open = false;
                                self.jump_anchor = None;
                                self.jump_error = None;
                            }
                            Err(error) => self.jump_error = Some(error.to_string()),
                        }
                    }
                }
                None => open = false,
            }
            if !open {
                self.jump_open = false;
                self.jump_anchor = None;
                self.jump_error = None;
            }
        }
        Ok(())
    }
}

impl Default for ChartWorkspace {
    fn default() -> Self {
        Self::new().expect("the default workspace boots with the shipped plugins")
    }
}

/// Paint the full terminal: Material top bar, full-window multi-pane canvas
/// with the upper-left hot-slot legend, inline settings pane with series
/// pickers, floating full-height drawer, and the titlebar-less picker popups.
///
/// Layout (Material, workstation + touch):
/// - topmost app bar spans the full width (dock toggle, ticker, save layout,
///   timeframe selector, display-mode pulldown, refresh button, the label-free
///   pan cluster with the replay play/pause transport between rewind and
///   forward, jump-to-time, bookmarks, indicator selector);
/// - below it, the chart canvas takes the whole window: price pane plus
///   oscillator pane with their divider, full remaining width and height;
/// - the indicator hotlist floats upper-left over the chart pane with eye /
///   bolt / X controls (X unloads immediately);
/// - the dock, when toggled, floats as a full-height drawer along the entire
///   left side below the app bar over the chart — never a side pane, so the
///   panes never shrink. It carries no titlebar and no close button; the
///   hamburger button alone toggles it. Sections scroll inside when their
///   content exceeds the height;
/// - chart-range picking lives as a dock expansion; the ticker, indicator,
///   bookmark, and jump pickers hang as titlebar-less popups below their
///   menu-bar buttons, like the Candles pulldown; refresh lives on the menu
///   bar.
///
/// The gateway is optional: a workspace seeded with rows (the demo shell)
/// paints without one, and a symbol or timeframe pick can only pull fresh
/// history when a gateway is supplied. The dock shows cached health,
/// portfolio, proposals, journals, range, replay, and arm-grant sections.
pub fn paint_workspace(
    ui: &mut egui::Ui,
    workspace: &mut ChartWorkspace,
    gateway: Option<&dyn TerminalGateway>,
) -> Result<(), EguiError> {
    // 1. Material app bar (full width, above everything).
    let top_action = {
        let symbol = workspace.active_symbol().to_owned();
        let timeframe = workspace.timeframe;
        let chart_type = workspace.chart_type;
        let dock_open = workspace.dock_open;
        let replay_playing = workspace.replay_playing();
        // Material app-bar frame: subtle elevation via stroke + rounding.
        egui::Frame::new()
            .fill(ui.visuals().panel_fill)
            .inner_margin(4.0)
            .outer_margin(0.0)
            .show(ui, |bar| {
                paint_top_bar(
                    bar,
                    &symbol,
                    timeframe,
                    chart_type,
                    dock_open,
                    replay_playing,
                    workspace.replay_speed,
                    &mut workspace.symbol_anchor,
                    &mut workspace.indicator_anchor,
                    &mut workspace.bookmark_anchor,
                    &mut workspace.jump_anchor,
                )
            })
            .inner
    };
    if let Some(action) = top_action {
        workspace.apply_top_bar_action(action, gateway)?;
    }
    ui.separator();
    // 2. Chart canvas takes the whole window below the menu bar: full
    // remaining width and height, price pane plus oscillator pane with their
    // divider. The dock never participates in this layout.
    {
        let _ = workspace.paint_main_column(ui, gateway);
    }
    // 3. Floating dock drawer along the entire left side below the app bar.
    // Overlay only: it draws over the chart via the context, so the panes
    // keep the full window whether it is open or closed. No titlebar and no
    // close button — the hamburger button alone toggles it. The drawer spans
    // the full height below the bar (Material navigation drawer); sections
    // scroll inside when their content exceeds the height.
    if workspace.dock_open {
        let top = 48.0;
        let height = (ui.ctx().viewport_rect().height() - top).max(200.0);
        egui::Window::new("Dock")
            .title_bar(false)
            .anchor(egui::Align2::LEFT_TOP, egui::vec2(0.0, top))
            .default_size(egui::vec2(300.0, height))
            .min_size(egui::vec2(260.0, 200.0))
            .resizable([true, false])
            .show(ui.ctx(), |dock_ui| {
                dock_ui.set_min_size(egui::vec2(0.0, height));
                egui::ScrollArea::vertical().show(dock_ui, |scroll| {
                    workspace.paint_dock_contents(scroll, gateway);
                });
            });
    }
    // 4. Replay play pump: pace steps to the selected speed while a run plays,
    // and keep frames coming until paused or the head is reached. Independent
    // of the dock (menu-bar play works with the drawer closed); the run's
    // own end clamp still bounds every step. The frame delta comes from
    // egui's own clock, so headless tests drive the same path with explicit
    // durations.
    // The frame delta is actual elapsed time, not the predicted next-frame
    // gap: the pump schedules candles against wall-clock deadlines, and a
    // prediction would accrue time that has not elapsed yet. A sleep-sized
    // gap is safe — the pump drops banked backlog instead of dumping it.
    let dt = std::time::Duration::from_secs_f32(ui.ctx().input(|input| input.unstable_dt).max(0.0));
    workspace.pump_replay_play(gateway, ui, dt);
    workspace.paint_modals(ui, gateway)?;
    Ok(())
}

#[cfg(test)]
#[path = "../tests/workspace.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/live_replay.rs"]
mod live_replay_tests;

#[cfg(test)]
#[path = "../tests/workspace_symbols.rs"]
mod workspace_symbols_tests;

#[cfg(test)]
#[path = "../tests/workspace_tape.rs"]
mod workspace_tape_tests;

#[cfg(test)]
#[path = "../tests/series_source.rs"]
mod series_source_tests;

#[cfg(test)]
#[path = "../tests/auto_height.rs"]
mod auto_height_tests;

#[cfg(test)]
#[path = "../tests/window_alignment.rs"]
mod window_alignment_tests;

#[cfg(test)]
#[path = "../tests/fetch_limit.rs"]
mod fetch_limit_tests;

#[cfg(test)]
#[path = "../tests/beginning_reset.rs"]
mod beginning_reset_tests;

#[cfg(test)]
#[path = "../tests/heikin_cache.rs"]
mod heikin_cache_tests;

#[cfg(test)]
#[path = "../tests/recompute_gate.rs"]
mod recompute_gate_tests;

#[cfg(test)]
#[path = "../tests/switch_order.rs"]
mod switch_order_tests;

#[cfg(test)]
#[path = "../tests/prefs.rs"]
mod prefs_tests;

#[cfg(test)]
#[path = "../tests/live_chart.rs"]
mod live_chart_tests;

#[cfg(test)]
#[path = "../tests/derived_panes.rs"]
mod derived_panes_tests;

#[cfg(test)]
#[path = "../tests/pane_routing.rs"]
mod pane_routing_tests;

#[cfg(test)]
#[path = "../tests/context_panes.rs"]
mod context_panes_tests;

#[cfg(test)]
#[path = "../tests/oscillator_display.rs"]
mod oscillator_display_tests;

#[cfg(test)]
#[path = "../tests/threshold_calibration.rs"]
mod threshold_calibration_tests;

#[cfg(test)]
#[path = "../tests/shared_oscillator.rs"]
mod shared_oscillator_tests;

#[cfg(test)]
#[path = "../tests/source_options.rs"]
mod source_options_tests;

#[cfg(test)]
#[path = "../tests/settings_style.rs"]
mod settings_style_tests;

#[cfg(test)]
#[path = "../tests/schema_coverage.rs"]
mod schema_coverage_tests;

#[cfg(test)]
#[path = "../tests/oscillator_panel.rs"]
mod oscillator_panel_tests;

#[cfg(test)]
#[path = "../tests/pane_taxonomy.rs"]
mod pane_taxonomy_tests;

#[cfg(test)]
#[path = "../tests/add_preserves_view.rs"]
mod add_preserves_view_tests;
