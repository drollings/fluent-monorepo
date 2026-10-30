//! Trading terminal widgets: multi-timeframe table and Material top bar.
//!
//! The table rows are plain upstream-filled display data (no kernel calls at
//! the paint edge). The top bar is the terminal's Material app bar
//! (workstation + touch): a dock toggle, a ticker button opening the symbol
//! catalogue, an indicator selector, the timeframe selector, a display-mode
//! pulldown, a refresh button, and a label-free pan cluster
//! (rewind-to-beginning, rewind, play/pause, forward, forward-to-last).
//! Chart-range picking (presets atop a Beginning calendar) lives as an
//! expansion of the dock drawer, not as a menu dialog, so the bar stays one
//! row. The replay window is the chart's own visible window at the moment
//! play is pressed — no picker — with its edges marked on the pane divider.
//! Every control draws one height and one bold type style, so the bar reads
//! as one Material row.
//! Selecting a range grows, prunes, or unloads the loaded history to that span
//! on demand; the chart never pre-fetches a range it is not showing.

use chrono::{Datelike as _, NaiveDate};
use fluent_wvr::prelude::*;
use fluent_wvr::{Describable, FieldAccess};
use protopyne_chart_model::{ChartType, DateRange};

use fluent_wvr::validate::{check_for_execute, valid_output, Validate};

use crate::chart::TimeframeKey;

/// Chart types the top bar offers: candles, the close line/area, and
/// Heikin-Ashi (bar-backed, converted over the core bar pipeline before
/// windowing). The pulldown lists exactly this array, so the menu can never
/// drift from the paint path.
pub const TOP_BAR_CHART_TYPES: [ChartType; 3] =
    [ChartType::Candles, ChartType::Line, ChartType::Heikin];

/// How long the pointer must stay down on the chart before the press
/// becomes a long-press and opens the canvas context menu, in egui UI
/// seconds.
pub const CHART_LONG_PRESS_SECS: f64 = 0.5;

/// Replay transport speed in candles at the selected timeframe, chosen from
/// the top-bar speed menu (the same switch-menu shape as the display-mode
/// pulldown). Rates name chart candles — one 5M candle per second at Normal
/// on a 5M chart replays five 1M candles per second, appearing as one candle
/// per second — never base bars, so the selected speed feels the same on
/// every timeframe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplaySpeed {
    /// One candle every five seconds.
    Crawl,
    /// One candle per second.
    #[default]
    Normal,
    /// Three candles per second.
    Swift,
    /// Five candles per second.
    Fast,
    /// Ten candles per second.
    Sprint,
    /// As fast as calculated: every frame steps the gateway maximum.
    Full,
}

/// Every replay speed, menu order: slowest first, uncapped last.
pub const TOP_BAR_REPLAY_SPEEDS: [ReplaySpeed; 6] = [
    ReplaySpeed::Crawl,
    ReplaySpeed::Normal,
    ReplaySpeed::Swift,
    ReplaySpeed::Fast,
    ReplaySpeed::Sprint,
    ReplaySpeed::Full,
];

impl ReplaySpeed {
    /// Short label for the speed menu button.
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Crawl => "0.2/s",
            Self::Normal => "1/s",
            Self::Swift => "3/s",
            Self::Fast => "5/s",
            Self::Sprint => "10/s",
            Self::Full => "∞",
        }
    }

    /// Menu-row label for the speed control.
    pub fn label(self) -> &'static str {
        match self {
            Self::Crawl => "slow · 1 candle / 5s",
            Self::Normal => "1 candle / s",
            Self::Swift => "3 candles / s",
            Self::Fast => "fast · 5 candles / s",
            Self::Sprint => "10 candles / s",
            Self::Full => "full speed",
        }
    }

    /// Paced steps per second in display candles, or `None` when uncapped.
    pub fn candles_per_sec(self) -> Option<f64> {
        match self {
            Self::Crawl => Some(0.2),
            Self::Normal => Some(1.0),
            Self::Swift => Some(3.0),
            Self::Fast => Some(5.0),
            Self::Sprint => Some(10.0),
            Self::Full => None,
        }
    }

    /// Paced steps per second in display candles, or `None` when uncapped.
    /// Kept as the bar-denominated alias: bars and candles are the same
    /// display unit, renamed for the terminal.
    pub fn bars_per_sec(self) -> Option<f64> {
        self.candles_per_sec()
    }
}

