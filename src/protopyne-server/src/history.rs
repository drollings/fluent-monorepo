//! One history-loading surface: extension dispatch plus one candidate ladder.
//!
//! A history file declares its encoding by extension: `.csv` reads through
//! the CSV ingest, `.parquet` through the market ingest reader. Anything else
//! refuses naming the extension. A list of candidates walks in order through
//! the shared first-accept combinator: a missing or unreadable file skips to
//! the next candidate, every parse or structural refusal stops the walk on
//! that candidate. A corrupt artifact is investigated, never skipped over.

use std::path::{Path, PathBuf};

use protopyne_types::Bar;

use crate::error::ServerError;

/// The encodings a history file may declare by extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// The encodings a history file may declare by extension.
pub enum HistoryFormat {
    /// Comma-separated bars (`.csv`).
    Csv,
    /// Market Parquet artifact (`.parquet`).
    Parquet,
}

/// Classify `path` by its file extension.
///
/// Unknown or missing extensions refuse naming what was found and what is
/// accepted. No inference, no guessing.
pub fn format_of(path: &Path) -> Result<HistoryFormat, ServerError> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("csv") => Ok(HistoryFormat::Csv),
        Some("parquet") => Ok(HistoryFormat::Parquet),
        other => Err(ServerError::Backtest(format!(
            "unknown history extension {} for {}: expected .csv or .parquet",
            other.map_or_else(|| "(none)".to_owned(), |ext| format!(".{ext}")),
            path.display()
        ))),
    }
}

/// Load closed bars from one history file, dispatching on its extension.
pub fn load_bars_file(path: &Path) -> Result<Vec<Bar>, ServerError> {
    match format_of(path)? {
        HistoryFormat::Csv => crate::csv::load_bars_csv(path),
        HistoryFormat::Parquet => crate::parquet::load_bars_parquet(path),
    }
}

/// Load the first usable history candidate, in order.
///
/// A candidate that cannot be opened (`Io`) skips to the next; any other
/// refusal — a bad header, a bad row, a schema mismatch, a mixed-ticker file,
/// an unknown extension — stops the walk with that refusal. Exhaustion
/// without a usable file refuses. Composes the shared first-accept combinator
/// rather than hand-rolling the fallback loop.
pub fn load_bars_candidates(candidates: &[PathBuf]) -> Result<Vec<Bar>, ServerError> {
    let outcome = fluent_concurrency::ladder::first_accept_in_order_sync(
        candidates.iter().cloned(),
        |path: PathBuf| match load_bars_file(path.as_path()) {
            Ok(bars) => Ok(Some(bars)),
            Err(ServerError::Io(_)) => Ok(None),
            Err(other) => Err(other),
        },
        |_: &ServerError| true,
    );
    match outcome {
        Ok(Some(bars)) => Ok(bars),
        Ok(None) => Err(ServerError::Backtest(
            "no candidate history file parsed".to_owned(),
        )),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[path = "../tests/history.rs"]
mod tests;