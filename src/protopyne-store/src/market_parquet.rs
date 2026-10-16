//! Market-data Parquet ingest: read vendor OHLCV files into committed bars.
//!
//! This is the **ingest** side of the Parquet surface, distinct from the
//! sealed-snapshot artifact ([`crate::snapshot`]): a snapshot is written by
//! this store at a pinned `Decimal` scale with a digest and a manifest row,
//! while a market file arrives from outside with whatever schema its
//! producer chose. The reader here accepts the announced columnar shape —
//! `timestamp` (any Arrow timestamp unit, optional timezone; naive stamps
//! read as UTC), `open`/`high`/`low`/`close` as `Float64`, `Float32`, or
//! `Decimal128`, `volume` as a float, integer, or decimal, and an optional
//! `ticker` name column — and never guesses beyond it: any other column
//! type, a missing required column, a null, or a NaN refuses with the row
//! named, or — through [`read_ohlcv_parquet_lenient`] — excludes that row
//! and counts it in [`LenientRead::skipped`]. Fractional volumes round to
//! the nearest share instead of refusing. Structural faults refuse in both
//! modes.
//!
//! Three disciplines hold, matching the CSV ingest:
//!
//! - **Order is established here.** Batches arrive in row-group order, not
//!   guaranteed stamp order; the reader sorts ascending before answering, so
//!   a shuffled file reads back as a valid series.
//! - **Restatements merge, they are not refused.** A second row at one stamp
//!   is the venue reading the same bar again; it folds through the same
//!   [`KeyedSnapshot::merge`](crate::commit::KeyedSnapshot) rule the live
//!   emitter uses (open stays first, close latest, range only widens), and
//!   the count is reported. Sorting is stable, so file order decides which
//!   reading is later.
//! - **Float prices cross exactly once.** The bit-patterned float is
//!   converted through `Decimal::from_f64` at the boundary and never touched
//!   again; `f64` appears nowhere past this module.
//!
//! A file that names several tickers refuses when no ticker filter is
//! supplied: interleaved instruments would otherwise merge across symbols on
//! a shared stamp. Name one ticker (or split the file) and read again.

use std::path::Path;

use protopyne_types::Bar;

use crate::error::StoreError;

/// One market Parquet read: the committed bars plus the ingest facts that
/// interpret them. Both counts are observations about the source file, never
/// confidence readings about the bars.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OhlcvRead {
    /// Bars ascending by `ts`, adjacent pairs `validate_new_bar`-clean, with
    /// equal-stamp restatements merged.
    pub bars: Vec<Bar>,
    /// Rows folded into an existing stamp because the venue restated it.
    pub restatements_merged: usize,
    /// Rows skipped because their ticker is not the requested one.
    pub rows_filtered: usize,
}

/// One row the lenient read excluded: the file-relative row index and the
/// refusal that excluded it. Counted observation, never a silent drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedRow {
    /// File-relative row index (0-based, across record batches in order).
    pub row: usize,
    /// The refusal that excluded the row, verbatim.
    pub reason: String,
}

/// One lenient market Parquet read: the committed bars plus every excluded
/// row, named. Structural faults (a missing column, a mixed-ticker file read
/// without a filter) still refuse the whole file — skipping cannot resolve
/// ambiguity about what the file is — while per-row value faults (a null, a
/// NaN, a fractional volume, an inverted bar) exclude that row and count it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LenientRead {
    /// The committed bars and merge/filter counts, as in the strict read.
    pub read: OhlcvRead,
    /// Rows excluded for per-row value faults, in file order.
    pub skipped: Vec<SkippedRow>,
}

