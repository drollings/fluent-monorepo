//! Layered JSON configuration with runtime-editable leaf sections.
//!
//! Leaf sections derive `FieldAccess + Describable`, so `orator config
//! get/set`, config-file validation, and future editors share one schema.
//! The root assembles the leaves; profile overrides land here later.

use std::path::{Path, PathBuf};

use bon::Builder;
use fluent_wvr::{Describable, FieldAccess};
use serde::{Deserialize, Serialize};

/// Home of downloaded model weights. Single source for every crate that
/// reads or writes the model directory.
///
/// Managed artifacts only (whisper, Kokoro): language-model weights
/// resolve from the llama.cpp installation layout first (see
/// [`resolve_llm_weights`]) and fall back here last. `None` when no
/// home base resolves — callers fail actionably instead of rooting
/// paths in a relative directory.
#[must_use]
pub fn models_dir() -> Option<PathBuf> {
    crate::paths::home_dir().map(|home| home.join(".local/share/orator/models"))
}

/// Orator configuration directory (`~/.config/orator`). `None` when
/// no home base resolves.
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    crate::paths::home_dir().map(|home| home.join(".config/orator"))
}

/// Canonical configuration file (`~/.config/orator/orator.json`).
/// `None` when no home base resolves.
#[must_use]
pub fn config_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("orator.json"))
}

/// Shared llama.cpp weights installations, in preference order. Both
/// machine paths resolve to one store; entries are kept only when the
/// directory exists, deduplicated by canonical path.
#[must_use]
pub fn llama_weights_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for candidate in [
        "/ai/models/gguf",
        "/app/ai/models/gguf",
        "/opt/ai/models/gguf",
    ] {
        let path = PathBuf::from(candidate);
        if path.is_dir() {
            let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if !dirs.contains(&canonical) {
                dirs.push(canonical);
            }
        }
    }
    dirs
}

/// Installation directory holding the reference audio bundle.
/// Pinned to LFM3-Audio-2B: the backbone plus its mmproj carry the
/// full voice pipeline (encoder + codec in the projector), served by
/// stock `llama-server`. Whisper/Kokoro stay selectable via
/// `--backend whisper|kokoro`.
pub const AUDIO_BUNDLE_DIR: &str = "/ai/models/gguf/liquidai/lfm3-audio-2b-gguf";

/// The three audio components plus the language backbone.
///
/// A speech model is a bundle, not a file: the audio encoder
/// projector (mmproj) rides alongside the language backbone. The
/// vocoder (audio decoder) and audio tokenizer are legacy LFM2.5
/// companions: stock `llama-server` voices LFM3-Audio from
/// backbone + mmproj alone (the codec lives in the projector), so
/// those two paths are optional. Paths resolve router-style —
/// absolute, or relative to the shared installation layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioBundle {
    /// Language backbone weights (`-m`).
    pub backbone: PathBuf,
    /// Audio encoder projector (`--mmproj`).
    pub mmproj: PathBuf,
    /// Audio decoder (vocoder) weights (legacy; empty/absent on LFM3).
    pub vocoder: PathBuf,
    /// Audio tokenizer weights (legacy; empty/absent on LFM3).
    pub tokenizer: PathBuf,
}

impl AudioBundle {
    /// Whether the bundle can voice: backbone + projector present.
    /// Vocoder/tokenizer are optional (LFM3 embeds the codec).
    #[must_use]
    pub fn present(&self) -> bool {
        self.backbone.exists() && self.mmproj.exists()
    }

    /// Required components missing from disk, in stable order.
    #[must_use]
    pub fn missing(&self) -> Vec<PathBuf> {
        [&self.backbone, &self.mmproj]
            .into_iter()
            .filter(|path| !path.exists())
            .cloned()
            .collect()
    }

    /// Legacy companions missing from disk (vocoder, tokenizer).
    /// Empty when the bundle voices without them (LFM3).
    #[must_use]
    pub fn missing_optional(&self) -> Vec<PathBuf> {
        [&self.vocoder, &self.tokenizer]
            .into_iter()
            .filter(|path| !path.as_os_str().is_empty() && !path.exists())
            .cloned()
            .collect()
    }
}

