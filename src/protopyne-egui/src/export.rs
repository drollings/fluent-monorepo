//! Replay export in the server shapes, field-wise.
//!
//! Export reuses the server shapes field-wise, never a second format: CSV
//! columns are `time,open,high,low,close,volume` (the header
//! [`parse_bars_csv`] accepts), JSON rows serialize the chart-model
//! [`ChartRows`] (field-identical to the server `ChartBars`), and overlay
//! JSON uses the [`OverlayExport`] shape (field-identical to the server
//! `OverlaySeries`). The round-trip tests parse these bytes with the server
//! functions; this module holds no CSV parser of its own.
//!
//! [`parse_bars_csv`]: protopyne_server::csv::parse_bars_csv

use std::fmt::Write as _;

use fluent_egui::EguiError;
use protopyne_chart_model::convert::{ChartRow, ChartRows};

/// CSV header spelling the server ingest accepts (lowercased before match,
/// so this canonical lowercase form always resolves).
pub const EXPORT_CSV_HEADER: &str = "time,open,high,low,close,volume";

/// Render one display float for CSV export. Chart rows carry finite legs by
/// construction; a non-finite leg refuses instead of writing a dialect the
/// server parser would read as a different value.
fn csv_float(value: f64, index: usize) -> Result<String, EguiError> {
    if !value.is_finite() {
        return Err(EguiError::InvalidParam {
            what: "rows".to_owned(),
            reason: format!("row {index} carries a non-finite leg"),
        });
    }
    Ok(value.to_string())
}

/// Export converted rows as server-shaped CSV: the [`EXPORT_CSV_HEADER`]
/// columns in order, one bar per line. No CSV parser lives here — the
/// round-trip tests read these bytes with the server ingest.
pub fn export_rows_csv(rows: &[ChartRow]) -> Result<String, EguiError> {
    let mut out = String::from(EXPORT_CSV_HEADER);
    out.push('\n');
    for (index, row) in rows.iter().enumerate() {
        writeln!(
            out,
            "{},{},{},{},{},{}",
            row.time,
            csv_float(row.open, index)?,
            csv_float(row.high, index)?,
            csv_float(row.low, index)?,
            csv_float(row.close, index)?,
            row.volume,
        )
        .expect("writing to a String never fails");
    }
    Ok(out)
}

/// Export converted rows as server-shaped JSON: the [`ChartRows`] struct
/// serializes field-identical to the server `ChartBars` (`rows`, `dropped`).
/// Refuses non-finite legs (JSON has no `NaN` literal to read back).
pub fn export_rows_json(rows: &[ChartRow], dropped: u64) -> Result<String, EguiError> {
    for (index, row) in rows.iter().enumerate() {
        for leg in [row.open, row.high, row.low, row.close] {
            csv_float(leg, index)?;
        }
    }
    serde_json::to_string(&ChartRows { rows: rows.to_vec(), dropped }).map_err(|e| {
        EguiError::Dependency { reason: format!("rows JSON refused: {e}") }
    })
}

/// Overlay payload field-identical to the server `OverlaySeries` (`kind`,
/// `period`, `values`, `dropped`): one struct so the JSON shape cannot drift
/// field-by-field from the contract the `/chart/overlays` clients read.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OverlayExport {
    /// Kernel id (for example `"sma"`).
    pub kind: String,
    /// Averaging length.
    pub period: i64,
    /// Per-bar values with warmup holes preserved.
    pub values: Vec<Option<f64>>,
    /// Bars without a value, counted never shifted.
    pub dropped: u64,
}

impl OverlayExport {
    /// Build the payload; refuses empty kernel ids with a reason.
    pub fn new(
        kind: impl Into<String>,
        period: i64,
        values: Vec<Option<f64>>,
        dropped: u64,
    ) -> Result<Self, EguiError> {
        let kind = kind.into();
        if kind.trim().is_empty() {
            return Err(EguiError::InvalidParam {
                what: "kind".to_owned(),
                reason: "overlay kind must not be empty".to_owned(),
            });
        }
        Ok(Self { kind, period, values, dropped })
    }
}

/// Export one overlay series as server-shaped JSON. `None` holes serialize
/// as `null` and read back as holes; non-finite `Some` legs refuse.
pub fn export_overlay_json(
    kind: &str,
    period: i64,
    values: &[Option<f64>],
    dropped: u64,
) -> Result<String, EguiError> {
    for value in values.iter().flatten() {
        if !value.is_finite() {
            return Err(EguiError::InvalidParam {
                what: "values".to_owned(),
                reason: "overlay values must be holes or finite".to_owned(),
            });
        }
    }
    let payload = OverlayExport::new(kind, period, values.to_vec(), dropped)?;
    serde_json::to_string(&payload)
        .map_err(|e| EguiError::Dependency { reason: format!("overlay JSON refused: {e}") })
}

#[cfg(test)]
#[path = "../tests/export.rs"]
mod tests;
