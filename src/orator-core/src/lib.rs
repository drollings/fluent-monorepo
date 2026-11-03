#![forbid(unsafe_code)]

//! orator-core: shared types, engine traits, configuration, and the single
//! wire-protocol registry for the Orator voice assistant.
//!
//! Every pipeline stage, engine backend, tool, and client builds on the
//! vocabulary defined here. Sibling `orator-*` crates compose these items;
//! nothing here knows about audio backends, models, or desktop surfaces.

pub mod bench;
pub mod config;
pub mod error;
pub mod gpu;
pub mod paths;
pub mod protocol;
pub mod prompts;
pub mod signal;
pub mod socket;
pub mod traits;
pub mod truncate;
pub mod types;
pub mod untrusted;

/// Serializes process-environment mutation across hermetic tests.
/// Environment is process-global; tests that set or unset variables
/// hold this while the mutation is visible.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub use bench::{histograms, render_stats, BenchEvent, BenchKind, BenchLog, Stats};
pub use config::{
    config_dir, config_path, bundle_from_s2s, default_bundle, load_hierarchical, llama_weights_dirs,
    models_dir, resolve_llm_weights, AudioBundle, AudioConfig, LlmEndpointConfig, OratorConfig,
    S2sConfig, SttConfig, TtsConfig, TurnConfig, AUDIO_BUNDLE_DIR, AUDIO_IN_RATE, AUDIO_OUT_RATE,
};
pub use error::{DuplexError, LlmError, ProtocolError, SttError, TtsError};
pub use paths::{current_uid, find_all_on_path, find_on_path, home_dir};
pub use protocol::{MessageType, METHOD_NAMES, validate_method};
pub use prompts::{REFINE_PROMPT, S2S_ASR_PROMPT, S2S_INTERLEAVED_PROMPT, S2S_VOICE_PROMPT};
pub use signal::shutdown_signal;
pub use socket::{check_peer_uid, socket_path, SocketFrame, FrameError, SOCKET_MODE, SOCKET_NAME, MAX_BODY};
pub use traits::{DuplexEngine, LlmSession, SttStream, TtsStream};
pub use truncate::{truncate_tool_output, TOOL_OUTPUT_CHARS};
pub use types::{
    AudioChunk, Channel, DuplexOut, Epoch, Event, EventKind, Frame, MediaTime, Sampler, SessionKind,
    StepOut, Token, Word, WordMark, WordUpdate,
};
pub use untrusted::{wrap_tool_result, wrap_untrusted, TRANSCRIPT_AS_DATA};
