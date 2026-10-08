//! Kernel-side typed errors: invalid inputs, over-cap runs, bad configuration.

use thiserror::Error;

/// Errors from run-context handling, input validation, and runtime checks.
#[derive(Error, Debug)]
pub enum KernelError {
    /// An input declaration or assignment failed validation.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// A run was offered more bars than the runtime allows.
    #[error("too many bars: got {got}, max {max}")]
    TooManyBars {
        /// Bars offered.
        got: u64,
        /// Configured cap.
        max: u64,
    },
    /// The runtime configuration contradicts itself.
    #[error("invalid runtime configuration: {0}")]
    ConfigConflict(String),
    /// No indicator is registered under the requested id.
    #[error("unknown indicator: {0}")]
    UnknownIndicator(String),
    /// An indicator id was registered twice.
    #[error("duplicate indicator: {0}")]
    DuplicateIndicator(String),
    /// A parameter is missing, mistyped, or out of range.
    #[error("invalid parameter: {0}")]
    InvalidParam(String),
    /// A research-frame conversion failed. Research helpers never run in the
    /// per-bar hot loop; this error only surfaces during analysis.
    #[error("research frame error: {0}")]
    Research(String),
}
