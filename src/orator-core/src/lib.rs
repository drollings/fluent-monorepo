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
pub mod prompts;
pub mod protocol;
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
    agreed_s2s_model, audio_bundle_dir, bundle_from_s2s, compose_role_params, config_dir,
    config_path, default_bundle, default_bundle_in, default_models, llama_weights_dirs,
    load_hierarchical, meetings_dir, models_dir, overlay_params, parse_endpoint,
    resolve_bundle_path, resolve_llm_weights, resolve_model_weights, resolve_role_model,
    resolve_role_model_or_default, should_prefer_s2s, AudioBundle, AudioConfig, EndpointParts,
    LlmEndpointConfig, ModelRoleOverride, ModelSpec, ModelsConfig, OratorConfig, RoleSpec,
    RolesConfig, S2sConfig, SttConfig, TtsConfig, TurnConfig, VadConfig, AUDIO_IN_RATE,
    AUDIO_OUT_RATE, DEFAULT_ASR_MODEL, DEFAULT_GGUF_DIR, DEFAULT_LLM_MODEL, DEFAULT_MODEL,
    DEFAULT_TTS_MODEL,
};
pub use error::{DuplexError, LlmError, ProtocolError, SttError, TtsError};
pub use paths::{
    current_uid, filter_validated_binaries, find_all_on_path, find_on_path, home_dir,
    resolve_validated_binary, ExplicitBinary,
};
pub use prompts::{REFINE_PROMPT, S2S_ASR_PROMPT, S2S_INTERLEAVED_PROMPT, S2S_VOICE_PROMPT};
pub use protocol::{validate_method, MessageType, METHOD_NAMES};
pub use signal::shutdown_signal;
pub use socket::{
    check_peer_uid, socket_path, FrameError, SocketFrame, MAX_BODY, SOCKET_MODE, SOCKET_NAME,
};
pub use traits::{DuplexEngine, LlmSession, SttStream, TtsStream};
pub use truncate::{truncate_tool_output, TOOL_OUTPUT_CHARS};
pub use types::{
    AudioChunk, Channel, DuplexOut, Epoch, Event, EventKind, Frame, MediaTime, Sampler,
    SessionKind, StepOut, Token, Word, WordMark, WordUpdate,
};
pub use untrusted::{wrap_tool_result, wrap_untrusted, TRANSCRIPT_AS_DATA};
