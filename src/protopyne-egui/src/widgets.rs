//! Trading terminal widgets: multi-timeframe table and timeframe toolbar.
//!
//! The table rows are plain upstream-filled display data (no kernel calls at
//! the paint edge); the toolbar presets come straight from the canonical
//! range list, so preset order can never drift from the range truth. The top
//! bar is the terminal's control strip: the active ticker, the timeframe and
//! range selectors, the chart-type selector, and the two global search
//! triggers that open the symbol and indicator modals.

use protopyne_chart_model::{ChartType, DateRange};

use crate::chart::TimeframeKey;

/// Chart types the top bar offers: the display-row pictures the workspace
/// canvas can render (candles and the close line/area). Heikin-Ashi needs the
/// core `Bar` pipeline, which the display-row workspace does not hold.
pub const TOP_BAR_CHART_TYPES: [ChartType; 3] =
    [ChartType::Candles, ChartType::Line, ChartType::Area];

/// Label of the top-bar symbol search trigger.
pub const SYMBOL_SEARCH_LABEL: &str = "Symbols";
/// Label of the top-bar indicator search trigger.
pub const INDICATOR_SEARCH_LABEL: &str = "Indicators";

/// What the top bar raised this frame: a selection or a modal opener.
#[derive(Debug, Clone, PartialEq)]
pub enum TopBarAction {
    /// The symbol selector picked a ticker.
    PickedSymbol(String),
    /// The symbol search trigger opened the Symbol Selection Modal.
    OpenSymbolSearch,
    /// A timeframe selector picked a key.
    PickedTimeframe(TimeframeKey),
    /// A range preset picked a range.
    PickedRange(DateRange),
    /// The chart-type selector picked a type.
    PickedChartType(ChartType),
    /// The indicator search trigger opened the Indicator Search modal.
    OpenIndicatorSearch,
}

/// Paint the terminal top bar; returns the one action this frame raised, if
/// any.
///
/// The ticker renders as the leading label (the Symbol Selection Modal is the
/// searchable picker behind the `Symbols` trigger), then the timeframe, range,
/// and chart-type selectors, then the `Indicators` trigger. An untouched frame
/// raises nothing.
pub fn paint_top_bar(
    ui: &mut egui::Ui,
    symbol: &str,
    timeframe: TimeframeKey,
    range: DateRange,
    chart_type: ChartType,
) -> Option<TopBarAction> {
    let mut action = None;
    ui.horizontal(|bar| {
        bar.strong(symbol);
        if bar.button(SYMBOL_SEARCH_LABEL).clicked() {
            action = Some(TopBarAction::OpenSymbolSearch);
        }
        bar.separator();
        if let Some(key) = paint_timeframe_toolbar(bar, timeframe) {
            action = Some(TopBarAction::PickedTimeframe(key));
        }
        bar.separator();
        if let Some(preset) = paint_toolbar(bar, range) {
            action = Some(TopBarAction::PickedRange(preset));
        }
        bar.separator();
        for kind in TOP_BAR_CHART_TYPES {
            if bar.selectable_label(kind == chart_type, chart_type_label(kind)).clicked() {
                action = Some(TopBarAction::PickedChartType(kind));
            }
        }
        bar.separator();
        if bar.button(INDICATOR_SEARCH_LABEL).clicked() {
            action = Some(TopBarAction::OpenIndicatorSearch);
        }
    });
    action
}

/// Short top-bar label for one wired chart type.
#[must_use]
pub fn chart_type_label(kind: ChartType) -> &'static str {
    match kind {
        ChartType::Candles => "Candles",
        ChartType::Heikin => "Heikin",
        ChartType::Line => "Line",
        ChartType::Area => "Area",
        other => other.name(),
    }
}

/// One multi-timeframe table row: plain display data filled upstream (no
/// kernel calls at the paint edge).
#[derive(Debug, Clone, PartialEq)]
pub struct MtfRow {
    /// Timeframe key (for example "M15").
    pub timeframe: String,
    /// Trend label (for example "bull", "bear", "flat").
    pub bias: String,
    /// Signed strength on the display band.
    pub strength: f64,
}

/// Table shape as `(columns, rows)`; three columns (timeframe, bias,
/// strength), one row per entry.
#[must_use]
pub fn mtf_table_shape(rows: &[MtfRow]) -> (usize, usize) {
    (3, rows.len())
}

/// Toolbar labels in preset order, straight from the canonical presets.
#[must_use]
pub fn toolbar_labels() -> Vec<&'static str> {
    DateRange::default_presets().iter().map(|preset| preset.label()).collect()
}

/// Timeframe key labels in toolbar order, straight from [`TimeframeKey`].
#[must_use]
pub fn timeframe_labels() -> Vec<&'static str> {
    TimeframeKey::all().iter().map(|key| key.label()).collect()
}

/// Paint the timeframe selector; returns the newly picked key, if any.
pub fn paint_timeframe_toolbar(ui: &mut egui::Ui, current: TimeframeKey) -> Option<TimeframeKey> {
    let mut picked = None;
    ui.horizontal(|ui| {
        for key in TimeframeKey::all() {
            if ui.selectable_label(key == current, key.label()).clicked() {
                picked = Some(key);
            }
        }
    });
    picked
}

/// Paint the multi-timeframe table widget.
pub fn paint_mtf_table(ui: &mut egui::Ui, rows: &[MtfRow]) {
    egui::Grid::new("mtf_table").num_columns(3).striped(true).show(ui, |ui| {
        ui.label("TF");
        ui.label("Bias");
        ui.label("Strength");
        ui.end_row();
        for row in rows {
            ui.label(&row.timeframe);
            ui.label(&row.bias);
            ui.label(format!("{:.1}", row.strength));
            ui.end_row();
        }
    });
}

/// Paint the timeframe toolbar skeleton; returns the newly picked preset, if any.
pub fn paint_toolbar(ui: &mut egui::Ui, current: DateRange) -> Option<DateRange> {
    let mut picked = None;
    ui.horizontal(|ui| {
        for preset in DateRange::default_presets() {
            let selected = preset == current;
            if ui.selectable_label(selected, preset.label()).clicked() {
                picked = Some(preset);
            }
        }
    });
    picked
}

#[cfg(test)]
#[path = "../tests/widgets.rs"]
mod tests;
