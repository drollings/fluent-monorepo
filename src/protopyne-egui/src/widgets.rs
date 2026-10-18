//! Trading terminal widgets: multi-timeframe table and Material top bar.
//!
//! The table rows are plain upstream-filled display data (no kernel calls at
//! the paint edge). The top bar is the terminal's Material app bar
//! (workstation + touch): a dock toggle, a ticker button opening the symbol
//! catalogue, an indicator selector, the timeframe selector, a display-mode
//! pulldown, a `Range` button opening the interactive range dialog (presets
//! atop a Beginning calendar, applied with OK), a refresh button, and a
//! label-free pan cluster (rewind-to-beginning, rewind, forward,
//! forward-to-last). Every control draws one height and one bold type style,
//! so the bar reads as one Material row.
//! Selecting a range grows, prunes, or unloads the loaded history to that span
//! on demand; the chart never pre-fetches a range it is not showing.

use chrono::{Datelike as _, NaiveDate};
use fluent_wvr::prelude::*;
use fluent_wvr::{Describable, FieldAccess};
use protopyne_chart_model::{ChartType, DateRange};
use protopyne_types::{check_for_execute, valid_output, Validate};

use crate::chart::TimeframeKey;

/// Chart types the top bar offers: candles, the close line/area, and
/// Heikin-Ashi (bar-backed, converted over the core bar pipeline before
/// windowing). The pulldown lists exactly this array, so the menu can never
/// drift from the paint path.
pub const TOP_BAR_CHART_TYPES: [ChartType; 4] = [
    ChartType::Candles,
    ChartType::Line,
    ChartType::Area,
    ChartType::Heikin,
];

/// Label of the top-bar indicator selector.
pub const INDICATOR_SEARCH_LABEL: &str = "Indicators";
/// Legacy label kept for the golden pin (the ticker button now shows the
/// symbol itself; this names the catalogue it opens).
pub const SYMBOL_SEARCH_LABEL: &str = "Symbols";

/// Ephemeral top-bar menu drafts: the Beginning and Jump-To-Time text fields
/// inside the data-range menu. A `Component` so the menu edits through the
/// same `FieldAccess` contract as every other control-plane surface, and the
/// two drafts round-trip through `get_field`/`set_field` without a second
/// spelling.
#[derive(
    Debug,
    Clone,
    Default,
    bon::Builder,
    serde::Serialize,
    serde::Deserialize,
    FieldAccess,
    Describable,
)]
#[builder(start_fn = new)]
pub struct TopBarMenuState {
    /// Beginning draft (`YYYY-MM-DD`): the custom window start applied on Go.
    #[field(desc = "Beginning date draft (YYYY-MM-DD)", max_len = 16)]
    #[builder(default = String::new())]
    #[serde(default)]
    pub beginning_text: String,
    /// Jump target draft (`YYYY-MM-DD HH:MM`): the time to scroll to on Go.
    #[field(desc = "Jump target draft (YYYY-MM-DD HH:MM)", max_len = 32)]
    #[builder(default = String::new())]
    #[serde(default)]
    pub jump_text: String,
}

impl Validate for TopBarMenuState {
    type Err = String;
    fn validate(&self) -> Result<(), Self::Err> {
        Ok(())
    }
}

impl WorkUnit for TopBarMenuState {
    fn name(&self) -> &str {
        "top_bar_menu"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        check_for_execute(self, "top bar menu invalid")?;
        Ok(valid_output("top bar menu ready".to_owned()))
    }
}

impl_component!(TopBarMenuState);

/// The month a Beginning calendar is showing. Owned by the workspace so the
/// picker navigates months without a schema field per cursor; the picked day
/// still lands in [`TopBarMenuState::beginning_text`], the one text truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalendarState {
    /// Displayed year.
    pub year: i32,
    /// Displayed month (`1..=12`).
    pub month: u32,
}

impl Default for CalendarState {
    fn default() -> Self {
        Self::today()
    }
}

