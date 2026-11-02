//! Audio-endpoint configuration and session selection.
//!
//! A speech model is a bundle, not a file (see
//! [`orator_core::AudioBundle`): the audio encoder projector
//! (mmproj), the vocoder (audio decoder), and the audio tokenizer
//! ride alongside the language backbone the supervisor boots with
//! `-m`. Paths resolve router-style — absolute, or relative to the
//! shared installation layout — and every session field stays
//! hot-editable through the shared schema.

use std::path::PathBuf;

use bon::Builder;
use fluent_wvr::{Describable, FieldAccess};
use serde::{Deserialize, Serialize};

use orator_core::{AudioBundle, AUDIO_BUNDLE_DIR};

/// Microphone-side sample rate in Hz (encoder preprocessor).
pub const AUDIO_IN_RATE: u32 = 16_000;

/// Speaker-side sample rate in Hz (vocoder output).
pub const AUDIO_OUT_RATE: u32 = 24_000;

/// Speech-session selection.
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, FieldAccess, Describable, Builder,
)]
#[builder(start_fn = new)]
pub struct S2sConfig {
    /// Session backend (`audio-server` selects this path).
    #[field(desc = "Speech backend", max_len = 32)]
    #[builder(default = "audio-server".to_string())]
    pub backend: String,
    /// Turn mode (`asr`, `tts`, `interleaved`).
    #[field(desc = "Speech turn mode", max_len = 16)]
    #[builder(default = "interleaved".to_string())]
    pub mode: String,
    /// Audio endpoint base URL.
    #[field(desc = "Audio endpoint base URL", max_len = 128)]
    #[builder(default = "http://127.0.0.1:8080".to_string())]
    pub endpoint: String,
    /// Served model tag.
    #[field(desc = "Audio model tag", max_len = 64)]
    #[builder(default = "liquidai/lfm2.5-audio-1.5b".to_string())]
    pub model: String,
    /// Audio encoder projector path (`--mmproj`).
    #[field(desc = "Audio projector path", max_len = 256)]
    #[builder(default = format!("{AUDIO_BUNDLE_DIR}/mmproj.gguf"))]
    pub mmproj: String,
    /// Audio decoder (vocoder) path.
    #[field(desc = "Vocoder path", max_len = 256)]
    #[builder(default = format!("{AUDIO_BUNDLE_DIR}/vocoder.gguf"))]
    pub vocoder: String,
    /// Audio tokenizer path.
    #[field(desc = "Audio tokenizer path", max_len = 256)]
    #[builder(default = format!("{AUDIO_BUNDLE_DIR}/tokenizer.gguf"))]
    pub tokenizer: String,
    /// System prompt used for TTS turns.
    #[field(desc = "TTS voice prompt", max_len = 128)]
    #[builder(default = "Perform TTS. Use the UK female voice.".to_string())]
    pub voice_prompt: String,
    /// Mic audio buffered per duplex turn in ms.
    #[field(desc = "Duplex turn window ms", min = 500, max = 10000)]
    #[builder(default = 2000)]
    pub turn_window_ms: u32,
    /// Generation token cap per turn.
    #[field(desc = "Max tokens per turn", min = 16, max = 4096)]
    #[builder(default = 512)]
    pub max_tokens: u16,
    /// Streamed-audio bound per turn in samples (24 kHz): the turn
    /// errors instead of growing past it.
    #[field(desc = "Max streamed audio samples per turn", min = 24_000, max = 96_000_000)]
    #[builder(default = 7_200_000)]
    pub max_audio_samples: u32,
}

impl Default for S2sConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

impl S2sConfig {
    /// Bundle described by this config (backbone from `model` when it
    /// names a file, else the reference backbone).
    #[must_use]
    pub fn bundle(&self, backbone: PathBuf) -> AudioBundle {
        AudioBundle {
            backbone,
            mmproj: PathBuf::from(&self.mmproj),
            vocoder: PathBuf::from(&self.vocoder),
            tokenizer: PathBuf::from(&self.tokenizer),
        }
    }
}

#[cfg(test)]
#[path = "../tests/config.rs"]
mod tests;
