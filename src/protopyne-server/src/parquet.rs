//! Parquet ingest for market OHLCV history, and load-time frame tallying.
//!
//! The columnar counterpart of [`crate::csv`]: one file per tape, read
//! through the store's schema-tolerant market reader
//! ([`protopyne_store :: read_ohlcv_parquet`]), producing the same
//! committed [`Bar`](protopyne_types::Bar) values the CSV arm produces, so
//! every downstream path (backtest, venue, gateway) is identical.
//!
//! [`fold_frames`] is the one load-time tally: a base series folds into
//! coarser whole-multiple frames through the store's bucket-anchored
//! [`fold_bars`](protopyne_store::fold_bars) — the same fold the durable
//! read path applies — never a second aggregation implementation.

use std::path::{Path, PathBuf};

use protopyne_store::{fold_bars, read_ohlcv_parquet, StoreError};
use protopyne_types::{Bar, Timeframe};

use crate::csv::load_bars_csv;
use crate::error::ServerError;

/// Load closed bars from a history file at `path`, dispatching on the file
/// extension: `.parquet` reads through the market ingest reader, `.csv`
/// through the CSV ingest. Any other extension refuses — no inference, no
/// guessing: a history file must declare its encoding.
pub fn load_bars_any(path: &Path) -> Result<Vec<Bar>, ServerError> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("parquet") => load_bars_parquet(path),
        Some("csv") => load_bars_csv(path),
        other => Err(ServerError::Backtest(format!(
            "unknown history extension {} for {}: expected .csv or .parquet",
            other.map_or_else(|| "(none)".to_owned(), |ext| format!(".{ext}")),
            path.display()
        ))),
    }
}

/// Load closed bars from a market Parquet file at `path`.
///
/// A file naming several tickers refuses — the reader insists on one series
/// when no ticker is named — and every malformed shape reports the file and
/// the reason rather than serving a partially-parsed series.
pub fn load_bars_parquet(path: &Path) -> Result<Vec<Bar>, ServerError> {
    read_ohlcv_parquet(path, None)
        .map(|read| read.bars)
        .map_err(|error| parquet_error(path, error))
}

/// Load the first usable Parquet candidate, in order.
///
/// A candidate that cannot be opened (`Io`) skips to the next; any other
/// refusal — a missing column, an undecodable value, a mixed-ticker file —
/// stops the walk naming that candidate. This mirrors the CSV ladder's
/// stop/skip split: a structurally wrong artifact is never skipped over as
/// if a later file could make it usable.
pub fn load_bars_parquet_with_fallback(candidates: &[PathBuf]) -> Result<Vec<Bar>, ServerError> {
    let outcome = fluent_concurrency::ladder::first_accept_in_order_sync(
        candidates.iter().cloned(),
        |path: PathBuf| match load_bars_parquet(path.as_path()) {
            Ok(bars) => Ok(Some(bars)),
            Err(ServerError::Io(_)) => Ok(None),
            Err(other) => Err(other),
        },
        |_| true,
    );
    outcome?
        .ok_or_else(|| ServerError::Backtest("no candidate parquet history file parsed".to_owned()))
}

/// Tally one base series into coarser frames at load time.
///
/// Every target must be a whole multiple of `base` (or the base itself);
/// anything else refuses rather than folding a source bar across two
/// buckets. Output follows the `targets` order so callers map frames to
/// their slots without a second lookup. The fold is the store's
/// bucket-anchored one: `open` is the first bar's open of each bucket,
/// `close` the last, extremes the extremes, volume the saturating sum, all
/// stamped at the UTC bucket start.
pub fn fold_frames(
    bars: &[Bar],
    base: Timeframe,
    targets: &[Timeframe],
) -> Result<Vec<(Timeframe, Vec<Bar>)>, ServerError> {
    let base_ms = base.duration_ms();
    let mut frames = Vec::with_capacity(targets.len());
    for &target in targets {
        if target == base {
            frames.push((target, bars.to_vec()));
            continue;
        }
        if target.duration_ms() % base_ms != 0 {
            return Err(ServerError::Backtest(format!(
                "cannot fold {base:?} bars into {target:?}: {target:?} is not a whole multiple of one bar"
            )));
        }
        let folded = fold_bars(bars, target)
            .map_err(|error| ServerError::Backtest(format!("fold into {target:?}: {error}")))?;
        frames.push((target, folded));
    }
    Ok(frames)
}

/// Map the store's ingest taxonomy onto the server's: I/O keeps its typed
/// shape (the fallback ladder discriminates on it), everything else reports
/// the file and the reason.
fn parquet_error(path: &Path, error: StoreError) -> ServerError {
    match error {
        StoreError::Io(io) => ServerError::Io(io),
        other => ServerError::Parquet {
            path: path.display().to_string(),
            reason: other.to_string(),
        },
    }
}

#[cfg(test)]
#[path = "../tests/parquet.rs"]
mod tests;