impl CalendarState {
    /// The calendar opened on the current month.
    #[must_use]
    pub fn today() -> Self {
        let now = chrono::Utc::now().date_naive();
        Self {
            year: now.year(),
            month: now.month(),
        }
    }

    /// Step the displayed month by `months` (negative steps back).
    pub fn shift(&mut self, months: i32) {
        let total = self.year * 12 + (self.month as i32 - 1) + months;
        self.year = total.div_euclid(12);
        self.month = (total.rem_euclid(12) + 1) as u32;
    }

    /// Weekday-aligned day cells starting Monday: `None` pads the lead-in,
    /// then the month's days as `Some(day)`. Empty when the cursor is
    /// out of range (a refused month, never a substituted one).
    #[must_use]
    pub fn cells(&self) -> Vec<Option<u32>> {
        let Some(first) = NaiveDate::from_ymd_opt(self.year, self.month, 1) else {
            return Vec::new();
        };
        let lead = first.weekday().num_days_from_monday() as usize;
        let mut cells = vec![None; lead];
        cells.extend((1..=days_in_month(self.year, self.month)).map(Some));
        cells
    }

    /// `(year, month)` as a `YYYY-MM` label.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{:04}-{:02}", self.year, self.month)
    }
}

/// Days in `month` of `year`: one step past the month minus one day, so leap
/// years come from the calendar itself, never a table.
fn days_in_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .and_then(|first| first.pred_opt())
        .map_or(30, |last| last.day())
}

/// Material month calendar: a header of `◀ label ▶` and a Mon-first day grid.
/// Clicking a day returns its `YYYY-MM-DD` text (the Beginning draft's
/// spelling) and highlights `selected` when the day matches. Pure paint: the
/// caller owns the cursor and the text.
pub fn paint_calendar(
    ui: &mut egui::Ui,
    state: &mut CalendarState,
    selected: Option<NaiveDate>,
) -> Option<String> {
    let mut picked = None;
    ui.horizontal(|row| {
        if row
            .small_button("◀")
            .on_hover_text("Previous month")
            .clicked()
        {
            state.shift(-1);
        }
        row.label(state.label());
        if row.small_button("▶").on_hover_text("Next month").clicked() {
            state.shift(1);
        }
    });
    let cells = state.cells();
    if cells.is_empty() {
        ui.label("unavailable month");
        return None;
    }
    egui::Grid::new(("calendar_grid", state.year, state.month))
        .num_columns(7)
        .show(ui, |grid| {
            for (index, day) in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"]
                .into_iter()
                .enumerate()
            {
                grid.label(day);
                if index == 6 {
                    grid.end_row();
                }
            }
            for (index, cell) in cells.iter().enumerate() {
                match cell {
                    Some(day) => {
                        let date = NaiveDate::from_ymd_opt(state.year, state.month, *day);
                        let active = date.is_some_and(|date| Some(date) == selected);
                        if grid.selectable_label(active, format!("{day:>2}")).clicked() {
                            if let Some(date) = date {
                                picked = Some(date.format("%Y-%m-%d").to_string());
                            }
                        }
                    }
                    None => {
                        grid.label("  ");
                    }
                }
                if index % 7 == 6 {
                    grid.end_row();
                }
            }
            // Close a partial trailing week so the grid never leaves a row open.
            if cells.len() % 7 != 0 {
                grid.end_row();
            }
        });
    picked
}

/// What the top bar raised this frame: a selection, a modal opener, a
/// refresh request, a view fit/scroll, or a dock/menu draft commit.
#[derive(Debug, Clone, PartialEq)]
pub enum TopBarAction {
    /// The dock toggle (far left of the bar) flipped the left drawer.
    ToggleDock,
    /// The symbol selector picked a ticker.
    PickedSymbol(String),
    /// The ticker button opened the Symbol Selection Modal.
    OpenSymbolSearch,
    /// A timeframe selector picked a key.
    PickedTimeframe(TimeframeKey),
    /// The Range button opened (or closed) the interactive range dialog.
    ToggleRangeMenu,
    /// The chart-type selector picked a type.
    PickedChartType(ChartType),
    /// The indicator search trigger opened the Indicator Search modal.
    OpenIndicatorSearch,
    /// The refresh button asked for fresh gateway reads.
    Refresh,
    /// The rewind-to-beginning button asked to show the earliest loaded bar.
    RewindToBeginning,
    /// The rewind button asked to step back one pan unit.
    Rewind,
    /// The forward button asked to step ahead one pan unit.
    Forward,
    /// The forward-to-last button asked to show the latest loaded bar.
    ForwardToEnd,
}

