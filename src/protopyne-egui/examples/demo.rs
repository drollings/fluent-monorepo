//! Native terminal demo shell over the headless chart stack.
//!
//! A TradingView-flavoured terminal: symbol + timeframe toolbar (`1M` `5M`
//! `15M` `1H` `4H`), range presets, a price pane (four-color momentum
//! candles, shaded ribbon area, volume strip, session grid, right price
//! axis with a last-price tag, OHLC legend, hover crosshair), the
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
//! fabricates none.
//!
//! Usage: `cargo run -p protopyne-egui --features demo --example demo --
//! [HISTORY.csv]`, or pass `--check` to run the headless data path without
//! opening a window (loads every slot, builds every pane, prints a summary).

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use fluent_egui::{
    BreakLine, CandleGeom, PivotDot, RibbonPolygon, RibbonView, ThemeConfig, Viewport, axis_ticks,
    candle_geoms, opacity_from_transparency, oscillator_points, paint_candles_as,
    paint_oscillator, paint_pivot_dots, paint_ribbon_fill, paint_session_breaks, pivot_dots,
    pivot_price_dots, ribbon_polygons, theme_color,
};
use protopyne_chart_model::{ChartRow, DailySessionProvider, DateRange, rows_of};
use protopyne_chart_scale::{PriceScaleMode, Tick};
use protopyne_egui::{
    IntegrationLiteView, MtfRow, OSCILLATOR_BRIGHTNESS, TimeframeKey, VOLUME_TRANSPARENCY,
    break_lines, candle_momentum_keys, integration_lite_view_calibrated, oscillator_transparency,
    paint_mtf_table, paint_timeframe_toolbar, paint_toolbar, ribbon_fill_transparency,
    window_for_range,
};
use protopyne_types::Bar;

/// Adaptive-leg smoothing matching the calibrated pane build.
const EIT_ALPHA: f64 = 0.14;
/// Ribbon fill strength.
const FILL_ALPHA: f64 = 0.5;
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
    override_m15: Option<PathBuf>,
    histories: HashMap<TimeframeKey, History>,
    view: IntegrationLiteView,
    mtf: Vec<MtfRow>,
    announced: bool,
}

