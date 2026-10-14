//! Native terminal demo shell over the headless chart stack.
//!
//! A TradingView-flavoured terminal: symbol + timeframe toolbar (`1M` `5M`
//! `15M` `1H` `4H`), range presets, a price pane (per-bar momentum-gradient
//! candles with conviction transparency via the ported
//! `getColorMomentumAlpha`, shaded ribbon area, volume strip, session grid,
//! right price axis with a last-price tag, OHLC legend, hover crosshair,
//! drag-pan and ctrl+wheel-zoom through the pure interaction machine), the
//! zero-centered oscillator pane (fast/slow, threshold rules, right axis),
//! and the multi-timeframe table. No decision logic lives here: every number
//! on screen comes from the headless kernels and views; this file only maps
//! them onto pixels.
//!
//! History slots: `1M`/`5M`/`15M` load their native CSV fixtures; `1H`/`4H`
//! resample the `15M` slot upward (×4/×16, trailing-aligned) through plain
//! OHLC aggregation in this shell. `PROTOPYNE_GUI_HISTORY=<csv>` (or the
//! positional arg) replaces the `15M` slot. The volume strip shows raw bar
//! totals only: buy/sell splits arrive precomputed upstream, and this shell
//! fabricates none. A Pine overlay script runs over the aligned window bars
//! and paints its projected plots/labels on the price pane; a refused
//! projection renders as an "unsupported" chip, never a partial overlay.
//!
//! Usage: `cargo run -p protopyne-egui --features demo --example demo --
//! [HISTORY.csv]`, or pass `--check` to run the headless data path without
//! opening a window (loads every slot, builds every pane, prints a summary).

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use fluent_egui::{
    AnnotationSet, BreakLine, CandleGeom, EguiError, InteractionState, MarkerDot, PivotDot,
    PointerInput, RibbonPolygon, RibbonView, ThemeConfig, TintedCandle, Viewport, annotate_at,
    area_polygons, axis_ticks, candle_geoms, crosshair_data, crosshair_pixel, hit_test, marker_dots,
    opacity_from_transparency, oscillator_points, paint_area_fill, paint_candles_tinted,
    paint_error_chip, paint_markers, paint_oscillator, paint_ribbon_fill, paint_session_breaks,
    pivot_dots, pivot_price_dots, resolve_color, ribbon_polygons,
};
use protopyne_chart_model::{Annotation, ChartRow, ChartType, DailySessionProvider, DateRange, rows_of};
use protopyne_chart_scale::{PriceScaleMode, Tick};
use protopyne_egui::{
    IntegrationLiteView, MomentumPalette, MomentumTint, MtfRow, OSCILLATOR_BRIGHTNESS, OverlayGeoms,
    TimeframeKey, VOLUME_TRANSPARENCY, annotations_of, append_new, area_view, break_lines,
    candle_momentum_keys, candle_momentum_series, integration_lite_view_calibrated, line_values,
    momentum_tints, oscillator_transparency, overlay_geoms, paint_mtf_table, paint_overlay,
    paint_timeframe_toolbar, paint_toolbar, paint_unsupported_chip, ribbon_fill_transparency,
    window_for_range,
};
use protopyne_kernels::{
    InputSet, OverlayProjection, PineEngine, PineRuntimeConfig, ProjectionRefusal, parse_program,
    project,
};
use protopyne_types::Bar;

/// Adaptive-leg smoothing matching the calibrated pane build.
const EIT_ALPHA: f64 = 0.14;
/// Ribbon fill strength.
const FILL_ALPHA: f64 = 0.5;
/// Area-fill strength for the area picture (top opacity; the foot fades to
/// transparent through the gradient).
const AREA_ALPHA: f64 = 0.35;
/// Chart types the toolbar offers: the three wired end-to-end plus the area
/// picture showcasing the gradient fill. Every other taxonomy variant stays
/// unwired and refuses in `chart_price_bars` instead of rendering a guess.
const CHART_TYPES: [ChartType; 4] =
    [ChartType::Candles, ChartType::Heikin, ChartType::Line, ChartType::Area];
/// Pixels per bar on every pane.
const BAR_SPACING: f32 = 8.0;
/// Right price-axis width, TradingView-style.
const AXIS_WIDTH: f32 = 68.0;
/// Bottom time-axis strip height on the price pane.
const TIME_STRIP: f32 = 18.0;
/// Price pane height.
const PRICE_HEIGHT: f32 = 420.0;
/// Oscillator pane height.
const OSC_HEIGHT: f32 = 160.0;
/// Volume strip height overlaid at the price pane bottom.
const VOLUME_STRIP: f32 = 48.0;
/// Crosshair dash length in pixels.
const DASH: f32 = 5.0;
/// Pine overlay script over the aligned window bars: a close plot plus a
/// periodic high label. Supported constructs only, so a refusal here names a
/// real regression; the chip path still renders any refusal.
const OVERLAY_SCRIPT: &str = "plot(close, title=\"Pine Close\")\n\
     if bar_index % 20 == 0\n\
     \x20   label.new(bar_index, high, \"mark\")";

/// Run the overlay script over window-aligned bars and project the outcome.
/// The script is a constant: unparseable/unrunnable means the shell is
/// broken, so it fails loud, while projection refusals ride the typed path
/// to the "unsupported" chip.
fn run_overlay(bars: &[Bar]) -> Result<OverlayProjection, ProjectionRefusal> {
    let program = parse_program(OVERLAY_SCRIPT).expect("overlay script parses");
    let outcome = PineEngine::new(PineRuntimeConfig::new().build())
        .run(&program, bars, InputSet::new())
        .expect("overlay script runs");
    project(&outcome)
}

/// Byte opacity as an emitter alpha fraction.
fn alpha_fraction(opacity: u8) -> f32 {
    f32::from(opacity) / 255.0
}