/// Every top-bar control draws at this height, so the app bar reads as one
/// row of Material buttons rather than a ragged mix.
pub const TOP_BAR_BUTTON_HEIGHT: f32 = 30.0;
/// Top-bar label size: a step above the body text, bold, for workstation
/// legibility (Material app-bar emphasis).
pub const TOP_BAR_FONT_SIZE: f32 = 15.0;

/// The one top-bar label treatment: bold at [`TOP_BAR_FONT_SIZE`], so every
/// button, menu button, and chip shares one type style.
#[must_use]
pub fn bar_text(label: &str) -> egui::RichText {
    egui::RichText::new(label).strong().size(TOP_BAR_FONT_SIZE)
}

/// One fixed-size top-bar button with the shared label treatment.
fn bar_button(ui: &mut egui::Ui, width: f32, label: &str) -> egui::Response {
    ui.add_sized(
        [width, TOP_BAR_BUTTON_HEIGHT],
        egui::Button::new(bar_text(label)),
    )
}

/// One top-bar menu button (opens a popup) with the shared label treatment.
fn bar_menu_button(ui: &mut egui::Ui, label: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    let _ = ui.menu_button(bar_text(label), add_contents);
}

/// Paint the terminal Material top bar; returns the one action this frame
/// raised, if any.
///
/// Left to right: dock toggle, ticker, indicator selector, the timeframe
/// selector, display-mode pulldown, the `Range` button (opens the interactive
/// range dialog), refresh button, and the label-free pan cluster
/// (rewind-to-beginning, rewind, forward, forward-to-last). Every control
/// draws at [`TOP_BAR_BUTTON_HEIGHT`] with the shared bold [`bar_text`]
/// style, so the app bar reads as one material row.
/// An untouched frame raises nothing.
#[allow(clippy::too_many_arguments)]
pub fn paint_top_bar(
    ui: &mut egui::Ui,
    symbol: &str,
    timeframe: TimeframeKey,
    range_label: &str,
    chart_type: ChartType,
    dock_open: bool,
) -> Option<TopBarAction> {
    let mut action = None;
    ui.horizontal(|bar| {
        bar.spacing_mut().item_spacing.x = 8.0;
        // Every control draws one height; menu buttons follow `interact_size`.
        bar.style_mut().spacing.interact_size.y = TOP_BAR_BUTTON_HEIGHT;
        // 1. Dock toggle — far left, same height as every other button.
        if bar
            .add_sized(
                [36.0, TOP_BAR_BUTTON_HEIGHT],
                egui::Button::new(bar_text("☰")).selected(dock_open),
            )
            .on_hover_text("Toggle dock drawer")
            .clicked()
        {
            action = Some(TopBarAction::ToggleDock);
        }
        bar.separator();
        // 2. Ticker button — current symbol, opens the catalogue.
        if bar_button(bar, 110.0, &format!("📈 {symbol} ▾"))
            .on_hover_text("Symbol catalogue")
            .clicked()
        {
            action = Some(TopBarAction::OpenSymbolSearch);
        }
        bar.separator();
        // 3. Timeframe selector — right of the indicators button, per the
        // app-bar grouping of chart controls.
        if let Some(key) = paint_timeframe_toolbar(bar, timeframe) {
            action = Some(TopBarAction::PickedTimeframe(key));
        }
        bar.separator();
        // 4. Refresh button: fresh gateway reads (replaces the dock's old
        // gateway-terminal refresh affordance).
        if bar_button(bar, 32.0, "⟳")
            .on_hover_text("Refresh gateway reads")
            .clicked()
        {
            action = Some(TopBarAction::Refresh);
        }
        bar.separator();
        // 5. Pan buttons: jump to the beginning, step back one pan unit,
        // step forward one pan unit, jump to the last bar. Each pan refits
        // the visible window's Y axis to the pane height. These used to live
        // on the price-axis right-click menu; they ride the bar now so the
        // eyes stay on the data.
        if bar
            .add_sized(
                [TOP_BAR_BUTTON_HEIGHT, TOP_BAR_BUTTON_HEIGHT],
                egui::Button::new(bar_text("⏮")),
            )
            .on_hover_text("Jump to the beginning of the loaded data")
            .clicked()
        {
            action = Some(TopBarAction::RewindToBeginning);
        }
        if bar
            .add_sized(
                [TOP_BAR_BUTTON_HEIGHT, TOP_BAR_BUTTON_HEIGHT],
                egui::Button::new(bar_text("⏪")),
            )
            .on_hover_text("Step back one timescale unit (day/week/month)")
            .clicked()
        {
            action = Some(TopBarAction::Rewind);
        }
        if bar
            .add_sized(
                [TOP_BAR_BUTTON_HEIGHT, TOP_BAR_BUTTON_HEIGHT],
                egui::Button::new(bar_text("⏩")),
            )
            .on_hover_text("Step forward one timescale unit (day/week/month)")
            .clicked()
        {
            action = Some(TopBarAction::Forward);
        }
        if bar
            .add_sized(
                [TOP_BAR_BUTTON_HEIGHT, TOP_BAR_BUTTON_HEIGHT],
                egui::Button::new(bar_text("⏭")),
            )
            .on_hover_text("Jump to the end of the loaded data")
            .clicked()
        {
            action = Some(TopBarAction::ForwardToEnd);
        }
        bar.separator();
        // 6. Indicator selector.
        if bar_button(bar, 140.0, &format!("📊 {INDICATOR_SEARCH_LABEL} ▾"))
            .on_hover_text("Indicator catalogue")
            .clicked()
        {
            action = Some(TopBarAction::OpenIndicatorSearch);
        }
        bar.separator();
        // 7. Display-mode pulldown.
        let current_label = chart_type_label(chart_type);
        bar_menu_button(bar, &format!("🕯 {current_label} ▾"), |menu_ui| {
            for kind in TOP_BAR_CHART_TYPES {
                if menu_ui
                    .selectable_label(kind == chart_type, bar_text(chart_type_label(kind)))
                    .clicked()
                {
                    action = Some(TopBarAction::PickedChartType(kind));
                    menu_ui.close();
                }
            }
        });
        bar.separator();
        // 8. Range button: opens the interactive range dialog (presets atop a
        // Beginning calendar, applied with OK). The dialog, not this button,
        // raises the range/beginning actions, so a preset never disappears the
        // chart mid-click.
        if bar_button(bar, 150.0, &format!("📅 Range: {range_label} ▾"))
            .on_hover_text("Chart range and Beginning")
            .clicked()
        {
            action = Some(TopBarAction::ToggleRangeMenu);
        }
    });
    action
}

