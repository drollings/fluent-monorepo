//! Terminal preferences: the serialized favorites, indicator loadout, and
//! non-default settings behind `protopyne.json`.
//!
//! One [`TerminalPrefs`] document is the whole persisted surface. It is pure
//! data — no paths, no I/O — so the workspace snapshots and applies it and the
//! composition root (an example binary) owns reading and writing the file
//! beside the historical/indicator/library roots. Absent fields fall back to
//! the live workspace defaults, so a partial or older file never strands a
//! setting.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The overlay's starred entries, split by catalog: the Favorites filter in
/// each picker keys on the matching set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Favorites {
    /// Starred ticker symbols.
    #[serde(default)]
    pub symbols: Vec<String>,
    /// Starred indicator ids.
    #[serde(default)]
    pub indicators: Vec<String>,
    /// Starred bookmark ids (`{symbol}|{timeframe}|{time}`).
    #[serde(default)]
    pub bookmarks: Vec<String>,
}

/// One bookmarked candle: the bar time plus the timeframe it was captured
/// on, so a 5M bookmark restores its own grid even when the chart has moved
/// on. The id is `{symbol}|{timeframe}|{time}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookmarkEntry {
    /// Bar epoch seconds.
    pub time: i64,
    /// Timeframe label the bookmark was captured on (`5M`, …).
    #[serde(default)]
    pub timeframe: String,
}

impl BookmarkEntry {
    /// Stable catalog id under `symbol`.
    #[must_use]
    pub fn id(&self, symbol: &str) -> String {
        format!("{symbol}|{}|{}", self.timeframe, self.time)
    }

    /// Picker row title: calendar date, clock time, and timeframe.
    #[must_use]
    pub fn title(&self) -> String {
        let when = chrono::DateTime::from_timestamp(self.time, 0).map_or_else(
            || self.time.to_string(),
            |moment| moment.format("%Y-%m-%d %H:%M").to_string(),
        );
        format!("{when} · {}", self.timeframe)
    }
}
/// One hot-slotted indicator's persisted state: its stable id, its visibility,
/// and every declared config/style field as a string (the `FieldAccess`
/// contract's own spelling).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotPrefs {
    /// Stable slot id (the indicator registry key).
    pub id: String,
    /// Whether the slot paints.
    #[serde(default = "default_true")]
    pub visible: bool,
    /// Config fields, in field-name order; an absent field keeps its default.
    #[serde(default)]
    pub config: BTreeMap<String, String>,
    /// Style fields (`color_*`, `line_width`); an absent field keeps its
    /// default.
    #[serde(default)]
    pub style: BTreeMap<String, String>,
}

fn default_true() -> bool {
    true
}

/// One ticker's saved layout: the chart settings the Save Layout button
/// snapshots for exactly one ticker, overwriting that ticker's previous
/// entry. Layouts never leak across tickers — switching tickers stashes the
/// current layout and applies the incoming ticker's own.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TickerLayout {
    /// Timeframe key label (`1M`/`5M`/…).
    #[serde(default)]
    pub timeframe: Option<String>,
    /// Range preset label (`1M`/`3M`/`YTD`/…). Absent when a custom
    /// Beginning start rules instead.
    #[serde(default)]
    pub range: Option<String>,
    /// Custom window start (epoch seconds) from the Beginning picker.
    #[serde(default)]
    pub custom_start: Option<i64>,
    /// Chart type name (`Candles`/`Line`/…).
    #[serde(default)]
    pub chart_type: Option<String>,
    /// Price-pane share of the canvas height.
    #[serde(default)]
    pub pane_share: Option<f32>,
    /// Indicator loadout, in stack (paint) order.
    #[serde(default)]
    pub slots: Vec<SlotPrefs>,
}

/// A stored backtest run configuration: the run params as text. The spec
/// document itself is not persisted (it is re-seeded from the served schema
/// on every boot); only the operator's run params round-trip. Best-effort
/// like every other prefs arm: absent or partial reads leave the live form
/// untouched.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BacktestPrefs {
    /// Deterministic seed, as text.
    #[serde(default)]
    pub seed: Option<String>,
    /// Opening equity, decimal text.
    #[serde(default)]
    pub initial_equity: Option<String>,
    /// Window start (RFC3339), or absent for the tape start.
    #[serde(default)]
    pub start: Option<String>,
    /// Window end (RFC3339), or absent for the tape end.
    #[serde(default)]
    pub end: Option<String>,
}