/// Ribbon fill opacity at the specified transparency for a fill strength.
fn ribbon_alpha(fill_strength: f64) -> f32 {
    alpha_fraction(opacity_from_transparency(ribbon_fill_transparency(fill_strength)))
}

/// Oscillator leg opacity at the specified dimming for the default brightness.
fn osc_alpha() -> f32 {
    alpha_fraction(opacity_from_transparency(oscillator_transparency(OSCILLATOR_BRIGHTNESS)))
}

/// Resolve a ported tint to its paint color at the specified transparency.
fn tint_color(tint: MomentumTint) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        tint.red,
        tint.green,
        tint.blue,
        opacity_from_transparency(tint.transparency),
    )
}

/// TradingView-dark chrome over the Pine parity palette: the candle, ribbon,
/// and oscillator hues stay the indicator inputs, while the background and
/// grid match the dark terminal.
fn tv_theme() -> ThemeConfig {
    ThemeConfig::new()
        .name("tradingview".to_owned())
        .background("#131722".to_owned())
        .grid("#1E2530".to_owned())
        .build()
}

fn history_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs/historical")
}

fn file_for(timeframe: TimeframeKey) -> Option<PathBuf> {
    let name = match timeframe {
        TimeframeKey::M1 => "SPY_1M.csv",
        TimeframeKey::M5 => "SPY_5M.csv",
        TimeframeKey::M15 | TimeframeKey::H1 | TimeframeKey::H4 => return None,
    };
    Some(history_dir().join(name))
}

fn default_m15() -> PathBuf {
    history_dir().join("SPY_15M.csv")
}

/// Loaded history: convertible rows plus the closed bars behind them.
/// Session breaks align index-for-index only when nothing dropped.
struct History {
    bars: Vec<Bar>,
    rows: Vec<ChartRow>,
    dropped: u64,
}

fn load_history(path: &PathBuf) -> Result<History, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let bars = protopyne_server::parse_bars_csv(&text)
        .map_err(|e| format!("parse {}: {e}", path.display()))?;
    if bars.is_empty() {
        return Err(format!("no bars in {}", path.display()));
    }
    let converted = rows_of(&bars);
    Ok(History { bars, rows: converted.rows, dropped: converted.dropped })
}

