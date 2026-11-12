use crate::{WorkError, WorkOutput};

/// Fail-closed self-consistency check shared by supervision (`execute`)
/// and the decision path. Implementors keep their existing error type.
pub trait Validate {
    type Err: core::fmt::Display;
    fn validate(&self) -> Result<(), Self::Err>;
}

/// Bridge a [`Validate`] impl into supervision `execute`: invalid config
/// becomes [`WorkError::Execution`] with the caller's context prefix.
pub fn check_for_execute<T: Validate>(value: &T, context: &str) -> Result<(), WorkError> {
    value
        .validate()
        .map_err(|e| WorkError::execution_context(context, e))?;
    Ok(())
}

/// Success [`WorkOutput`] for a validated surface.
#[must_use]
/// Success [`WorkOutput`] for a validated surface.
pub fn valid_output(message: impl Into<String>) -> WorkOutput {
    WorkOutput::ok(message.into())
}

#[cfg(test)]
#[path = "../tests/validate.rs"]
mod tests;