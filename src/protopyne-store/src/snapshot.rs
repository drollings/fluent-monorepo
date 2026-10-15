//! Sealed Parquet snapshots: a byte-stable, content-addressed run input.
//!
//! A snapshot exists for a different reason than the working tape. It is not
//! a cold tier, not a partition hierarchy, not a coarsened materialization:
//! it is **one sealed artifact with one digest** that a rerun can be asserted
//! against. CSV cannot serve that role — no schema, no guaranteed ordering
//! across writers, no stable digest — so the artifact is Parquet, and the
//! research path keeps its own `polars` reader rather than sharing a writer.
//!
//! The write is a durability sequence, not a file dump: the artifact is
//! written to a `*.tmp` sibling, fsynced, then atomically renamed into place,
//! and only then is its manifest row committed — in one transaction. A crash
//! can therefore produce an orphan file (no row), which
//! [`reconcile_orphans`] removes loudly, but never a manifest row pointing at
//! a missing or partial file.
//!
//! Prices are `Decimal128` at the market's pinned [`PriceScale`]. A value the
//! scale cannot represent **refuses**; it is never rounded and never
//! truncated. `ts_ms` is epoch milliseconds UTC, no timezone. The corporate
//! action `adj` column is carried verbatim so a sealed bar equals its stored
//! bar field for field.
//!
//! [`PriceScale`]: crate::catalog::PriceScale

#[cfg(feature = "parquet")]
use std::fs::File;
use std::path::{Path, PathBuf};

use protopyne_types::{Bar, validate_new_bar};

use crate::capability::check_present;
use crate::catalog::{Market, MarketCatalog};
use crate::error::{PlanError, SnapshotError, StoreError};
use crate::key::TapeKey;
use crate::manifest::{self, SnapshotRecord};
use crate::sqlite_tape::{fetch_bars, order_and_validate};
use crate::store::DEFAULT_APPEND_CHUNK;

#[cfg(feature = "parquet")]
use std::sync::Arc;

#[cfg(feature = "parquet")]
use arrow::array::{Array, Decimal128Array, Int64Array, StringArray};
#[cfg(feature = "parquet")]
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
#[cfg(feature = "parquet")]
use arrow::record_batch::RecordBatch;
#[cfg(feature = "parquet")]
use chrono::DateTime;
#[cfg(feature = "parquet")]
use parquet::arrow::ArrowWriter;
#[cfg(feature = "parquet")]
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
#[cfg(feature = "parquet")]
use parquet::basic::Compression;
#[cfg(feature = "parquet")]
use parquet::file::properties::WriterProperties;
#[cfg(feature = "parquet")]
use rust_decimal::Decimal;

/// Decimal128 precision: the Arrow maximum of 38 significant digits, wide
/// enough for every scale up to the `Decimal` maximum of 28.
#[cfg(feature = "parquet")]
const DECIMAL_PRECISION: u8 = 38;

/// The minimum rows per Parquet row group, a **cost bound** — not an accuracy
/// threshold. Too many tiny trailing groups cost open/metadata IO for no
/// data; a trailing group smaller than this merges into its predecessor.
///
/// The value is fixed by the recorded seal-economics measurement at a fixed
/// chunk size over a representative corpus: it is the smallest threshold
/// beyond which merging additional rows does not reduce the artifact's byte
/// count. It is a storage-economy number and is deliberately not tuned by
/// any accuracy experiment.
pub const DEFAULT_MIN_PART_ROWS: usize = 16;

/// How a seal chunks rows into Parquet row groups. Both are **cost bounds**.
#[derive(Debug, Clone, Copy, PartialEq, Eq, bon::Builder)]
#[builder(start_fn = new)]
pub struct SealOptions {
    /// Rows per row group before a trailing merge. Defaults to the shared
    /// append-chunk bound.
    #[builder(default = DEFAULT_APPEND_CHUNK)]
    pub row_group_rows: usize,
    /// Smallest trailing row group that is written as its own group. A
    /// smaller trailing group merges into its predecessor.
    #[builder(default = DEFAULT_MIN_PART_ROWS)]
    pub min_part_rows: usize,
}

