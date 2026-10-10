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
    /// A money amount failed to parse.
    #[error("invalid money: {0}")]
    InvalidMoney(String),
    /// A quantity failed to parse.
    #[error("invalid quantity: {0}")]
    InvalidQuantity(String),
    /// An option contract or chain quote failed structural validation.
    #[error("invalid contract: {0}")]
    InvalidContract(String),
    /// A spread failed structural or defined-risk validation.
    #[error("invalid spread: {0}")]
    InvalidSpread(String),
    /// A portfolio failed structural validation.
    #[error("invalid portfolio: {0}")]
    InvalidPortfolio(String),
    /// A symbol failed index validation (empty spelling).
    #[error("invalid symbol: {0}")]
    InvalidSymbol(String),
    /// A scoped symbol table refused a new entry at capacity.
    #[error("symbol table full: {0}")]
    TooManySymbols(String),
    /// Underlying I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] IoError),
}

common_core::impl_from_io_error!(TypesError);

#[cfg(test)]
#[path = "../tests/error.rs"]
mod tests;
