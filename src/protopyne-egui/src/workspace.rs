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
    anchor_window, auto_height_range, axis_ticks, candle_body_width, column_candle_geoms,
    column_delta_geoms, crosshair_data, crosshair_lines, needs_column_aggregation,
    opacity_from_transparency, paint_candles_tinted, paint_catalog_modal, paint_crosshair,
    paint_delta_bars_tinted, paint_hot_slot_stack, paint_oscillator, paint_ribbon,
    paint_ribbon_fill, paint_slot_settings_modal, proportional_ribbon_polygons, resolve_color,
    sampled_line_points, sampled_ribbon_bands, translate_bands, translate_candles,
    translate_delta_bars, translate_points, translate_polygons, visible_rows, wheel_zoom_factor,
    zoom_range_around, CatalogAction, CatalogModal, CrosshairLines, EguiError, HotSlotAction,
    HotSlotStack, InteractionState, PaneCache, PanePlan, PaneZones, PointerInput, SettingsCommit,
    SettingsTab, SlotAttachment, ThemeConfig, TintedCandle, TintedDeltaBar, Viewport, WheelTarget,
    WheelZones,
};
use fluent_wvr::{component_downcast_ref, Component, FieldAccess};
use protopyne_chart_model::{heikin_rows_of, ChartRow, ChartType, DateRange};
use protopyne_chart_scale::PriceScaleMode;
use protopyne_types::{BucketAccum, Money, SymKey, SymbolIndex, Timeframe};
use protopyne_view::{
    pick_default_symbol, symbol_catalog, AssetClass, ReplayDecisionView, ReplayStartParams,
    ReplayStateView, SymbolInfoView, TerminalGateway,
};

use crate::chart::{
    fetch_for_span, line_values, window_for_range, ChartConfig, TimeframeKey, DEFAULT_RANGE,
    DEFAULT_SYMBOL,
};
use crate::indicators::{
    build_indicator_catalog, instantiate_indicator, ChartIndicator, IndicatorCategory,
    IndicatorEvaluation,
};
use crate::integration_lite::{
    candle_momentum_keys, candle_momentum_series, integration_lite_sub_pane, momentum_key_for,
    momentum_tints, ribbon_fill_transparency, IntegrationLiteConfig, IntegrationLiteSubPane,
    MomentumPalette, MomentumTint,
};
use crate::prefs::{Favorites, SlotPrefs, TerminalPrefs};
use crate::session::{fmt_boundary_time, time_grid_lines, time_major_unit, SessionBreakCache};
use crate::topology::SeriesCatalog;
use crate::views::{load_bundle, InboxAction, InboxModel, TicketAction, TicketModel, ViewsBundle};
use crate::widgets::{
    paint_range_dialog, paint_top_bar, CalendarState, RangeChoice, TopBarAction, TopBarMenuState,
};
use crate::{paint_grant_form, GrantFormState, ReplayScrub};

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
    state: ReplayStateView,
    /// Whether the play loop steps while the section paints.
    playing: bool,
    /// Play rate in bars per second.
    rate_bars_per_sec: u64,
    /// Last control refusal, cleared on the next success.
    error: Option<String>,
    /// Last play tick, for rate pacing.
    last_tick: Option<std::time::Instant>,
}

impl LiveReplay {
    /// Rehearsal economics a replay opens under: the preview's own per-trade
    /// booking, so a replay and a preview over the same bars agree on money.
    const MAX_LOSS: i64 = 400;
    /// See [`Self::MAX_LOSS`].
    const NOTIONAL: i64 = 2_000;
    /// See [`Self::MAX_LOSS`]: front-leg days to expiry.
    const DTE_DAYS: u32 = 30;

    fn new(session_id: String, state: ReplayStateView) -> Self {
        Self {
            session_id,
            state,
            playing: false,
            rate_bars_per_sec: 5,
            error: None,
            last_tick: None,
        }
    }
}

/// The terminal's central state store: active symbol, selectors, hot-slot
/// stack, pane plan, pane cache, the 2D interaction machine, and the Material
/// dock (left navigation drawer) with its cached gateway views.
///
/// One place owns all terminal state so the Material top bar, the symbol and
/// indicator modals, the upper-left hot-slot legend, the multi-pane canvas,
/// and the dock's collapsible subpanes read and write the same truth.
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
    symbol_modal: Option<CatalogModal<AssetClass>>,
    indicator_modal: Option<CatalogModal<IndicatorCategory>>,
    /// Starred ticker symbols, owned here (not in the on-demand modal) so the
    /// Favorites filter survives the modal closing and serializes to
    /// `protopyne.json`.
    favorite_symbols: BTreeSet<String>,
    /// Starred indicator ids; the indicator-catalog twin of
    /// [`favorite_symbols`](Self::favorite_symbols).
    favorite_indicators: BTreeSet<String>,
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
    /// Top-bar menu drafts (Beginning + Jump texts) behind the data-range
    /// dialog. Owned here so the Range button and its dialog share one truth.
    top_menu: TopBarMenuState,
    /// Whether the interactive range dialog is open.
    range_menu_open: bool,
    /// Draft preset in the range dialog; `None` when a Beginning is drafted.
    range_draft: Option<DateRange>,
    /// Month the Beginning calendar is showing. Owned here (not in the
    /// reflection-backed menu component) so the picker navigates months
    /// without a schema field per cursor.
    calendar: CalendarState,
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
    /// Journal lines pre-rendered in provenance order at refresh time.
    dock_journal_lines: Vec<String>,
    /// Arming form over the patch descriptor: drafts, preview, submit all
    /// through the gateway. Owned here so the dock's arm-grant subpane and
    /// the terminal example share one truth; the schema is set once via
    /// [`set_grant_schema`](Self::set_grant_schema) when a descriptor is
    /// available, else the form stays empty and read-only tier texts show.
    grant_form: GrantFormState,
    /// Replay scrub over the standard topology; the gateway supplies the
    /// window, this owns where the cursor stands. Lives in the dock.
    replay: Option<ReplayScrub>,
    /// Replay refusal, when ingest failed.
    replay_error: Option<String>,
    /// Interactive replay run (proposals + paper fills) when one is live:
    /// the controls and the cached core-side state the chart and panes
    /// truncate to. `None` is the live chart.
    live_replay: Option<LiveReplay>,
    /// Replay start draft: a bar index or a `YYYY-MM-DD` date. Lives outside
    /// the run so the operator can draft the next open while one is live.
    replay_start_draft: String,
    /// Replay seed draft: a bare integer naming deterministic proposal ids.
    replay_seed_draft: String,
    /// Replay grant draft: an explicit grant document as JSON, or empty for
    /// the replay rehearsal grant. Lives outside the run so the operator can
    /// draft the next open while one is live; the core alone parses what is
    /// sent, exactly like `arm` (never armed here, only replayed under).
    replay_grant_draft: String,
    /// Replay structure draft: the `"Single"`/`"Vertical"` tag the next open
    /// forwards to the core, which builds the replay strategy and grant
    /// scope from it. A plain string, never the core vocabulary — the paint
    /// edge links no strategy types.
    replay_structure_draft: String,
    /// Bar index under the pointer at the last painted frame, for the
    /// replay's start-at-crosshair control.
    last_hover_bar: Option<usize>,
    /// Manual-ticket issue drafts: the operator's exact ticket fields. The
    /// core alone parses what is sent — drafts stay plain strings until the
    /// issue click builds the intent document.
    ticket_underlying: String,
    /// Ticket side draft (`Buy`/`Sell`).
    ticket_side: String,
    /// Ticket quantity draft (whole contracts).
    ticket_quantity: String,
    /// Ticket limit-price draft (decimal text).
    ticket_limit: String,
    /// Ticket client-id sequence: each issue click mints `manual-{seq}`.
    ticket_seq: u64,
    /// Ticket refusal, when the last ticket action failed.
    ticket_error: Option<String>,
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