/// Read one market Parquet file into committed bars.
///
/// `ticker` names the symbol to serve when the file carries a `ticker`
/// column; rows of other tickers are skipped and counted. Without a filter,
/// a file naming more than one ticker refuses — one series at a time is the
/// only shape a bar tape can be. A file with no ticker column ignores the
/// filter. With the `parquet` feature off, reports the disabled feature.
pub fn read_ohlcv_parquet(path: &Path, ticker: Option<&str>) -> Result<OhlcvRead, StoreError> {
    #[cfg(feature = "parquet")]
    {
        parquet_read::read(path, ticker)
    }
    #[cfg(not(feature = "parquet"))]
    {
        let _ = (path, ticker);
        Err(StoreError::FeatureDisabled("parquet".to_owned()))
    }
}

/// Read one market Parquet file leniently: per-row value faults exclude that
/// row and count it in [`LenientRead::skipped`] instead of refusing the
/// file. Same `ticker` contract and same structural refusals as the strict
/// read; the served bars are identical whenever no row faults. With the
/// `parquet` feature off, reports the disabled feature.
pub fn read_ohlcv_parquet_lenient(
    path: &Path,
    ticker: Option<&str>,
) -> Result<LenientRead, StoreError> {
    #[cfg(feature = "parquet")]
    {
        parquet_read::read_lenient(path, ticker)
    }
    #[cfg(not(feature = "parquet"))]
    {
        let _ = (path, ticker);
        Err(StoreError::FeatureDisabled("parquet".to_owned()))
    }
}

#[cfg(feature = "parquet")]
mod parquet_read {
    use std::fs::File;

    use arrow::datatypes::Schema;
    use arrow::record_batch::RecordBatch;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use protopyne_types::{validate_new_bar, Bar};
    use rust_decimal::Decimal;

    use super::{LenientRead, OhlcvRead, SkippedRow};
    use crate::arrow_decode::{
        DecodeError, PriceColumn, StringColumn, TimeColumn, VolumeColumn, find,
        price_column_of, string_column_of, time_column_of, volume_column_of,
    };
    use crate::commit::KeyedSnapshot as _;
    use crate::error::StoreError;