/// What one seal produced: the artifact, its coverage, and its digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedSnapshot {
    /// The tape that was sealed.
    pub key: TapeKey,
    /// The artifact's final path.
    pub path: PathBuf,
    /// The first sealed bar stamp.
    pub first_ts_ms: i64,
    /// The last sealed bar stamp.
    pub last_ts_ms: i64,
    /// Rows sealed.
    pub row_count: u64,
    /// Artifact size in bytes.
    pub byte_len: u64,
    /// The artifact's content digest (BLAKE3 hex).
    pub digest: String,
}

impl SealedSnapshot {
    /// The manifest row this seal would commit.
    #[must_use]
    pub fn record(&self, written_at_ms: i64) -> SnapshotRecord {
        SnapshotRecord {
            symbol_id: self.key.symbol_id,
            timeframe: self.key.timeframe,
            path: self.path.to_string_lossy().into_owned(),
            first_ts_ms: self.first_ts_ms,
            last_ts_ms: self.last_ts_ms,
            row_count: self.row_count,
            byte_len: self.byte_len,
            digest: self.digest.clone(),
            written_at_ms,
        }
    }
}

/// Write sealed artifacts into one directory with one chunk policy.
///
/// The writer is file-only: it never touches the database. [`seal`] composes
/// it with the manifest transaction.
#[derive(Debug)]
pub struct SnapshotWriter<'a> {
    out_dir: &'a Path,
    options: SealOptions,
}

impl<'a> SnapshotWriter<'a> {
    /// A writer into `out_dir` under `options`.
    #[must_use]
    pub fn new(out_dir: &'a Path, options: SealOptions) -> Self {
        Self { out_dir, options }
    }

    /// The chunk policy this writer applies.
    #[must_use]
    pub fn options(&self) -> SealOptions {
        self.options
    }

    /// The artifact path for `key`: deterministic, so re-sealing a tape
    /// replaces its artifact in place.
    #[must_use]
    pub fn path_for(&self, key: TapeKey) -> PathBuf {
        self.out_dir.join(format!(
            "{:010}-{}.parquet",
            key.symbol_id.0,
            key.timeframe.code()
        ))
    }

    /// Seal `bars` for `market` as an artifact. Writes `*.tmp`, fsyncs, then
    /// atomically renames into place. Does not touch the database.
    pub fn write(
        &self,
        key: TapeKey,
        market: &Market,
        bars: &[Bar],
    ) -> Result<SealedSnapshot, StoreError> {
        feature_gate()?;
        check_series(key, market, bars)?;
        #[cfg(feature = "parquet")]
        {
            write_parquet(self, key, market, bars)
        }
        #[cfg(not(feature = "parquet"))]
        {
            let _ = (key, market, bars);
            Err(StoreError::FeatureDisabled("parquet".to_owned()))
        }
    }
}

/// Seal `key`'s tape into `out_dir`: read the committed bars, write the
/// artifact, then commit the manifest row in one transaction. Returns the
/// sealed artifact; the row and the artifact commit together.
pub fn seal(
    catalog: &MarketCatalog,
    key: TapeKey,
    out_dir: &Path,
    options: SealOptions,
    now_ms: i64,
) -> Result<SealedSnapshot, StoreError> {
    feature_gate()?;
    check_present(catalog.permit())?;
    let market = catalog.market_by_id(key.symbol_id)?.ok_or_else(|| {
        StoreError::Plan(PlanError::UnknownSymbol(format!(
            "symbol_id {}",
            key.symbol_id.0
        )))
    })?;
    let bars = {
        let conn = common_core::sync::lock(catalog.connection());
        let raw = fetch_bars(&conn, key.symbol_id, None, None)?;
        order_and_validate(raw)?
    };
    let sealed = SnapshotWriter::new(out_dir, options).write(key, &market, &bars)?;
    let record = sealed.record(now_ms);
    {
        let mut conn = common_core::sync::lock(catalog.connection());
        let tx = conn.transaction()?;
        manifest::upsert_record(&tx, &record)?;
        tx.commit()?;
    }
    tracing::info!(
        symbol_id = key.symbol_id.0,
        timeframe = key.timeframe.code(),
        rows = sealed.row_count,
        bytes = sealed.byte_len,
        digest = %sealed.digest,
        "sealed snapshot"
    );
    Ok(sealed)
}