impl TerminalApp {
    fn load(override_m15: Option<PathBuf>) -> Result<Self, String> {
        let mut app = Self {
            theme: tv_theme(),
            range: DateRange::All,
            timeframe: TimeframeKey::default(),
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
            mtf: Vec::new(),
            announced: false,
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

    fn refresh(&mut self) -> Result<(), String> {
        for timeframe in TimeframeKey::all() {
            self.ensure_history(timeframe)?;
        }
        self.view = self.build_view(self.timeframe)?;
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
        format!(
            "tf={} bars={} dropped={} window={} pivots={} threshold={}",
            self.timeframe,
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
        });

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let mut hover_x = None;
                self.paint_price(ui, &mut hover_x);
                ui.separator();
                self.paint_oscillator(ui, hover_x);
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
    fn price_window(&self) -> (f64, f64) {
        let rows = self.window_rows(self.timeframe);
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for row in rows {
            min = min.min(row.low);
            max = max.max(row.high);
        }
        (min, max)
    }

    fn price_viewport(&self, height: f32) -> Option<Viewport> {
        let (min, max) = self.price_window();
        if !min.is_finite() || !max.is_finite() || min >= max {
            return None;
        }
        let pad = (max - min) * 0.02;
        Viewport::new(PriceScaleMode::Normal, min - pad, max + pad, height, BAR_SPACING, min, false)
            .ok()
    }

    /// Paint the right price axis: tick labels plus the last-price tag.
    fn paint_axis(
        &self,
        painter: &egui::Painter,
        axis: egui::Rect,
        viewport: Viewport,
        origin_y: f32,
        min: f64,
        max: f64,
        last_price: Option<(f64, bool)>,
    ) {
        let grid = theme_color(&self.theme, "grid");
        let ticks: Vec<Tick> = axis_ticks(min, max, 8, 8);
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
                let tag = theme_color(&self.theme, if up { "bull" } else { "bear" });
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
    }

    /// Paint the OHLC legend over the plot's top-left, TradingView-style.
    fn paint_legend(&self, painter: &egui::Painter, origin: egui::Pos2) {
        let rows = self.window_rows(self.timeframe);
        let Some(last) = rows.last() else { return };
        let previous = rows.len().checked_sub(2).and_then(|index| rows.get(index));
        let change = previous.map_or(0.0, |bar| last.close - bar.close);
        let base = previous.map_or(last.close, |bar| bar.close);
        let percent = if base == 0.0 { 0.0 } else { change / base * 100.0 };
        let up = change >= 0.0;
        let ink = theme_color(&self.theme, if up { "bull" } else { "bear" });
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
    }

    /// Paint faint volume bars over the plot's bottom strip from raw totals.
    fn paint_volume(
        &self,
        painter: &egui::Painter,
        plot: egui::Rect,
        viewport: Viewport,
        momentum: &[&str],
    ) {
        let rows = self.window_rows(self.timeframe);
        let peak = rows.iter().map(|row| row.volume).max().unwrap_or(0);
        if peak == 0 {
            return;
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
            let base = theme_color(&self.theme, key);
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
    }

    /// Paint horizontal gridlines for the price ticks across the plot.
    fn paint_hgrid(
        &self,
        painter: &egui::Painter,
        plot: egui::Rect,
        viewport: Viewport,
        min: f64,
        max: f64,
    ) {
        let grid = theme_color(&self.theme, "grid");
        let stroke = egui::Stroke::new(1.0, grid);
        for tick in axis_ticks(min, max, 8, 8) {
            if let Some(y) = viewport.price_to_y(tick.value) {
                let y = y + plot.top();
                painter.line_segment(
                    [egui::Pos2::new(plot.left(), y), egui::Pos2::new(plot.right(), y)],
                    stroke,
                );
            }
        }
    }

    fn paint_price(&self, ui: &mut egui::Ui, hover_x: &mut Option<f32>) {
        let rows = self.window_rows(self.timeframe);
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), PRICE_HEIGHT),
            egui::Sense::hover(),
        );
        let mut painter = ui.painter_at(rect);
        painter.set_clip_rect(rect);
        painter.rect_filled(rect, 0.0, theme_color(&self.theme, "background"));
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
        let Some(viewport) = self.price_viewport(plot.height()) else {
            return;
        };
        // Pane-local geometry must shift by the plot origin, not the pane's:
        // the time strip and axis are chrome, not pane space.
        let origin = plot.left_top();
        let (min, max) = self.price_window();
        self.paint_hgrid(&painter, plot, viewport, min, max);

        let bullish = &self.view.ribbon.bullish;
        let momentum: Vec<&str> = candle_momentum_keys(rows, bullish).unwrap_or_default();
        let polygons = place_polygons(ribbon_polygons(&self.view.ribbon, &viewport), origin);
        paint_ribbon_fill(&painter, &polygons, &self.theme, ribbon_alpha(FILL_ALPHA));
        self.paint_volume(&painter, plot, viewport, &momentum);
        if let Ok(geoms) = candle_geoms(rows, bullish, &viewport) {
            let geoms = place_candles(geoms, origin);
            for key in ["bull", "bear", "bull_down", "bear_up"] {
                let group: Vec<CandleGeom> = geoms
                    .iter()
                    .filter(|geom| momentum.get(geom.index).copied().unwrap_or("bull") == key)
                    .copied()
                    .collect();
                if !group.is_empty() {
                    paint_candles_as(&painter, &group, key, BAR_SPACING * 0.7, &self.theme);
                }
            }
        }
        if let Some(bars) = self.aligned_bars() {
            let provider = DailySessionProvider::continuous();
            let lines = place_breaks(break_lines(&viewport, bars, &provider), plot.left());
            paint_session_breaks(&painter, &lines, plot.top(), plot.bottom(), &self.theme);
        }
        let dots = place_dots(pivot_price_dots(&self.view.pivots, rows, &viewport), origin);
        paint_pivot_dots(&painter, &dots, &self.theme);
        self.paint_legend(&painter, plot.left_top());

        let last = rows.last();
        self.paint_axis(
            &painter,
            axis,
            viewport,
            origin.y,
            min,
            max,
            last.map(|bar| (bar.close, bar.close >= bar.open)),
        );
        self.paint_time_axis(&painter, time_strip, rows);

        if let Some(hover) = response.hover_pos() {
            if plot.contains(hover) {
                *hover_x = Some(hover.x);
                self.paint_crosshair(&painter, plot, axis, viewport, rows, hover);
            }
        }
    }