/// Aggregate closed bars upward by `group` (trailing-aligned, like exchange
/// resampling): open from the first bar, close/timestamp from the last,
/// high/low as extremes, volumes summed. Drops a leading partial chunk so
/// every output bar covers exactly `group` inputs.
fn aggregate_bars(bars: &[Bar], group: usize) -> Vec<Bar> {
    if group <= 1 {
        return bars.to_vec();
    }
    let skip = bars.len() % group;
    bars[skip..]
        .chunks(group)
        .filter(|chunk| chunk.len() == group)
        .map(|chunk| {
            let first = &chunk[0];
            let last = &chunk[group - 1];
            let mut high = first.h;
            let mut low = first.l;
            let mut volume = 0_u64;
            for bar in chunk {
                if bar.h > high {
                    high = bar.h;
                }
                if bar.l < low {
                    low = bar.l;
                }
                volume = volume.saturating_add(bar.volume);
            }
            Bar::new()
                .ts(last.ts)
                .o(first.o)
                .h(high)
                .l(low)
                .c(last.c)
                .volume(volume)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

fn history_of(bars: Vec<Bar>) -> Result<History, String> {
    if bars.is_empty() {
        return Err("resample refused: no bars to aggregate".to_owned());
    }
    let converted = rows_of(&bars);
    Ok(History { bars, rows: converted.rows, dropped: converted.dropped })
}

struct TerminalApp {
    theme: ThemeConfig,
    range: DateRange,
    timeframe: TimeframeKey,
    chart_type: ChartType,
    override_m15: Option<PathBuf>,
    histories: HashMap<TimeframeKey, History>,
    view: IntegrationLiteView,
    tints: Vec<Option<MomentumTint>>,
    mtf: Vec<MtfRow>,
    announced: bool,
    /// Pure pan/zoom machine bound to the price data extent (`None` while
    /// the rows are degenerate); the window itself stays caller-held below.
    interaction: Option<InteractionState>,
    /// Last stepped price window; `None` until the first gesture, and reset
    /// on every data change so a switch never pans stale geometry.
    price_window: Option<Viewport>,
    /// Pine overlay projection over the aligned window bars: `None` while
    /// rows and bars misalign (dropped rows), so drawings never misplace.
    overlay: Option<Result<OverlayProjection, ProjectionRefusal>>,
    /// Operator annotation journal: Pine drawings enter here on every data
    /// refresh (new drawings only, never duplicates), clicks append through
    /// the radius-confirmed path, and undo retracts. Painted as markers over
    /// the price pane every frame.
    annotations: AnnotationSet,
    /// Rows painted on the price pane for the active chart type: the window
    /// rows for candles/line/area, Heikin-Ashi rows converted over the full
    /// history (the smoothing is recursive, so windowing first would
    /// miscompute every open).
    price_rows: Vec<ChartRow>,
}

impl TerminalApp {
    fn load(override_m15: Option<PathBuf>) -> Result<Self, String> {
        let mut app = Self {
            theme: tv_theme(),
            range: DateRange::All,
            timeframe: TimeframeKey::default(),
            chart_type: ChartType::Candles,
            override_m15,
            histories: HashMap::new(),
            view: IntegrationLiteView {
                fast: Vec::new(),
                slow: Vec::new(),
                ribbon: RibbonView {
                    upper: Vec::new(),
                    lower: Vec::new(),
                    bullish: Vec::new(),
                    alpha: FILL_ALPHA,
                },
                pivots: Vec::new(),
                threshold: 100.0,
            },
            tints: Vec::new(),
            mtf: Vec::new(),
            announced: false,
            interaction: None,
            price_window: None,
            overlay: None,
            annotations: AnnotationSet::new(),
            price_rows: Vec::new(),
        };
        app.refresh()?;
        Ok(app)
    }

    fn ensure_history(&mut self, timeframe: TimeframeKey) -> Result<(), String> {
        if self.histories.contains_key(&timeframe) {
            return Ok(());
        }
        let history = match timeframe {
            TimeframeKey::M1 | TimeframeKey::M5 => {
                let path = file_for(timeframe).expect("native slot has a file");
                load_history(&path)?
            }
            TimeframeKey::M15 => {
                let path = self.override_m15.clone().unwrap_or_else(default_m15);
                load_history(&path)?
            }
            TimeframeKey::H1 | TimeframeKey::H4 => {
                self.ensure_history(TimeframeKey::M15)?;
                let base = &self.histories[&TimeframeKey::M15].bars;
                history_of(aggregate_bars(base, timeframe.m15_group()))?
            }
        };
        self.histories.insert(timeframe, history);
        Ok(())
    }

    fn window_rows(&self, timeframe: TimeframeKey) -> &[ChartRow] {
        let history = &self.histories[&timeframe];
        window_for_range(&history.rows, self.range, timeframe.seconds())
    }

    fn build_view(&self, timeframe: TimeframeKey) -> Result<IntegrationLiteView, String> {
        integration_lite_view_calibrated(self.window_rows(timeframe), EIT_ALPHA, FILL_ALPHA)
            .map_err(|e| format!("pane build refused ({timeframe}): {e}"))
    }

    /// Rows for the active chart type over the current window, delegating
    /// to the tested [`protopyne_egui::picture_rows`] dispatch (Heikin-Ashi
    /// converts over the full history first — recursive opens — then
    /// windows; drops and unwired types refuse).
    fn picture_rows(&self) -> Result<Vec<ChartRow>, String> {
        let history = &self.histories[&self.timeframe];
        protopyne_egui::picture_rows(
            &history.bars,
            self.range,
            self.timeframe.seconds(),
            self.chart_type,
        )
        .map_err(|e| format!("picture refused: {e}"))
    }

    fn refresh(&mut self) -> Result<(), String> {
        for timeframe in TimeframeKey::all() {
            self.ensure_history(timeframe)?;
        }
        self.price_rows = self.picture_rows()?;
        // Rebind the machine to the fresh data extent and drop the held
        // window: a timeframe/range switch pans nothing stale. Degenerate
        // rows leave no machine, and painting falls back to the data window.
        let (min, max) = self.price_data_extent();
        let pad = (max - min) * 0.02;
        self.interaction = InteractionState::new(min - pad, max + pad).ok();
        self.price_window = None;
        self.view = self.build_view(self.timeframe)?;
        let palette = MomentumPalette::from_theme(&self.theme)
            .map_err(|e| format!("palette refused: {e}"))?;
        let momentum = candle_momentum_series(self.window_rows(self.timeframe), EIT_ALPHA)
            .map_err(|e| format!("momentum build refused ({}): {e}", self.timeframe))?;
        self.tints = momentum_tints(self.window_rows(self.timeframe), &momentum, EIT_ALPHA, &palette)
            .map_err(|e| format!("tint build refused ({}): {e}", self.timeframe))?;
        // Overlay runs last: it needs the aligned window bars, and a
        // dropped row anywhere misaligns drawings, so those windows paint no
        // overlay at all.
        let overlay = self.aligned_bars().map(run_overlay);
        // Pine drawings enter the annotation journal here, once per refresh:
        // conversion is idempotent per content, so a re-run appends nothing
        // and operator entries are never touched. The bar borrow ends before
        // the append begins, so conversion and append stay sequential.
        if let Some(Ok(projection)) = &overlay {
            if let Some(bars) = self.aligned_bars() {
                let fresh = annotations_of(&projection.primitives, bars);
                append_new(&mut self.annotations, &fresh);
            }
        }
        self.overlay = overlay;
        self.mtf = Vec::new();
        for timeframe in TimeframeKey::all() {
            let view = self.build_view(timeframe)?;
            let strength = view
                .slow
                .iter()
                .rev()
                .flatten()
                .find(|v| v.is_finite())
                .copied()
                .unwrap_or(0.0);
            let bias =
                if strength > 0.0 { "bull" } else if strength < 0.0 { "bear" } else { "flat" };
            self.mtf.push(MtfRow {
                timeframe: timeframe.label().to_owned(),
                bias: bias.to_owned(),
                strength,
            });
        }
        Ok(())
    }

    fn summary(&self) -> String {
        let history = &self.histories[&self.timeframe];
        let overlay = match &self.overlay {
            Some(Ok(projection)) => format!("overlay={}", projection.primitives.len()),
            Some(Err(refusal)) => format!("overlay=refused({refusal})"),
            None => "overlay=none".to_owned(),
        };
        format!(
            "tf={} chart={} bars={} dropped={} window={} pivots={} threshold={} {overlay}",
            self.timeframe,
            self.chart_type.name(),
            history.bars.len(),
            history.dropped,
            self.window_rows(self.timeframe).len(),
            self.view.pivots.len(),
            self.view.threshold,
        )
    }

    /// Trailing bars aligned with the row window, if nothing dropped.
    fn aligned_bars(&self) -> Option<&[Bar]> {
        let history = &self.histories[&self.timeframe];
        if history.dropped != 0 {
            return None;
        }
        let keep = self.window_rows(self.timeframe).len();
        let bars = &history.bars;
        Some(&bars[bars.len() - keep..])
    }
}

impl eframe::App for TerminalApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("header").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("SPY");
                if let Some(picked) = paint_timeframe_toolbar(ui, self.timeframe) {
                    self.timeframe = picked;
                    if let Err(e) = self.refresh() {
                        ui.colored_label(egui::Color32::RED, e);
                    }
                }
                ui.label(self.summary());
            });
            if let Some(picked) = paint_toolbar(ui, self.range) {
                self.range = picked;
                if let Err(e) = self.refresh() {
                    ui.colored_label(egui::Color32::RED, e);
                }
            }
            ui.label("Chart:");
            for kind in CHART_TYPES {
                if ui.selectable_label(self.chart_type == kind, kind.name()).clicked() {
                    self.chart_type = kind;
                    if let Err(e) = self.refresh() {
                        ui.colored_label(egui::Color32::RED, e);
                    }
                }
            }
        });

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                if let Some(Err(refusal)) = &self.overlay {
                    paint_unsupported_chip(ui, refusal);
                }
                let mut hover_x = None;
                if let Err(e) = self.paint_price(ui, &mut hover_x) {
                    paint_error_chip(ui, &e);
                }
                ui.separator();
                if let Err(e) = self.paint_oscillator(ui, hover_x) {
                    paint_error_chip(ui, &e);
                }
                ui.separator();
                paint_mtf_table(ui, &self.mtf);
            });
        });
        if !self.announced {
            self.announced = true;
            eprintln!("protopyne-gui: window ready ({})", self.summary());
        }
    }
}