/// Label of the top-bar indicator selector.
pub const INDICATOR_SEARCH_LABEL: &str = "Indicators";
/// Label of the top-bar bookmark picker.
pub const BOOKMARK_SEARCH_LABEL: &str = "Bookmarks";
/// Legacy label kept for the golden pin (the ticker button now shows the
/// symbol itself; this names the catalogue it opens).
pub const SYMBOL_SEARCH_LABEL: &str = "Symbols";
/// Pulldown chevron painted after every catalogue button label.
///
/// U+25BE (▾): the conventional small down triangle, carried by the
/// monospace bundle's proportional-fallback leg (and by Hack). The previous
/// U+2BC6 (⯆) is in no chain font — not the bundles, not the egui
/// defaults, not even DejaVu — so it painted as tofu everywhere.
pub const PULLDOWN_CHEVRON: &str = "▾";

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
///
/// `salt` names this calendar instance: every calendar painted in the same
/// UI needs a distinct salt, or their day grids share an egui id and the
/// second use mispaints (two replay calendars on the same month collide
/// without it).
pub fn paint_calendar(
    ui: &mut egui::Ui,
    salt: &str,
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
    egui::Grid::new((salt, state.year, state.month))
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

/// What the top bar raised this frame: a selection, a picker opener, a
/// refresh request, a view fit/scroll, or the replay transport toggle.
#[derive(Debug, Clone, PartialEq)]
pub enum TopBarAction {
    /// The dock toggle (far left of the bar) flipped the left drawer.
    ToggleDock,
    /// The symbol selector picked a ticker.
    PickedSymbol(String),
    /// The ticker button opened the symbol picker.
    OpenSymbolSearch,
    /// A timeframe selector picked a key.
    PickedTimeframe(TimeframeKey),
    /// The chart-type selector picked a type.
    PickedChartType(ChartType),
    /// The indicator search trigger opened the indicator picker.
    OpenIndicatorSearch,
    /// The bookmark button opened the bookmark picker for the active ticker.
    OpenBookmarkSearch,
    /// The magnifier button toggled the Jump To Time popup.
    OpenJumpSearch,
    /// The Save Layout button asked to persist the active ticker's layout.
    SaveLayout,
    /// The refresh button asked for fresh gateway reads.
    Refresh,
    /// The rewind-to-beginning button asked to show the earliest loaded bar.
    RewindToBeginning,
    /// The rewind button asked to step back one pan unit.
    Rewind,
    /// The replay play/pause button toggled the backtest transport.
    ToggleReplayPlay,
    /// The replay speed menu picked a pace.
    PickedReplaySpeed(ReplaySpeed),
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

/// One fixed-width top-bar menu button (opens a popup) with the shared label
/// treatment. The width is a minimum, like `bar_button`'s exact size for the
/// usual short labels: a wider label still fits instead of clipping.
fn bar_menu_button(
    ui: &mut egui::Ui,
    width: f32,
    label: &str,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let _ = egui::containers::menu::MenuButton::from_button(
        egui::Button::new(bar_text(label)).min_size(egui::vec2(width, TOP_BAR_BUTTON_HEIGHT)),
    )
    .ui(ui, add_contents);
}

/// Paint the terminal Material top bar; returns the one action this frame
/// raised, if any.
///
/// Left to right: dock toggle, ticker, save layout, the timeframe selector,
/// display-mode pulldown, refresh button, and the label-free pan cluster
/// (rewind-to-beginning, rewind, play/pause, replay-speed menu, forward,
/// forward-to-last), jump-to-time, bookmarks, indicator selector. Tapping
/// play toggles the backtest transport; the speed menu beside it picks the
/// replay pace from the six documented rates, like the Candles pulldown.
/// Every control draws at [`TOP_BAR_BUTTON_HEIGHT`] with the shared bold
/// [`bar_text`] style, so the app bar reads as one material row.
/// An untouched frame raises nothing.
///
/// The picker buttons (ticker, bookmarks, indicators, jump) record their
/// button rects into the anchor outs on click: the workspace paints the
/// corresponding titlebar-less popup anchored below the button, like the
/// Candles pulldown, so no picker needs its own window chrome.
///
/// Range picking lives in the dock drawer, not here: the bar carries no
/// dialog state, so a preset or calendar day can never blank the pane
/// mid-gesture.
#[allow(clippy::too_many_arguments)]
pub fn paint_top_bar(
    ui: &mut egui::Ui,
    symbol: &str,
    timeframe: TimeframeKey,
    chart_type: ChartType,
    dock_open: bool,
    replay_playing: bool,
    replay_speed: ReplaySpeed,
    symbol_anchor: &mut Option<egui::Rect>,
    indicator_anchor: &mut Option<egui::Rect>,
    bookmark_anchor: &mut Option<egui::Rect>,
    jump_anchor: &mut Option<egui::Rect>,
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
        // 2. Ticker button — current symbol, opens the catalogue. The rect
        // is stored so the popup hangs below this button.
        let ticker_response = bar_button(bar, 100.0, &format!("📈 {symbol} {PULLDOWN_CHEVRON}"))
            .on_hover_text("Symbol catalogue");
        if ticker_response.clicked() {
            *symbol_anchor = Some(ticker_response.rect);
            action = Some(TopBarAction::OpenSymbolSearch);
        }
        bar.separator();
        // 3. Refresh button: fresh gateway reads (replaces the dock's old
        // gateway-terminal refresh affordance).
        if bar_button(bar, 32.0, "⟳")
            .on_hover_text("Refresh gateway reads")
            .clicked()
        {
            action = Some(TopBarAction::Refresh);
        }
        // 4. Jump To Time — magnifier toggling the jump popup below this
        // button, like the Candles pulldown. The popup carries the time
        // input, Go, and a calendar that sets the date part.
        let jump_response = bar
            .add_sized(
                [TOP_BAR_BUTTON_HEIGHT, TOP_BAR_BUTTON_HEIGHT],
                egui::Button::new(bar_text("🔍")),
            )
            .on_hover_text("Jump to time");
        if jump_response.clicked() {
            *jump_anchor = Some(jump_response.rect);
            action = Some(TopBarAction::OpenJumpSearch);
        }
        // 5. Bookmarks — the active ticker's bookmarked candles. The rect
        // is stored so the picker hangs below this button.
        let bookmark_response = bar_button(bar, 40.0, &format!("🔖 {PULLDOWN_CHEVRON}"))
            .on_hover_text("Bookmarked candles for this ticker");
        if bookmark_response.clicked() {
            *bookmark_anchor = Some(bookmark_response.rect);
            action = Some(TopBarAction::OpenBookmarkSearch);
        }
        bar.separator();
        // 6. Indicator selector. The rect is stored so the picker hangs
        // below this button.
        let indicator_response =
            bar_button(bar, 130.0, &format!("📊 Indicators {PULLDOWN_CHEVRON}"))
                .on_hover_text("Indicator catalogue");
        if indicator_response.clicked() {
            *indicator_anchor = Some(indicator_response.rect);
            action = Some(TopBarAction::OpenIndicatorSearch);
        }
        // 7. Display-mode pulldown.
        let current_symbol = chart_type_symbol(chart_type);
        bar_menu_button(
            bar,
            40.0,
            &format!("{current_symbol} {PULLDOWN_CHEVRON}"),
            |menu_ui| {
                for kind in TOP_BAR_CHART_TYPES {
                    let option_symbol = chart_type_symbol(kind);
                    let option_label = chart_type_label(kind);
                    if menu_ui
                        .selectable_label(
                            kind == chart_type,
                            format!("{option_symbol} {option_label}"),
                        )
                        .clicked()
                    {
                        action = Some(TopBarAction::PickedChartType(kind));
                        menu_ui.close();
                    }
                }
            },
        );
        bar.separator();
        // 8. Pan buttons: jump to the beginning, step back one pan unit,
        // the replay play/pause transport, step forward one pan unit, jump
        // to the last bar. Each pan refits the visible window's Y axis to
        // the pane height. These used to live on the price-axis right-click
        // menu; they ride the bar now so the eyes stay on the data. The
        // play/pause button sits between rewind and forward: with no replay
        // window picked it backtests the full loaded history from its first
        // bar, else it replays the picked start..end window.
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
        // The play button toggles the backtest transport with a plain tap;
        // the replay pace lives on the speed menu beside it.
        if bar
            .add_sized(
                [TOP_BAR_BUTTON_HEIGHT, TOP_BAR_BUTTON_HEIGHT],
                egui::Button::new(bar_text(if replay_playing { "⏸" } else { "▶" })),
            )
            .on_hover_text(if replay_playing {
                "Pause the backtest replay"
            } else {
                "Play the backtest replay"
            })
            .clicked()
        {
            action = Some(TopBarAction::ToggleReplayPlay);
        }
        // Replay speed menu: the six documented paces, like the
        // display-mode pulldown. The button shows the current pace.
        bar_menu_button(
            bar,
            56.0,
            &format!("{} {PULLDOWN_CHEVRON}", replay_speed.short_label()),
            |menu_ui| {
                for speed in TOP_BAR_REPLAY_SPEEDS {
                    if menu_ui
                        .selectable_label(
                            speed == replay_speed,
                            format!("{} {}", speed.short_label(), speed.label()),
                        )
                        .clicked()
                    {
                        action = Some(TopBarAction::PickedReplaySpeed(speed));
                        menu_ui.close();
                    }
                }
            },
        );
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
        // 9. Timeframe selector — right of the indicators button, per the
        // app-bar grouping of chart controls.
        if let Some(key) = paint_timeframe_toolbar(bar, timeframe) {
            action = Some(TopBarAction::PickedTimeframe(key));
        }
        bar.separator();
        // 10. Save Layout — persists the active ticker's chart and indicator
        // settings, overwriting that ticker's previous entry. Nothing else
        // writes `protopyne.json`.
        if bar_button(bar, 50.0, "💾")
            .on_hover_text("Save the current ticker's layout")
            .clicked()
        {
            action = Some(TopBarAction::SaveLayout);
        }
    });
    action
}

