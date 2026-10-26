//! CSV ingest for versioned bar history.
//!
//! Normalizes the legacy export shape (`Volume` header, epoch-seconds `time`)
//! into [`Bar`](protopyne_types::Bar) values. Every malformed row is a typed
//! [`ServerError::BadRow`](crate::error::ServerError) carrying its 1-based
//! line number; rows are never skipped silently.

use std::path::Path;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::error::ServerError;
use protopyne_types::{validate_new_bar, Bar};

/// Required columns after lowercasing the header row.
const REQUIRED_COLUMNS: [&str; 6] = ["time", "open", "high", "low", "close", "volume"];

/// Shared field readers for the crate's CSV ingests: the bar reader below
/// and the chain reader both refuse malformed cells in this one vocabulary,
/// so a refusal names the same line, column, and value whichever file broke.
pub(crate) fn parse_decimal_field(
    field: &str,
    line: usize,
    column: &str,
) -> Result<Decimal, ServerError> {
    field.parse::<Decimal>().map_err(|_| ServerError::BadRow {
        line,
        reason: format!("invalid decimal in column '{column}': {field:?}"),
    })
}

pub(crate) fn parse_timestamp_field(
    field: &str,
    line: usize,
) -> Result<DateTime<Utc>, ServerError> {
    let secs: i64 = field.parse().map_err(|_| ServerError::BadRow {
        line,
        reason: format!("invalid epoch timestamp: {field:?}"),
    })?;
    DateTime::from_timestamp(secs, 0).ok_or(ServerError::BadRow {
        line,
        reason: format!("epoch timestamp out of range: {field:?}"),
    })
}

/// Shared integer reader for optional chain columns (sizes, open interest):
/// a non-empty cell that will not parse refuses with its column named.
pub(crate) fn parse_int_field<T: std::str::FromStr>(
    field: &str,
    line: usize,
    column: &str,
) -> Result<T, ServerError> {
    field.parse::<T>().map_err(|_| ServerError::BadRow {
        line,
        reason: format!("invalid integer in column '{column}': {field:?}"),
    })
}

/// Position of one optional column in a CSV header row, after lowercasing:
/// `None` when the file does not carry it. The caller decides the absent
/// default; a present-but-malformed cell still refuses at the field reader.
pub(crate) fn optional_column(record: &csv::StringRecord, name: &str) -> Option<usize> {
    record.iter().position(|h| h.to_lowercase() == name)
}

/// Positions of the `required` columns in a CSV header row, in `required`
/// order, after lowercasing. Refuses with the missing column names when any
/// is absent, so both ingests name the same gap.
pub(crate) fn header_positions(
    record: &csv::StringRecord,
    required: &[&str],
) -> Result<Vec<usize>, ServerError> {
    let headers = record.iter().map(str::to_lowercase).collect::<Vec<_>>();
    let index_of = |name: &str| headers.iter().position(|h| h == name);
    let missing: Vec<&str> = required
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
    Ok(required
        .iter()
        .map(|name| index_of(name).expect("column presence checked"))
        .collect())
}

fn parse_volume(field: &str, line: usize) -> Result<u64, ServerError> {
    field.parse::<u64>().map_err(|_| ServerError::BadRow {
        line,
        reason: format!("invalid volume: {field:?}"),
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
        .map_err(|e| ServerError::BadHeader(format!("cannot read header row: {e}")))?;
    let positions = header_positions(headers, &REQUIRED_COLUMNS)?;
    let (time_c, open_c, high_c, low_c, close_c, vol_c) = (
        positions[0],
        positions[1],
        positions[2],
        positions[3],
        positions[4],
        positions[5],
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
        let bar = Bar::new()
            .ts(parse_timestamp_field(field(time_c, "time")?, line)?)
            .o(parse_decimal_field(field(open_c, "open")?, line, "open")?)
            .h(parse_decimal_field(field(high_c, "high")?, line, "high")?)
            .l(parse_decimal_field(field(low_c, "low")?, line, "low")?)
            .c(parse_decimal_field(
                field(close_c, "close")?,
                line,
                "close",
            )?)
            .volume(parse_volume(field(vol_c, "volume")?, line)?)
            .maybe_adj(None)
            .build();
        // Structural validation shares the one rule with the streaming feed
        // (`BarWindow::push` enforces the same `validate_new_bar`): an
        // inverted OHLC row or a timestamp that does not advance past the
        // previously accepted row refuses the file in the existing `BadRow`
        // shape, so the fallback ladder and audits treat it exactly like any
        // malformed row. Tampered or gapped history never flows into decisions.
        if let Err(e) = validate_new_bar(bars.last(), &bar) {
            return Err(ServerError::BadRow {
                line,
                reason: e.to_string(),
            });
        }
        bars.push(bar);
    }
    Ok(bars)
}

/// Load closed bars from a CSV file at `path`.
pub(crate) fn load_bars_csv(path: &Path) -> Result<Vec<Bar>, ServerError> {
    let content = common_core::io::read_to_string_err(path)?;
    parse_bars_csv(&content)
}

#[cfg(test)]
#[path = "../tests/csv.rs"]
mod tests;
