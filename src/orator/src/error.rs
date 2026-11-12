//! Engine and protocol error types.
//!
//! Each engine trait carries its own error enum so a failure names the
//! stage that produced it. Variants stay structural (backend output,
//! cancellation, unavailability) rather than backend-specific.

use thiserror::Error;

/// Failures from the speech-to-text stage.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
/// Failures from the speech-to-text stage.
pub enum SttError {
    /// The STT backend reported a failure.
    #[error("STT backend failed: {0}")]
    Backend(String),
    /// Decoding was cancelled (barge-in, endpoint abort).
    #[error("STT aborted")]
    Aborted,
    /// No usable model or backend is loaded.
    #[error("STT unavailable: {0}")]
    Unavailable(String),
}

/// Failures from the language-model session.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
/// Failures from the language-model session.
pub enum LlmError {
    /// The endpoint or transport reported a failure.
    #[error("LLM backend failed: {0}")]
    Backend(String),
    /// The request was cancelled (barge-in, epoch abort).
    #[error("LLM aborted")]
    Aborted,
    /// No usable endpoint or model is configured.
    #[error("LLM unavailable: {0}")]
    Unavailable(String),
}

/// Failures from the text-to-speech stage.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
/// Failures from the text-to-speech stage.
pub enum TtsError {
    /// The TTS backend reported a failure.
    #[error("TTS backend failed: {0}")]
    Backend(String),
    /// Synthesis was cancelled (barge-in, epoch abort).
    #[error("TTS aborted")]
    Aborted,
    /// No usable voice or backend is loaded.
    #[error("TTS unavailable: {0}")]
    Unavailable(String),
}

/// Failures from a native speech-to-speech engine.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
/// Failures from a native speech-to-speech engine.
pub enum DuplexError {
    /// The duplex backend reported a failure.
    #[error("duplex backend failed: {0}")]
    Backend(String),
    /// The step was cancelled.
    #[error("duplex step aborted")]
    Aborted,
    /// No usable duplex model is loaded.
    #[error("duplex engine unavailable: {0}")]
    Unavailable(String),
}

/// Failures on the daemon/client wire protocol.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
/// Failures on the daemon/client wire protocol.
pub enum ProtocolError {
    /// The method name is not part of the wire contract.
    #[error("unknown method: {0}")]
    UnknownMethod(String),
    /// The message-type byte is not part of the wire contract.
    #[error("unknown message type: {0}")]
    UnknownType(u8),
}

#[cfg(test)]
#[path = "../tests/error.rs"]
mod tests;