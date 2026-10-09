//! Typed errors for headless chart-model construction.

/// Fail-closed refusal for chart-model constructors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChartModelError {
    /// A transform parameter (brick size, reversal, …) is not usable.
    #[error("invalid parameter for {what}: {reason}")]
    InvalidParam {
        /// Which parameter refused.
        what: String,
        /// Why it refused.
        reason: String,
    },
}
