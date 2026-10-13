//! Trading terminal widgets: multi-timeframe table and timeframe toolbar.
//!
//! The table rows are plain upstream-filled display data (no kernel calls at
//! the paint edge); the toolbar presets come straight from the canonical
//! range list, so preset order can never drift from the range truth.

use protopyne_chart_model::DateRange;

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