/// Reference bundle: the native audio backbone plus its three
/// audio components, all in the installation layout. Override any
/// field for other pairings.
#[must_use]
pub fn default_bundle() -> AudioBundle {
    let dir = PathBuf::from(AUDIO_BUNDLE_DIR);
    AudioBundle {
        backbone: dir.join("latest.gguf"),
        mmproj: dir.join("mmproj.gguf"),
        vocoder: dir.join("vocoder.gguf"),
        tokenizer: dir.join("tokenizer.gguf"),
    }
}

/// Microphone-side sample rate in Hz (encoder preprocessor).
pub const AUDIO_IN_RATE: u32 = 16_000;

/// Speaker-side sample rate in Hz (vocoder output).
pub const AUDIO_OUT_RATE: u32 = 24_000;

/// Speech-session selection: the llama.cpp audio endpoint (STT, TTS,
/// and speech-to-speech over one supervised server). Lives in the
/// config file under `s2s.*` so the endpoint, model tag, and bundle
/// paths are operator-settable; the endpoint defaults to port 8080,
/// the same loopback default as the text endpoint.
///
/// Every leaf carries its builder value as the serde default (the
/// `DaemonConfig` precedent), so hand-written files may set only the
/// endpoint and still load port-8080-complete everywhere else.
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, FieldAccess, Describable, Builder,
)]
#[builder(start_fn = new)]
pub struct S2sConfig {
    /// Session backend (`audio-server` selects this path).
    #[field(desc = "Speech backend", max_len = 32)]
    #[builder(default = "audio-server".to_string())]
    #[serde(default = "default_s2s_backend")]
    pub backend: String,
    /// Turn mode (`asr`, `tts`, `interleaved`).
    #[field(desc = "Speech turn mode", max_len = 16)]
    #[builder(default = "interleaved".to_string())]
    #[serde(default = "default_s2s_mode")]
    pub mode: String,
    /// Audio endpoint base URL (llama.cpp server; default port 8080).
    #[field(desc = "Audio endpoint base URL", max_len = 128)]
    #[builder(default = "http://127.0.0.1:8080".to_string())]
    #[serde(default = "default_s2s_endpoint")]
    pub endpoint: String,
    /// Served model tag.
    #[field(desc = "Audio model tag", max_len = 64)]
    #[builder(default = "liquidai/lfm3-audio-2b".to_string())]
    #[serde(default = "default_s2s_model")]
    pub model: String,
    /// Audio encoder projector path (`--mmproj`).
    #[field(desc = "Audio projector path", max_len = 256)]
    #[builder(default = format!("{AUDIO_BUNDLE_DIR}/mmproj.gguf"))]
    #[serde(default = "default_s2s_mmproj")]
    pub mmproj: String,
    /// Audio decoder (vocoder) path (legacy; absent on LFM3).
    #[field(desc = "Vocoder path", max_len = 256)]
    #[builder(default = format!("{AUDIO_BUNDLE_DIR}/vocoder.gguf"))]
    #[serde(default = "default_s2s_vocoder")]
    pub vocoder: String,
    /// Audio tokenizer path (legacy; absent on LFM3).
    #[field(desc = "Audio tokenizer path", max_len = 256)]
    #[builder(default = format!("{AUDIO_BUNDLE_DIR}/tokenizer.gguf"))]
    #[serde(default = "default_s2s_tokenizer")]
    pub tokenizer: String,
    /// System prompt used for TTS turns.
    #[field(desc = "TTS voice prompt", max_len = 128)]
    #[builder(default = crate::prompts::S2S_VOICE_PROMPT.to_string())]
    #[serde(default = "default_s2s_voice_prompt")]
    pub voice_prompt: String,
    /// Mic audio buffered per duplex turn in ms.
    #[field(desc = "Duplex turn window ms", min = 500, max = 10000)]
    #[builder(default = 2000)]
    #[serde(default = "default_s2s_turn_window_ms")]
    pub turn_window_ms: u32,
    /// Generation token cap per turn.
    #[field(desc = "Max tokens per turn", min = 16, max = 4096)]
    #[builder(default = 512)]
    #[serde(default = "default_s2s_max_tokens")]
    pub max_tokens: u16,
    /// Streamed-audio bound per turn in samples (24 kHz): the turn
    /// errors instead of growing past it.
    #[field(desc = "Max streamed audio samples per turn", min = 24_000, max = 96_000_000)]
    #[builder(default = 7_200_000)]
    #[serde(default = "default_s2s_max_audio_samples")]
    pub max_audio_samples: u32,
    /// Boot a supervised voice server when none answers (explicit
    /// opt-in, default off). Off means the configured endpoint is
    /// expected to be independently started: absence fails loudly
    /// with the remedy instead of spawning a child.
    #[field(desc = "Boot a supervised voice server when none answers")]
    #[builder(default = false)]
    #[serde(default = "default_s2s_auto_boot")]
    pub auto_boot: bool,
}

