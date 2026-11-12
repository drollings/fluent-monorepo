//! Typed errors for scale construction and mapping.

/// Fail-closed refusal for scale/mapping construction.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
/// Typed errors for scale construction and mapping.
/// Fail-closed refusal for scale/mapping construction.
pub enum ScaleError {
    /// A scale parameter (height, range, base value, …) is not usable.
    #[error("invalid scale parameter for {what}: {reason}")]
    InvalidParam {
        /// Which parameter refused.
        what: String,
        /// Why it refused.
        reason: String,
    },
}