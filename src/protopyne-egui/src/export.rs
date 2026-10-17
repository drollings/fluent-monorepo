//! Replay export in the server shapes, field-wise.
//!
//! Export reuses the server shapes field-wise, never a second format: CSV
//! columns are `time,open,high,low,close,volume` (the header
//! [`parse_bars_csv`] accepts), JSON rows serialize the chart-model
//! [`ChartBars`] (the server's own wire container), and overlay
//! JSON serializes the wire [`OverlaySeries`] directly — one type, no local
//! twin. The round-trip tests parse these bytes with the server functions;
//! this module holds no CSV parser of its own.
//!
//! [`parse_bars_csv`]: protopyne_server::csv::parse_bars_csv

use std::fmt::Write as _;

use fluent_egui::EguiError;
use protopyne_chart_model::convert::{ChartBars, ChartRow};
use protopyne_view::OverlaySeries;

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

/// Export converted rows as server-shaped JSON: the [`ChartBars`] struct is
/// the server's own wire container (`rows`, `dropped`), serialized directly.
/// Refuses non-finite legs (JSON has no `NaN` literal to read back).
pub fn export_rows_json(rows: &[ChartRow], dropped: u64) -> Result<String, EguiError> {
    for (index, row) in rows.iter().enumerate() {
        for leg in [row.open, row.high, row.low, row.close] {
            csv_float(leg, index)?;
        }
    }
    serde_json::to_string(&ChartBars { rows: rows.to_vec(), dropped }).map_err(|e| {
        EguiError::Dependency { reason: format!("rows JSON refused: {e}") }
    })
}

/// Export one overlay series as server-shaped JSON. `None` holes serialize
/// as `null` and read back as holes; non-finite `Some` legs and empty kernel
/// ids refuse. Serializes the wire [`OverlaySeries`] itself (field-identical
/// to the contract the `/chart/overlays` clients read), so no local twin can
/// drift from it.
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
    if kind.trim().is_empty() {
        return Err(EguiError::InvalidParam {
            what: "kind".to_owned(),
            reason: "overlay kind must not be empty".to_owned(),
        });
    }
    serde_json::to_string(&OverlaySeries {
        kind: kind.to_owned(),
        period,
        values: values.to_vec(),
        dropped,
    })
    .map_err(|e| EguiError::Dependency { reason: format!("overlay JSON refused: {e}") })
}

#[cfg(test)]
#[path = "../tests/export.rs"]
mod tests;
