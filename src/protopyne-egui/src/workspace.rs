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
    CatalogAction, CatalogModal, CrosshairLines, EguiError, HotSlotAction, HotSlotStack,
    InteractionState, PaneCache, PanePlan, PaneZones, PointerInput,
    SettingsCommit, SettingsTab, SlotAttachment, ThemeConfig, TintedCandle, TintedDeltaBar, Viewport,
    WheelTarget, WheelZones, axis_ticks, candle_body_width, column_candle_geoms, column_delta_geoms,
    crosshair_data, crosshair_lines, needs_column_aggregation, opacity_from_transparency,
    paint_candles_tinted, paint_catalog_modal, paint_crosshair,
    paint_delta_bars_tinted, paint_hot_slot_stack, paint_oscillator, paint_ribbon, paint_ribbon_fill,
    paint_slot_settings_modal, proportional_ribbon_polygons, resolve_color, sampled_line_points,
    sampled_ribbon_bands, translate_bands, translate_breaks, translate_candles, translate_delta_bars,
    translate_points, translate_polygons, visible_rows, wheel_zoom_factor, zoom_range_around,
};
use fluent_wvr::{FieldAccess, component_downcast_ref};
use protopyne_chart_model::{ChartRow, ChartType, DateRange};
use protopyne_chart_scale::PriceScaleMode;
use protopyne_types::{BucketAccum, SymbolIndex, SymKey, Timeframe};
use protopyne_view::{ChartBars, TerminalGateway};

use crate::chart::{ChartConfig, DEFAULT_SYMBOL, TimeframeKey, line_values, window_for_range};
use crate::indicators::{
    ChartIndicator, IndicatorCategory, IndicatorEvaluation, build_indicator_catalog,
    instantiate_indicator,
};
use crate::integration_lite::{
    IntegrationLiteConfig, IntegrationLiteSubPane, MomentumPalette, MomentumTint,
    candle_momentum_keys, candle_momentum_series, integration_lite_sub_pane, momentum_key_for,
    momentum_tints, ribbon_fill_transparency,
};
use crate::session::{labels_show_clock_time, time_grid_lines};
use crate::widgets::{TopBarAction, paint_top_bar};

/// Stable id of the IntegrationLite slot whose richer sub-pane geometry
/// (delta histogram) the canvas paints alongside the plugin's series.
const INTEGRATION_LITE_ID: &str = "integration_lite";

