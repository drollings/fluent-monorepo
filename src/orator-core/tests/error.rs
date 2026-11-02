//! Unit tests for [`crate::error`]: display strings stay stable.

use crate::error::{DuplexError, LlmError, ProtocolError, SttError, TtsError};

#[test]
fn error_strings_name_the_stage() {
    assert!(SttError::Aborted.to_string().contains("STT"));
    assert!(LlmError::Aborted.to_string().contains("LLM"));
    assert!(TtsError::Aborted.to_string().contains("TTS"));
    assert!(DuplexError::Aborted.to_string().contains("duplex"));
    assert!(ProtocolError::UnknownType(9).to_string().contains('9'));
}
