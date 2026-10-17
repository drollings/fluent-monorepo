//! Directory history roots: `TICKER/YYYY/MM/*.parquet` on demand.
//!
//! A history root is a directory whose ticker subtrees hold immutable
//! one-minute Parquet months (`SPY/2026/03/SPY-2026-03-1M.parquet`), with CSV
//! files alongside as the superseding supplement: Parquet is the backup of
//! record, CSV rows win every timestamp they both name. Files are discovered
//! by walking `<root>/<TICKER>/**` in sorted order, so the load is
//! deterministic; nothing is preloaded — one ticker's months merge only when
//! that ticker is asked for, and [`HistoryCache`] holds the merged series so
//! a symbol switch back costs a clone, never a re-read.
//!
//! One dirty month never strands a ticker: month files read leniently, so a
//! row that refuses to parse is excluded and counted instead of discarding
//! the month's good bars, while fractional vendor volumes round to the
//! nearest share instead of refusing at all. Row exclusions ride
//! [`TickerLoad::skipped_rows`]; a month that refuses structurally is
//! excluded whole and named in [`TickerLoad::skipped`]; a file that cannot
//! be opened at all skips to the next file, like the single-file ladder. Nothing here is ever silent —
//! every exclusion rides the return value, every caller logs or renders it,
//! and the CSV supplement is the fix path (a corrected month in CSV
//! supersedes the refused rows outright). A merged series that is empty or
//! fails the shared order validator still refuses the whole load: holes are
//! disclosed, disorder is not served.
//!
//! Display stays on the existing optimized path: callers fold the merged 1M
//! series into toolbar frames through [`crate::fold_frames`] (whole-multiple
//! targets, `CompleteOnly` edges) and paint windows through the workspace's
//! trailing-window reads. Per-month paging would not move the needle — the
//! frame paints a culled window and the indicators need the full run-in for
//! correct warmup — so on demand means per ticker, not per month.
//!
//! [`load_history_path`] is the one dispatch both binaries share: a file
//! argument keeps the legacy single-file ladder, a directory argument opens
//! the root and serves the preferred ticker (the shared default rule:
//! `SPY` when present, else the first in sorted order).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use protopyne_store::{KeyedSnapshot as _, read_ohlcv_parquet_lenient};
use protopyne_types::{Bar, Timeframe, validate_new_bar};
use protopyne_view::{
    AssetClass, InstrumentDecl, SymbolInfoView, discovered_symbol_rows, pick_default_symbol,
};

use crate::error::ServerError;
use crate::parquet::parquet_error;

/// Sidecar a ticker directory may carry to declare its display name and
/// asset class: `{"name": "...", "class": "Stocks"}`. A shipped ticker keeps
/// its shipped identity regardless; a sidecar only speaks for tickers the
/// shipped list does not carry. Absent, unreadable, or an unrecognized
/// class label declares nothing — the row lands on [`AssetClass::Unknown`],
/// never a guessed class.
const SIDECAR_FILE: &str = ".coral-instrument";

/// The bar spacing every month file in a history root carries. The layout
/// contract is one-minute months (`*-1M.parquet`); CSV supplements ride the
/// same grid, and anything off-grid refuses at parse time.
pub const HISTORY_ROOT_BASE: Timeframe = Timeframe::M1;

/// The ticker the directory dispatch serves when no preferred ticker is
/// named and that ticker exists under the root.
pub const DEFAULT_TICKER: &str = "SPY";

/// A history directory root: `<root>/TICKER/YYYY/MM/*.parquet` plus CSV
/// supplements. Holds only the root path — discovery and parsing run per
/// read, merged series live in [`HistoryCache`].
#[derive(Debug, Clone)]
pub struct HistoryRoot {
    root: PathBuf,
}

