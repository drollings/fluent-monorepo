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
/// each modal keys on the matching set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Favorites {
    /// Starred ticker symbols.
    #[serde(default)]
    pub symbols: Vec<String>,
    /// Starred indicator ids.
    #[serde(default)]
    pub indicators: Vec<String>,
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

/// The persisted terminal state: the session selection, the chart's
/// non-default settings, the starred entries, and the indicator loadout.
///
/// Every field is optional so an absent or partial file leaves the live
/// defaults in place; a present field wins.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TerminalPrefs {
    /// Active ticker.
    #[serde(default)]
    pub symbol: Option<String>,
    /// Active timeframe key label (`1M`/`5M`/…).
    #[serde(default)]
    pub timeframe: Option<String>,
    /// Active range preset label (`1M`/`3M`/`YTD`/…).
    #[serde(default)]
    pub range: Option<String>,
    /// Active chart type name (`Candles`/`Line`/…).
    #[serde(default)]
    pub chart_type: Option<String>,
    /// Theme key.
    #[serde(default)]
    pub theme: Option<String>,
    /// Price-pane share of the canvas height.
    #[serde(default)]
    pub pane_share: Option<f32>,
    /// Dock drawer open state.
    #[serde(default)]
    pub dock_open: Option<bool>,
    /// Starred catalog entries.
    #[serde(default)]
    pub favorites: Favorites,
    /// Indicator loadout, in stack (paint) order.
    #[serde(default)]
    pub slots: Vec<SlotPrefs>,
}

impl TerminalPrefs {
    /// Whether the document carries no setting at all (a fresh file).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
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
