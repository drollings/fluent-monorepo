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
//! [`fold_bars_edges`](protopyne_store::fold_bars_edges) — the same fold the
//! durable read path applies — never a second aggregation implementation.

use std::path::Path;

use protopyne_store::{fold_bars_edges, read_ohlcv_parquet, StoreError};
use protopyne_types::{Bar, FoldEdges, Timeframe};

use crate::error::ServerError;

/// Load closed bars from a market Parquet file at `path`.
/// A file naming several tickers refuses — the reader insists on one series
/// when no ticker is named — and every malformed shape reports the file and
/// the reason rather than serving a partially-parsed series.
pub(crate) fn load_bars_parquet(path: &Path) -> Result<Vec<Bar>, ServerError> {
    read_ohlcv_parquet(path, None)
        .map(|read| read.bars)
        .map_err(|error| parquet_error(path, error))
}

/// Tally one base series into coarser frames at load time under an explicit
/// edge policy.
///
/// Every target must be a whole multiple of `base` (or the base itself);
/// anything else refuses rather than folding a source bar across two
/// buckets. Output follows the `targets` order so callers map frames to
/// their slots without a second lookup. Chart-serving callers pass
/// [`FoldEdges::CompleteOnly`] so slots agree with the gateway and the
/// workspace; under it a short or torn bucket is omitted, so session data
/// may tally to no daily bars. The fold is the store's bucket-anchored one:
/// `open` is the first bar's open of each bucket, `close` the last, extremes
/// the extremes, volume the saturating sum, all stamped at the UTC bucket
/// start.
pub fn fold_frames(
    bars: &[Bar],
    base: Timeframe,
    targets: &[Timeframe],
    edges: FoldEdges,
) -> Result<Vec<(Timeframe, Vec<Bar>)>, ServerError> {
    let mut frames = Vec::with_capacity(targets.len());
    for &target in targets {
        if target == base {
            frames.push((target, bars.to_vec()));
            continue;
        }
        if !target.is_whole_multiple_of(base) {
            return Err(ServerError::Backtest(format!(
                "cannot fold {base:?} bars into {target:?}: {target:?} is not a whole multiple of one bar"
            )));
        }
        let folded = fold_bars_edges(bars, target, edges)
            .map_err(|error| ServerError::Backtest(format!("fold into {target:?}: {error}")))?;
        frames.push((target, folded));
    }
    Ok(frames)
}

/// Map the store's ingest taxonomy onto the server's: I/O keeps its typed
/// shape (the fallback ladder discriminates on it), everything else reports
/// the file and the reason.
pub(crate) fn parquet_error(path: &Path, error: StoreError) -> ServerError {
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

#[cfg(test)]
#[path = "../tests/ladder_characterization.rs"]
mod ladder_characterization_tests;