impl HistoryRoot {
    /// Open the history root at `root`. Refuses a missing directory or a
    /// file, naming what was found: a root is a directory by contract.
    pub fn open(root: &Path) -> Result<Self, ServerError> {
        if !root.is_dir() {
            return Err(ServerError::Backtest(format!(
                "history root {} is not a directory",
                root.display()
            )));
        }
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    /// The root path this history was opened over.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Tickers with history under this root, in sorted order: top-level
    /// directories with a valid ticker spelling. Root-level files are never
    /// ticker data — the attic holds tick tapes, trims, and vendor dumps
    /// that load only through an explicit file argument — so discovery reads
    /// directories alone. An empty root answers empty; the caller decides
    /// whether that refuses.
    pub fn tickers(&self) -> Result<Vec<String>, ServerError> {
        let mut tickers: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if is_ticker_spelling(&name) {
                tickers.push(name);
            }
        }
        tickers.sort();
        tickers.dedup();
        Ok(tickers)
    }

    /// The sidecar declaration for `ticker`, if the directory carries one
    /// that parses. An unreadable sidecar declares nothing (no error: the
    /// row lands on Unknown, never a guess).
    fn instrument_decl(&self, ticker: &str) -> Option<InstrumentDecl> {
        let sidecar = self.root.join(ticker).join(SIDECAR_FILE);
        let text = std::fs::read_to_string(sidecar).ok()?;
        let raw: serde_json::Value = serde_json::from_str(&text).ok()?;
        let name = raw.get("name").and_then(serde_json::Value::as_str).map(str::to_owned);
        let class = raw
            .get("class")
            .and_then(serde_json::Value::as_str)
            .and_then(AssetClass::parse);
        Some(InstrumentDecl::new(name, class))
    }

    /// Wire rows for every ticker under this root: the shipped catalog for
    /// shipped tickers, a sidecar declaration or `Unknown` class for the
    /// rest. Discovery reads directories alone; an empty root answers an
    /// empty row list, and the caller decides whether that refuses.
    pub fn symbol_rows(&self) -> Result<Vec<SymbolInfoView>, ServerError> {
        let tickers = self.tickers()?;
        Ok(discovered_symbol_rows(&tickers, |ticker| {
            self.instrument_decl(ticker)
        }))
    }

