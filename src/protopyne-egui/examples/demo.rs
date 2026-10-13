//! Native terminal demo shell over the headless chart stack.
//!
//! Loads one CSV history file, converts it through the canonical
//! bar-to-display boundary, builds the calibrated IntegrationLite pane, and
//! paints the price pane (candles, ribbon, session breaks) plus the
//! zero-centered oscillator pane (fast/slow, threshold rules, pivot dots)
//! behind the timeframe toolbar. No decision logic lives here: every number
//! on screen comes from the headless kernels and views; this file only maps
//! them onto pixels.
//!
//! Usage: `cargo run -p protopyne-egui --features demo --example demo --
//! [HISTORY.csv]`, or pass `--check` to run the headless data path without
//! opening a window (load, convert, build the pane, print a summary).
//! The volume strip is intentionally absent: buy/sell splits arrive
//! precomputed upstream, and this shell fabricates none.

use std::path::PathBuf;

use fluent_egui::{
    BreakLine, CandleGeom, PivotDot, RibbonBand, ThemeConfig, Viewport, candle_geoms,
    oscillator_points, paint_candles, paint_oscillator, paint_pivot_dots, paint_ribbon,
    paint_session_breaks, pivot_dots, ribbon_bands, theme_color,
};
use protopyne_chart_model::{ChartRow, DailySessionProvider, DateRange, rows_of};
use protopyne_chart_scale::PriceScaleMode;
use protopyne_egui::{
    ChartConfig, IntegrationLiteView, MtfRow, break_lines, integration_lite_view_calibrated,
    paint_mtf_table, paint_toolbar, window_for_range,
};
use protopyne_types::Bar;

/// Adaptive-leg smoothing matching the calibrated pane build.
const EIT_ALPHA: f64 = 0.14;
/// Ribbon fill strength.
const FILL_ALPHA: f64 = 0.5;
/// Pixels per bar on every pane.
const BAR_SPACING: f32 = 8.0;
/// Bar seconds for the 15-minute fixture the toolbar presets assume.
const BAR_SECONDS: u64 = 900;

fn default_history() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs/historical/SPY_15M.csv")
}

/// Loaded history: convertible rows plus the closed bars behind them.
/// Session breaks align index-for-index only when nothing dropped.
struct History {
    bars: Vec<Bar>,
    rows: Vec<ChartRow>,
    dropped: u64,
}

fn load_history(path: &PathBuf) -> Result<History, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    let bars =
        protopyne_server::parse_bars_csv(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
    if bars.is_empty() {
        return Err(format!("no bars in {}", path.display()));
    }
    let converted = rows_of(&bars);
    Ok(History { bars, rows: converted.rows, dropped: converted.dropped })
}

struct TerminalApp {
    config: ChartConfig,
    theme: ThemeConfig,
    range: DateRange,
    history: History,
    view: IntegrationLiteView,
    announced: bool,
}

impl TerminalApp {
    fn load(path: &PathBuf) -> Result<Self, String> {
        let history = load_history(path)?;
        let config = ChartConfig::default();
        let range = DateRange::All;
        let view = Self::build_view(&history, range)?;
        Ok(Self { config, theme: ThemeConfig::default(), range, history, view, announced: false })
    }

    fn window_rows(&self) -> &[ChartRow] {
        window_for_range(&self.history.rows, self.range, BAR_SECONDS)
    }

    fn build_view(history: &History, range: DateRange) -> Result<IntegrationLiteView, String> {
        let window = window_for_range(&history.rows, range, BAR_SECONDS);
        integration_lite_view_calibrated(window, EIT_ALPHA, FILL_ALPHA)
            .map_err(|e| format!("pane build refused: {e}"))
    }

    fn refresh(&mut self) -> Result<(), String> {
        self.view = Self::build_view(&self.history, self.range)?;
        Ok(())
    }

    fn summary(&self) -> String {
        format!(
            "bars={} dropped={} window={} pivots={} threshold={}",
            self.history.bars.len(),
            self.history.dropped,
            self.window_rows().len(),
            self.view.pivots.len(),
            self.view.threshold,
        )
    }

    /// Trailing bars aligned with the row window, if nothing dropped.
    fn aligned_bars(&self) -> Option<&[Bar]> {
        if self.history.dropped != 0 {
            return None;
        }
        let keep = self.window_rows().len();
        let bars = &self.history.bars;
        Some(&bars[bars.len() - keep..])
    }
}

impl eframe::App for TerminalApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("header").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("protopyne terminal");
                ui.label(self.summary());
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(picked) = paint_toolbar(ui, self.range) {
                self.range = picked;
                if let Err(e) = self.refresh() {
                    ui.colored_label(egui::Color32::RED, e);
                }
            }
            ui.separator();
            self.paint_price(ui);
            ui.separator();
            self.paint_oscillator(ui);
            ui.separator();
            paint_mtf_table(ui, &[self.mtf_row()]);
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