/// Pane-local geometry arrives with its origin at the pane's top-left;
/// shift one layer into UI space by the pane rect's origin before painting.
fn place_candles(geoms: Vec<CandleGeom>, origin: egui::Pos2) -> Vec<CandleGeom> {
    geoms
        .into_iter()
        .map(|mut geom| {
            geom.x += origin.x;
            geom.open_y += origin.y;
            geom.high_y += origin.y;
            geom.low_y += origin.y;
            geom.close_y += origin.y;
            geom
        })
        .collect()
}

/// Shift ribbon fill runs into UI space (see [`place_candles`]).
fn place_polygons(polygons: Vec<RibbonPolygon>, origin: egui::Pos2) -> Vec<RibbonPolygon> {
    polygons
        .into_iter()
        .map(|mut polygon| {
            for point in &mut polygon.points {
                point.0 += origin.x;
                point.1 += origin.y;
            }
            polygon
        })
        .collect()
}

/// Shift session gridlines into UI space (see [`place_candles`]).
fn place_breaks(lines: Vec<BreakLine>, dx: f32) -> Vec<BreakLine> {
    lines
        .into_iter()
        .map(|mut line| {
            line.x += dx;
            line
        })
        .collect()
}

/// Shift oscillator points into UI space (see [`place_candles`]).
fn place_points(
    points: Vec<Option<(f32, f32)>>,
    origin: egui::Pos2,
) -> Vec<Option<(f32, f32)>> {
    points
        .into_iter()
        .map(|point| point.map(|(x, y)| (x + origin.x, y + origin.y)))
        .collect()
}

/// Shift overlay geometry into UI space (see [`place_candles`]).
fn place_overlay(mut geoms: OverlayGeoms, origin: egui::Pos2) -> OverlayGeoms {
    for run in &mut geoms.series {
        run.points = place_points(std::mem::take(&mut run.points), origin);
    }
    for y in &mut geoms.levels {
        *y += origin.y;
    }
    for (x, y) in &mut geoms.markers {
        *x += origin.x;
        *y += origin.y;
    }
    for label in &mut geoms.labels {
        label.x += origin.x;
        label.y += origin.y;
    }
    for (x0, y0, x1, y1) in &mut geoms.areas {
        *x0 += origin.x;
        *y0 += origin.y;
        *x1 += origin.x;
        *y1 += origin.y;
    }
    geoms
}

/// Shift pivot dots into UI space (see [`place_candles`]).
fn place_dots(dots: Vec<PivotDot>, origin: egui::Pos2) -> Vec<PivotDot> {
    dots
        .into_iter()
        .map(|mut dot| {
            dot.x += origin.x;
            dot.y += origin.y;
            dot
        })
        .collect()
}

/// Shift annotation markers into UI space (see [`place_candles`]).
fn place_markers(dots: Vec<MarkerDot>, origin: egui::Pos2) -> Vec<MarkerDot> {
    dots
        .into_iter()
        .map(|mut dot| {
            dot.x += origin.x;
            dot.y += origin.y;
            dot
        })
        .collect()
}

/// Format an epoch timestamp for the time axis: intraday hours when the
/// window spans days, month-day otherwise.
fn fmt_time(epoch: i64, intraday: bool) -> String {
    let date: Option<DateTime<Utc>> = DateTime::from_timestamp(epoch, 0);
    match date {
        Some(moment) if intraday => moment.format("%H:%M").to_string(),
        Some(moment) => moment.format("%m-%d").to_string(),
        None => String::new(),
    }
}

/// Draw a dashed crosshair line through one axis point.
fn dashed_line(painter: &egui::Painter, a: egui::Pos2, b: egui::Pos2, stroke: egui::Stroke) {
    let length = (b - a).length();
    if !length.is_finite() || length <= 0.0 {
        return;
    }
    let direction = (b - a) / length;
    let mut travelled = 0.0;
    while travelled < length {
        let end = (travelled + DASH).min(length);
        painter.line_segment(
            [a + direction * travelled, a + direction * end],
            stroke,
        );
        travelled += DASH * 2.0;
    }
}

impl TerminalApp {
    /// Data extent behind the price pane (unpadded): the seed the viewport
    /// is built from, never the window on screen. The interaction machine
    /// binds to the padded form, and the held window carries the padding;
    /// grid, axis, and area reads come from the stepped viewport instead.
    fn price_data_extent(&self) -> (f64, f64) {
        let rows = &self.price_rows;
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for row in rows {
            min = min.min(row.low);
            max = max.max(row.high);
        }
        (min, max)
    }

    fn price_viewport(&self, height: f32) -> Option<Viewport> {
        let (min, max) = self.price_data_extent();
        if !min.is_finite() || !max.is_finite() || min >= max {
            return None;
        }
        let pad = (max - min) * 0.02;
        Viewport::new(PriceScaleMode::Normal, min - pad, max + pad, height, BAR_SPACING, min, false)
            .ok()
    }