    /// Load one ticker's merged 1M series: every `*.parquet` month under
    /// `<root>/<TICKER>/**` in sorted path order, then every `*.csv`
    /// supplement under the same subtree overwriting the timestamps it
    /// names. Months read leniently: unparseable rows are excluded and
    /// counted in [`TickerLoad::skipped_rows`], never silently, while
    /// equal-stamp Parquet rows fold through the shared restatement rule and
    /// CSV rows replace wholesale. The answer is ascending and
    /// `validate_new_bar`-clean, or the load refuses naming the reason. A
    /// month that cannot be opened skips; a month that refuses structurally
    /// is excluded whole and named in [`TickerLoad::skipped`] — the same
    /// stop/skip spirit as the single-file ladder, with every exclusion
    /// carried loudly instead of stranding the ticker. A CSV supplement that
    /// refuses stops the load: supplements are operator data, and a corrupt
    /// one is investigated, never skipped over.
    pub fn load_ticker(&self, ticker: &str) -> Result<TickerLoad, ServerError> {
        if !is_ticker_spelling(ticker) {
            return Err(ServerError::Backtest(format!(
                "invalid ticker {ticker:?}: expected letters, digits, '.', '-', '_'",
            )));
        }
        let mut parquet_files = Vec::new();
        let mut csv_files = Vec::new();
        let subtree = self.root.join(ticker);
        if subtree.is_dir() {
            collect_by_extension(&subtree, &mut parquet_files, &mut csv_files)?;
        }
        if parquet_files.is_empty() && csv_files.is_empty() {
            return Err(ServerError::Backtest(format!(
                "no history for ticker {ticker:?} under {}",
                self.root.display()
            )));
        }
        let mut merged: BTreeMap<i64, Bar> = BTreeMap::new();
        let mut skipped: Vec<SkippedFile> = Vec::new();
        let mut skipped_rows = 0usize;
        for path in &parquet_files {
            let lenient = match read_ohlcv_parquet_lenient(path, Some(ticker)) {
                Ok(lenient) => lenient,
                Err(protopyne_store::StoreError::Io(_)) => continue,
                Err(other) => {
                    skipped.push(SkippedFile {
                        path: path.clone(),
                        reason: parquet_error(path, other).to_string(),
                    });
                    continue;
                }
            };
            skipped_rows += lenient.skipped.len();
            let bars = lenient.read.bars;
            for bar in bars {
                let stamp = bar.ts.timestamp_millis();
                match merged.get(&stamp) {
                    Some(held) => {
                        let folded = held.merge(&bar).map_err(|error| {
                            ServerError::Backtest(format!(
                                "restatement at {} in {} cannot merge: {error}",
                                bar.ts.to_rfc3339(),
                                path.display()
                            ))
                        })?;
                        merged.insert(stamp, folded);
                    }
                    None => {
                        merged.insert(stamp, bar);
                    }
                }
            }
        }
        let mut superseded = 0usize;
        for path in &csv_files {
            let Ok(content) = common_core::io::read_to_string_err(path) else {
                continue;
            };
            let bars = crate::csv::parse_bars_csv(&content).map_err(|error| match error {
                ServerError::BadRow { line, reason } => ServerError::BadRow {
                    line,
                    reason: format!("{}: {reason}", path.display()),
                },
                ServerError::BadHeader(reason) => {
                    ServerError::BadHeader(format!("{}: {reason}", path.display()))
                }
                other => other,
            })?;
            // The supplement supersedes: a CSV row replaces the Parquet bar
            // at its stamp wholesale, it never folds into it.
            for bar in bars {
                let stamp = bar.ts.timestamp_millis();
                if merged.contains_key(&stamp) {
                    superseded += 1;
                }
                merged.insert(stamp, bar);
            }
        }
        let bars: Vec<Bar> = merged.into_values().collect();
        if bars.is_empty() {
            let mut reason = format!(
                "no bars for ticker {ticker:?} under {}",
                self.root.display()
            );
            if let Some(first) = skipped.first() {
                use std::fmt::Write as _;
                write!(
                    reason,
                    "; {} month file(s) refused, first: {}: {}",
                    skipped.len(),
                    first.path.display(),
                    first.reason
                )
                .expect("writing to a String never fails");
            }
            return Err(ServerError::Backtest(reason));
        }
        let mut last: Option<&Bar> = None;
        for bar in &bars {
            validate_new_bar(last, bar).map_err(|error| {
                ServerError::Backtest(format!(
                    "merged {ticker} series is invalid at {}: {error}",
                    bar.ts.to_rfc3339()
                ))
            })?;
            last = Some(bar);
        }
        Ok(TickerLoad { bars, skipped, skipped_rows, superseded })
    }
}

/// One month file excluded from a ticker merge: the path and the refusal
/// that excluded it. Rendered by every caller — never dropped silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedFile {
    /// The month file that refused to parse.
    pub path: PathBuf,
    /// The reader's refusal, verbatim.
    pub reason: String,
}

/// One ticker's merged history: the ascending series plus the load facts
/// that interpret it. Skips are observations, never confidence — a non-zero
/// count means the series carries holes the caller must disclose.
#[derive(Debug, Clone, PartialEq)]
pub struct TickerLoad {
    /// Merged 1M bars, ascending and `validate_new_bar`-clean.
    pub bars: Vec<Bar>,
    /// Month files excluded whole from the merge, in load order.
    pub skipped: Vec<SkippedFile>,
    /// Parquet rows excluded for per-row value faults, across all months.
    pub skipped_rows: usize,
    /// CSV supplement rows that replaced a Parquet bar at their stamp.
    pub superseded: usize,
}