    /// What a per-row value fault does: refuse the file, or exclude the row
    /// and count it. Structural faults refuse either way — one flag, one row
    /// loop, never a second decoder.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum OnBadRow {
        /// Refuse the file naming the row (the strict read).
        Refuse,
        /// Exclude the row and count it (the lenient read).
        Skip,
    }

    /// Refuse the file naming `row`, or exclude the row and count it,
    /// per `mode`. Diverges, so call sites read as value matches.
    macro_rules! refuse_or_skip {
        ($row:expr, $reason:expr, $skipped:expr, $mode:expr) => {
            match $mode {
                OnBadRow::Refuse => return Err(row_err($row, $reason)),
                OnBadRow::Skip => {
                    $skipped.push(SkippedRow { row: $row, reason: $reason.to_string() });
                    continue;
                }
            }
        };
    }

    /// The column indices this reader requires, with the optional ticker.
    struct Columns {
        time: usize,
        open: usize,
        high: usize,
        low: usize,
        close: usize,
        volume: usize,
        ticker: Option<usize>,
    }

    fn ingest(err: impl std::fmt::Display) -> StoreError {
        StoreError::Ingest {
            reason: err.to_string(),
        }
    }

    fn row_err(row: usize, reason: impl std::fmt::Display) -> StoreError {
        StoreError::Ingest {
            reason: format!("row {row}: {reason}"),
        }
    }

    /// Map a decode failure into the ingest taxonomy with this reader's
    /// words. The `BadScale` arm is unreachable from the timestamp and
    /// string resolvers — only the price and volume arms check scales — and
    /// stays total so a shared-layer contract break is loud, never silent.
    fn decode(label: &str, expected: &str, err: DecodeError) -> StoreError {
        let reason = match err {
            DecodeError::UnexpectedType { actual } => {
                format!("{label} has type {actual}; expected {expected}")
            }
            DecodeError::BadScale { column, scale } => {
                format!("column {column} carries negative decimal scale {scale}")
            }
        };
        StoreError::Ingest { reason }
    }

    fn required(schema: &Schema, names: &[&str], what: &str) -> Result<usize, StoreError> {
        find(schema, names).ok_or_else(|| StoreError::Ingest {
            reason: format!(
                "required column {what} ({}) is missing; file carries: {}",
                names.join("/"),
                schema
                    .fields()
                    .iter()
                    .map(|field| field.name().as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        })
    }

    fn time_column(batch: &RecordBatch, index: usize) -> Result<TimeColumn<'_>, StoreError> {
        time_column_of(batch, index).map_err(|err| {
            decode(
                "timestamp column",
                "a timestamp unit or ts_ms integer",
                err,
            )
        })
    }

    fn price_column<'a>(
        batch: &'a RecordBatch,
        index: usize,
        name: &str,
    ) -> Result<PriceColumn<'a>, StoreError> {
        price_column_of(batch, index, name)
            .map_err(|err| decode(&format!("price column {name}"), "float or decimal", err))
    }

    fn volume_column(batch: &RecordBatch, index: usize) -> Result<VolumeColumn<'_>, StoreError> {
        volume_column_of(batch, index, "volume")
            .map_err(|err| decode("volume column", "numeric", err))
    }

    fn string_column<'a>(
        batch: &'a RecordBatch,
        index: usize,
        name: &str,
    ) -> Result<StringColumn<'a>, StoreError> {
        string_column_of(batch, index)
            .map_err(|err| decode(&format!("column {name}"), "a string", err))
    }

    /// Read one market Parquet file into committed bars. Structural faults
    /// refuse before any row is read; row faults refuse naming the row.
    pub(super) fn read(
        path: &std::path::Path,
        ticker: Option<&str>,
    ) -> Result<OhlcvRead, StoreError> {
        read_with(path, ticker, OnBadRow::Refuse).map(|lenient| lenient.read)
    }

    /// Read one market Parquet file leniently: row faults exclude the row
    /// and count it instead of refusing the file.
    pub(super) fn read_lenient(
        path: &std::path::Path,
        ticker: Option<&str>,
    ) -> Result<LenientRead, StoreError> {
        read_with(path, ticker, OnBadRow::Skip)
    }

    /// The one row loop behind both reads. Structural faults (schema, ticker
    /// ambiguity, unreadable batches) refuse either way; per-row value
    /// faults follow `bad_row`.
    fn read_with(
        path: &std::path::Path,
        ticker: Option<&str>,
        bad_row: OnBadRow,
    ) -> Result<LenientRead, StoreError> {
        let file = File::open(path)?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(ingest)?;
        let schema = builder.schema().clone();
        let columns = Columns {
            time: required(&schema, &["timestamp", "time", "ts_ms"], "timestamp")?,
            open: required(&schema, &["open"], "open")?,
            high: required(&schema, &["high"], "high")?,
            low: required(&schema, &["low"], "low")?,
            close: required(&schema, &["close"], "close")?,
            volume: required(&schema, &["volume"], "volume")?,
            ticker: find(&schema, &["ticker", "symbol"]),
        };

        let reader = builder.build().map_err(ingest)?;
        let mut bars: Vec<Bar> = Vec::new();
        let mut rows_filtered = 0usize;
        let mut skipped: Vec<SkippedRow> = Vec::new();
        let mut seen_ticker: Option<String> = None;
        let mut row_index = 0usize;
        for batch in reader {
            let batch = batch.map_err(ingest)?;
            let time = time_column(&batch, columns.time)?;
            let open = price_column(&batch, columns.open, "open")?;
            let high = price_column(&batch, columns.high, "high")?;
            let low = price_column(&batch, columns.low, "low")?;
            let close = price_column(&batch, columns.close, "close")?;
            let volume = volume_column(&batch, columns.volume)?;
            let ticker_col = columns
                .ticker
                .map(|index| string_column(&batch, index, "ticker"))
                .transpose()?;
            for batch_row in 0..batch.num_rows() {
                let row = row_index;
                row_index += 1;
                if let Some(col) = ticker_col {
                    if col.is_null(batch_row) {
                        refuse_or_skip!(row, "ticker is null", skipped, bad_row);
                    }
                    let value = col.value(batch_row);
                    match ticker {
                        Some(want) if value != want => {
                            rows_filtered += 1;
                            continue;
                        }
                        Some(_) => {}
                        None => match seen_ticker.as_deref() {
                            None => seen_ticker = Some(value.to_owned()),
                            Some(first) if first != value => {
                                return Err(StoreError::Ingest {
                                    reason: format!(
                                        "file carries ticker {value:?} at row {row} after \
                                         {first:?}: name one ticker to read it"
                                    ),
                                });
                            }
                            Some(_) => {}
                        },
                    }
                }
                if time.is_null(batch_row) {
                    refuse_or_skip!(row, "timestamp is null", skipped, bad_row);
                }
                let ts_ms = match time.millis(batch_row) {
                    Ok(stamp) => stamp,
                    Err(reason) => refuse_or_skip!(row, reason, skipped, bad_row),
                };
                let Some(stamp) = chrono::DateTime::from_timestamp_millis(ts_ms) else {
                    refuse_or_skip!(
                        row,
                        format!("timestamp {ts_ms}ms out of range"),
                        skipped,
                        bad_row
                    );
                };
                let price = |column: &PriceColumn<'_>| -> Result<Decimal, String> {
                    if column.is_null(batch_row) {
                        return Err("price is null".to_owned());
                    }
                    column.decimal(batch_row)
                };
                if volume.is_null(batch_row) {
                    refuse_or_skip!(row, "volume is null", skipped, bad_row);
                }
                let shares = match volume.whole(batch_row) {
                    Ok(shares) => shares,
                    Err(reason) => refuse_or_skip!(row, reason, skipped, bad_row),
                };
                let o = match price(&open) {
                    Ok(value) => value,
                    Err(reason) => refuse_or_skip!(row, reason, skipped, bad_row),
                };
                let h = match price(&high) {
                    Ok(value) => value,
                    Err(reason) => refuse_or_skip!(row, reason, skipped, bad_row),
                };
                let l = match price(&low) {
                    Ok(value) => value,
                    Err(reason) => refuse_or_skip!(row, reason, skipped, bad_row),
                };
                let c = match price(&close) {
                    Ok(value) => value,
                    Err(reason) => refuse_or_skip!(row, reason, skipped, bad_row),
                };
                let bar =
                    Bar::new().ts(stamp).o(o).h(h).l(l).c(c).volume(shares).maybe_adj(None).build();
                if let Err(reason) = bar.validate() {
                    refuse_or_skip!(row, reason, skipped, bad_row);
                }
                bars.push(bar);
            }
        }

        // Order is established here: a stable sort keeps file order among
        // equal stamps, and every equal-stamp row after the first folds
        // through the shared restatement rule.
        bars.sort_by_key(|bar| bar.ts);
        let mut merged: Vec<Bar> = Vec::with_capacity(bars.len());
        let mut restatements_merged = 0usize;
        for bar in bars {
            match merged.last_mut() {
                Some(last) if last.ts == bar.ts => {
                    let restated = last.merge(&bar).map_err(|e| StoreError::Ingest {
                        reason: format!("restatement at {} cannot merge: {e}", bar.ts.to_rfc3339()),
                    })?;
                    *last = restated;
                    restatements_merged += 1;
                }
                _ => merged.push(bar),
            }
        }
        // The boundary proves what it serves, through the shared validator.
        let mut last: Option<&Bar> = None;
        for bar in &merged {
            validate_new_bar(last, bar).map_err(|e| StoreError::Ingest {
                reason: format!("ingested series is invalid: {e}"),
            })?;
            last = Some(bar);
        }
        Ok(LenientRead {
            read: OhlcvRead { bars: merged, restatements_merged, rows_filtered },
            skipped,
        })
    }
}

#[cfg(all(test, feature = "parquet"))]
#[path = "../tests/market_parquet.rs"]
mod tests_market_parquet;