    /// Paint evenly spaced time labels across the bottom strip.
    fn paint_time_axis(&self, painter: &egui::Painter, strip: egui::Rect, rows: &[ChartRow]) {
        if rows.len() < 2 {
            return;
        }
        let span = rows.last().map_or(0, |bar| bar.time)
            - rows.first().map_or(0, |bar| bar.time);
        let intraday = span < 3 * 86_400;
        let grid = theme_color(&self.theme, "grid");
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
    }

    /// Paint the dashed hover crosshair with axis labels, TradingView-style.
    fn paint_crosshair(
        &self,
        painter: &egui::Painter,
        plot: egui::Rect,
        axis: egui::Rect,
        viewport: Viewport,
        rows: &[ChartRow],
        hover: egui::Pos2,
    ) {
        let grid = theme_color(&self.theme, "grid");
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
        let local_y = hover.y - plot.top();
        if let Some(price) = viewport.y_to_price(local_y) {
            let rect = egui::Rect::from_min_max(
                egui::Pos2::new(axis.left() + 2.0, hover.y - 9.0),
                egui::Pos2::new(axis.right() - 2.0, hover.y + 9.0),
            );
            painter.rect_filled(rect, 3.0, grid);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                format!("{price:.2}"),
                egui::FontId::proportional(11.0),
                egui::Color32::WHITE,
            );
        }
        let local_x = hover.x - plot.left();
        if let Some(index) = viewport.x_to_index(local_x) {
            let rounded = index.round() as usize;
            if let Some(row) = rows.get(rounded) {
                let span = rows.last().map_or(0, |bar| bar.time)
                    - rows.first().map_or(0, |bar| bar.time);
                painter.text(
                    egui::Pos2::new(hover.x, plot.bottom() + TIME_STRIP / 2.0),
                    egui::Align2::CENTER_CENTER,
                    fmt_time(row.time, span < 3 * 86_400),
                    egui::FontId::proportional(11.0),
                    egui::Color32::WHITE,
                );
            }
        }
    }

    fn oscillator_viewport(&self, height: f32) -> Option<Viewport> {
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for value in self.view.fast.iter().chain(self.view.slow.iter()).flatten() {
            if value.is_finite() {
                min = min.min(*value);
                max = max.max(*value);
            }
        }
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

    fn paint_oscillator(&self, ui: &mut egui::Ui, hover_x: Option<f32>) {
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), OSC_HEIGHT),
            egui::Sense::hover(),
        );
        let mut painter = ui.painter_at(rect);
        painter.set_clip_rect(rect);
        painter.rect_filled(rect, 0.0, theme_color(&self.theme, "background"));
        let plot = egui::Rect::from_min_max(
            rect.left_top(),
            egui::Pos2::new(rect.right() - AXIS_WIDTH, rect.bottom()),
        );
        let axis = egui::Rect::from_min_max(
            egui::Pos2::new(rect.right() - AXIS_WIDTH, rect.top()),
            rect.right_bottom(),
        );
        let Some(viewport) = self.oscillator_viewport(plot.height()) else {
            return;
        };
        let origin = plot.left_top();
        let (min, max) = {
            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            for value in self.view.fast.iter().chain(self.view.slow.iter()).flatten() {
                if value.is_finite() {
                    lo = lo.min(*value);
                    hi = hi.max(*value);
                }
            }
            (lo, hi)
        };
        self.paint_hgrid(&painter, plot, viewport, min, max);
        painter.text(
            plot.left_top() + egui::vec2(6.0, 4.0),
            egui::Align2::LEFT_TOP,
            "Integration Lite",
            egui::FontId::proportional(12.0),
            theme_color(&self.theme, "grid"),
        );
        for (values, key) in [(&self.view.fast, "fast"), (&self.view.slow, "slow")] {
            let points = place_points(oscillator_points(values, &viewport), origin);
            paint_oscillator(&painter, &points, key, &self.theme, osc_alpha());
        }
        let grid = egui::Stroke::new(1.0, theme_color(&self.theme, "grid"));
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
                egui::Stroke::new(1.0, theme_color(&self.theme, "slow")),
            );
        }
        let dots = place_dots(pivot_dots(&self.view.pivots, &self.view.fast, &viewport), origin);
        paint_pivot_dots(&painter, &dots, &self.theme);
        self.paint_axis(&painter, axis, viewport, origin.y, min, max, None);
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