fn default_s2s_auto_boot() -> bool {
    false
}

fn default_s2s_backend() -> String {
    "audio-server".to_string()
}

fn default_s2s_mode() -> String {
    "interleaved".to_string()
}

fn default_s2s_endpoint() -> String {
    "http://127.0.0.1:8080".to_string()
}

fn default_s2s_model() -> String {
    "liquidai/lfm3-audio-2b".to_string()
}

fn default_s2s_mmproj() -> String {
    format!("{AUDIO_BUNDLE_DIR}/mmproj.gguf")
}

fn default_s2s_vocoder() -> String {
    format!("{AUDIO_BUNDLE_DIR}/vocoder.gguf")
}

fn default_s2s_tokenizer() -> String {
    format!("{AUDIO_BUNDLE_DIR}/tokenizer.gguf")
}

fn default_s2s_voice_prompt() -> String {
    crate::prompts::S2S_VOICE_PROMPT.to_string()
}

fn default_s2s_turn_window_ms() -> u32 {
    2000
}

fn default_s2s_max_tokens() -> u16 {
    512
}

fn default_s2s_max_audio_samples() -> u32 {
    7_200_000
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

/// Reference bundle with its audio component paths taken from the
/// loaded `s2s` section: the backbone stays the pinned installation
/// file while projector/vocoder/tokenizer honor operator overrides.
#[must_use]
pub fn bundle_from_s2s(s2s: &S2sConfig) -> AudioBundle {
    let reference = default_bundle();
    AudioBundle {
        backbone: reference.backbone,
        mmproj: PathBuf::from(&s2s.mmproj),
        vocoder: PathBuf::from(&s2s.vocoder),
        tokenizer: PathBuf::from(&s2s.tokenizer),
    }
}

/// Daemon listeners. The Unix socket always serves; the WebSocket
/// listener is additive and off unless configured. When enabled it
/// binds loopback only and every handshake carries the bearer token —
/// tokenless or wrong-token peers are refused, never downgraded.
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, FieldAccess, Describable, Builder,
)]
#[builder(start_fn = new)]
pub struct DaemonConfig {
    /// WebSocket listen address (`127.0.0.1:PORT`). Empty means
    /// socket-only (the default).
    #[field(desc = "WS listen address (empty disables)", max_len = 64)]
    #[builder(default = String::new())]
    #[serde(default)]
    pub ws_listen: String,
    /// Bearer token the WS handshake must present. Empty with a set
    /// `ws_listen` refuses to start (fail closed, never unauthenticated).
    #[field(desc = "WS bearer token", max_len = 256)]
    #[builder(default = String::new())]
    #[serde(default)]
    pub ws_token: String,
    /// Concurrent turns admitted past the shared endpoints. Past the
    /// bound turns refuse fast with a named reason instead of
    /// queueing. Calibrated on serial-stub fixtures (see the
    /// calibration register); servers that parallelize raise it here.
    #[field(desc = "Concurrent turn bound", min = 1, max = 64)]
    #[builder(default = 2)]
    #[serde(default = "default_admission_max")]
    pub admission_max: u16,
    /// WS keepalive ping cadence in seconds. Idle connections get a
    /// ping on this cadence; the peer's pong resets the silence clock.
    #[field(desc = "WS ping cadence in seconds", min = 1, max = 60)]
    #[builder(default = 5)]
    #[serde(default = "default_ws_ping_secs")]
    pub ws_ping_secs: u64,
    /// WS silence bound in seconds: a connection with no inbound
    /// message for this long is reaped. Applies between calls, never
    /// mid-call (a slow turn holds no silence while it runs).
    #[field(desc = "WS silence bound in seconds", min = 5, max = 3600)]
    #[builder(default = 10)]
    #[serde(default = "default_ws_inactivity_secs")]
    pub ws_inactivity_secs: u64,
    /// WS connection lifetime cap in seconds. Zero disables; otherwise
    /// connections older than the cap close after the in-flight call.
    #[field(desc = "WS connection lifetime cap in seconds (0 disables)", min = 0, max = 86400)]
    #[builder(default = 360)]
    #[serde(default = "default_ws_cap_secs")]
    pub ws_cap_secs: u64,
}

