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
//! type, a missing required column, a null, a NaN, or a fractional volume
//! refuses with the row named.
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

#[cfg(feature = "parquet")]
mod parquet_read {
    use std::fs::File;

    use arrow::array::{
        Array, Decimal128Array, Float32Array, Float64Array, Int32Array, Int64Array,
        LargeStringArray, StringArray, TimestampMicrosecondArray, TimestampMillisecondArray,
        TimestampNanosecondArray, TimestampSecondArray,
    };
    use arrow::datatypes::{DataType, Schema, TimeUnit};
    use arrow::record_batch::RecordBatch;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use protopyne_types::{validate_new_bar, Bar};
    use rust_decimal::prelude::{FromPrimitive as _, ToPrimitive as _};
    use rust_decimal::Decimal;

    use super::OhlcvRead;
    use crate::commit::KeyedSnapshot as _;
    use crate::error::StoreError;

    /// The row's timestamp column, resolved once per file into its concrete
    /// array type. Every variant reads to epoch milliseconds.
    enum TimeColumn<'a> {
        Nanos(&'a TimestampNanosecondArray),
        Micros(&'a TimestampMicrosecondArray),
        Millis(&'a TimestampMillisecondArray),
        Seconds(&'a TimestampSecondArray),
        EpochMillis(&'a Int64Array),
    }

    impl TimeColumn<'_> {
        fn is_null(&self, row: usize) -> bool {
            match self {
                Self::Nanos(v) => v.is_null(row),
                Self::Micros(v) => v.is_null(row),
                Self::Millis(v) => v.is_null(row),
                Self::Seconds(v) => v.is_null(row),
                Self::EpochMillis(v) => v.is_null(row),
            }
        }

        /// The row's stamp in epoch milliseconds. A sub-millisecond stamp
        /// floors toward the earlier millisecond; a second stamp that would
        /// overflow refuses.
        fn millis(&self, row: usize) -> Result<i64, String> {
            match self {
                Self::Nanos(v) => Ok(v.value(row).div_euclid(1_000_000)),
                Self::Micros(v) => Ok(v.value(row).div_euclid(1_000)),
                Self::Millis(v) => Ok(v.value(row)),
                Self::Seconds(v) => v
                    .value(row)
                    .checked_mul(1_000)
                    .ok_or_else(|| format!("timestamp {}s overflows milliseconds", v.value(row))),
                Self::EpochMillis(v) => Ok(v.value(row)),
            }
        }
    }

    /// The row's price column: float or decimal, resolved once.
    enum PriceColumn<'a> {
        F64(&'a Float64Array),
        F32(&'a Float32Array),
        Decimal(&'a Decimal128Array, u32),
    }

    impl PriceColumn<'_> {
        fn is_null(&self, row: usize) -> bool {
            match self {
                Self::F64(v) => v.is_null(row),
                Self::F32(v) => v.is_null(row),
                Self::Decimal(v, _) => v.is_null(row),
            }
        }

        /// The row's price as an exact `Decimal`. A NaN or an infinity has no
        /// decimal form and refuses rather than silently becoming zero.
        fn decimal(&self, row: usize) -> Result<Decimal, String> {
            match self {
                Self::F64(v) => Decimal::from_f64(v.value(row))
                    .ok_or_else(|| format!("price is not finite: {}", v.value(row))),
                Self::F32(v) => Decimal::from_f32(v.value(row))
                    .ok_or_else(|| format!("price is not finite: {}", v.value(row))),
                Self::Decimal(v, scale) => Ok(Decimal::from_i128_with_scale(v.value(row), *scale)),
            }
        }
    }

    /// The row's volume column: any numeric encoding, resolved once.
    enum VolumeColumn<'a> {
        F64(&'a Float64Array),
        F32(&'a Float32Array),
        I64(&'a Int64Array),
        I32(&'a Int32Array),
        Decimal(&'a Decimal128Array, u32),
    }

    impl VolumeColumn<'_> {
        fn is_null(&self, row: usize) -> bool {
            match self {
                Self::F64(v) => v.is_null(row),
                Self::F32(v) => v.is_null(row),
                Self::I64(v) => v.is_null(row),
                Self::I32(v) => v.is_null(row),
                Self::Decimal(v, _) => v.is_null(row),
            }
        }

        /// The row's volume as whole shares. A fractional or negative value
        /// refuses: a fraction means the file is not OHLCV share counts, and
        /// a negative count is not representable, so neither may be rounded
        /// into a plausible number.
        fn whole(&self, row: usize) -> Result<u64, String> {
            let value = match self {
                Self::F64(v) => Decimal::from_f64(v.value(row))
                    .ok_or_else(|| format!("volume is not finite: {}", v.value(row)))?,
                Self::F32(v) => Decimal::from_f32(v.value(row))
                    .ok_or_else(|| format!("volume is not finite: {}", v.value(row)))?,
                Self::I64(v) => Decimal::from(v.value(row)),
                Self::I32(v) => Decimal::from(v.value(row)),
                Self::Decimal(v, scale) => Decimal::from_i128_with_scale(v.value(row), *scale),
            };
            if value.fract() != Decimal::ZERO {
                return Err(format!("volume {value} is not a whole share count"));
            }
            value
                .to_u64()
                .ok_or_else(|| format!("volume {value} is negative or out of range"))
        }
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

    /// Find one column by any of its accepted spellings, case-insensitively.
    fn find(schema: &Schema, names: &[&str]) -> Option<usize> {
        schema.fields().iter().position(|field| {
            let lowered = field.name().to_lowercase();
            names.iter().any(|name| lowered == *name)
        })
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

    /// A decimal scale that cannot be named as a `Decimal` scale refuses.
    fn checked_scale(scale: i8, column: &str) -> Result<u32, StoreError> {
        u32::try_from(scale).map_err(|_| StoreError::Ingest {
            reason: format!("column {column} carries negative decimal scale {scale}"),
        })
    }

    fn time_column(batch: &RecordBatch, index: usize) -> Result<TimeColumn<'_>, StoreError> {
        let column = batch.column(index);
        let data_type = column.data_type();
        let refuse = || StoreError::Ingest {
            reason: format!(
                "timestamp column has type {data_type}; expected a timestamp unit or ts_ms integer"
            ),
        };
        match data_type {
            DataType::Timestamp(TimeUnit::Nanosecond, _) => column
                .as_any()
                .downcast_ref::<TimestampNanosecondArray>()
                .map(TimeColumn::Nanos)
                .ok_or_else(refuse),
            DataType::Timestamp(TimeUnit::Microsecond, _) => column
                .as_any()
                .downcast_ref::<TimestampMicrosecondArray>()
                .map(TimeColumn::Micros)
                .ok_or_else(refuse),
            DataType::Timestamp(TimeUnit::Millisecond, _) => column
                .as_any()
                .downcast_ref::<TimestampMillisecondArray>()
                .map(TimeColumn::Millis)
                .ok_or_else(refuse),
            DataType::Timestamp(TimeUnit::Second, _) => column
                .as_any()
                .downcast_ref::<TimestampSecondArray>()
                .map(TimeColumn::Seconds)
                .ok_or_else(refuse),
            DataType::Int64 => column
                .as_any()
                .downcast_ref::<Int64Array>()
                .map(TimeColumn::EpochMillis)
                .ok_or_else(refuse),
            _ => Err(refuse()),
        }
    }

    fn price_column<'a>(
        batch: &'a RecordBatch,
        index: usize,
        name: &str,
    ) -> Result<PriceColumn<'a>, StoreError> {
        let column = batch.column(index);
        let data_type = column.data_type();
        let refuse = || StoreError::Ingest {
            reason: format!("price column {name} has type {data_type}; expected float or decimal"),
        };
        match data_type {
            DataType::Float64 => column
                .as_any()
                .downcast_ref::<Float64Array>()
                .map(PriceColumn::F64)
                .ok_or_else(refuse),
            DataType::Float32 => column
                .as_any()
                .downcast_ref::<Float32Array>()
                .map(PriceColumn::F32)
                .ok_or_else(refuse),
            DataType::Decimal128(_, scale) => {
                let scale = checked_scale(*scale, name)?;
                column
                    .as_any()
                    .downcast_ref::<Decimal128Array>()
                    .map(|array| PriceColumn::Decimal(array, scale))
                    .ok_or_else(refuse)
            }
            _ => Err(refuse()),
        }
    }

    fn volume_column(batch: &RecordBatch, index: usize) -> Result<VolumeColumn<'_>, StoreError> {
        let column = batch.column(index);
        let data_type = column.data_type();
        let refuse = || StoreError::Ingest {
            reason: format!("volume column has type {data_type}; expected numeric"),
        };
        match data_type {
            DataType::Float64 => column
                .as_any()
                .downcast_ref::<Float64Array>()
                .map(VolumeColumn::F64)
                .ok_or_else(refuse),
            DataType::Float32 => column
                .as_any()
                .downcast_ref::<Float32Array>()
                .map(VolumeColumn::F32)
                .ok_or_else(refuse),
            DataType::Int64 => column
                .as_any()
                .downcast_ref::<Int64Array>()
                .map(VolumeColumn::I64)
                .ok_or_else(refuse),
            DataType::Int32 => column
                .as_any()
                .downcast_ref::<Int32Array>()
                .map(VolumeColumn::I32)
                .ok_or_else(refuse),
            DataType::Decimal128(_, scale) => {
                let scale = checked_scale(*scale, "volume")?;
                column
                    .as_any()
                    .downcast_ref::<Decimal128Array>()
                    .map(|array| VolumeColumn::Decimal(array, scale))
                    .ok_or_else(refuse)
            }
            _ => Err(refuse()),
        }
    }

    /// The row's string column: 32- or 64-bit offsets (polars emits
    /// `LargeUtf8`), resolved once.
    #[derive(Clone, Copy)]
    enum StringColumn<'a> {
        Utf8(&'a StringArray),
        Large(&'a LargeStringArray),
    }

    impl StringColumn<'_> {
        fn is_null(&self, row: usize) -> bool {
            match self {
                Self::Utf8(v) => v.is_null(row),
                Self::Large(v) => v.is_null(row),
            }
        }

        fn value(&self, row: usize) -> &str {
            match self {
                Self::Utf8(v) => v.value(row),
                Self::Large(v) => v.value(row),
            }
        }
    }

    fn string_column<'a>(
        batch: &'a RecordBatch,
        index: usize,
        name: &str,
    ) -> Result<StringColumn<'a>, StoreError> {
        let column = batch.column(index);
        let data_type = column.data_type();
        let refuse = || StoreError::Ingest {
            reason: format!("column {name} has type {data_type}; expected a string"),
        };
        match data_type {
            DataType::Utf8 => column
                .as_any()
                .downcast_ref::<StringArray>()
                .map(StringColumn::Utf8)
                .ok_or_else(refuse),
            DataType::LargeUtf8 => column
                .as_any()
                .downcast_ref::<LargeStringArray>()
                .map(StringColumn::Large)
                .ok_or_else(refuse),
            _ => Err(refuse()),
        }
    }

    /// Read one market Parquet file into committed bars. Structural faults
    /// refuse before any row is read; row faults refuse naming the row.
    pub(super) fn read(
        path: &std::path::Path,
        ticker: Option<&str>,
    ) -> Result<OhlcvRead, StoreError> {
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
                        return Err(row_err(row, "ticker is null"));
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
                    return Err(row_err(row, "timestamp is null"));
                }
                let ts_ms = time.millis(batch_row).map_err(|e| row_err(row, e))?;
                let stamp = chrono::DateTime::from_timestamp_millis(ts_ms)
                    .ok_or_else(|| row_err(row, format!("timestamp {ts_ms}ms out of range")))?;
                let price = |column: &PriceColumn<'_>| -> Result<Decimal, StoreError> {
                    if column.is_null(batch_row) {
                        return Err(row_err(row, "price is null"));
                    }
                    column.decimal(batch_row).map_err(|e| row_err(row, e))
                };
                if volume.is_null(batch_row) {
                    return Err(row_err(row, "volume is null"));
                }
                let shares = volume.whole(batch_row).map_err(|e| row_err(row, e))?;
                let bar = Bar::new()
                    .ts(stamp)
                    .o(price(&open)?)
                    .h(price(&high)?)
                    .l(price(&low)?)
                    .c(price(&close)?)
                    .volume(shares)
                    .maybe_adj(None)
                    .build();
                bar.validate().map_err(|e| row_err(row, e))?;
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
        Ok(OhlcvRead {
            bars: merged,
            restatements_merged,
            rows_filtered,
        })
    }
}

#[cfg(all(test, feature = "parquet"))]
#[path = "../tests/market_parquet.rs"]
mod tests_market_parquet;