/// Paint the interactive range dialog: preset spans and an interactive
/// Beginning calendar over the workspace drafts, applied only when the
/// operator presses OK (Cancel discards).
///
/// Returns the chosen range on OK, or `None` while the dialog is untouched.
/// The caller owns the open flag, the draft preset, and the calendar cursor,
/// so the dialog is pure paint over one truth.
pub fn paint_range_dialog(
    ui: &mut egui::Ui,
    draft_range: &mut Option<DateRange>,
    beginning_text: &mut String,
    jump_text: &mut String,
    calendar: &mut CalendarState,
    active_range: Option<DateRange>,
) -> Option<RangeChoice> {
    let mut choice = None;
    // One Material row of slim preset chips: the widened dialog holds all six
    // without wrapping.
    ui.horizontal(|row| {
        row.spacing_mut().item_spacing.x = 8.0;
        for preset in DateRange::chart_presets() {
            let selected = (*draft_range == Some(preset))
                || (draft_range.is_none()
                    && beginning_text.trim().is_empty()
                    && Some(preset) == active_range);
            if row
                .add_sized(
                    [RANGE_PRESET_BUTTON_WIDTH, TOP_BAR_BUTTON_HEIGHT],
                    egui::Button::new(bar_text(preset.label())).selected(selected),
                )
                .on_hover_text(preset.tooltip())
                .clicked()
            {
                *draft_range = Some(preset);
                beginning_text.clear();
            }
        }
    });
    ui.separator();
    ui.label(bar_text("Beginning (from date to the current data)"));
    let selected = NaiveDate::parse_from_str(beginning_text.trim(), "%Y-%m-%d").ok();
    if let Some(date) = paint_calendar(ui, calendar, selected) {
        beginning_text.clone_from(&date);
        *draft_range = None;
    }
    ui.horizontal(|row| {
        row.label("From");
        row.add(
            egui::TextEdit::singleline(beginning_text)
                .hint_text("YYYY-MM-DD")
                .desired_width(120.0),
        );
        if !beginning_text.trim().is_empty() && row.small_button("clear").clicked() {
            beginning_text.clear();
        }
    });
    ui.separator();
    ui.label(bar_text("Jump To Time"));
    let mut close = false;
    ui.horizontal(|row| {
        row.add(
            egui::TextEdit::singleline(jump_text)
                .hint_text("2026-03-02 09:30")
                .desired_width(150.0),
        );
        if row.button(bar_text("Go")).clicked() {
            choice = Some(RangeChoice::Jump);
            close = true;
        }
    });
    ui.separator();
    ui.horizontal(|row| {
        if row.button(bar_text("OK")).clicked() {
            choice = Some(RangeChoice::Apply);
            close = true;
        }
        if row.button(bar_text("Cancel")).clicked() {
            choice = Some(RangeChoice::Cancel);
            close = true;
        }
    });
    if close {
        ui.close();
    }
    choice
}

