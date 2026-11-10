#![forbid(unsafe_code)]

//! orator-voice: the realtime voice pipeline.
//!
//! Five stages, one crate: `vad` gates speech, `stt` transcribes it,
//! `turn` decides who holds the floor, `tts` voices replies, and `s2s`
//! is the native speech-to-speech adapter over the supervised audio
//! endpoint. The cascade (whisper/Kokoro) and the native backend stay
//! selectable behind the same config keys — never imposed. The `s2s`
//! stage never touches the `stt` backends directly: server-side audio
//! flows through `orator-audio` (WAV/codec home), enforced by the
//! source guard.

pub mod s2s;
pub mod stt;
pub mod tts;
pub mod turn;
pub mod vad;