/// Read a sealed artifact back into committed bars, verifying that its
/// manifest row exists and that the file's digest matches the recorded one.
///
/// A file with no manifest row and a manifest row with no file are **both**
/// loud [`StoreError`]s: a snapshot is never silently absent.
pub fn read_snapshot(
    catalog: &MarketCatalog,
    key: TapeKey,
    path: &Path,
) -> Result<Vec<Bar>, StoreError> {
    feature_gate()?;
    check_present(catalog.permit())?;
    let path_text = path.to_string_lossy().into_owned();
    let record = {
        let conn = common_core::sync::lock(catalog.connection());
        manifest::record_for(&conn, key.symbol_id, key.timeframe, &path_text)?
    };
    let Some(record) = record else {
        return Err(StoreError::Snapshot(SnapshotError::Unmanifested {
            path: path_text,
        }));
    };
    if !path.is_file() {
        return Err(StoreError::Snapshot(SnapshotError::MissingArtifact {
            path: path_text,
        }));
    }
    let measured = common_core::hash::hash_file(path, common_core::hash::HashAlgorithm::Blake3)?;
    if measured != record.digest {
        return Err(StoreError::Snapshot(SnapshotError::Artifact {
            reason: format!(
                "digest mismatch for {path_text}: recorded {}, measured {measured}",
                record.digest
            ),
        }));
    }
    #[cfg(feature = "parquet")]
    {
        let bars = read_parquet(path)?;
        if bars.len() as u64 != record.row_count {
            return Err(StoreError::Snapshot(SnapshotError::Artifact {
                reason: format!(
                    "row count mismatch for {path_text}: recorded {}, read {}",
                    record.row_count,
                    bars.len()
                ),
            }));
        }
        order_and_validate(bars)
    }
    #[cfg(not(feature = "parquet"))]
    {
        let _ = record;
        Err(StoreError::FeatureDisabled("parquet".to_owned()))
    }
}

/// What a reconcile pass observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Manifest rows seen.
    pub rows_checked: usize,
    /// Orphan files removed: no manifest row named them.
    pub orphans_removed: Vec<String>,
}

