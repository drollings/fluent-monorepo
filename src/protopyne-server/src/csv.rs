//! CSV ingest for versioned bar history.
//!
//! Normalizes the legacy export shape (`Volume` header, epoch-seconds `time`)
//! into [`Bar`](protopyne_types::Bar) values. Every malformed row is a typed
//! [`ServerError::BadRow`](crate::error::ServerError) carrying its 1-based
//! line number; rows are never skipped silently.

use std::path::Path;

use chrono::DateTime;
use rust_decimal::Decimal;

use crate::error::ServerError;
use protopyne_types::Bar;

/// Required columns after lowercasing the header row.
const REQUIRED_COLUMNS: [&str; 6] = ["time", "open", "high", "low", "close", "volume"];

fn parse_decimal(field: &str, line: usize, column: &str) -> Result<Decimal, ServerError> {
    field.parse::<Decimal>().map_err(|_| ServerError::BadRow {
        line,
        reason: format!("invalid decimal in column '{column}': {field:?}"),
    })
}

fn parse_volume(field: &str, line: usize) -> Result<u64, ServerError> {
    field.parse::<u64>().map_err(|_| ServerError::BadRow {
        line,
        reason: format!("invalid volume: {field:?}"),
    })
}

fn parse_timestamp(field: &str, line: usize) -> Result<chrono::DateTime<chrono::Utc>, ServerError> {
    let secs: i64 = field.parse().map_err(|_| ServerError::BadRow {
        line,
        reason: format!("invalid epoch timestamp: {field:?}"),
    })?;
    DateTime::from_timestamp(secs, 0).ok_or(ServerError::BadRow {
        line,
        reason: format!("epoch timestamp out of range: {field:?}"),
    })
}

/// Parse CSV content into closed bars.
pub fn parse_bars_csv(content: &str) -> Result<Vec<Bar>, ServerError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .trim(csv::Trim::All)
        .from_reader(content.as_bytes());

    let headers = reader
        .headers()
        .map_err(|e| ServerError::BadHeader(format!("cannot read header row: {e}")))?
        .iter()
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    let index_of = |name: &str| headers.iter().position(|h| h == name);
    let missing: Vec<&str> = REQUIRED_COLUMNS
        .iter()
        .copied()
        .filter(|name| index_of(name).is_none())
        .collect();
    if !missing.is_empty() {
        return Err(ServerError::BadHeader(format!(
            "missing columns: {}",
            missing.join(", ")
        )));
    }
    let col = |name: &str| index_of(name).expect("column presence checked");
    let (time_c, open_c, high_c, low_c, close_c, vol_c) = (
        col("time"),
        col("open"),
        col("high"),
        col("low"),
        col("close"),
        col("volume"),
    );

    let mut bars = Vec::new();
    for (row_idx, record) in reader.records().enumerate() {
        // Line 1 is the header, so the first data row is line 2.
        let line = row_idx + 2;
        let record = record.map_err(|e| ServerError::BadRow {
            line,
            reason: format!("cannot read row: {e}"),
        })?;
        let field = |idx: usize, name: &str| {
            record.get(idx).ok_or(ServerError::BadRow {
                line,
                reason: format!("row has no column '{name}'"),
            })
        };
        bars.push(
            Bar::new()
                .ts(parse_timestamp(field(time_c, "time")?, line)?)
                .o(parse_decimal(field(open_c, "open")?, line, "open")?)
                .h(parse_decimal(field(high_c, "high")?, line, "high")?)
                .l(parse_decimal(field(low_c, "low")?, line, "low")?)
                .c(parse_decimal(field(close_c, "close")?, line, "close")?)
                .volume(parse_volume(field(vol_c, "volume")?, line)?)
                .maybe_adj(None)
                .build(),
        );
    }
    Ok(bars)
}

/// Load closed bars from a CSV file at `path`.
pub fn load_bars_csv(path: &Path) -> Result<Vec<Bar>, ServerError> {
    let content = common_core::io::read_to_string_err(path)?;
    parse_bars_csv(&content)
}

#[cfg(test)]
#[path = "../tests/csv.rs"]
mod tests;