impl ChartWorkspace {
    /// Boot the terminal: the default ticker (`SPY`), the five-minute default
    /// timeframe, a one-month default window, and the three scripted
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
            symbol_modal: None,
            indicator_modal: None,
            favorite_symbols: BTreeSet::new(),
            favorite_indicators: BTreeSet::new(),
            session_breaks: SessionBreakCache::new(),
            dock_open: false,
            top_menu: TopBarMenuState::new().build(),
            range_menu_open: false,
            range_draft: None,
            calendar: CalendarState::default(),
            custom_start: None,
            pending_auto_height: false,
            last_plot_width: PANE_WIDTH_PX,
            dock_bundle: None,
            dock_bundle_error: None,
            dock_journal_lines: Vec::new(),
            grant_form: GrantFormState::default(),
            replay: ReplayScrub::standard().ok(),
            replay_error: None,
            live_replay: None,
            replay_start_draft: String::new(),
            replay_seed_draft: "7".to_owned(),
            replay_grant_draft: String::new(),
            replay_structure_draft: "Single".to_owned(),
            last_hover_bar: None,
            ticket_underlying: "SPY".to_owned(),
            ticket_side: "Buy".to_owned(),
            ticket_quantity: "1".to_owned(),
            ticket_limit: String::new(),
            ticket_seq: 0,
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
    /// contract, then recompute so the edit is live.
    pub fn set_slot_field(&mut self, id: &str, name: &str, value: &str) -> Result<(), EguiError> {
        self.stack.set_slot_field(id, name, value)?;
        self.recompute()
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
        self.recompute()?;
        Ok(refused)
    }