/// Remove artifacts that no manifest row names. A file with no row is an
/// orphan from an interrupted seal (the file was renamed into place but the
/// manifest transaction never committed). Removal is logged, never silent.
///
/// The reverse — a manifest row whose file is missing — is **not** removed
/// here: it is a loud read error from [`read_snapshot`], because deleting the
/// row would hide a durability fault.
pub fn reconcile_orphans(
    catalog: &MarketCatalog,
    out_dir: &Path,
) -> Result<ReconcileReport, StoreError> {
    check_present(catalog.permit())?;
    let known: std::collections::HashSet<String> = {
        let conn = common_core::sync::lock(catalog.connection());
        manifest::all_records(&conn)?
            .into_iter()
            .map(|record| record.path)
            .collect()
    };
    let mut orphans_removed = Vec::new();
    match std::fs::read_dir(out_dir) {
        Ok(entries) => {
            for entry in entries {
                let path = entry?.path();
                let is_artifact = path.extension().and_then(|ext| ext.to_str()) == Some("parquet");
                if !is_artifact {
                    continue;
                }
                let text = path.to_string_lossy().into_owned();
                if !known.contains(&text) {
                    std::fs::remove_file(&path)?;
                    tracing::warn!(path = %text, "removed orphan snapshot with no manifest row");
                    orphans_removed.push(text);
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    orphans_removed.sort();
    Ok(ReconcileReport {
        rows_checked: known.len(),
        orphans_removed,
    })
}

/// Refuse early when the crate was built without the `parquet` feature, so a
/// lean build reports a disabled feature instead of a compile error.
fn feature_gate() -> Result<(), StoreError> {
    if cfg!(feature = "parquet") {
        Ok(())
    } else {
        Err(StoreError::FeatureDisabled("parquet".to_owned()))
    }
}

/// Validate the series and its spacing before any byte is written: strictly
/// ascending stamps, every bar structurally valid, and a spacing the market
/// holds. A snapshot never reorders silently — order is part of the
/// content-addressed identity.
fn check_series(key: TapeKey, market: &Market, bars: &[Bar]) -> Result<(), StoreError> {
    if key.timeframe != market.timeframe {
        return Err(StoreError::Plan(PlanError::UnsupportedTimeframe {
            symbol: market.symbol.clone(),
            timeframe: key.timeframe,
        }));
    }
    let mut last: Option<&Bar> = None;
    for bar in bars {
        validate_new_bar(last, bar).map_err(|e| StoreError::InvalidBar(e.to_string()))?;
        last = Some(bar);
    }
    Ok(())
}

/// The trailing-merge rule: split `n` rows into chunks of at most
/// `row_group_rows`, then fold a trailing chunk smaller than `min_part_rows`
/// into its predecessor while more than one chunk remains. Returns the chunk
/// sizes in order. Both bounds are clamped to at least one row.
#[cfg(feature = "parquet")]
#[must_use]
pub(crate) fn plan_parts(n: usize, row_group_rows: usize, min_part_rows: usize) -> Vec<usize> {
    let chunk = row_group_rows.max(1);
    let floor = min_part_rows.max(1);
    let mut parts = Vec::new();
    let mut remaining = n;
    while remaining > 0 {
        let take = remaining.min(chunk);
        parts.push(take);
        remaining -= take;
    }
    while parts.len() > 1 && parts.last().is_some_and(|&last| last < floor) {
        if let Some(last) = parts.pop() {
            if let Some(previous) = parts.last_mut() {
                *previous += last;
            }
        }
    }
    parts
}

#[cfg(feature = "parquet")]
fn write_parquet(
    writer: &SnapshotWriter<'_>,
    key: TapeKey,
    market: &Market,
    bars: &[Bar],
) -> Result<SealedSnapshot, StoreError> {
    if bars.is_empty() {
        return Err(StoreError::Snapshot(SnapshotError::Artifact {
            reason: "refusing to seal a tape with no committed rows".to_owned(),
        }));
    }
    std::fs::create_dir_all(writer.out_dir)?;
    let final_path = writer.path_for(key);
    let tmp_path = final_path.with_extension("parquet.tmp");
    let scale = market.price_scale.digits;
    let scale_i8 = i8::try_from(scale).map_err(|_| StoreError::Config(format!(
        "price scale {scale} does not fit the Arrow decimal scale"
    )))?;
    let schema = arrow_schema(scale_i8);
    let properties = WriterProperties::builder()
        .set_compression(Compression::SNAPPY)
        .build();

    let parts = plan_parts(
        bars.len(),
        writer.options.row_group_rows,
        writer.options.min_part_rows,
    );
    {
        let file = File::create(&tmp_path)?;
        let mut arrow_writer = ArrowWriter::try_new(file, Arc::clone(&schema), Some(properties))
            .map_err(artifact)?;
        let mut start = 0_usize;
        for part in &parts {
            let batch = build_batch(&schema, market, &bars[start..start + *part], scale_i8)?;
            arrow_writer.write(&batch).map_err(artifact)?;
            arrow_writer.flush().map_err(artifact)?;
            start += *part;
        }
        arrow_writer.close().map_err(artifact)?;
    }
    // Durability: fsync the finished artifact before it can be named.
    File::open(&tmp_path)?.sync_all()?;
    std::fs::rename(&tmp_path, &final_path)?;
    sync_dir(writer.out_dir);

    let byte_len = std::fs::metadata(&final_path)?.len();
    let digest = common_core::hash::hash_file(&final_path, common_core::hash::HashAlgorithm::Blake3)?;
    Ok(SealedSnapshot {
        key,
        path: final_path,
        first_ts_ms: bars.first().map_or(0, |bar| bar.ts.timestamp_millis()),
        last_ts_ms: bars.last().map_or(0, |bar| bar.ts.timestamp_millis()),
        row_count: bars.len() as u64,
        byte_len,
        digest,
    })
}

/// Best-effort directory fsync so the rename is durable. A directory that
/// cannot be opened for sync is not fatal — the artifact is already fsynced;
/// the rename's durability is the filesystem's responsibility.
#[cfg(feature = "parquet")]
fn sync_dir(dir: &Path) {
    if let Ok(handle) = File::open(dir) {
        let _ = handle.sync_all();
    }
}

#[cfg(feature = "parquet")]
fn arrow_schema(scale: i8) -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("ts_ms", DataType::Int64, false),
        Field::new("open", DataType::Decimal128(DECIMAL_PRECISION, scale), false),
        Field::new("high", DataType::Decimal128(DECIMAL_PRECISION, scale), false),
        Field::new("low", DataType::Decimal128(DECIMAL_PRECISION, scale), false),
        Field::new("close", DataType::Decimal128(DECIMAL_PRECISION, scale), false),
        Field::new("volume", DataType::Int64, false),
        Field::new("adj", DataType::Utf8, true),
    ]))
}