    /// Paint the right price axis: tick labels plus the last-price tag. Ticks
    /// come from the window on screen, so a zoomed axis labels the zoomed
    /// prices instead of mapping the data extent off the pane.
    #[allow(clippy::too_many_arguments)]
    fn paint_axis(
        &self,
        painter: &egui::Painter,
        axis: egui::Rect,
        viewport: Viewport,
        origin_y: f32,
        last_price: Option<(f64, bool)>,
    ) -> Result<(), EguiError> {
        let grid = resolve_color(&self.theme, "grid")?;
        let window = viewport.visible_range();
        let ticks: Vec<Tick> = axis_ticks(window.min, window.max, 8, 8);
        for tick in &ticks {
            if let Some(y) = viewport.price_to_y(tick.value) {
                painter.text(
                    egui::Pos2::new(axis.right() - 4.0, y + origin_y),
                    egui::Align2::RIGHT_CENTER,
                    &tick.label,
                    egui::FontId::proportional(11.0),
                    grid,
                );
            }
        }
        if let Some((price, up)) = last_price {
            if let Some(y) = viewport.price_to_y(price) {
                let y = y + origin_y;
                let tag = resolve_color(&self.theme, if up { "bull" } else { "bear" })?;
                let rect = egui::Rect::from_min_max(
                    egui::Pos2::new(axis.left() + 2.0, y - 9.0),
                    egui::Pos2::new(axis.right() - 2.0, y + 9.0),
                );
                painter.rect_filled(rect, 3.0, tag);
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("{price:.2}"),
                    egui::FontId::proportional(11.0),
                    egui::Color32::WHITE,
                );
            }
        }
        Ok(())
    }

    /// Paint the OHLC legend over the plot's top-left, TradingView-style.
    /// Reads the painted rows, so Heikin-Ashi legends describe Heikin-Ashi
    /// candles, never the raw bars behind them.
    fn paint_legend(&self, painter: &egui::Painter, origin: egui::Pos2) -> Result<(), EguiError> {
        let rows = &self.price_rows;
        let Some(last) = rows.last() else { return Ok(()) };
        let previous = rows.len().checked_sub(2).and_then(|index| rows.get(index));
        let change = previous.map_or(0.0, |bar| last.close - bar.close);
        let base = previous.map_or(last.close, |bar| bar.close);
        let percent = if base == 0.0 { 0.0 } else { change / base * 100.0 };
        let up = change >= 0.0;
        let ink = resolve_color(&self.theme, if up { "bull" } else { "bear" })?;
        painter.text(
            origin + egui::vec2(6.0, 4.0),
            egui::Align2::LEFT_TOP,
            format!(
                "SPY · {} · O{:.2} H{:.2} L{:.2} C{:.2} {change:+.2} ({percent:+.2}%)",
                self.timeframe, last.open, last.high, last.low, last.close,
            ),
            egui::FontId::proportional(12.0),
            ink,
        );
        Ok(())
    }

    /// Paint faint volume bars over the plot's bottom strip from raw totals.
    fn paint_volume(
        &self,
        painter: &egui::Painter,
        plot: egui::Rect,
        viewport: Viewport,
        momentum: &[&str],
    ) -> Result<(), EguiError> {
        let rows = self.window_rows(self.timeframe);
        let peak = rows.iter().map(|row| row.volume).max().unwrap_or(0);
        if peak == 0 {
            return Ok(());
        }
        let base_y = plot.bottom();
        let half = BAR_SPACING * 0.35;
        for (index, row) in rows.iter().enumerate() {
            let height = row.volume as f64 / peak as f64 * f64::from(VOLUME_STRIP);
            if height <= 0.0 {
                continue;
            }
            let x = viewport.index_to_x(index) + plot.left();
            if x < plot.left() || x > plot.right() {
                continue;
            }
            let key = momentum.get(index).copied().unwrap_or("bull");
            let base = resolve_color(&self.theme, key)?;
            let opacity = opacity_from_transparency(VOLUME_TRANSPARENCY);
            let fill =
                egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), opacity);
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::Pos2::new(x - half, base_y - height as f32),
                    egui::Pos2::new(x + half, base_y),
                ),
                0.0,
                fill,
            );
        }
        Ok(())
    }

    /// Paint horizontal gridlines for the price ticks across the plot. The
    /// grid reads the same window the candles map through, so zooming keeps
    /// every line inside the plot instead of clipping the data extent away.
    fn paint_hgrid(
        &self,
        painter: &egui::Painter,
        plot: egui::Rect,
        viewport: Viewport,
    ) -> Result<(), EguiError> {
        let grid = resolve_color(&self.theme, "grid")?;
        let stroke = egui::Stroke::new(1.0, grid);
        let window = viewport.visible_range();
        for tick in axis_ticks(window.min, window.max, 8, 8) {
            if let Some(y) = viewport.price_to_y(tick.value) {
                let y = y + plot.top();
                painter.line_segment(
                    [egui::Pos2::new(plot.left(), y), egui::Pos2::new(plot.right(), y)],
                    stroke,
                );
            }
        }
        Ok(())
    }

    /// Advance the held price window under this frame's pointer gestures.
    ///
    /// The single egui→interaction translation site: pointer gestures become
    /// typed inputs and elapsed time comes from the frame clock
    /// (`stable_dt`); the pure machine owns the price window from here.
    /// Wheel zoom hides behind ctrl so plain scrolling still reaches the
    /// outer scroll area; pinch stays unwired (no touch shell yet). Split
    /// from `paint_price` so the row borrow never spans the mutable step.
    fn step_price_window(
        &mut self,
        ui: &mut egui::Ui,
        response: &egui::Response,
        plot: egui::Rect,
    ) -> Option<Viewport> {
        let base = self.price_viewport(plot.height())?;
        let dt = f64::from(ui.input(|i| i.stable_dt));
        let mut events = Vec::new();
        let drag = response.drag_delta();
        if drag.y != 0.0 {
            events.push(PointerInput::Drag { dy_px: drag.y });
        }
        if response.hovered() && ui.input(|i| i.modifiers.ctrl) {
            let wheel = ui.input(|i| i.smooth_scroll_delta.y);
            if wheel != 0.0 {
                if let Some(hover) = response.hover_pos() {
                    events.push(PointerInput::Wheel {
                        delta_px: wheel,
                        anchor_y: hover.y - plot.top(),
                    });
                }
            }
        }
        match self.interaction.as_mut() {
            Some(machine) => {
                // The held window persists across frames so gestures
                // accumulate; a pane resize rebuilds from data instead of
                // stretching stale geometry (bit-exact height comparison is
                // this codebase's float-compare idiom).
                let held = self
                    .price_window
                    .filter(|window| {
                        window.scale().height().to_bits() == plot.height().to_bits()
                    })
                    .unwrap_or(base);
                let next = machine.step(dt, &held, &events);
                self.price_window = Some(next);
                Some(next)
            }
            None => Some(base),
        }
    }

    /// Paint the price pane for the active chart type. A broken theme
    /// refuses the whole pane through the error chip in the panel above —
    /// no pane paints a silent gray default.
    fn paint_price(&mut self, ui: &mut egui::Ui, hover_x: &mut Option<f32>) -> Result<(), EguiError> {
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), PRICE_HEIGHT),
            egui::Sense::drag(),
        );
        let mut painter = ui.painter_at(rect);
        painter.set_clip_rect(rect);
        painter.rect_filled(rect, 0.0, resolve_color(&self.theme, "background")?);
        let plot = egui::Rect::from_min_max(
            rect.left_top(),
            egui::Pos2::new(rect.right() - AXIS_WIDTH, rect.bottom() - TIME_STRIP),
        );
        let axis = egui::Rect::from_min_max(
            egui::Pos2::new(rect.right() - AXIS_WIDTH, rect.top()),
            egui::Pos2::new(rect.right(), rect.bottom() - TIME_STRIP),
        );
        let time_strip = egui::Rect::from_min_max(
            egui::Pos2::new(rect.left(), rect.bottom() - TIME_STRIP),
            egui::Pos2::new(rect.right() - AXIS_WIDTH, rect.bottom()),
        );
        let Some(viewport) = self.step_price_window(ui, &response, plot) else {
            return Ok(());
        };
        // Pane-local geometry must shift by the plot origin, not the pane's:
        // the time strip and axis are chrome, not pane space.
        let origin = plot.left_top();
        self.paint_hgrid(&painter, plot, viewport)?;

        // Shared borrows only from here: the mutable step ran above, so the
        // row borrow never spans it.
        let rows = &self.price_rows;
        let bullish = &self.view.ribbon.bullish;
        let discrete: Vec<&str> = candle_momentum_keys(rows, bullish).unwrap_or_default();
        let polygons = place_polygons(ribbon_polygons(&self.view.ribbon, &viewport), origin);
        paint_ribbon_fill(&painter, &polygons, &self.theme, ribbon_alpha(FILL_ALPHA))?;
        self.paint_volume(&painter, plot, viewport, &discrete)?;
        match self.chart_type {
            ChartType::Candles | ChartType::Heikin => {
                if let Ok(geoms) = candle_geoms(rows, bullish, &viewport) {
                    let mut candles = Vec::with_capacity(geoms.len());
                    for geom in place_candles(geoms, origin) {
                        let color =
                            if let Some(tint) = self.tints.get(geom.index).and_then(|slot| *slot)
                            {
                                tint_color(tint)
                            } else {
                                let key =
                                    discrete.get(geom.index).copied().unwrap_or("bull");
                                resolve_color(&self.theme, key)?
                            };
                        candles.push(TintedCandle { geom, color });
                    }
                    paint_candles_tinted(&painter, &candles, BAR_SPACING * 0.7);
                }
            }
            ChartType::Line => {
                let points =
                    place_points(oscillator_points(&line_values(rows), &viewport), origin);
                paint_oscillator(&painter, &points, "fast", &self.theme, 1.0)?;
            }
            ChartType::Area => {
                self.paint_area(&painter, viewport, origin, rows)?;
            }
            // Unreachable through the toolbar picker (four wired kinds
            // only): refusals delegate to the single chart_type decision
            // rather than keeping a local wording in sync by hand.
            other => {
                return Err(protopyne_egui::refuse_chart_type(other));
            }
        }
        if let Some(bars) = self.aligned_bars() {
            let provider = DailySessionProvider::continuous();
            let lines = place_breaks(break_lines(&viewport, bars, &provider), plot.left());
            paint_session_breaks(&painter, &lines, plot.top(), plot.bottom(), &self.theme)?;
        }
        let dots = place_dots(pivot_price_dots(&self.view.pivots, rows, &viewport), origin);
        // Pivots lower to markers and paint through the one marker emitter;
        // the journal paints beside them, resolved against the same window.
        let mut markers: Vec<MarkerDot> =
            dots.iter().map(MarkerDot::from_pivot).collect();
        let live: Vec<Annotation> = self.annotations.iter().cloned().collect();
        markers.extend(place_markers(marker_dots(&live, rows, &viewport), origin));
        paint_markers(&painter, &markers, &self.theme)?;
        // The overlay paints over the price pane from the same viewport, so
        // its drawings sit on their bars; refusals paint nothing here — the
        // chip renders in the panel above instead.
        if let Some(Ok(projection)) = &self.overlay {
            let geoms = place_overlay(overlay_geoms(&projection.primitives, &viewport), origin);
            paint_overlay(&painter, &geoms, &self.theme, plot.width())?;
        }
        self.paint_legend(&painter, plot.left_top())?;

        let last = rows.last();
        self.paint_axis(
            &painter,
            axis,
            viewport,
            origin.y,
            last.map(|bar| (bar.close, bar.close >= bar.open)),
        )?;
        self.paint_time_axis(&painter, time_strip, rows)?;

        if let Some(hover) = response.hover_pos() {
            if plot.contains(hover) {
                *hover_x = Some(hover.x);
                self.paint_crosshair(&painter, plot, axis, viewport, rows, hover)?;
            }
        }
        // Plain click annotates the candle under the pointer: the radius
        // confirms intent, the bar fixes the key, and misses append nothing.
        // Drags pan (handled above) and never reach here — `clicked` fires
        // only without drag — so annotating and panning coexist.
        if response.clicked() && ui.input(|i| i.modifiers == egui::Modifiers::NONE) {
            if let Some(pressed) = response.hover_pos() {
                if plot.contains(pressed) {
                    let local =
                        egui::Pos2::new(pressed.x - origin.x, pressed.y - origin.y);
                    if let Some(annotation) = annotate_at((local.x, local.y), &viewport, rows)
                    {
                        self.annotations.append(annotation);
                    }
                }
            }
        }
        Ok(())
    }

    /// Paint the area picture: gradient fill under the close line (one mesh
    /// per reused ribbon polygon, top color fading to transparent) plus the
    /// line itself on top. Fill colors resolve here — the polygons carry
    /// shape only. The fill foot sits at the window floor, not the data
    /// floor, so a zoomed area fades to the zoomed baseline.
    fn paint_area(
        &self,
        painter: &egui::Painter,
        viewport: Viewport,
        origin: egui::Pos2,
        rows: &[ChartRow],
    ) -> Result<(), EguiError> {
        // The fill foot sits at the window floor, not the data floor, so a
        // zoomed area fades to the zoomed baseline.
        let view = area_view(line_values(rows), viewport.visible_range().min, AREA_ALPHA)?;
        let polygons = place_polygons(area_polygons(&view.values, view.baseline, &viewport), origin);
        let base = resolve_color(&self.theme, "fast")?;
        let opacity = (AREA_ALPHA.clamp(0.0, 1.0) * 255.0).round().clamp(0.0, 255.0) as u8;
        let top = egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), opacity);
        let bottom = egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 0);
        paint_area_fill(painter, &polygons, top, bottom);
        let points = place_points(oscillator_points(&view.values, &viewport), origin);
        paint_oscillator(painter, &points, "fast", &self.theme, 1.0)?;
        Ok(())
    }

    /// Paint evenly spaced time labels across the bottom strip.
    fn paint_time_axis(
        &self,
        painter: &egui::Painter,
        strip: egui::Rect,
        rows: &[ChartRow],
    ) -> Result<(), EguiError> {
        if rows.len() < 2 {
            return Ok(());
        }
        let span = rows.last().map_or(0, |bar| bar.time)
            - rows.first().map_or(0, |bar| bar.time);
        let intraday = span < 3 * 86_400;
        let grid = resolve_color(&self.theme, "grid")?;
        let shown = 6_usize.min(rows.len());
        for slot in 0..shown {
            let index = slot * (rows.len() - 1) / (shown - 1).max(1);
            let x = BAR_SPACING * index as f32 + strip.left();
            if x > strip.right() - 20.0 {
                continue;
            }
            painter.text(
                egui::Pos2::new(x, strip.center().y),
                egui::Align2::LEFT_CENTER,
                fmt_time(rows[index].time, intraday),
                egui::FontId::proportional(11.0),
                grid,
            );
        }
        Ok(())
    }

    /// Paint the dashed hover crosshair with axis labels, TradingView-style.
    ///
    /// Bar selection and the readout both run through the shared headless
    /// mapping: the pointer names a bar through `crosshair_data` (a pointer
    /// exactly between two bars reads the higher index, deterministically),
    /// and the readout maps back through `crosshair_pixel`, so the labels
    /// sit where the mapping says the bar and price are — never where a
    /// second local rounding puts them. The OHLC block is a separate layer:
    /// it appears only when `hit_test` names the candle under the pointer
    /// (within the hit radius of the wick). Otherwise the crosshair still
    /// draws and the OHLC block is omitted.
    #[allow(clippy::too_many_arguments)]
    fn paint_crosshair(
        &self,
        painter: &egui::Painter,
        plot: egui::Rect,
        axis: egui::Rect,
        viewport: Viewport,
        rows: &[ChartRow],
        hover: egui::Pos2,
    ) -> Result<(), EguiError> {
        let grid = resolve_color(&self.theme, "grid")?;
        let stroke = egui::Stroke::new(1.0, grid);
        dashed_line(
            painter,
            egui::Pos2::new(plot.left(), hover.y),
            egui::Pos2::new(plot.right(), hover.y),
            stroke,
        );
        dashed_line(
            painter,
            egui::Pos2::new(hover.x, plot.top()),
            egui::Pos2::new(hover.x, plot.bottom()),
            stroke,
        );
        let local = (hover.x - plot.left(), hover.y - plot.top());
        let Some(data) = crosshair_data(local, &viewport, rows.len()) else {
            return Ok(());
        };
        let Some((snap_x, snap_y)) = crosshair_pixel(&data, &viewport) else {
            return Ok(());
        };
        let price_rect = egui::Rect::from_min_max(
            egui::Pos2::new(axis.left() + 2.0, snap_y + plot.top() - 9.0),
            egui::Pos2::new(axis.right() - 2.0, snap_y + plot.top() + 9.0),
        );
        painter.rect_filled(price_rect, 3.0, grid);
        painter.text(
            price_rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("{:.2}", data.price),
            egui::FontId::proportional(11.0),
            egui::Color32::WHITE,
        );
        if let Some(row) = rows.get(data.index) {
            let span =
                rows.last().map_or(0, |bar| bar.time) - rows.first().map_or(0, |bar| bar.time);
            painter.text(
                egui::Pos2::new(snap_x + plot.left(), plot.bottom() + TIME_STRIP / 2.0),
                egui::Align2::CENTER_CENTER,
                fmt_time(row.time, span < 3 * 86_400),
                egui::FontId::proportional(11.0),
                egui::Color32::WHITE,
            );
            if let Some(hit) = hit_test(local, &viewport, rows) {
                if let Some(tipped) = rows.get(hit.index) {
                    painter.text(
                        egui::Pos2::new(snap_x + plot.left() + 8.0, hover.y - 28.0),
                        egui::Align2::LEFT_TOP,
                        format!(
                            "O{:.2} H{:.2} L{:.2} C{:.2}",
                            tipped.open, tipped.high, tipped.low, tipped.close,
                        ),
                        egui::FontId::proportional(11.0),
                        egui::Color32::WHITE,
                    );
                }
            }
        }
        Ok(())
    }

    /// Data extent behind the oscillator pane (unpadded): the seed the
    /// viewport is built from, never the window on screen. Grid and axis
    /// read the viewport instead.
    fn oscillator_data_extent(&self) -> (f64, f64) {
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for value in self.view.fast.iter().chain(self.view.slow.iter()).flatten() {
            if value.is_finite() {
                min = min.min(*value);
                max = max.max(*value);
            }
        }
        (min, max)
    }

    fn oscillator_viewport(&self, height: f32) -> Option<Viewport> {
        let (min, max) = self.oscillator_data_extent();
        if !min.is_finite() || !max.is_finite() || min >= max {
            return None;
        }
        let pad = (max - min).max(1.0) * 0.05;
        Viewport::new(
            PriceScaleMode::Normal,
            min - pad,
            max + pad,
            height,
            BAR_SPACING,
            0.0,
            false,
        )
        .ok()
    }

    fn paint_oscillator(&self, ui: &mut egui::Ui, hover_x: Option<f32>) -> Result<(), EguiError> {
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), OSC_HEIGHT),
            egui::Sense::hover(),
        );
        let mut painter = ui.painter_at(rect);
        painter.set_clip_rect(rect);
        painter.rect_filled(rect, 0.0, resolve_color(&self.theme, "background")?);
        let plot = egui::Rect::from_min_max(
            rect.left_top(),
            egui::Pos2::new(rect.right() - AXIS_WIDTH, rect.bottom()),
        );
        let axis = egui::Rect::from_min_max(
            egui::Pos2::new(rect.right() - AXIS_WIDTH, rect.top()),
            rect.right_bottom(),
        );
        let Some(viewport) = self.oscillator_viewport(plot.height()) else {
            return Ok(());
        };
        let origin = plot.left_top();
        self.paint_hgrid(&painter, plot, viewport)?;
        painter.text(
            plot.left_top() + egui::vec2(6.0, 4.0),
            egui::Align2::LEFT_TOP,
            "Integration Lite",
            egui::FontId::proportional(12.0),
            resolve_color(&self.theme, "grid")?,
        );
        for (values, key) in [(&self.view.fast, "fast"), (&self.view.slow, "slow")] {
            let points = place_points(oscillator_points(values, &viewport), origin);
            paint_oscillator(&painter, &points, key, &self.theme, osc_alpha())?;
        }
        let grid = egui::Stroke::new(1.0, resolve_color(&self.theme, "grid")?);
        for signed in [self.view.threshold, -self.view.threshold] {
            if let Some(y) = viewport.price_to_y(signed) {
                let y = y + origin.y;
                painter.line_segment(
                    [egui::Pos2::new(plot.left(), y), egui::Pos2::new(plot.right(), y)],
                    grid,
                );
            }
        }
        if let Some(zero) = viewport.price_to_y(0.0) {
            let y = zero + origin.y;
            painter.line_segment(
                [egui::Pos2::new(plot.left(), y), egui::Pos2::new(plot.right(), y)],
                egui::Stroke::new(1.0, resolve_color(&self.theme, "slow")?),
            );
        }
        let dots = place_dots(pivot_dots(&self.view.pivots, &self.view.fast, &viewport), origin);
        let markers: Vec<MarkerDot> = dots.iter().map(MarkerDot::from_pivot).collect();
        paint_markers(&painter, &markers, &self.theme)?;
        self.paint_axis(&painter, axis, viewport, origin.y, None)?;
        if let Some(x) = hover_x {
            if x >= plot.left() && x <= plot.right() {
                dashed_line(
                    &painter,
                    egui::Pos2::new(x, plot.top()),
                    egui::Pos2::new(x, plot.bottom()),
                    grid,
                );
            }
        }
        Ok(())
    }
}