/// The persisted terminal state: per-ticker layouts (written only by the Save
/// Layout button), the session selection, the starred entries, and the
/// per-ticker bookmarks.
///
/// Every field is optional so an absent or partial file leaves the live
/// defaults in place; a present field wins. Nothing is written unless the
/// operator presses Save Layout — the composition root gates the write on
/// [`ChartWorkspace::take_save_request`](crate::workspace::ChartWorkspace::take_save_request).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TerminalPrefs {
    /// Active ticker.
    #[serde(default)]
    pub active_symbol: Option<String>,
    /// Saved layouts by ticker: the Save Layout button overwrites exactly
    /// the active ticker's entry.
    #[serde(default)]
    pub tickers: BTreeMap<String, TickerLayout>,
    /// Starred catalog entries (symbols, indicators, bookmarks).
    #[serde(default)]
    pub favorites: Favorites,
    /// Bookmarked candles by ticker, in capture order.
    #[serde(default)]
    pub bookmarks: BTreeMap<String, Vec<BookmarkEntry>>,
    /// Dock drawer open state.
    #[serde(default)]
    pub dock_open: Option<bool>,
    /// Theme key.
    #[serde(default)]
    pub theme: Option<String>,
    /// Oscillator kind defaults behind the dock Oscillators section.
    #[serde(default)]
    pub oscillators: crate::oscillator_panel::OscillatorPanelState,
    /// Stored backtest run configuration (run params only).
    #[serde(default)]
    pub backtest: Option<BacktestPrefs>,
    // Legacy flat layout (pre-ticker documents): read-only migration source,
    // never serialized. When `tickers` is empty and these name a ticker, they
    // fold into that ticker's layout once (see `ticker_layout`).
    /// Legacy active ticker.
    #[serde(default, skip_serializing)]
    pub symbol: Option<String>,
    /// Legacy timeframe key label.
    #[serde(default, skip_serializing)]
    pub timeframe: Option<String>,
    /// Legacy range preset label.
    #[serde(default, skip_serializing)]
    pub range: Option<String>,
    /// Legacy chart type name.
    #[serde(default, skip_serializing)]
    pub chart_type: Option<String>,
    /// Legacy price-pane share.
    #[serde(default, skip_serializing)]
    pub pane_share: Option<f32>,
    /// Legacy indicator loadout.
    #[serde(default, skip_serializing)]
    pub slots: Vec<SlotPrefs>,
}

impl TerminalPrefs {
    /// Whether the document carries no setting at all (a fresh file).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// The saved layout for `symbol`: the tickers map wins; a legacy flat
    /// document (no map, old fields naming this ticker) folds into an
    /// equivalent layout so an older file still restores.
    #[must_use]
    pub fn ticker_layout(&self, symbol: &str) -> Option<TickerLayout> {
        if let Some(layout) = self.tickers.get(symbol) {
            return Some(layout.clone());
        }
        let legacy_symbol = self.symbol.as_deref().filter(|s| !s.trim().is_empty())?;
        if legacy_symbol != symbol {
            return None;
        }
        Some(TickerLayout {
            timeframe: self.timeframe.clone(),
            range: self.range.clone(),
            custom_start: None,
            chart_type: self.chart_type.clone(),
            pane_share: self.pane_share,
            slots: self.slots.clone(),
        })
    }
}

/// The persisted file name, beside the historical/indicator/library roots.
pub const PREFS_FILE: &str = "protopyne.json";

/// The `protopyne.json` path for a history entry point: the nearest ancestor
/// directory that carries an `indicators` or `libraries` sibling (the data
/// root), else the entry's own parent.
///
/// The composition root owns the path so the workspace never does file I/O;
/// this keeps the search rule in one place shared by every shell.
#[must_use]
pub fn prefs_path(history: &Path) -> PathBuf {
    let start = if history.is_dir() {
        history
    } else {
        history.parent().unwrap_or(history)
    };
    let mut dir = Some(start);
    while let Some(candidate) = dir {
        if candidate.join("indicators").is_dir() || candidate.join("libraries").is_dir() {
            return candidate.join(PREFS_FILE);
        }
        dir = candidate.parent();
    }
    start.join(PREFS_FILE)
}
