//! Server-side ingest and service errors.

use common_core::error::IoError;
use thiserror::Error;

/// Errors from CSV ingest and other server-side file handling.
#[derive(Error, Debug)]
/// Errors from CSV ingest and other server-side file handling.
pub enum ServerError {
    /// A CSV row could not be converted to a [`Bar`](protopyne_types::Bar).
    /// `line` is 1-based and counts the header line, so the first data row
    /// is line 2. Rows are never skipped silently.
    #[error("bad CSV row at line {line}: {reason}")]
    BadRow {
        /// 1-based line number including the header line.
        line: usize,
        /// What failed to parse and why.
        reason: String,
    },
    /// The CSV header lacks a required column.
    #[error("bad CSV header: {0}")]
    BadHeader(String),
    /// A market-data Parquet artifact could not be read: a schema mismatch,
    /// an undecodable or null value, a fractional volume, or a mixed-ticker
    /// file read without a ticker name. Carries the file and the reason;
    /// never skipped silently.
    #[error("bad parquet history {path}: {reason}")]
    Parquet {
        /// The candidate file that failed.
        path: String,
        /// What failed and why.
        reason: String,
    },
    /// Underlying I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] IoError),
    /// Backtest misconfiguration or empty history. Fail closed.
    #[error("backtest error: {0}")]
    Backtest(String),
    /// HTTP service failure (bind, boot gates, bundled signal). Fail closed.
    #[error("service error: {0}")]
    Service(String),
}

common_core::impl_from_io_error!(ServerError);