/// Shift ribbon fill strips into UI space (see [`place_candles`]).
fn place_bands(bands: Vec<Option<RibbonBand>>, origin: egui::Pos2) -> Vec<Option<RibbonBand>> {
    bands
        .into_iter()
        .map(|band| {
            band.map(|mut strip| {
                strip.x += origin.x;
                strip.top_y += origin.y;
                strip.bottom_y += origin.y;
                strip
            })
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

impl TerminalApp {
    fn mtf_row(&self) -> MtfRow {
        let strength = self
            .view
            .slow
            .iter()
            .rev()
            .flatten()
            .find(|v| v.is_finite())
            .copied()
            .unwrap_or(0.0);
        let bias = if strength > 0.0 { "bull" } else if strength < 0.0 { "bear" } else { "flat" };
        MtfRow { timeframe: self.config.timeframe.clone(), bias: bias.to_owned(), strength }
    }

    fn price_viewport(&self, height: f32) -> Option<Viewport> {
        let rows = self.window_rows();
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for row in rows {
            min = min.min(row.low);
            max = max.max(row.high);
        }
        if !min.is_finite() || !max.is_finite() || min >= max {
            return None;
        }
        let pad = (max - min) * 0.02;
        Viewport::new(PriceScaleMode::Normal, min - pad, max + pad, height, BAR_SPACING, min, false)
            .ok()
    }

    fn paint_price(&self, ui: &mut egui::Ui) {
        let rows = self.window_rows();
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 300.0),
            egui::Sense::hover(),
        );
        let mut painter = ui.painter_at(rect);
        painter.set_clip_rect(rect);
        painter.rect_filled(rect, 0.0, theme_color(&self.theme, "background"));
        let Some(viewport) = self.price_viewport(rect.height()) else {
            return;
        };
        painter.text(
            rect.left_top() + egui::vec2(4.0, 2.0),
            egui::Align2::LEFT_TOP,
            "PRICE",
            egui::FontId::proportional(12.0),
            theme_color(&self.theme, "grid"),
        );
        let bullish = &self.view.ribbon.bullish;
        if let Ok(geoms) = candle_geoms(rows, bullish, &viewport) {
            let geoms = place_candles(geoms, rect.left_top());
            paint_candles(&painter, &geoms, BAR_SPACING * 0.7, &self.theme);
        }
        let bands = ribbon_bands(&self.view.ribbon, &viewport);
        let bands = place_bands(bands, rect.left_top());
        paint_ribbon(&painter, &bands, BAR_SPACING * 0.4, &self.theme, bullish, FILL_ALPHA as f32);
        if let Some(bars) = self.aligned_bars() {
            let provider = DailySessionProvider::continuous();
            let lines = place_breaks(break_lines(&viewport, bars, &provider), rect.left());
            paint_session_breaks(&painter, &lines, rect.top(), rect.bottom(), &self.theme);
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
            eprintln!("DEBUG osc viewport refused: min={min} max={max}");
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

    fn paint_oscillator(&self, ui: &mut egui::Ui) {
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 150.0),
            egui::Sense::hover(),
        );
        let mut painter = ui.painter_at(rect);
        painter.set_clip_rect(rect);
        painter.rect_filled(rect, 0.0, theme_color(&self.theme, "background"));
        let Some(viewport) = self.oscillator_viewport(rect.height()) else {
            return;
        };
        painter.text(
            rect.left_top() + egui::vec2(4.0, 2.0),
            egui::Align2::LEFT_TOP,
            "OSC",
            egui::FontId::proportional(12.0),
            theme_color(&self.theme, "grid"),
        );
        for (values, key) in [(&self.view.fast, "fast"), (&self.view.slow, "slow")] {
            let points = place_points(oscillator_points(values, &viewport), rect.left_top());
            paint_oscillator(&painter, &points, key, &self.theme);
        }
        let grid = egui::Stroke::new(1.0, theme_color(&self.theme, "grid"));
        for signed in [self.view.threshold, -self.view.threshold] {
            if let Some(y) = viewport.price_to_y(signed) {
                let y = y + rect.top();
                painter.line_segment(
                    [egui::Pos2::new(rect.left(), y), egui::Pos2::new(rect.right(), y)],
                    grid,
                );
            }
        }
        let dots = place_dots(pivot_dots(&self.view.pivots, &self.view.fast, &viewport), rect.left_top());
        paint_pivot_dots(&painter, &dots, &self.theme);
    }
}

fn main() {
    let mut path = default_history();
    let mut check = false;
    for arg in std::env::args().skip(1) {
        if arg == "--check" {
            check = true;
        } else {
            path = PathBuf::from(arg);
        }
    }
    let app = match TerminalApp::load(&path) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("protopyne-gui: {e}");
            std::process::exit(1);
        }
    };
    if check {
        println!("protopyne-gui check: {}", app.summary());
        return;
    }
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
