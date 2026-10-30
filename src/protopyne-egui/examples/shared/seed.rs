//! Shared seeded-history switching for the example shells: load a picked
//! ticker's months through the history cache, fold every toolbar frame,
//! and reseed the workspace — with no gateway involved. Both the chart
//! shell (`demo`) and the mock-mode terminal use this, so a ticker pick
//! behaves identically with or without a server: the picked market loads,
//! a refusal seeds empty frames under the picked label (never stale rows)
//! and reports the reason on the status line.

use protopyne_chart_model::{rows_of, ChartRow};
use protopyne_egui::{ChartWorkspace, TimeframeKey};
use protopyne_server::{fold_frames, HistoryCache, TickerLoad, HISTORY_ROOT_BASE};
use protopyne_types::{Bar, FoldEdges, Timeframe};

/// The toolbar key behind a store-grid timeframe.
fn key_of(timeframe: Timeframe) -> Option<TimeframeKey> {
    TimeframeKey::all()
        .into_iter()
        .find(|key| key.timeframe() == timeframe)
}

/// Fold one 1M series into every toolbar frame the base reaches: the base is
/// taken as-is and every whole-multiple coarser target is tallied through
/// the shared bucket-anchored fold. Frames finer than the base stay absent
/// (they cannot be synthesized); `1D` folds from a one-minute base.
pub fn frames_of(bars: &[Bar]) -> Result<Vec<(TimeframeKey, Vec<ChartRow>)>, String> {
    let base = HISTORY_ROOT_BASE;
    let targets: Vec<Timeframe> = TimeframeKey::all()
        .into_iter()
        .map(TimeframeKey::timeframe)
        .filter(|target| target.is_whole_multiple_of(base))
        .collect();
    // Sub-daily frames fold complete-only (torn intraday buckets are omitted);
    // day-and-coarser frames fold keep-partial, because a trading session
    // cannot tile a 24-hour UTC day, so a complete-only daily fold emits
    // nothing and the 1D chart would go dark. This mirrors the workspace's
    // display fold, so a seeded 1D frame and an on-demand resample agree.
    let (intraday, daily): (Vec<Timeframe>, Vec<Timeframe>) = targets
        .into_iter()
        .partition(|target| target.duration_ms() < Timeframe::D1.duration_ms());
    let mut seeded = Vec::new();
    for (group, edges) in [
        (intraday, FoldEdges::CompleteOnly),
        (daily, FoldEdges::KeepPartial),
    ] {
        let frames = fold_frames(bars, base, &group, edges)
            .map_err(|e| format!("tally 1M series: {e}"))?;
        for (timeframe, frame) in frames {
            let key =
                key_of(timeframe).ok_or_else(|| format!("no toolbar slot for {timeframe:?}"))?;
            seeded.push((key, rows_of(&frame).rows));
        }
    }
    Ok(seeded)
}

/// Empty frames under every key: the fail-closed seed for a ticker that
/// refuses to load — its label over no rows, never another ticker's.
pub fn empty_frames() -> Vec<(TimeframeKey, Vec<ChartRow>)> {
    TimeframeKey::all()
        .into_iter()
        .map(|key| (key, Vec::new()))
        .collect()
}

/// A load notice for the status line: excluded rows, excluded months, and
/// superseded rows are observations about the source tree, rendered, never
/// dropped.
pub fn load_notice(ticker: &str, load: &TickerLoad) -> Option<Notice> {
    if load.skipped.is_empty() && load.superseded == 0 && load.skipped_rows == 0 {
        return None;
    }
    let mut parts = Vec::new();
    if load.skipped_rows > 0 {
        parts.push(format!(
            "{} row(s) excluded as unparseable",
            load.skipped_rows
        ));
    }
    if !load.skipped.is_empty() {
        let names: Vec<String> = load
            .skipped
            .iter()
            .filter_map(|skip| {
                skip.path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .collect();
        parts.push(format!(
            "{} month file(s) excluded [{}]",
            load.skipped.len(),
            names.join(", ")
        ));
    }
    if load.superseded > 0 {
        parts.push(format!(
            "{} parquet row(s) superseded by CSV",
            load.superseded
        ));
    }
    Some(Notice::warning(format!(
        "{ticker}: {} (a corrected month in CSV is the fix path)",
        parts.join(", ")
    )))
}

/// A status-line notice: warnings disclose holes in served data, errors
/// refuse the requested ticker outright.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    Warning(String),
    Error(String),
}

impl Notice {
    pub fn warning(text: String) -> Self {
        Self::Warning(text)
    }

    pub fn error(text: String) -> Self {
        Self::Error(text)
    }

    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::Warning(text) | Self::Error(text) => text,
        }
    }

    #[must_use]
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }
}

/// On-demand ticker switch over seeded months: when the modal picked a
/// symbol the seeded frames do not hold, load its months through the cache
/// and reseed. Runs ahead of the paint so the frame draws the picked
/// ticker; a refusal seeds empty frames under the picked label and reports
/// the reason instead of painting stale rows. A settled switch overwrites
/// the notice (clearing it when the load is clean); an already-loaded
/// symbol touches nothing, so a sticky notice survives idle frames.
pub fn sync_symbol(
    workspace: &mut ChartWorkspace,
    cache: &mut HistoryCache,
    loaded_symbol: &mut Option<String>,
    notice: &mut Option<Notice>,
) {
    let picked = workspace.active_symbol().to_owned();
    if loaded_symbol.as_deref() == Some(picked.as_str()) {
        return;
    }
    match cache.load_for(&picked) {
        Ok(load) => {
            let load_notice = load_notice(&picked, &load);
            match frames_of(&load.bars).and_then(|seeded| {
                workspace
                    .switch_symbol_seeded(&picked, seeded)
                    .map_err(|e| format!("seed {picked}: {e}"))
            }) {
                Ok(()) => {
                    *loaded_symbol = Some(picked);
                    *notice = load_notice;
                }
                Err(reason) => {
                    *notice = Some(Notice::error(reason));
                }
            }
        }
        Err(error) => {
            let _ = workspace.switch_symbol_seeded(&picked, empty_frames());
            *loaded_symbol = Some(picked.clone());
            *notice = Some(Notice::error(format!("load {picked}: {error}")));
        }
    }
}
