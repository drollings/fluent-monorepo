#![forbid(unsafe_code)]

//! orator-stt: Streaming speech-to-text: the whisper binding behind a commit/provisional wrapper.

pub mod streaming;
pub mod wer;
pub mod whisper;

pub use streaming::{BlockDecoder, DecodedWord, StreamingStt};
pub use wer::wer;
pub use whisper::WhisperBackend;
