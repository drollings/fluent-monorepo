#![forbid(unsafe_code)]

//! Speech-to-speech over the supervised audio endpoint.
//!
//! A native audio model (audio encoder + language backbone + audio
//! decoder) serves ASR, TTS, and interleaved voice chat through one
//! OpenAI-compatible endpoint: base64 WAV in, streamed text plus
//! base64 PCM audio out. The cascade (whisper/Kokoro) stays intact
//! behind the same config keys — this backend is selected, never
//! imposed. Text always flows alongside audio, so transcripts, tools,
//! history, and the event log keep working with or without spoken
//! audio from the server.

pub mod config;
pub mod ensure;
pub mod protocol;
pub mod session;
pub mod voice;
pub mod worker;

pub use config::{S2sConfig, AUDIO_IN_RATE, AUDIO_OUT_RATE};
pub use ensure::{boot_audio_server, ensure_audio_server, serves_model, AudioEndpoint};
pub use protocol::{completions_url, StreamEvent, SYSTEM_ASR, SYSTEM_INTERLEAVED};
pub use session::{audio_input_unsupported, should_advance_chain, AudioSession, ChatTurn, S2sError};
pub use voice::{
    audio_supervisor_config, audio_server_home, managed_binary, resolve_audio_server,
    resolve_audio_server_candidates, voice_server_args, AUDIO_SERVER_BIN, AUDIO_SERVER_ENV,
    VOICE_BOOT_TIMEOUT,
};
pub use worker::{resolve_tts_binary, synth_blocking, TtsWorkerError, LLAMA_TTS_BIN, LLAMA_TTS_ENV, TTS_TIMEOUT};

/// Serializes process-environment mutation across hermetic tests.
/// Resolution reads `PATH` and the binary overrides from the shared
/// environment; tests that move them hold this while visible.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