/// Pixels per bar used when no configured spacing applies.
const DEFAULT_BAR_SPACING: f32 = 8.0;
/// Trailing-bar cap for one chart read. A **cost bound** — the pane paints a
/// trailing window, so a five-thousand-bar ceiling fills every range preset
/// without asking the read to scan a whole tape. A gateway with no durable
/// tape ignores it; one that plans a store read narrows to it.
const CHART_WINDOW_BARS: usize = 5_000;
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
const MIN_TIME_LABEL_GAP: f32 = 64.0;
/// Width of the crosshair time tag on the time strip.
const TIME_TAG_WIDTH: f32 = 56.0;
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
    let mut folded = Vec::new();
    let mut open: Option<(i64, ChartAccum)> = None;
    for row in rows {
        let start_ms = timeframe.bucket_start_ms(row.time.saturating_mul(1_000));
        let start = start_ms / 1_000;
        match open.as_mut() {
            Some((held, accum)) if *held == start => accum.absorb_row(row),
            _ => {
                if let Some((held, accum)) = open.take() {
                    if let Some(folded_row) = accum.finish(held, step_ms) {
                        folded.push(folded_row);
                    }
                }
                open = Some((start, ChartAccum::first(start_ms, row)));
            }
        }
    }
    if let Some((held, accum)) = open.take() {
        if let Some(folded_row) = accum.finish(held, step_ms) {
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

    /// Emit the folded row stamped at `start_sec` when the absorbed rows
    /// tile the `step_ms` bucket whole, else `None`.
    fn finish(self, start_sec: i64, step_ms: i64) -> Option<ChartRow> {
        if !self.complete.is_complete(step_ms) {
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
        let plot_bottom =
            if time_strip { (rect.bottom() - TIME_STRIP).max(rect.top()) } else { rect.bottom() };
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
    let split_y =
        (canvas.top() + canvas.height() * share).clamp(canvas.top(), canvas.bottom());
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
/// right, clear of the right price axis, with a small inset. The origin is
/// set before gestures are routed so wheel-over-legend hits the legend this
/// frame, not last frame's position.
fn legend_origin(chart: egui::Rect, card_width: f32) -> (f32, f32) {
    let x = (chart.right() - AXIS_WIDTH - card_width - 8.0).max(chart.left() + 8.0);
    (x, chart.top() + 8.0)
}

/// Format an epoch timestamp for the time axis: clock time on a sub-daily
/// timeframe, month-day otherwise.
fn fmt_axis_time(epoch: i64, intraday: bool) -> String {
    match chrono::DateTime::from_timestamp(epoch, 0) {
        Some(moment) if intraday => moment.format("%H:%M").to_string(),
        Some(moment) => moment.format("%m-%d").to_string(),
        None => String::new(),
    }
}

/// The dimmed, thin stroke every background gridline draws with, so the
/// horizontal and vertical rules stay visually subordinate to the candles.
fn grid_stroke(theme: &ThemeConfig) -> Result<egui::Stroke, EguiError> {
    Ok(egui::Stroke::new(GRID_STROKE_WIDTH, resolve_color(theme, "grid")?.gamma_multiply(GRID_DIM)))
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
                [egui::pos2(layout.plot.left(), y), egui::pos2(layout.plot.right(), y)],
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
    for line in translate_breaks(time_grid_lines(&viewport, rows, bar_seconds), layout.plot.left()) {
        painter.line_segment(
            [egui::pos2(line.x, layout.plot.top()), egui::pos2(line.x, layout.plot.bottom())],
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
                format!("{price:.2}"),
                egui::FontId::proportional(AXIS_FONT),
                egui::Color32::WHITE,
            );
        }
    }
    Ok(())
}

/// Paint thinned time labels along the bottom strip, one per major boundary,
/// centered on the boundary's vertical gridline. The label granularity
/// follows the chart timeframe through [`labels_show_clock_time`] — any
/// sub-daily timeframe reads hour and minute — never the window span.
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
    let clock_time = labels_show_clock_time(bar_seconds);
    let mut last_x = f32::NEG_INFINITY;
    for line in translate_breaks(time_grid_lines(&viewport, rows, bar_seconds), layout.plot.left()) {
        let x = line.x;
        if x < strip.left() + TIME_TAG_WIDTH / 2.0
            || x > strip.right() - TIME_TAG_WIDTH / 2.0
            || x - last_x < MIN_TIME_LABEL_GAP
        {
            continue;
        }
        last_x = x;
        if let Some(row) = rows.get(line.index) {
            painter.text(
                egui::pos2(x, strip.center().y),
                egui::Align2::CENTER_CENTER,
                fmt_axis_time(row.time, clock_time),
                egui::FontId::proportional(AXIS_FONT),
                ink,
            );
        }
    }
    Ok(())
}

/// Paint the hover crosshair: snapped vertical/horizontal lines plus the
/// price tag on the axis and the time tag on the strip. The time tag shares
/// the axis granularity ([`labels_show_clock_time`]), so the hovered readout
/// and the strip labels can never disagree.
fn paint_crosshair_overlay(
    plot_painter: &egui::Painter,
    chrome_painter: &egui::Painter,
    viewport: Viewport,
    rows: &[ChartRow],
    bar_seconds: u64,
    layout: PaneLayout,
    hover: egui::Pos2,
    theme: &ThemeConfig,
) -> Result<(), EguiError> {
    let local = (hover.x - layout.plot.left(), hover.y - layout.plot.top());
    let Some(data) = crosshair_data(local, &viewport, rows.len()) else {
        return Ok(());
    };
    let Some(span) = crosshair_lines(&data, &viewport, layout.plot.width(), layout.plot.height())
    else {
        return Ok(());
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
        format!("{:.2}", data.price),
        egui::FontId::proportional(AXIS_FONT),
        egui::Color32::WHITE,
    );
    if let (Some(strip), Some(row)) = (layout.time, rows.get(data.index)) {
        let half = TIME_TAG_WIDTH / 2.0;
        let tag_x = span
            .x
            .clamp(layout.plot.left() + half, layout.plot.right() - half);
        let time_rect = egui::Rect::from_center_size(
            egui::pos2(tag_x, strip.center().y),
            egui::vec2(TIME_TAG_WIDTH, strip.height() - 4.0),
        );
        chrome_painter.rect_filled(time_rect, 3.0, tag);
        chrome_painter.text(
            time_rect.center(),
            egui::Align2::CENTER_CENTER,
            fmt_axis_time(row.time, labels_show_clock_time(bar_seconds)),
            egui::FontId::proportional(AXIS_FONT),
            egui::Color32::WHITE,
        );
    }
    Ok(())
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
    /// Derived-pane builds actually executed: incremented only when the
    /// published series revisions (or the rows shape) moved and the
    /// sub-pane, momentum keys, and tints were rebuilt. Identical
    /// republishes backdate every revision and skip the rebuild, so this
    /// counts work done, never frames seen.
    derived_builds: u64,
    /// Last built series revisions, as sorted `(key, revision)` pairs.
    last_series_revisions: Vec<(u64, u64)>,
    /// Last built rows shape: length plus first/last bar times.
    last_rows_shape: (usize, Option<i64>, Option<i64>),
    rows: Vec<ChartRow>,
    histories: HashMap<TimeframeKey, Vec<ChartRow>>,
    /// Share of the canvas height the price pane takes; the oscillator takes
    /// the rest. Owned here so the divider drag, the canvas split, and the
    /// viewport binds all read one truth.
    pane_share: f32,
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
            derived_builds: 0,
            last_series_revisions: Vec::new(),
            last_rows_shape: (0, None, None),
            rows: Vec::new(),
            histories: HashMap::new(),
            pane_share: DEFAULT_PANE_SHARE,
            interaction: None,
            main: None,
            sub: None,
            symbol_modal: None,
            indicator_modal: None,
        };
        workspace.slot_indicator(INTEGRATION_LITE_ID)?;
        workspace.slot_indicator("fast_mas")?;
        workspace.slot_indicator(crate::delta_volume_profile::DELTA_VOLUME_PROFILE_ID)?;
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
        // Prune the removed evaluation's keys from the revision snapshot so
        // the next recompute compares against what is actually cached — the
        // direct rebuild below counts, like every derived build.
        if let Some(evaluation) = self.evaluations.remove(id) {
            for series in &evaluation.series {
                self.last_series_revisions.retain(|(key, _)| *key != series.key);
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
        Ok(())
    }

    /// Pull fresh bars for the active symbol and timeframe through `gateway`,
    /// rebind the viewports, and recompute every active indicator.
    pub fn refresh(&mut self, gateway: &dyn TerminalGateway) -> Result<(), EguiError> {
        // A bounded read: a store-backed gateway plans a trailing window
        // through the durable tape, a store-less one serves its own history
        // and ignores the cap. The call is the same on both, so switching a
        // symbol or a timeframe replays through the identical read.
        let bars = gateway
            .chart_bars_for_limit(
                self.active_symbol(),
                self.timeframe.label(),
                Some(CHART_WINDOW_BARS),
            )
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
    /// the current rows and range. Heights follow the pane share over the
    /// nominal canvas (the price plot minus its time strip); the first
    /// painted frame resyncs both to the measured window.
    fn rebind_viewports(&mut self) -> Result<(), EguiError> {
        let window = window_for_range(&self.rows, self.range, self.timeframe.seconds());
        let (lo, hi) = price_extent(window);
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
    /// for the canvas — unless every republish backdated. The cache answers
    /// whether content moved (identical series keep their revisions), so the
    /// per-bar derived vectors rebuild only on change; evaluations and
    /// readouts still refresh, so the legend never goes stale on a skip.
    fn recompute(&mut self) -> Result<(), EguiError> {
        if self.rows.is_empty() {
            self.evaluations.clear();
            self.sub_pane = None;
            self.momentum_keys.clear();
            self.tints.clear();
            self.last_series_revisions.clear();
            self.last_rows_shape = (0, None, None);
            return Ok(());
        }
        let mut revisions = Vec::new();
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
                revisions.push((series.key, self.cache.publish(series.key, series.values.clone())));
            }
            let readouts = self.plugins[index].readouts(&evaluation);
            self.stack.set_readouts(&id, readouts)?;
            self.evaluations.insert(id, evaluation);
        }
        revisions.sort_unstable();
        let rows_shape =
            (self.rows.len(), self.rows.first().map(|row| row.time), self.rows.last().map(|row| row.time));
        if revisions == self.last_series_revisions && rows_shape == self.last_rows_shape {
            return Ok(());
        }
        self.last_series_revisions = revisions;
        self.last_rows_shape = rows_shape;
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
    /// machine from this frame's pointer gestures first. The canvas fills
    /// the window: full available width and full remaining height, split
    /// between the panes at the divider share.
    fn paint_canvas(&mut self, ui: &mut egui::Ui) -> Result<(), EguiError> {
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
        // Float the legend over the chart's upper-right before routing, so a
        // wheel already sitting over it scrolls the list this frame.
        let legend = legend_origin(main_rect, self.stack.card_width());
        self.stack.set_origin(legend);
        self.step_from_response(ui, &response, &[layout, sub_layout], divider_response.dragged());
        let bar_seconds = self.timeframe.seconds();

        let theme = self.theme.clone();
        let background = resolve_color(&theme, "background")?;
        // One trailing range window for every layer (candles, overlays, and
        // the sub-pane), so the price pane and the oscillator share a time
        // axis instead of one showing the newest and the other the oldest bars.
        let window = window_for_range(&self.rows, self.range, self.timeframe.seconds());
        let window_start = self.rows.len().saturating_sub(window.len());

        let chrome = ui.painter_at(main_rect);
        chrome.rect_filled(main_rect, 0.0, background);
        let plot = ui.painter_at(layout.plot);
        if let Some(main) = self.main {
            paint_price_grid(&plot, main, layout, &theme)?;
            paint_time_grid(&plot, main, window, bar_seconds, layout, &theme)?;
            // The trend ribbon (IntegrationLite's main-pane leg) and the
            // candle tint both read the same borrowed trend slice: the legs
            // line up with the full history, so the trailing range borrows
            // through the shared slicer — no per-frame window copy.
            let ribbon_window = self.sub_pane.as_ref().map(|pane| {
                let len = window.len();
                (
                    visible_rows(pane.ribbon.upper.as_slice(), window_start, len),
                    visible_rows(pane.ribbon.lower.as_slice(), window_start, len),
                    visible_rows(pane.ribbon.bullish.as_slice(), window_start, len),
                    pane.ribbon.alpha,
                    pane.show_ribbon,
                )
            });
            // Candles need a trend key per bar; the ribbon supplies it, and
            // a ribbon-less workspace reads every bar as bearish. The
            // fallback allocates only when no ribbon exists (the default
            // workspace always carries one, so the common frame borrows).
            let fallback_trend =
                if ribbon_window.is_none() { vec![false; window.len()] } else { Vec::new() };
            let trend: &[bool] = match &ribbon_window {
                Some((_, _, bullish, _, _)) => bullish,
                None => &fallback_trend,
            };
            if let Some((upper, lower, bullish, alpha, show)) = &ribbon_window {
                if *show
                    && self.sub_pane.as_ref().is_some_and(|pane| pane.show_ribbon)
                    && self.stack.visible(INTEGRATION_LITE_ID) == Some(true)
                {
                    let paint_alpha = f32::from(opacity_from_transparency(
                        ribbon_fill_transparency(*alpha),
                    )) / 255.0;
                    if needs_column_aggregation(main.scale().bar_spacing()) {
                        // One band per painted column through the shared
                        // band sampler (display-only: values untouched,
                        // holes stay holes), blitted as rects through the
                        // same rect emitter as the native strips.
                        let (bands, trends) =
                            sampled_ribbon_bands(upper, lower, bullish, &main, layout.plot.width());
                        let placed = translate_bands(
                            bands,
                            (layout.plot.left(), layout.plot.top()),
                        );
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
            if self.chart_type == ChartType::Candles {
                // Candles dispatch by pixel columns: native geometry above
                // one pixel per bar, one folded candle per screen column
                // below it. Rows arrive transformed at native resolution
                // first (bar-backed pictures convert before windowing), so
                // the fold only ever samples display pixels, never feeds a
                // recursive transform.
                let geoms = column_candle_geoms(window, trend, &main, layout.plot.width())?;
                let placed = translate_candles(geoms, (layout.plot.left(), layout.plot.top()));
                // Per-column momentum keys from the same borrowed trend
                // slice: one single-bar lookup per painted candle, never a
                // fresh window `Vec` — identical keys to the windowed fold
                // through the one single-bar core.
                let mut candles = Vec::with_capacity(placed.len());
                for geom in &placed {
                    let key = match (window.get(geom.index), trend.get(geom.index)) {
                        (Some(row), Some(bull)) => momentum_key_for(row.open, row.close, *bull),
                        _ => "bull",
                    };
                    let color = self.bar_color(window_start + geom.index, key, &theme)?;
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
                // Culled to the viewport window first (borrowed, no copy),
                // then one vertex per column through the shared line sampler
                // at sub-pixel zoom (display-only) or the native mapping
                // untouched above it — same pixels the full window would
                // paint, off-pane shapes never built.
                let (cstart, cend) = main.culled_bar_range(layout.plot.width(), window.len());
                let cwindow = visible_rows(window, cstart, cend.saturating_sub(cstart));
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
                    // Full-history evaluations borrow the trailing window
                    // through the shared slicer inside each painter — no
                    // per-frame, per-plugin window copy on this path. Native
                    // legs cull to the twice-visible borrow (display-only).
                    plugin.paint_main_overlay(
                        &plot,
                        evaluation,
                        &main,
                        layout.plot.left_top(),
                        window_start,
                        layout.plot.width(),
                        &theme,
                        config,
                        &self.stack.indicator_style(plugin.id()),
                    )?;
                }
            }
            let last = window.last();
            paint_price_axis(
                &chrome,
                main,
                layout,
                last.map(|bar| (bar.close, bar.close >= bar.open)),
                &theme,
            )?;
            paint_time_axis(&chrome, main, window, bar_seconds, layout, &theme)?;
            if let Some(hover) = response.hover_pos() {
                if layout.plot.contains(hover) {
                    paint_crosshair_overlay(
                        &plot, &chrome, main, window, bar_seconds, layout, hover, &theme,
                    )?;
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
            paint_time_grid(&sub_plot, sub, window, bar_seconds, sub_layout, &theme)?;
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
                    // Full-history evaluations borrow the trailing window
                    // through the shared slicer inside each painter — no
                    // per-frame, per-plugin window copy on this path. Native
                    // legs cull to the twice-visible borrow (display-only).
                    plugin.paint_sub_pane(
                        &sub_plot,
                        evaluation,
                        &sub,
                        sub_layout.plot.left_top(),
                        window_start,
                        sub_layout.plot.width(),
                        &theme,
                        config,
                        &self.stack.indicator_style(plugin.id()),
                    )?;
                }
            }
            if let Some(pane) = &self.sub_pane {
                // The delta histogram borrows its trailing window and
                // dispatches by pixel columns exactly as the candles do:
                // native bars above one pixel per bar, one folded bar per
                // screen column below it — display density only, values
                // untouched, holes staying holes per leg.
                let len = window.len();
                let buy = visible_rows(pane.delta_volume.buy_top.as_slice(), window_start, len);
                let sell =
                    visible_rows(pane.delta_volume.sell_bottom.as_slice(), window_start, len);
                let bars = column_delta_geoms(buy, sell, &sub, sub_layout.plot.width())?;
                let placed =
                    translate_delta_bars(bars, (sub_layout.plot.left(), sub_layout.plot.top()));
                let mut tinted = Vec::with_capacity(placed.len());
                for geom in &placed {
                    let global = window_start + geom.index;
                    let key = self.momentum_keys.get(global).copied().unwrap_or("bull");
                    let color = self.bar_color(global, key, &theme)?;
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
                events.push(PointerInput::Pan2D { dx_px: drag.x, dy_px: drag.y });
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
                        Some(WheelTarget::Price { pane: PRICE_PANE_ZONE, anchor_y }) => {
                            events.push(PointerInput::Wheel { delta_px: scroll.y, anchor_y });
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
        let Some(next) = zoom_range_around(scale.range(), anchor, wheel_zoom_factor(delta_px))
        else {
            return;
        };
        let base_span = SUB_MAX - SUB_MIN;
        let span = (next.max - next.min).clamp(
            base_span * SUB_MIN_SPAN_FRACTION,
            base_span * SUB_MAX_SPAN_FRACTION,
        );
        let mid = f64::midpoint(next.min, next.max);
        let (rebuilt, offset) = (
            Viewport::new(
                scale.mode(),
                mid - span / 2.0,
                mid + span / 2.0,
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
        let outcome = paint_slot_settings_modal(ui, &mut workspace.stack, &id, &theme)?;
        match outcome.commit {
            SettingsCommit::Apply => {
                workspace.commit_slot_settings(&id)?;
            }
            SettingsCommit::Ok => {
                workspace.commit_slot_settings(&id)?;
                let _ = workspace.stack.close_settings(&id);
            }
            SettingsCommit::Cancel => {
                let _ = workspace.stack.discard_settings(&id);
                let _ = workspace.stack.close_settings(&id);
            }
            SettingsCommit::None => {}
        }
    }

    workspace.paint_modals(ui, gateway)?;
    Ok(())
}

#[cfg(test)]
#[path = "../tests/workspace.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/workspace_tape.rs"]
mod workspace_tape_tests;