/// Whether `name` is a safe single-path-component ticker spelling: letters,
/// digits, `.`, `-`, `_`. Anything else (including `..` and separators)
/// refuses before it can steer discovery outside the root.
fn is_ticker_spelling(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

/// Walk `dir` recursively, collecting `*.parquet` and `*.csv` paths in
/// sorted order. Deterministic by construction: entries sort per directory
/// and recursion follows that order.
fn collect_by_extension(
    dir: &Path,
    parquet_files: &mut Vec<PathBuf>,
    csv_files: &mut Vec<PathBuf>,
) -> Result<(), ServerError> {
    let mut entries: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        entries.push(entry?.path());
    }
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_by_extension(&path, parquet_files, csv_files)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("parquet") {
            parquet_files.push(path);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("csv") {
            csv_files.push(path);
        }
    }
    Ok(())
}

/// On-demand per-ticker cache over a [`HistoryRoot`]: the first read of a
/// ticker walks and merges its months, every later read clones the held
/// load. One ticker's merged series is the cost bound — the workspace holds
/// its own converted rows beside this, and switching back never re-reads.
#[derive(Debug)]
pub struct HistoryCache {
    root: HistoryRoot,
    loads: HashMap<String, TickerLoad>,
}

impl HistoryCache {
    /// Cache over an already-open root.
    #[must_use]
    pub fn new(root: HistoryRoot) -> Self {
        Self {
            root,
            loads: HashMap::new(),
        }
    }

    /// The root this cache reads through.
    #[must_use]
    pub fn root(&self) -> &HistoryRoot {
        &self.root
    }

    /// Tickers with history under the root, in sorted order.
    pub fn tickers(&self) -> Result<Vec<String>, ServerError> {
        self.root.tickers()
    }

    /// The merged load for `ticker`, reading on first use. Cloned on every
    /// call so callers own their copy and the cache never aliases.
    pub fn load_for(&mut self, ticker: &str) -> Result<TickerLoad, ServerError> {
        if let Some(load) = self.loads.get(ticker) {
            return Ok(load.clone());
        }
        let load = self.root.load_ticker(ticker)?;
        self.loads.insert(ticker.to_owned(), load.clone());
        Ok(load)
    }

    /// The merged series for `ticker`, loading on first use.
    pub fn bars_for(&mut self, ticker: &str) -> Result<Vec<Bar>, ServerError> {
        self.load_for(ticker).map(|load| load.bars)
    }
}

/// Load history from one CLI argument: a file keeps the legacy single-file
/// ladder, a directory opens the history root and serves the preferred
/// ticker (`preferred`, else the shared default rule: `SPY` when present,
/// else the first in sorted order, else refuse). Refuses an empty series
/// naming the argument.
pub fn load_history_path(path: &Path, preferred: Option<&str>) -> Result<Vec<Bar>, ServerError> {
    if path.is_dir() {
        let root = HistoryRoot::open(path)?;
        let ticker = if let Some(ticker) = preferred {
            ticker.to_owned()
        } else {
            let tickers = root.tickers()?;
            pick_default_symbol(DEFAULT_TICKER, tickers.iter().map(String::as_str)).ok_or_else(
                || ServerError::Backtest(format!("no tickers under history root {}", path.display())),
            )?
        };
        let load = root.load_ticker(&ticker)?;
        if load.skipped_rows > 0 {
            tracing::warn!(
                skipped_rows = load.skipped_rows,
                "history months for {ticker} excluded {} unparseable rows; series carries the holes",
                load.skipped_rows
            );
        }
        for skipped in &load.skipped {
            tracing::warn!(
                path = %skipped.path.display(),
                reason = %skipped.reason,
                "history month excluded from {ticker}; series carries the hole"
            );
        }
        if load.bars.is_empty() {
            return Err(ServerError::Backtest(format!(
                "no bars for ticker {ticker:?} under {}",
                path.display()
            )));
        }
        return Ok(load.bars);
    }
    crate::history::load_bars_candidates(std::slice::from_ref(&path.to_path_buf()))
}

#[cfg(test)]
#[path = "../tests/history_root.rs"]
mod tests;