/// Slim preset chip width: the six short labels (`1M`–`All`) fit one row in
/// a widened dialog without crowding.
pub const RANGE_PRESET_BUTTON_WIDTH: f32 = 48.0;
/// Range dialog width: wide enough that the six slim preset chips sit on one
/// Material row with the shared 8dp spacing.
pub const RANGE_DIALOG_WIDTH: f32 = 560.0;

/// What the range dialog's action row raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeChoice {
    /// Apply the drafted range or Beginning.
    Apply,
    /// Discard the draft and close.
    Cancel,
    /// Jump the time axis to the drafted target.
    Jump,
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
    DateRange::default_presets()
        .iter()
        .map(|preset| preset.label())
        .collect()
}

/// Timeframe key labels in toolbar order, straight from [`TimeframeKey`].
#[must_use]
pub fn timeframe_labels() -> Vec<&'static str> {
    TimeframeKey::all().iter().map(|key| key.label()).collect()
}

/// Paint the timeframe selector; returns the newly picked key, if any.
///
/// Chips are the same height and type style as every other top-bar control,
/// so the selector reads as part of the app bar row rather than a sidebar.
pub fn paint_timeframe_toolbar(ui: &mut egui::Ui, current: TimeframeKey) -> Option<TimeframeKey> {
    let mut picked = None;
    ui.horizontal(|ui| {
        for key in TimeframeKey::all() {
            if ui
                .add_sized(
                    [40.0, TOP_BAR_BUTTON_HEIGHT],
                    egui::Button::new(bar_text(key.label())).selected(key == current),
                )
                .clicked()
            {
                picked = Some(key);
            }
        }
    });
    picked
}

/// Paint the multi-timeframe table widget.
pub fn paint_mtf_table(ui: &mut egui::Ui, rows: &[MtfRow]) {
    egui::Grid::new("mtf_table")
        .num_columns(3)
        .striped(true)
        .show(ui, |ui| {
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

#[cfg(test)]
#[path = "../tests/widgets.rs"]
mod tests;