#[cfg(feature = "parquet")]
fn build_batch(
    schema: &SchemaRef,
    market: &Market,
    bars: &[Bar],
    scale: i8,
) -> Result<RecordBatch, StoreError> {
    let width = bars.len();
    let mut ts = Vec::with_capacity(width);
    let mut open = Vec::with_capacity(width);
    let mut high = Vec::with_capacity(width);
    let mut low = Vec::with_capacity(width);
    let mut close = Vec::with_capacity(width);
    let mut volume = Vec::with_capacity(width);
    let mut adj: Vec<Option<String>> = Vec::with_capacity(width);
    let scale_u32 = scale as u32;
    for bar in bars {
        ts.push(bar.ts.timestamp_millis());
        open.push(encode_decimal(&market.symbol, bar.o, scale_u32)?);
        high.push(encode_decimal(&market.symbol, bar.h, scale_u32)?);
        low.push(encode_decimal(&market.symbol, bar.l, scale_u32)?);
        close.push(encode_decimal(&market.symbol, bar.c, scale_u32)?);
        volume.push(i64::try_from(bar.volume).map_err(|_| {
            StoreError::InvalidBar(format!("volume {} exceeds the sealed range", bar.volume))
        })?);
        adj.push(
            bar.adj
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| {
                    StoreError::Snapshot(SnapshotError::Artifact {
                        reason: format!("adjustment does not serialize: {e}"),
                    })
                })?,
        );
    }
    let decimal = |values: Vec<i128>| -> Result<Decimal128Array, StoreError> {
        Decimal128Array::from(values)
            .with_precision_and_scale(DECIMAL_PRECISION, scale)
            .map_err(artifact)
    };
    RecordBatch::try_new(
        Arc::clone(schema),
        vec![
            Arc::new(Int64Array::from(ts)),
            Arc::new(decimal(open)?),
            Arc::new(decimal(high)?),
            Arc::new(decimal(low)?),
            Arc::new(decimal(close)?),
            Arc::new(Int64Array::from(volume)),
            Arc::new(StringArray::from(adj)),
        ],
    )
    .map_err(artifact)
}

/// Encode one price at the pinned scale. A value needing more digits refuses
/// with the market, price, and scale named; a value whose scaled magnitude
/// would overflow `Decimal128` refuses the same way. Never rounded, never
/// truncated.
#[cfg(feature = "parquet")]
fn encode_decimal(symbol: &str, value: Decimal, scale: u32) -> Result<i128, StoreError> {
    let normalized = value.normalize();
    let value_scale = normalized.scale();
    let refuse = || StoreError::PriceScale {
        symbol: symbol.to_owned(),
        value,
        scale,
    };
    if value_scale > scale {
        return Err(refuse());
    }
    let factor = 10_i128.checked_pow(scale - value_scale).ok_or_else(refuse)?;
    normalized.mantissa().checked_mul(factor).ok_or_else(refuse)
}