/// Default matching the builder so `#[serde(default)]` on the leaf
/// agrees with `DaemonConfig::default()` on old files.
fn default_admission_max() -> u16 {
    2
}

/// Default matching the builder so `#[serde(default)]` on the leaf
/// agrees with `DaemonConfig::default()` on old files.
fn default_ws_ping_secs() -> u64 {
    5
}

/// Default matching the builder so `#[serde(default)]` on the leaf
/// agrees with `DaemonConfig::default()` on old files.
fn default_ws_inactivity_secs() -> u64 {
    10
}

/// Default matching the builder so `#[serde(default)]` on the leaf
/// agrees with `DaemonConfig::default()` on old files.
fn default_ws_cap_secs() -> u64 {
    360
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

/// Audio backend selection and device routing.
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, FieldAccess, Describable, Builder,
)]
#[builder(start_fn = new)]
pub struct AudioConfig {
    /// Capture/playout backend (`cpal`, later `pipewire`).
    #[field(desc = "Audio backend", max_len = 32)]
    #[builder(default = "cpal".to_string())]
    pub backend: String,
    /// Input device selector.
    #[field(desc = "Microphone selector", max_len = 128)]
    #[builder(default = "default".to_string())]
    pub mic: String,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

/// Speech-to-text selection.
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, FieldAccess, Describable, Builder,
)]
#[builder(start_fn = new)]
pub struct SttConfig {
    /// STT backend (`audio-server`, `whisper`).
    #[field(desc = "STT backend", max_len = 32)]
    #[builder(default = "audio-server".to_string())]
    pub backend: String,
    /// Model tag, re-pinned against the model manifest when downloading.
    #[field(desc = "STT model tag", max_len = 64)]
    #[builder(default = "large-v3-turbo-q5_0".to_string())]
    pub model: String,
    /// Recognition language (`en`, …; auto-detect off by default).
    #[field(desc = "Recognition language", max_len = 16)]
    #[builder(default = "en".to_string())]
    pub language: String,
    /// GPU offload (explicit opt-in, default off). When set, the
    /// backend attempts GPU inference and fails actionably without a
    /// device — never silently CPU. Outputs may differ in trailing
    /// float bits across devices; equality holds per device.
    #[field(desc = "GPU offload (explicit opt-in)")]
    #[builder(default = false)]
    #[serde(default)]
    pub gpu: bool,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

/// Local language-model endpoint selection.
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, FieldAccess, Describable, Builder,
)]
#[builder(start_fn = new)]
pub struct LlmEndpointConfig {
    /// Supervised endpoint socket address.
    #[field(desc = "LLM endpoint address", max_len = 128)]
    #[builder(default = "127.0.0.1:8080".to_string())]
    pub endpoint: String,
    /// Served model tag.
    #[field(desc = "LLM model tag", max_len = 64)]
    #[builder(default = "bartowski/lfm3-1.7b".to_string())]
    pub model: String,
    /// Context window in tokens.
    #[field(desc = "Context window", min = 1024, max = 65535)]
    #[builder(default = 8192)]
    pub ctx: u16,
    /// Prefill chunk size in tokens (cancel-latency bound).
    #[field(desc = "Prefill batch size", min = 64, max = 65535)]
    #[builder(default = 512)]
    pub n_batch: u16,
    /// Weights declaration, router-style: an absolute path, or a path
    /// relative to `gguf_dir`. Empty means search the installation
    /// layout for `<model>.gguf` before the managed directory.
    #[field(desc = "LLM weights declaration", max_len = 256)]
    #[builder(default = "bartowski/lfm3-1.7b/latest.gguf".to_string())]
    pub weights: String,
    /// GGUF root prefixing a relative `weights` declaration. Empty
    /// means the shared llama.cpp installation layout.
    #[field(desc = "GGUF directory", max_len = 256)]
    #[builder(default = "/ai/models/gguf".to_string())]
    pub gguf_dir: String,
    /// GPU offload (explicit opt-in, default off). When set, the
    /// server boots with full layer offload and fails actionably
    /// without a device — never silently CPU. Outputs may differ in
    /// trailing float bits across devices; equality holds per device.
    #[field(desc = "GPU offload (explicit opt-in)")]
    #[builder(default = false)]
    #[serde(default)]
    pub gpu: bool,
    /// Boot a supervised llama-server when none answers (explicit
    /// opt-in, default off). Off means the configured endpoint is
    /// expected to be independently started: absence fails loudly
    /// with the remedy instead of spawning a child.
    #[field(desc = "Boot a supervised server when none answers")]
    #[builder(default = false)]
    #[serde(default = "default_llm_auto_boot")]
    pub auto_boot: bool,
}