/// Paint the range picker: preset spans and an interactive Beginning
/// calendar over the workspace drafts. The caller owns the
/// drafts and applies the returned choice; nothing on the chart moves until
/// the caller applies, so a preset click or a calendar day never blanks the
/// pane mid-gesture.
///
/// Dock-embedded (no window of its own): the caller renders this inside its
/// own surface, which grows on the Y axis to fit it, and the choice (if any)
/// applies immediately. Wraps the preset chips so narrow drawers still show
/// every span.
pub fn paint_range_dialog(
    ui: &mut egui::Ui,
    draft_range: &mut Option<DateRange>,
    beginning_text: &mut String,
    calendar: &mut CalendarState,
    active_range: Option<DateRange>,
) -> Option<RangeChoice> {
    let mut choice = None;
    // Slim preset chips in a wrapping row: narrow drawers stack them instead
    // of clipping them.
    ui.horizontal_wrapped(|row| {
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
    if let Some(date) = paint_calendar(ui, "range-beginning", calendar, selected) {
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
    ui.horizontal(|row| {
        if row.button(bar_text("OK")).clicked() {
            choice = Some(RangeChoice::Apply);
        }
        if row.button(bar_text("Cancel")).clicked() {
            choice = Some(RangeChoice::Cancel);
        }
    });
    choice
}

/// Slim preset chip width: the six short labels (`1M`–`All`) share one
/// Material row height with every other top-bar control.
pub const RANGE_PRESET_BUTTON_WIDTH: f32 = 48.0;

/// What the range dialog's action row raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeChoice {
    /// Apply the drafted range or Beginning.
    Apply,
    /// Discard the draft and close.
    Cancel,
}

/// Short top-bar label for one wired chart type.
#[must_use]
pub fn chart_type_symbol(kind: ChartType) -> &'static str {
    match kind {
        ChartType::Candles | ChartType::Heikin => "┿",
        ChartType::Line => "🗠",
        other => other.name(),
    }
}

/// Short top-bar label for one wired chart type.
#[must_use]
pub fn chart_type_label(kind: ChartType) -> &'static str {
    match kind {
        ChartType::Candles => "Candles",
        ChartType::Heikin => "Heikin-Ashi",
        ChartType::Line => "Line",
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

/// Paint the reference status table's rows: the bar the row belongs to
/// plus the eight table rows (position, day P&L, trades today, loss streak,
/// cooldown, halt line, win rate, net P&L), straight from the stored view.
pub fn paint_status_table(ui: &mut egui::Ui, status: &protopyne_view::BacktestStatusView) {
    egui::Grid::new("backtest_status")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            for (label, value) in [
                ("Bar", status.at.as_str()),
                ("Position", status.position.as_str()),
                ("Day P&L", status.day_pnl_pct.as_str()),
                ("Trades today", status.trades_today.as_str()),
                ("Loss streak", &status.loss_streak.to_string()),
                ("Cooldown", status.cooldown.as_str()),
                ("Status", status.status.as_str()),
                ("Win rate", status.win_rate.as_str()),
                ("Net P&L", status.net_pnl.as_str()),
            ] {
                ui.label(label);
                ui.label(value);
                ui.end_row();
            }
        });
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