#[cfg(feature = "parquet")]
fn read_parquet(path: &Path) -> Result<Vec<Bar>, StoreError> {
    let file = File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(artifact)?;
    let schema = builder.schema().clone();
    let scale = decimal_scale(&schema)?;
    let reader = builder.build().map_err(artifact)?;
    let mut bars = Vec::new();
    for batch in reader {
        let batch = batch.map_err(artifact)?;
        let ts = column::<Int64Array>(&batch, 0, "ts_ms")?;
        let open = column::<Decimal128Array>(&batch, 1, "open")?;
        let high = column::<Decimal128Array>(&batch, 2, "high")?;
        let low = column::<Decimal128Array>(&batch, 3, "low")?;
        let close = column::<Decimal128Array>(&batch, 4, "close")?;
        let volume = column::<Int64Array>(&batch, 5, "volume")?;
        let adj = column::<StringArray>(&batch, 6, "adj")?;
        for i in 0..batch.num_rows() {
            let ts_ms = ts.value(i);
            let stamp = DateTime::from_timestamp_millis(ts_ms).ok_or_else(|| {
                StoreError::Snapshot(SnapshotError::Artifact {
                    reason: format!("sealed stamp {ts_ms} out of range"),
                })
            })?;
            let price = |value: i128| Decimal::from_i128_with_scale(value, scale);
            let adjustment = if adj.is_null(i) {
                None
            } else {
                Some(serde_json::from_str(adj.value(i)).map_err(|e| {
                    StoreError::Snapshot(SnapshotError::Artifact {
                        reason: format!("sealed adjustment is not valid JSON: {e}"),
                    })
                })?)
            };
            bars.push(
                Bar::new()
                    .ts(stamp)
                    .o(price(open.value(i)))
                    .h(price(high.value(i)))
                    .l(price(low.value(i)))
                    .c(price(close.value(i)))
                    .volume(u64::try_from(volume.value(i)).map_err(|_| {
                        StoreError::Snapshot(SnapshotError::Artifact {
                            reason: "sealed volume is negative".to_owned(),
                        })
                    })?)
                    .maybe_adj(adjustment)
                    .build(),
            );
        }
    }
    Ok(bars)
}

/// Downcast the positional column to the sealed schema's pinned array
/// type. Positions are the contract — `ts_ms` first, `adj` last — and the
/// shared decode layer performs the downcast.
#[cfg(feature = "parquet")]
fn column<'a, T: 'static>(batch: &'a RecordBatch, index: usize, name: &str) -> Result<&'a T, StoreError> {
    crate::arrow_decode::column_at(batch, index).map_err(|_| {
        StoreError::Snapshot(SnapshotError::Artifact {
            reason: format!("sealed column {name} has an unexpected type"),
        })
    })
}

/// Read the sealed price scale: the `open` field must carry `Decimal128`,
/// and its scale must check through the shared scale rule. A negative
/// scale now refuses where the private reader silently read zero — no
/// sealed artifact the writer produces carries one, so no valid artifact
/// changes meaning.
#[cfg(feature = "parquet")]
fn decimal_scale(schema: &SchemaRef) -> Result<u32, StoreError> {
    crate::arrow_decode::decimal_scale_of(schema, "open").map_err(|err| {
        StoreError::Snapshot(SnapshotError::Artifact {
            reason: match err {
                crate::arrow_decode::DecodeError::BadScale { scale, .. } => {
                    format!("sealed column open carries negative decimal scale {scale}")
                }
                crate::arrow_decode::DecodeError::UnexpectedType { .. } => {
                    "sealed schema carries no decimal price scale".to_owned()
                }
            },
        })
    })
}

#[cfg(feature = "parquet")]
fn artifact(err: impl std::fmt::Display) -> StoreError {
    StoreError::Snapshot(SnapshotError::Artifact {
        reason: err.to_string(),
    })
}

#[cfg(all(test, feature = "parquet"))]
#[path = "../tests/common/snapshot.rs"]
pub(crate) mod test_snapshot;

#[cfg(all(test, feature = "parquet"))]
#[path = "../tests/snapshot_roundtrip.rs"]
mod tests_roundtrip;

#[cfg(all(test, feature = "parquet"))]
#[path = "../tests/snapshot_market_parity.rs"]
mod tests_market_parity;

#[cfg(all(test, feature = "parquet"))]
#[path = "../tests/snapshot_manifest_crash.rs"]
mod tests_manifest_crash;

#[cfg(all(test, feature = "parquet"))]
#[path = "../tests/snapshot_digest_stability.rs"]
mod tests_digest_stability;

#[cfg(all(test, feature = "parquet"))]
#[path = "../tests/snapshot_price_scale.rs"]
mod tests_price_scale;

#[cfg(all(test, feature = "parquet"))]
#[path = "../tests/snapshot_seal_economics.rs"]
mod tests_seal_economics;