/// Default matching the builder so `#[serde(default)]` on the leaf
/// agrees with `LlmEndpointConfig::default()` on old files.
fn default_llm_auto_boot() -> bool {
    false
}

/// Resolve language-model weights, router-consistent: the explicit
/// `weights` declaration first (absolute, or joined under `gguf_dir`
/// when relative), then `<model>.gguf` under the shared installation
/// layout, then the managed directory. Returns the declared path even
/// when missing so callers name it; `None` only when nothing declares
/// a location and no candidate file exists.
#[must_use]
pub fn resolve_llm_weights(config: &LlmEndpointConfig) -> Option<PathBuf> {
    if !config.weights.is_empty() {
        let raw = Path::new(&config.weights);
        if raw.is_absolute() {
            return Some(raw.to_path_buf());
        }
        if !config.gguf_dir.is_empty() {
            return Some(PathBuf::from(&config.gguf_dir).join(raw));
        }
        return Some(raw.to_path_buf());
    }
    let file = format!("{}.gguf", config.model);
    if !config.gguf_dir.is_empty() {
        let candidate = PathBuf::from(&config.gguf_dir).join(&file);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    for root in llama_weights_dirs() {
        let direct = root.join(&file);
        if direct.exists() && crate::paths::within_root(&root, &direct) {
            return Some(direct);
        }
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.take(200).filter_map(std::result::Result::ok) {
                let nested = entry.path().join(&file);
                if nested.exists() && crate::paths::within_root(&root, &nested) {
                    return Some(nested);
                }
            }
        }
    }
    if let Some(managed) = models_dir().map(|dir| dir.join(&file)) {
        if managed.exists() {
            return Some(managed);
        }
    }
    None
}

impl Default for LlmEndpointConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