    /// Remove an indicator: close its pane handle, unslot it, drop its
    /// evaluation, and refresh the derived pane/tint state.
    pub fn remove_indicator(&mut self, id: &str) -> Result<(), EguiError> {
        self.stack.remove(id)?;
        self.plugins.retain(|plugin| plugin.id() != id);
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
        if id == INTEGRATION_LITE_ID {
            self.sub_pane = None;
            self.momentum_keys.clear();
            self.tints.clear();
        }
        if self.rows.is_empty() {
            return Ok(());
        }
        self.sub_pane = self.compute_sub_pane()?;
        self.recompute_tint()?;
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
            timeframe.seconds(),
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

    /// Pull fresh bars for the active symbol and timeframe through `gateway`,
    /// rebind the viewports, recompute every active indicator, and refresh
    /// the dock's cached gateway views (health/portfolio/proposals/journal)
    /// plus the replay window. This is the full refresh a **live feed** drives
    /// (new bars keep arriving, so the dock and replay move too); with no live
    /// feed the menu-bar button calls
    /// [`verify_chart_range`](Self::verify_chart_range) instead, which touches
    /// the chart alone.
    pub fn refresh(&mut self, gateway: &dyn TerminalGateway) -> Result<(), EguiError> {
        self.refresh_chart(gateway, None)?;
        self.refresh_dock_bundle(gateway);
        self.refresh_replay(gateway);
        Ok(())
    }

    /// Verify the chart's selected range is loaded — the menu-bar Refresh
    /// action when no live feed is active. It confirms the loaded window
    /// covers the active range and reads the missing trailing delta through
    /// `gateway` when it does not; an already-covered range only rebinds and
    /// recomputes. Either way the indicators redraw and the visible window
    /// refits, exactly like a rewind/forward landing. The dock bundle and
    /// replay are left untouched: with no live feed they cannot have
    /// changed, so their serialization cost is not paid by a chart verify
    /// (the dock still loads on first open).
    pub fn verify_chart_range(&mut self, gateway: &dyn TerminalGateway) -> Result<(), EguiError> {
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
        self.apply_auto_height(self.last_plot_width);
        Ok(())
    }

    /// Build the issue-click intent document from the ticket drafts. Drafts
    /// stay plain strings; the core alone parses them, so a bad draft
    /// refuses here with the field named and never reaches the gateway.
    fn ticket_draft(&self) -> Result<serde_json::Value, String> {
        let underlying = self.ticket_underlying.trim();
        if underlying.is_empty() {
            return Err("ticket needs an underlying".to_owned());
        }
        if self.ticket_side != "Buy" && self.ticket_side != "Sell" {
            return Err("ticket side is Buy or Sell".to_owned());
        }
        let quantity: u32 = self
            .ticket_quantity
            .trim()
            .parse()
            .map_err(|_| "ticket quantity must be whole contracts".to_owned())?;
        if quantity == 0 {
            return Err("ticket quantity must be whole contracts".to_owned());
        }
        if self.ticket_limit.trim().is_empty() {
            return Err("ticket needs a limit price".to_owned());
        }
        Ok(serde_json::json!({
            "client_id": format!("manual-{}", self.ticket_seq),
            "proposal_id": "manual",
            "strategy": "manual",
            "underlying": underlying,
            "side": self.ticket_side,
            "quantity": quantity,
            "limit_price": self.ticket_limit.trim(),
        }))
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
        // Live tier texts ride beside the form's editors (display only).
        if let Ok(tiers) = gateway.grant_tiers() {
            for (tier, text) in tiers {
                let _ = (tier, text);
            }
        }
    }

    /// Refresh the replay window from the gateway without moving the
    /// operator's cursor: a repeated read widens the window, never yanks a
    /// scrub.
    pub fn refresh_replay(&mut self, gateway: &dyn TerminalGateway) {
        if let Some(scrub) = self.replay.as_mut() {
            self.replay_error = scrub.ingest(gateway).err().map(|error| error.to_string());
        }
    }

    /// Bars the chart and indicators may read while a replay is live: the
    /// history prefix ending at the cursor. `None` is the live chart (the
    /// full store). A prefix never renumbers — indices ride untouched — so
    /// every viewport mapping and slot space stays valid.
    fn replay_prefix_len(&self) -> Option<usize> {
        self.live_replay
            .as_ref()
            .map(|live| (live.state.cursor as usize).min(self.rows.len()))
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

    /// Resolve the replay start draft: empty is the head, a bare integer is
    /// a bar index, else a `YYYY-MM-DD` date naming the first bar at or
    /// after midnight UTC. Refuses garbage, indices past the last bar, and
    /// dates past the last bar — the same refusal shape as Beginning.
    fn resolve_replay_start(&self, text: &str) -> Result<usize, EguiError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(0);
        }
        if let Ok(index) = trimmed.parse::<usize>() {
            if index > self.rows.len() {
                return Err(EguiError::InvalidParam {
                    what: "replay start".to_owned(),
                    reason: format!("bar {index} is past {} bars", self.rows.len()),
                });
            }
            return Ok(index);
        }
        let date = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d").map_err(|_| {
            EguiError::InvalidParam {
                what: "replay start".to_owned(),
                reason: format!("{trimmed:?} is a bar index or YYYY-MM-DD"),
            }
        })?;
        let start = date
            .and_hms_opt(0, 0, 0)
            .map_or(0, |dt| dt.and_utc().timestamp());
        if self.rows.last().is_some_and(|last| start > last.time) {
            return Err(EguiError::InvalidParam {
                what: "replay start".to_owned(),
                reason: "replay start is past the last bar".to_owned(),
            });
        }
        Ok(self.rows.partition_point(|row| row.time < start))
    }

    /// Resolve the replay grant draft: empty is the rehearsal grant (`None`
    /// core-side); anything else must parse as a JSON grant document the core
    /// alone validates — the same shape as the `arm` refusal, without arming.
    /// Garbage refuses here with the reason on the section, never core-side.
    fn resolve_replay_grant(text: &str) -> Result<Option<serde_json::Value>, EguiError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        serde_json::from_str(trimmed)
            .map(Some)
            .map_err(|error| EguiError::InvalidParam {
                what: "replay grant".to_owned(),
                reason: format!("grant document is not JSON: {error}"),
            })
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

    /// Open (or replace) the interactive replay session core-side, then land
    /// the chart on it: indicators recompute over the start prefix and the
    /// cursor jumps into view with the Y fit to the prefix. Rehearsal
    /// economics match the preview's own booking; an explicit grant document
    /// replays under that grant instead (parsed core-side, never armed).
    /// `structure` is the picker tag (`"Single"`/`"Vertical"`); the core
    /// alone maps it onto strategy and grant scope.
    fn open_live_replay(
        &mut self,
        gateway: &dyn TerminalGateway,
        start_bar: usize,
        seed: u64,
        grant: Option<serde_json::Value>,
        structure: String,
    ) {
        let params = ReplayStartParams {
            session_id: "terminal".to_owned(),
            underlying: self.active_symbol().to_owned(),
            strategy_name: "ribbon".to_owned(),
            max_loss: Money::from(LiveReplay::MAX_LOSS),
            notional: Money::from(LiveReplay::NOTIONAL),
            dte_days: LiveReplay::DTE_DAYS,
            warmup_bars: 0,
            seed,
            start_bar: start_bar as u64,
            structure,
            grant,
        };
        match gateway.replay_start(params) {
            Ok(state) => {
                self.live_replay = Some(LiveReplay::new("terminal".to_owned(), state));
                let _ = self.recompute();
                self.jump_to_replay_cursor();
                self.apply_auto_height_now();
            }
            Err(error) => {
                self.replay_error = Some(error.to_string());
            }
        }
    }

    /// Step the live session, then recompute indicators over the new prefix
    /// and follow the cursor when it leaves the view. A refused step stops
    /// the play loop with the reason on the section; the session stays live
    /// so the operator can retry.
    fn step_live_replay(&mut self, gateway: &dyn TerminalGateway, bars: u64) {
        let Some(live) = self.live_replay.as_mut() else {
            return;
        };
        match gateway.replay_step(&live.session_id, bars) {
            Ok(state) => {
                live.state = state;
                live.error = None;
                if live.state.cursor >= live.state.total_bars {
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
        let _ = self.recompute();
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

    /// Set the grant schema once when a descriptor is available (the terminal
    /// composition root owns the descriptor; this crate never names the
    /// trading path). Until set, the arm-grant subpane shows read-only tier
    /// texts.
    pub fn set_grant_schema(&mut self, schema: &serde_json::Value) {
        self.grant_form = GrantFormState::from_schema(schema);
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
        let window = window_for_range(&self.rows, self.range, self.timeframe.seconds());
        match self.custom_start {
            Some(start) => {
                let skip = window.iter().take_while(|row| row.time < start).count();
                &window[skip.min(window.len())..]
            }
            None => window,
        }
    }

    /// Parse `YYYY-MM-DD` and set it as the custom window start (Beginning).
    /// Clears to the default one-month window on empty input. Refuses
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
        match gateway {
            Some(gateway) => {
                let _ = self.refresh_chart(gateway, None);
            }
            None => self.rebound_from_history(),
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
        match gateway {
            Some(gateway) => {
                if self.refresh_chart(gateway, None).is_err() {
                    self.rebind_viewports()?;
                    self.restore_anchor(anchor, self.last_plot_width)?;
                    return Ok(false);
                }
            }
            None => self.rebound_from_history(),
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
        let seconds = self.timeframe.seconds().max(1) as i64;
        (crate::session::pan_unit_seconds(self.timeframe.seconds()) / seconds).max(1)
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
    pub fn switch_symbol(
        &mut self,
        symbol: &str,
        gateway: &dyn TerminalGateway,
    ) -> Result<(), EguiError> {
        let timeframe = self.timeframe;
        let size = self.target_fetch(timeframe);
        let rows = Self::fetch_rows(gateway, symbol, timeframe, size)?;
        self.apply_fetched(symbol, timeframe, rows)
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

    /// Slot one indicator plugin by catalog id; a duplicate is a no-op.
    fn slot_indicator(&mut self, id: &str) -> Result<(), EguiError> {
        if self.stack.contains(id) {
            return Ok(());
        }
        let plugin = instantiate_indicator(id)?;
        let config = plugin.default_config();
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
    /// so edge-free panes evaluate exactly as slotted.
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
        let share = self.pane_share.clamp(MIN_PANE_SHARE, MAX_PANE_SHARE);
        let price_plot = NOMINAL_CANVAS_HEIGHT * share - TIME_STRIP;
        let sub_plot = NOMINAL_CANVAS_HEIGHT * (1.0 - share);
        let main = Viewport::new(
            PriceScaleMode::Normal,
            lo,
            hi,
            price_plot,
            spacing,
            PRICE_BASE,
            false,
        )?;
        let sub = Viewport::new(
            PriceScaleMode::Normal,
            SUB_MIN,
            SUB_MAX,
            sub_plot,
            spacing,
            0.0,
            false,
        )?;
        self.main = Some(main);
        self.sub = Some(sub);
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
            let config = self
                .stack
                .config(plugin.id())
                .ok_or_else(|| EguiError::InvalidParam {
                    what: plugin.id().to_owned(),
                    reason: "slot owns no configuration".to_owned(),
                })?;
            hashes.push((
                plugin.id().to_owned(),
                plugin.config_key(config)?.to_le_bytes(),
            ));
        }
        let pairs: Vec<(&str, &[u8])> = hashes
            .iter()
            .map(|(id, hash)| (id.as_str(), &hash[..]))
            .collect();
        Ok(fingerprint_framed(&pairs))
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
        let order = self.eval_order()?;
        let mut revisions = Vec::new();
        self.series_catalog.clear();
        for index in order {
            let id = self.plugins[index].id().to_owned();
            let evaluation = {
                let config = self
                    .stack
                    .config(&id)
                    .ok_or_else(|| EguiError::InvalidParam {
                        what: id.clone(),
                        reason: "slot owns no configuration".to_owned(),
                    })?;
                self.plugins[index].evaluate(eval_rows, config, &self.series_catalog)?
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
            self.stack.set_readouts(&id, readouts)?;
            // Store the padded evaluation: painters read this through the
            // global slot space, so stored series keep full length with
            // holes past the cursor. Readouts stay prefix-native (answered
            // above, from the unpadded evaluation).
            let mut stored = evaluation;
            for series in &mut stored.series {
                series.values.resize(full_len, None);
            }
            self.evaluations.insert(id, stored);
        }
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
        if !self.stack.contains(INTEGRATION_LITE_ID) {
            return Ok(None);
        }
        let Some(config) = self.stack.config(INTEGRATION_LITE_ID) else {
            return Ok(None);
        };
        let Some(config) = component_downcast_ref::<IntegrationLiteConfig>(config) else {
            return Ok(None);
        };
        Ok(Some(integration_lite_sub_pane(
            self.replay_view_rows(&self.rows),
            config,
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
                self.symbol_modal = Some(symbol_modal_for(gateway)?);
                self.seed_favorites();
            }
            TopBarAction::OpenIndicatorSearch => {
                self.indicator_modal = Some(build_indicator_catalog()?);
                self.seed_favorites();
            }
            TopBarAction::PickedSymbol(symbol) => self.apply_symbol_pick(&symbol, gateway)?,
            TopBarAction::PickedTimeframe(timeframe) => match gateway {
                Some(gateway) => self.switch_timeframe(timeframe, gateway)?,
                None => self.select_timeframe(timeframe)?,
            },
            TopBarAction::ToggleRangeMenu => {
                self.range_menu_open = !self.range_menu_open;
                if self.range_menu_open {
                    // Seed the draft from the live window: the highlighted
                    // preset, or a Beginning text carrying the custom start.
                    self.range_draft = self.active_range();
                    self.top_menu.beginning_text =
                        self.custom_start.map(format_date_text).unwrap_or_default();
                }
            }
            TopBarAction::PickedChartType(chart_type) => self.chart_type = chart_type,
            TopBarAction::Refresh => {
                // No live feed is wired, so refresh verifies the chart range
                // only: the dock bundle and replay cannot have changed and are
                // not re-serialized here. Either way the indicators redraw and
                // the visible window refits like a rewind/forward landing.
                if let Some(gateway) = gateway {
                    self.verify_chart_range(gateway)?;
                } else {
                    self.rebind_viewports()?;
                    self.recompute()?;
                    self.apply_auto_height(self.last_plot_width);
                }
            }
            TopBarAction::RewindToBeginning => {
                self.rewind_to_beginning_view()?;
            }
            TopBarAction::Rewind => {
                self.rewind_view()?;
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

    /// Paint the interactive range dialog and apply its choice on OK. The
    /// dialog edits this workspace's drafts (preset, Beginning text, calendar
    /// cursor, jump target); nothing on the chart moves until OK, so a preset
    /// click or a calendar day never blanks the pane mid-gesture.
    fn paint_range_window(
        &mut self,
        ui: &mut egui::Ui,
        gateway: Option<&dyn TerminalGateway>,
    ) -> Result<(), EguiError> {
        if !self.range_menu_open {
            return Ok(());
        }
        let active_range = self.active_range();
        let mut open = true;
        let mut outcome = None;
        egui::Window::new("Chart range")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(crate::widgets::RANGE_DIALOG_WIDTH)
            .anchor(egui::Align2::LEFT_TOP, egui::vec2(120.0, 48.0))
            .show(ui.ctx(), |win| {
                outcome = paint_range_dialog(
                    win,
                    &mut self.range_draft,
                    &mut self.top_menu.beginning_text,
                    &mut self.top_menu.jump_text,
                    &mut self.calendar,
                    active_range,
                );
            });
        match outcome {
            Some(RangeChoice::Apply) => {
                self.range_menu_open = false;
                if self.top_menu.beginning_text.trim().is_empty() {
                    if let Some(range) = self.range_draft {
                        self.select_range(range, gateway)?;
                    }
                } else {
                    let text = self.top_menu.beginning_text.clone();
                    self.set_beginning(&text, gateway)?;
                }
            }
            Some(RangeChoice::Cancel) => {
                self.range_menu_open = false;
            }
            Some(RangeChoice::Jump) => {
                self.range_menu_open = false;
                let text = self.top_menu.jump_text.clone();
                let width = self.last_plot_width;
                self.jump_to_time(&text, width, gateway)?;
            }
            None => {}
        }
        if !open {
            self.range_menu_open = false;
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
        let mut panes = split_canvas(canvas, share);
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
            panes = split_canvas(canvas, share);
            divider = divider_rect(canvas, share);
        }
        let [main_rect, sub_rect] = panes;
        let layout = PaneLayout::split(main_rect, true);
        let sub_layout = PaneLayout::split(sub_rect, false);
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
        self.step_from_response(
            ui,
            &response,
            &[layout, sub_layout],
            divider_response.dragged(),
        );
        let bar_seconds = self.timeframe.seconds();

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
                    if needs_column_aggregation(main.scale().bar_spacing()) {
                        // One band per painted column through the shared
                        // band sampler (display-only: values untouched,
                        // holes stay holes), blitted as rects through the
                        // same rect emitter as the native strips.
                        let (bands, trends) =
                            sampled_ribbon_bands(upper, lower, bullish, &main, layout.plot.width());
                        let placed =
                            translate_bands(bands, (layout.plot.left(), layout.plot.top()));
                        paint_ribbon(&plot, &placed, 0.5, &theme, &trends, paint_alpha)?;
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
                            ),
                            (layout.plot.left(), layout.plot.top()),
                        );
                        paint_ribbon_fill(&plot, &placed, &theme, paint_alpha)?;
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
                if plugin.attachment() != SlotAttachment::MainOverlay
                    || self.stack.visible(plugin.id()) != Some(true)
                {
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
            // The hovered bar feeds the replay's start-at-crosshair control.
            self.last_hover_bar = None;
            if let Some(hover) = response.hover_pos() {
                if layout.plot.contains(hover) {
                    cross_x = paint_crosshair_overlay(
                        &plot, &chrome, main, view_rows, layout, hover, &theme,
                    )?
                    .map(|(x, index)| {
                        self.last_hover_bar = Some(index);
                        x
                    });
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
                            self.last_hover_bar = Some(data.index);
                        }
                    }
                }
            }
        }

        // The sub-pane rect and layout were allocated at the top of the frame;
        // reuse them so the oscillator paints into the region routing knows.
        let sub_chrome = ui.painter_at(sub_rect);
        sub_chrome.rect_filled(sub_rect, 0.0, background);
        let sub_plot = ui.painter_at(sub_layout.plot);
        if let Some(sub) = self.sub {
            paint_price_grid(&sub_plot, sub, sub_layout, &theme)?;
            paint_time_grid(&sub_plot, sub, view_rows, bar_seconds, sub_layout, &theme)?;
            for plugin in &self.plugins {
                if plugin.attachment() != SlotAttachment::SubPane
                    || self.stack.visible(plugin.id()) != Some(true)
                {
                    continue;
                }
                if let Some(evaluation) = self.evaluations.get(plugin.id()) {
                    let Some(config) = self.stack.config(plugin.id()) else {
                        continue;
                    };
                    // Full-history evaluations paint through the global slot
                    // space, so the base is zero: each painter culls its own
                    // visible tail through the shared samplers — no per-frame,
                    // per-plugin window copy on this path. Native legs cull to
                    // the twice-visible borrow (display-only).
                    plugin.paint_sub_pane(
                        &sub_plot,
                        evaluation,
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
                paint_delta_bars_tinted(
                    &sub_plot,
                    &tinted,
                    candle_body_width(sub.scale().bar_spacing()) * DELTA_BAR_BODY_SHARE / 2.0,
                );
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

    /// Translate this frame's pointer gestures into typed interaction inputs.
    ///
    /// The wheel acts on the region under the pointer, decided once by
    /// [`WheelZones`] so no call site re-guesses it: the candle and oscillator
    /// plots zoom the shared time axis, each pane's right-hand price strip
    /// zooms that pane's own price axis, and the floating indicator legend
    /// scrolls instead of moving the chart. Holding <kbd>Ctrl</kbd> overrides
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
        layouts: &[PaneLayout; 2],
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
                    let zones = WheelZones::new(
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
                    match zones.route(hover.into()) {
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
                        None => {}
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
        let Some(sub) = self.sub else {
            return;
        };
        if pane != SUB_PANE_ZONE {
            return;
        }
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
            self.sub = Some(view);
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
            let outcome = paint_slot_settings_modal(ui, &mut self.stack, &id, &theme)?;
            // Series picker beside the generic editors: price-source catalogue
            // plus live producer nodes, writing into the staged draft so Cancel
            // still discards and Apply commits through the live contract.
            // This is the dynamic half of the source picker (static price
            // sources already ride the schema `variants`); style colors/widths
            // ride the Material color/slider editors in the Style tab.
            self.paint_series_picker(ui, &id);
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
        }
        Ok(())
    }

    /// Series picker for a slot's `source` field: price-source catalogue plus
    /// live producer nodes (`series:<node>`). Writes into the staged draft
    /// when the settings pane is open; a closed pane refuses (the caller
    /// ignores the refusal — no source field, no picker).
    fn paint_series_picker(&mut self, ui: &mut egui::Ui, id: &str) {
        let has_source = self
            .stack
            .config(id)
            .is_some_and(|config| config.field_names().contains(&"source"));
        if !has_source {
            return;
        }
        let nodes: Vec<String> = {
            let mut nodes: Vec<String> = crate::topology::price_source_ids();
            let mut series: Vec<String> = self
                .series_catalog
                .node_names()
                .into_iter()
                .map(|node| format!("series:{node}"))
                .collect();
            series.sort();
            nodes.extend(series);
            nodes
        };
        ui.horizontal_wrapped(|row| {
            row.label("Source:");
            for node in nodes {
                if row.small_button(&node).clicked() {
                    let _ = self.stack.set_staged_field(id, "source", &node);
                }
            }
        });
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

    /// Ensure the replay window is ingested when a gateway can serve it: loads
    /// once, then holds (a refusal needs the retry button). Called inside the
    /// replay body, so the graph build never runs until that header opens.
    fn ensure_replay(&mut self, gateway: Option<&dyn TerminalGateway>) {
        let needs = self
            .replay
            .as_ref()
            .is_some_and(|scrub| scrub.window().is_none())
            && self.replay_error.is_none();
        if needs {
            if let Some(gateway) = gateway {
                self.refresh_replay(gateway);
            }
        }
    }

    fn paint_dock_contents(&mut self, ui: &mut egui::Ui, gateway: Option<&dyn TerminalGateway>) {
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
            self.ensure_dock_bundle(gateway);
            let mut decided = false;
            if let Some(bundle) = &self.dock_bundle {
                let inbox = InboxModel::of(bundle.proposals.clone());
                body.label(format!(
                    "{} actionable · {} settled",
                    inbox.actionable().len(),
                    inbox.settled().len()
                ));
                let mut actions = Vec::new();
                for proposal in inbox.actionable() {
                    body.horizontal(|ui| {
                        ui.label(format!(
                            "{} {} {} {}",
                            proposal.id, proposal.underlying, proposal.direction, proposal.state
                        ));
                        if ui.button("approve").clicked() {
                            actions.push(InboxAction::approve(&proposal.id));
                        }
                        if ui.button("reject").clicked() {
                            actions
                                .push(InboxAction::reject(&proposal.id, "rejected from terminal"));
                        }
                    });
                }
                for proposal in inbox.settled().iter().take(20) {
                    body.label(format!(
                        "{} {} {} {}",
                        proposal.id, proposal.underlying, proposal.direction, proposal.state
                    ));
                }
                if !actions.is_empty() {
                    if let Some(gateway) = gateway {
                        let mut refused = None;
                        for action in &actions {
                            if let Err(error) = action.apply(gateway) {
                                refused = Some(error.to_string());
                            }
                        }
                        match refused {
                            Some(reason) => {
                                self.dock_bundle_error =
                                    Some(format!("decision refused: {reason}"));
                            }
                            None => decided = true,
                        }
                    } else {
                        body.label("Connect a gateway to decide");
                    }
                }
            } else if let Some(error) = &self.dock_bundle_error {
                body.colored_label(egui::Color32::RED, error.clone());
                if body.button("retry").clicked() {
                    self.dock_bundle_error = None;
                }
            } else {
                body.label("No proposals pull yet");
            }
            if decided {
                if let Some(gateway) = gateway {
                    self.refresh_dock_bundle(gateway);
                }
            }
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
                row.text_edit_singleline(&mut self.ticket_side);
                row.text_edit_singleline(&mut self.ticket_quantity);
                row.text_edit_singleline(&mut self.ticket_limit);
                if row.button("issue").clicked() {
                    match self.ticket_draft() {
                        Ok(ticket) => {
                            self.ticket_seq += 1;
                            actions.push(TicketAction::issue(ticket));
                        }
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
        // Arm grant pane.
        ui.collapsing("arm grant", |body| {
            if let Some(gateway) = gateway {
                paint_grant_form(body, &mut self.grant_form, gateway);
            } else {
                body.label("Connect a gateway to arm");
            }
        });
        // Replay: an interactive backtest over the shared decide path. The
        // gateway owns the session (decisions, fills, journal); this owns
        // the controls and the cached state the chart and panes truncate
        // to. Singles fill honestly; spreads fill when the window's chains
        // cover the bar and refuse with the feed named otherwise.
        ui.collapsing("replay", |body| {
            self.ensure_replay(gateway);
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
            body.horizontal(|row| {
                row.label("start");
                row.text_edit_singleline(&mut self.replay_start_draft);
                row.label("seed");
                row.text_edit_singleline(&mut self.replay_seed_draft);
                for tag in ["Single", "Vertical"] {
                    if row
                        .selectable_label(self.replay_structure_draft == tag, tag)
                        .clicked()
                    {
                        tag.clone_into(&mut self.replay_structure_draft);
                    }
                }
                if row.button("at crosshair").clicked() {
                    if let Some(index) = self.last_hover_bar {
                        self.replay_start_draft = index.to_string();
                    }
                }
                if row.button("start").clicked() {
                    if let Some(gateway) = gateway {
                        let draft = self.replay_start_draft.clone();
                        let seed_draft = self.replay_seed_draft.clone();
                        let grant_draft = self.replay_grant_draft.clone();
                        let structure = self.replay_structure_draft.clone();
                        let parsed = self.resolve_replay_start(&draft).and_then(|start| {
                            seed_draft.trim().parse::<u64>().map_or_else(
                                |_| {
                                    Err(EguiError::InvalidParam {
                                        what: "replay seed".to_owned(),
                                        reason: format!(
                                            "{:?} is a bare integer",
                                            seed_draft.trim()
                                        ),
                                    })
                                },
                                |seed| Ok((start, seed)),
                            )
                        });
                        let parsed = parsed.and_then(|(start, seed)| {
                            Self::resolve_replay_grant(&grant_draft)
                                .map(|grant| (start, seed, grant))
                        });
                        match parsed {
                            Ok((start, seed, grant)) => {
                                self.open_live_replay(gateway, start, seed, grant, structure);
                            }
                            Err(error) => {
                                self.replay_error = Some(error.to_string());
                            }
                        }
                    }
                }
            });
            body.horizontal(|row| {
                row.label("grant JSON (empty = rehearsal)");
                row.text_edit_singleline(&mut self.replay_grant_draft);
                if row.button("clear").clicked() {
                    self.replay_grant_draft.clear();
                }
            });
            if self.live_replay.is_some() {
                let (playing, rate) = self
                    .live_replay
                    .as_ref()
                    .map_or((false, 5), |live| (live.playing, live.rate_bars_per_sec));
                body.horizontal(|row| {
                    if row.button("step 1").clicked() {
                        if let Some(gateway) = gateway {
                            self.step_live_replay(gateway, 1);
                        }
                    }
                    if row.button("step 10").clicked() {
                        if let Some(gateway) = gateway {
                            self.step_live_replay(gateway, 10);
                        }
                    }
                    if row.button(if playing { "pause" } else { "play" }).clicked() {
                        if let Some(live) = self.live_replay.as_mut() {
                            live.playing = !live.playing;
                            live.last_tick = Some(std::time::Instant::now());
                        }
                    }
                    for candidate in [1_u64, 5, 20] {
                        if row
                            .selectable_label(
                                rate == candidate,
                                format!("{candidate}/s"),
                            )
                            .clicked()
                        {
                            if let Some(live) = self.live_replay.as_mut() {
                                live.rate_bars_per_sec = candidate;
                            }
                        }
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
                body.label("rehearsal economics (400/2000/30d) unless a grant document is supplied; singles fill, spreads refuse.");
                // Play loop: pace steps to the selected rate while the
                // section paints, and keep frames coming until paused or the
                // head is reached. Closed sections never advance.
                let advance = self.live_replay.as_ref().and_then(|live| {
                    if !live.playing {
                        return None;
                    }
                    let elapsed = live.last_tick.map_or(0.0, |tick| {
                        std::time::Instant::now().duration_since(tick).as_secs_f64()
                    });
                    let due = (elapsed * live.rate_bars_per_sec as f64).floor() as u64;
                    (due >= 1).then(|| due.min(256))
                });
                if let Some(bars) = advance {
                    if let Some(gateway) = gateway {
                        if let Some(live) = self.live_replay.as_mut() {
                            live.last_tick = Some(std::time::Instant::now());
                        }
                        self.step_live_replay(gateway, bars);
                    }
                }
                if self.live_replay.as_ref().is_some_and(|live| live.playing) {
                    body.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
            if let Some(scrub) = self.replay.as_ref() {
                match scrub.window() {
                    Some(window) => {
                        body.label(format!(
                            "window: {} bars, head at {}",
                            window.total_bars, window.cursor
                        ));
                    }
                    None => {
                        body.label("No replay window yet");
                    }
                }
            } else {
                body.label("No replay window yet");
            }
            if gateway.is_some() && body.button("refresh replay").clicked() {
                if let Some(gateway) = gateway {
                    self.refresh_replay(gateway);
                }
            }
        });
    }

    /// Snapshot the persisted terminal state: the session selection, the
    /// chart's non-default settings, the starred entries, and the indicator
    /// loadout. The composition root writes it to `protopyne.json`.
    #[must_use]
    pub fn prefs(&self) -> TerminalPrefs {
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
        TerminalPrefs {
            symbol: Some(self.active_symbol().to_owned()),
            timeframe: Some(self.timeframe.label().to_owned()),
            range: self
                .custom_start
                .is_none()
                .then(|| self.range.label().to_owned()),
            chart_type: Some(self.chart_type.name().to_owned()),
            theme: Some(self.config.theme.clone()),
            pane_share: Some(self.pane_share),
            dock_open: Some(self.dock_open),
            favorites: Favorites {
                symbols: self.favorite_symbols.iter().cloned().collect(),
                indicators: self.favorite_indicators.iter().cloned().collect(),
            },
            slots,
        }
    }

    /// Apply a loaded [`TerminalPrefs`]: restore the session selection, the
    /// chart settings, the starred entries, and the indicator loadout. Every
    /// field is best-effort — an unknown timeframe or indicator id is skipped
    /// rather than refusing the whole document — so a partial or older file
    /// never strands the terminal. Favorites and loadout then rebind and
    /// recompute once.
    pub fn apply_prefs(&mut self, prefs: &TerminalPrefs) -> Result<(), EguiError> {
        if let Some(symbol) = prefs
            .symbol
            .as_deref()
            .filter(|symbol| !symbol.trim().is_empty())
        {
            self.set_symbol(symbol)?;
        }
        if let Some(label) = prefs.timeframe.as_deref() {
            if let Some(key) = TimeframeKey::all()
                .into_iter()
                .find(|key| key.label() == label)
            {
                self.timeframe = key;
            }
        }
        if let Some(label) = prefs.range.as_deref() {
            if let Some(range) = DateRange::from_label(label) {
                self.range = range;
                self.custom_start = None;
            }
        }
        if let Some(name) = prefs.chart_type.as_deref() {
            if let Some(kind) = ChartType::all()
                .iter()
                .copied()
                .find(|kind| kind.name() == name)
            {
                self.chart_type = kind;
            }
        }
        if let Some(theme) = prefs
            .theme
            .as_deref()
            .filter(|theme| !theme.trim().is_empty())
        {
            self.config.theme.clone_from(&theme.to_owned());
        }
        if let Some(share) = prefs.pane_share {
            self.set_pane_share(share);
        }
        if let Some(dock) = prefs.dock_open {
            self.dock_open = dock;
        }
        self.favorite_symbols = prefs.favorites.symbols.iter().cloned().collect();
        self.favorite_indicators = prefs.favorites.indicators.iter().cloned().collect();
        self.apply_slot_prefs(&prefs.slots)?;
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

    /// Seed each open catalog's favorite flags from this workspace's starred
    /// sets, so a reopened modal shows the persisted favorites, not a blank
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
                                match action {
                                    CatalogAction::Picked(id) => picked_symbol = Some(id),
                                    // The star toggles the entry itself; the
                                    // modal owns the filter, the workspace
                                    // owns the persisted set.
                                    CatalogAction::ToggledFavorite(id) => {
                                        let _ = modal.toggle_favorite(&id);
                                    }
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
        if let Some(modal) = self.symbol_modal.as_ref() {
            self.favorite_symbols = modal
                .ids()
                .into_iter()
                .filter(|id| modal.is_favorite(id) == Some(true))
                .collect();
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
                                match action {
                                    CatalogAction::Picked(id) => picked_indicator = Some(id),
                                    CatalogAction::ToggledFavorite(id) => {
                                        let _ = modal.toggle_favorite(&id);
                                    }
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
        if let Some(modal) = self.indicator_modal.as_ref() {
            self.favorite_indicators = modal
                .ids()
                .into_iter()
                .filter(|id| modal.is_favorite(id) == Some(true))
                .collect();
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

/// Paint the full terminal: Material top bar, full-window multi-pane canvas
/// with the upper-left hot-slot legend, inline settings pane with series
/// pickers, floating left-boundary dock drawer, and the symbol and indicator
/// modals.
///
/// Layout (Material, workstation + touch):
/// - topmost app bar spans the full width (dock toggle far left, ticker,
///   indicator selector, timeframe selector, display-mode pulldown, the
///   `Range` button that opens the interactive range dialog, refresh button,
///   and the label-free pan cluster (rewind-to-beginning, rewind, forward,
///   forward-to-last));
/// - below it, the chart canvas takes the whole window: price pane plus
///   oscillator pane with their divider, full remaining width and height;
/// - the indicator hotlist floats upper-left over the chart pane with eye /
///   bolt / X controls (X unloads immediately);
/// - the dock, when toggled, floats as a left-boundary drawer window below
///   the app bar over the chart — never a side pane, so the panes never
///   shrink.
///
/// The gateway is optional: a workspace seeded with rows (the demo shell)
/// paints without one, and a symbol or timeframe pick can only pull fresh
/// history when a gateway is supplied. The dock shows cached health,
/// portfolio, proposals, journals, arm-grant, and replay sections; refresh
/// lives on the menu bar.
pub fn paint_workspace(
    ui: &mut egui::Ui,
    workspace: &mut ChartWorkspace,
    gateway: Option<&dyn TerminalGateway>,
) -> Result<(), EguiError> {
    // 1. Material app bar (full width, above everything).
    let top_action = {
        let symbol = workspace.active_symbol().to_owned();
        let timeframe = workspace.timeframe;
        let range_label = workspace.range_label();
        let chart_type = workspace.chart_type;
        let dock_open = workspace.dock_open;
        // Material app-bar frame: subtle elevation via stroke + rounding.
        egui::Frame::new()
            .fill(ui.visuals().panel_fill)
            .inner_margin(4.0)
            .outer_margin(0.0)
            .show(ui, |bar| {
                paint_top_bar(bar, &symbol, timeframe, &range_label, chart_type, dock_open)
            })
            .inner
    };
    if let Some(action) = top_action {
        workspace.apply_top_bar_action(action, gateway)?;
    }
    // 1b. Interactive range dialog (opened by the Range button), applied on OK.
    workspace.paint_range_window(ui, gateway)?;
    ui.separator();
    // 2. Chart canvas takes the whole window below the menu bar: full
    // remaining width and height, price pane plus oscillator pane with their
    // divider. The dock never participates in this layout.
    {
        let _ = workspace.paint_main_column(ui, gateway);
    }
    // 3. Floating dock drawer along the left boundary below the app bar.
    // Overlay only: it draws over the chart via the context, so the panes
    // keep the full window whether it is open or closed. Full-height
    // drawer styling — narrow, left-anchored, movable/resizable.
    if workspace.dock_open {
        let mut open = true;
        egui::Window::new("Dock")
            .open(&mut open)
            .anchor(egui::Align2::LEFT_TOP, egui::vec2(8.0, 48.0))
            .default_size(egui::vec2(300.0, 480.0))
            .min_size(egui::vec2(260.0, 200.0))
            .collapsible(false)
            .resizable(true)
            .show(ui.ctx(), |dock_ui| {
                workspace.paint_dock_contents(dock_ui, gateway);
            });
        if !open {
            workspace.dock_open = false;
        }
    }
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
