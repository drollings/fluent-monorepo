//! Fail-closed configuration validation behind one trait shape.
//!
//! The nine validated surfaces in this workspace historically exposed the
//! same `validate()` shape with five different error types (typed
//! `TypesError`/`ServerError`/`KernelError` at API boundaries vs. plain
//! `String` internally). This module unifies the shape — not the error
//! type — so callers keep byte-identical behavior while supervision can
//! bridge every impl to [`WorkError::Execution`] through one helper.

use fluent_wvr::{WorkError, WorkOutput};

/// Fail-closed self-consistency check shared by supervision (`execute`)
/// and the decision path. Each implementor keeps its existing error type;
/// only the shape is unified.
pub trait Validate {
    /// The existing error type for this surface (unchanged by this trait).
    type Err: core::fmt::Display;
    /// Refuse invalid configuration with a reason; never clamp or default.
    fn validate(&self) -> Result<(), Self::Err>;
}

/// Bridge a [`Validate`] impl into a supervision `execute` body: invalid
/// configuration becomes [`WorkError::Execution`] with the caller's
/// context prefix, valid configuration passes through.
pub fn check_for_execute<T: Validate>(value: &T, context: &str) -> Result<(), WorkError> {
    value
        .validate()
        .map_err(|e| WorkError::Execution(format!("{context}: {e}")))?;
    Ok(())
}

/// Build the success [`WorkOutput`] for a validated surface. Kept beside
/// [`check_for_execute`] so every `execute` bridge shares one shape.
#[must_use]
pub fn valid_output(message: impl Into<String>) -> WorkOutput {
    WorkOutput::ok(message.into())
}

#[cfg(test)]
#[path = "../tests/validate.rs"]
mod tests;