/// Speech-synthesis selection.
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, FieldAccess, Describable, Builder,
)]
#[builder(start_fn = new)]
pub struct TtsConfig {
    /// TTS backend (`audio-server`, `kokoro`).
    #[field(desc = "TTS backend", max_len = 32)]
    #[builder(default = "audio-server".to_string())]
    pub backend: String,
    /// Voice tag: Kokoro voice id (`bf_emma`, …) on the local
    /// fallback path; the audio-server path voices from its own
    /// prompt instead.
    #[field(desc = "Voice tag", max_len = 64)]
    #[builder(default = "bf_emma".to_string())]
    pub voice: String,
    /// GPU offload (explicit opt-in, default off). When set, the
    /// backend attempts a CUDA session and fails actionably without
    /// a device — never silently CPU. Outputs may differ in trailing
    /// float bits across devices; equality holds per device.
    #[field(desc = "GPU offload (explicit opt-in)")]
    #[builder(default = false)]
    #[serde(default)]
    pub gpu: bool,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

/// Turn-taking tunables. These are task-value numbers: they ship only
/// inside their calibrated bands (see the calibration register).
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, FieldAccess, Describable, Builder,
)]
#[builder(start_fn = new)]
pub struct TurnConfig {
    /// Acoustic hold floor in ms.
    #[field(desc = "Endpoint hold floor ms", min = 50, max = 2000)]
    #[builder(default = 300)]
    pub hold_min_ms: u16,
    /// Acoustic hold ceiling in ms.
    #[field(desc = "Endpoint hold ceiling ms", min = 50, max = 2000)]
    #[builder(default = 700)]
    pub hold_max_ms: u16,
    /// Sustained-speech time before barge-in detection trips, in ms.
    #[field(desc = "Barge-in detect ms", min = 50, max = 2000)]
    #[builder(default = 150)]
    pub barge_in_ms: u16,
}

impl Default for TurnConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

/// The whole daemon configuration. Leaves stay individually editable at
/// runtime; the root loads from disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct OratorConfig {
    /// Audio section.
    #[serde(default)]
    pub audio: AudioConfig,
    /// Speech-to-text section.
    #[serde(default)]
    pub stt: SttConfig,
    /// Language-model section.
    #[serde(default)]
    pub llm: LlmEndpointConfig,
    /// Speech-synthesis section.
    #[serde(default)]
    pub tts: TtsConfig,
    /// Speech endpoint section (llama.cpp audio server: STT, TTS,
    /// speech-to-speech; default port 8080).
    #[serde(default)]
    pub s2s: S2sConfig,
    /// Turn-taking section.
    #[serde(default)]
    pub turn: TurnConfig,
    /// Daemon listener section.
    #[serde(default)]
    pub daemon: DaemonConfig,
}

impl OratorConfig {
    /// Load the file at `path`, falling back to defaults when it is
    /// missing or unreadable.
    #[must_use]
    pub fn load_or_default(path: &Path) -> Self {
        common_core::config::load_json_or_default(path)
    }
}

/// Audio keys removed from [`AudioConfig`] (`system_source`,
/// `echo_cancel` were write-only: no capture path or AEC gate ever
/// read them). Configs that still set them refuse with a `Usage`
/// error naming the removal instead of silently ignoring them.
/// Returns the removed key names present in the file at `path`
/// (empty when absent, unreadable, or clean).
#[must_use]
pub fn removed_audio_keys(path: &Path) -> Vec<String> {
    const REMOVED: [&str; 2] = ["system_source", "echo_cancel"];
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(audio) = value.get("audio") else {
        return Vec::new();
    };
    REMOVED
        .into_iter()
        .filter(|key| audio.get(key).is_some())
        .map(str::to_string)
        .collect()
}

/// Load from the hierarchical lookup: `orator.json` in the working
/// directory first, then the canonical [`config_path`]. Falls back to
/// defaults when no file exists. Returns the config plus the path it
/// came from (the canonical path when absent and known, else the
/// working-directory file, so writers have a target even on first
/// run).
#[must_use]
pub fn load_hierarchical() -> (OratorConfig, PathBuf) {
    let mut candidates = vec![PathBuf::from("orator.json")];
    candidates.extend(config_path());
    let fallback = config_path().unwrap_or_else(|| PathBuf::from("orator.json"));
    let path = common_core::config::find_hierarchical(&candidates).unwrap_or(fallback);
    (OratorConfig::load_or_default(&path), path)
}

#[cfg(test)]
#[path = "../tests/config.rs"]
mod tests;
