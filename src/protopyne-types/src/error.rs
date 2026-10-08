//! Error types for the base market-data vocabulary.

use common_core::error::IoError;
use thiserror::Error;

/// Errors produced while parsing or validating base market-data types.
#[derive(Error, Debug)]
pub enum TypesError {
    /// Unknown timeframe code (expected one of `M1/M5/M15/M30/H1/H4/D1`).
    #[error("unknown timeframe code: {0}")]
    UnknownTimeframe(String),
    /// A bar failed structural validation.
    #[error("invalid bar: {0}")]
    InvalidBar(String),
    /// Underlying I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] IoError),
}

common_core::impl_from_io_error!(TypesError);

#[cfg(test)]
#[path = "../tests/error.rs"]
mod tests;