fn main() {
    let mut path: Option<PathBuf> = None;
    let mut check = false;
    for arg in std::env::args().skip(1) {
        if arg == "--check" {
            check = true;
        } else {
            path = Some(PathBuf::from(arg));
        }
    }
    if check {
        let mut lines = Vec::new();
        for timeframe in TimeframeKey::all() {
            let mut probe = match TerminalApp::load(path.clone()) {
                Ok(app) => app,
                Err(e) => {
                    eprintln!("protopyne-gui: {e}");
                    std::process::exit(1);
                }
            };
            probe.timeframe = timeframe;
            match probe.refresh() {
                Ok(()) => lines.push(format!("{timeframe}: {}", probe.summary())),
                Err(e) => {
                    eprintln!("protopyne-gui: {e}");
                    std::process::exit(1);
                }
            }
        }
        for line in lines {
            println!("protopyne-gui check: {line}");
        }
        return;
    }
    let app = match TerminalApp::load(path) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("protopyne-gui: {e}");
            std::process::exit(1);
        }
    };
    let options = eframe::NativeOptions::default();
    let run = eframe::run_native(
        "protopyne terminal",
        options,
        Box::new(|_cc| Ok::<_, Box<dyn std::error::Error + Send + Sync>>(Box::new(app))),
    );
    if let Err(e) = run {
        eprintln!("protopyne-gui: window refused: {e}");
        std::process::exit(1);
    }
}
