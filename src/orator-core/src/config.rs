//! Layered JSON configuration with runtime-editable leaf sections.
//!
//! Leaf sections derive `FieldAccess + Describable`, so `orator config
//! get/set`, config-file validation, and future editors share one schema.
//! The root assembles the leaves through `#[field(nested)]` delegation,
//! so the whole tree reads through one control-plane handle; the
//! models/roles tables stay whole-table JSON (skipped, like the
//! router's table fields). Profile overrides land here later.

use std::collections::HashMap;
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

/// Default home for GGUF weights (mirrors `coral-router.json`
/// `gguf_dir`). Absolute; configurable via the top-level `gguf_dir`
/// key (`orator config set gguf_dir ...`) or the `llm.gguf_dir`
/// per-endpoint override. The installation layout
/// ([`llama_weights_dirs`]) stays consultable regardless.
pub const DEFAULT_GGUF_DIR: &str = "/app/ai/models/gguf";

/// Default speech-to-text model tag: the specialized streaming ASR
/// model. Served by a future moonshine backend; until it lands, the
/// audio-server path keeps serving the integrated model and the
/// whisper path uses its own tag verbatim (a moonshine tag there
/// fails loudly naming the missing file).
pub const DEFAULT_ASR_MODEL: &str = "handy-computer/moonshine-streaming-medium";

/// Default text-to-speech model tag: the specialized local synthesis
/// model, served by the kokoro backend.
pub const DEFAULT_TTS_MODEL: &str = "agentory/kokoro-82m";

/// Default model tag: the near-complete solution, served through
/// llama.cpp over the OpenAI-compatible endpoint. Task and role
/// defaults (moonshine ASR, kokoro TTS) win where they are set; this
/// is the fallback when a role list is empty or resolves to nothing
/// servable.
pub const DEFAULT_MODEL: &str = "liquidai/lfm3-audio-2b";

/// Default chat model tag: the supervised text endpoint's model.
/// The `roles.chat` head defaults to this tag and the `llm.model`
/// leaf defaults to it too, so default configs resolve identically
/// through either layer.
pub const DEFAULT_LLM_MODEL: &str = "bartowski/lfm3-1.7b";

/// Home of meeting export artifacts (`<session>.md`, atomically
/// written). Single source for the daemon writer and every reader.
/// `None` when no home base resolves.
#[must_use]
pub fn meetings_dir() -> Option<PathBuf> {
    crate::paths::home_dir().map(|home| home.join(".local/share/orator/meetings"))
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
        "/app/ai/models/gguf",
        "/ai/models/gguf",
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

/// Installation directory holding the reference audio bundle,
/// derived from the top-level `gguf_dir` (never repeated): the LFM3
/// bundle lives at `<gguf_dir>/liquidai/lfm3-audio-2b-gguf`.
#[must_use]
pub fn audio_bundle_dir(gguf_dir: &str) -> PathBuf {
    PathBuf::from(gguf_dir).join("liquidai/lfm3-audio-2b-gguf")
}

/// The three audio components plus the language backbone.
///
/// A speech model is a bundle, not a file: the audio encoder
/// projector (mmproj) rides alongside the language backbone. The
/// vocoder (audio decoder) and audio tokenizer are legacy LFM2.5
/// companions: a capable server voices LFM3-Audio from backbone +
/// mmproj alone (the codec lives in the projector), so those two
/// paths are optional. Paths resolve router-style — absolute, or
/// relative to the top-level `gguf_dir` (never repeated per key).
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

/// Bundle path resolution, router-style: absolute declarations
/// pass through verbatim; relative ones join under `gguf_dir`
/// (never repeated per key); empty stays empty (absent companion).
#[must_use]
pub fn resolve_bundle_path(declared: &str, gguf_dir: &str) -> PathBuf {
    if declared.is_empty() {
        return PathBuf::new();
    }
    let raw = Path::new(declared);
    if raw.is_absolute() {
        return raw.to_path_buf();
    }
    if gguf_dir.is_empty() {
        return raw.to_path_buf();
    }
    PathBuf::from(gguf_dir).join(raw)
}

/// Reference bundle: the native audio backbone plus its three audio
/// components, all in the installation layout. Override any field
/// for other pairings. Paths resolve router-style (below).
#[must_use]
pub fn default_bundle() -> AudioBundle {
    default_bundle_in(DEFAULT_GGUF_DIR)
}

/// Reference bundle rooted at `gguf_dir`: same layout, no repeated
/// root. Single home for every default/assurance path so a relocated
/// store moves the whole bundle at once.
#[must_use]
pub fn default_bundle_in(gguf_dir: &str) -> AudioBundle {
    let dir = audio_bundle_dir(gguf_dir);
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
    /// Audio encoder projector path (`--mmproj`): absolute, or
    /// relative to the top-level `gguf_dir` (never repeated here).
    #[field(desc = "Audio projector path", max_len = 256)]
    #[builder(default = "liquidai/lfm3-audio-2b-gguf/mmproj.gguf".to_string())]
    #[serde(default = "default_s2s_mmproj")]
    pub mmproj: String,
    /// Audio decoder (vocoder) path (legacy; absent on LFM3).
    /// Absolute, or relative to `gguf_dir`.
    #[field(desc = "Vocoder path", max_len = 256)]
    #[builder(default = "liquidai/lfm3-audio-2b-gguf/vocoder.gguf".to_string())]
    #[serde(default = "default_s2s_vocoder")]
    pub vocoder: String,
    /// Audio tokenizer path (legacy; absent on LFM3).
    /// Absolute, or relative to `gguf_dir`.
    #[field(desc = "Audio tokenizer path", max_len = 256)]
    #[builder(default = "liquidai/lfm3-audio-2b-gguf/tokenizer.gguf".to_string())]
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
    #[field(
        desc = "Max streamed audio samples per turn",
        min = 24_000,
        max = 96_000_000
    )]
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
    /// GPU offload for the supervised voice server (explicit opt-in,
    /// default off). When set, the boot requests full layer offload
    /// (`--n-gpu-layers 999`) and fails actionably without a device —
    /// never silently CPU. Applies to supervised boots only; an
    /// independently started server offloads (or not) by its own argv.
    #[field(desc = "GPU offload for the supervised voice server")]
    #[builder(default = false)]
    #[serde(default = "default_s2s_gpu")]
    pub gpu: bool,
}

fn default_s2s_auto_boot() -> bool {
    false
}

fn default_s2s_gpu() -> bool {
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
    "liquidai/lfm3-audio-2b-gguf/mmproj.gguf".to_string()
}

fn default_s2s_vocoder() -> String {
    "liquidai/lfm3-audio-2b-gguf/vocoder.gguf".to_string()
}

fn default_s2s_tokenizer() -> String {
    "liquidai/lfm3-audio-2b-gguf/tokenizer.gguf".to_string()
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
pub fn bundle_from_s2s(s2s: &S2sConfig, models: &ModelsConfig, gguf_dir: &str) -> AudioBundle {
    // Backbone follows the served model tag through the models table
    // (single home with the weights declarations); unresolvable tags
    // keep the reference bundle instead of failing the path.
    let backbone = resolve_model_weights(models, gguf_dir, &s2s.model)
        .unwrap_or_else(|| audio_bundle_dir(gguf_dir).join("latest.gguf"));
    AudioBundle {
        backbone,
        mmproj: resolve_bundle_path(&s2s.mmproj, gguf_dir),
        vocoder: resolve_bundle_path(&s2s.vocoder, gguf_dir),
        tokenizer: resolve_bundle_path(&s2s.tokenizer, gguf_dir),
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
    #[field(
        desc = "WS connection lifetime cap in seconds (0 disables)",
        min = 0,
        max = 86400
    )]
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

/// One servable model entry: the coral-router.json `models` table
/// shape trimmed to orator's needs (weights declaration, serving
/// backends, base params, per-role overrides — no fleet/thinking
/// machinery, whose orator equivalents live in the role tables and
/// the daemon admission gate).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSpec {
    /// Router-style weights declaration: an absolute path, or a path
    /// relative to the top-level `gguf_dir`. `None` when the backend
    /// owns its files (whisper ggml, kokoro onnx).
    #[serde(default)]
    pub weights: Option<String>,
    /// Serving backends selecting the runtimes this model runs
    /// through (`whisper`, `kokoro`, `audio-server`, `moonshine`),
    /// first entry wins for dispatch. Listed under the model so each
    /// entry advertises every backend it can serve through.
    pub backends: Vec<String>,
    /// Base params for every role serving this model (sampling and
    /// backend tunables as JSON). Composed under the role run block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    /// Sparse per-role override, keyed by role name (serde name
    /// `roles`, exactly like the router). Params-only: per-role
    /// tuning composed last in the chain, never membership — a model
    /// is served through the roles that bind it in their `models`
    /// list. `models` inside an override is rejected loudly: it
    /// belongs on the role side and would otherwise be silently
    /// ignored.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "roles")]
    pub role_params: Option<std::collections::HashMap<String, ModelRoleOverride>>,
}

impl ModelSpec {
    /// Dispatch backend: the first listed backend, or `None` when the
    /// entry lists none (callers skip it naming the reason, never a
    /// silent pick).
    #[must_use]
    pub fn primary_backend(&self) -> Option<&str> {
        self.backends.first().map(String::as_str)
    }
}

/// One model-side per-role override: the value type of
/// `ModelSpec.role_params` (serde name `roles`), keyed by role name.
/// Params-only by construction, mirroring the router's
/// `ModelRoleOverride` — unknown fields are rejected so a misplaced
/// `models` list fails loudly with a pointer at the role side.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRoleOverride {
    /// Sparse params update for the named role, composed last in the
    /// params chain (absent contributes nothing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// The `models` table: tag → spec. Unknown tags resolve to nothing
/// (callers fail loudly naming the tag), never a default model.
/// Transparent over the map so the file shape matches the router's
/// (`"models": {tag: spec}`) — no wrapper object in between.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelsConfig {
    /// Model specs by tag.
    pub models: std::collections::HashMap<String, ModelSpec>,
}

fn model_spec(
    weights: Option<&str>,
    backends: &[&str],
    params: Option<serde_json::Value>,
) -> ModelSpec {
    ModelSpec {
        weights: weights.map(str::to_string),
        backends: backends.iter().map(ToString::to_string).collect(),
        params,
        role_params: None,
    }
}

/// The shipped models table: the integrated generative model plus the
/// specialized ASR/TTS models. LFM3-Audio-2B voices every role through
/// the audio server; moonshine (streaming ASR) and kokoro (local TTS)
/// lead their roles once their backends serve them. Membership lives
/// on the role side (`RolesConfig`); per-role tuning lives in each
/// entry's `roles` overrides.
#[must_use]
pub fn default_models() -> HashMap<String, ModelSpec> {
    HashMap::from([
        (
            DEFAULT_ASR_MODEL.to_string(),
            model_spec(
                Some("handy-computer/moonshine-streaming-medium-gguf/latest.gguf"),
                &["moonshine"],
                None,
            ),
        ),
        (
            "large-v3-turbo-q5_0".to_string(),
            model_spec(None, &["whisper"], None),
        ),
        (
            DEFAULT_TTS_MODEL.to_string(),
            model_spec(None, &["kokoro"], None),
        ),
        (
            "liquidai/lfm3-audio-2b".to_string(),
            model_spec(
                Some("liquidai/lfm3-audio-2b-gguf/latest.gguf"),
                &["audio-server"],
                None,
            ),
        ),
        (
            DEFAULT_LLM_MODEL.to_string(),
            model_spec(
                Some(&format!("{DEFAULT_LLM_MODEL}/latest.gguf")),
                &["llm"],
                None,
            ),
        ),
    ])
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            models: default_models(),
        }
    }
}

/// One capability role: the coral-router.json `roles` entry shape
/// trimmed to orator's needs (ordered model preference plus the role
/// run-block params every dispatch through the role composes over).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleSpec {
    /// Ordered preference: the first entry is the role default.
    /// Unknown tags are skipped by resolution, never served.
    /// Membership lives here — models never name roles back.
    pub models: Vec<String>,
    /// Role run-block params (sampling and backend tunables as JSON).
    /// Composed over the model base params; the model-side
    /// per-role override composes last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// The `roles` table: capability → ordered model preference plus run
/// params. The integrated model backs the voice roles; specialized
/// models lead the roles they serve (moonshine for ASR, kokoro for
/// TTS); the chat role follows the supervised text endpoint's model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RolesConfig {
    /// Speech-recognition preference.
    pub asr: RoleSpec,
    /// Speech-synthesis preference.
    pub tts: RoleSpec,
    /// Chat preference: the supervised text model. Old files without
    /// this section load today's chat behavior (the head defaults to
    /// the `llm.model` default tag).
    #[serde(default = "default_chat_role")]
    pub chat: RoleSpec,
}

/// Default chat role: head on the supervised text model's tag, no
/// run-block params — seeded equal to the `llm.model` leaf default
/// so authority and fallback agree out of the box.
fn default_chat_role() -> RoleSpec {
    RoleSpec {
        models: vec![DEFAULT_LLM_MODEL.to_string()],
        params: None,
    }
}

impl Default for RolesConfig {
    fn default() -> Self {
        Self {
            asr: RoleSpec {
                models: vec![
                    DEFAULT_ASR_MODEL.to_string(),
                    "large-v3-turbo-q5_0".to_string(),
                    "liquidai/lfm3-audio-2b".to_string(),
                ],
                params: None,
            },
            tts: RoleSpec {
                models: vec![
                    DEFAULT_TTS_MODEL.to_string(),
                    "liquidai/lfm3-audio-2b".to_string(),
                ],
                params: None,
            },
            chat: default_chat_role(),
        }
    }
}

/// Shallow params overlay with the router's `overlay_params`
/// semantics: each key wins wholesale, never a deep merge; a
/// non-object side contributes nothing (the object side survives
/// whole). `None` on both sides yields an empty object.
#[must_use]
pub fn overlay_params(
    base: Option<&serde_json::Value>,
    over: Option<&serde_json::Value>,
) -> serde_json::Value {
    use serde_json::Value;
    match (base, over) {
        (Some(b), Some(o)) => match (b.as_object(), o.as_object()) {
            (Some(base_map), Some(over_map)) => {
                let mut merged = base_map.clone();
                for (key, over_value) in over_map {
                    merged.insert(key.clone(), over_value.clone());
                }
                Value::Object(merged)
            }
            (Some(base_map), None) => Value::Object(base_map.clone()),
            (None, Some(over_map)) => Value::Object(over_map.clone()),
            (None, None) => Value::Object(serde_json::Map::new()),
        },
        (Some(b), None) => b.as_object().map_or_else(
            || Value::Object(serde_json::Map::new()),
            |m| Value::Object(m.clone()),
        ),
        (None, Some(o)) => o.as_object().map_or_else(
            || Value::Object(serde_json::Map::new()),
            |m| Value::Object(m.clone()),
        ),
        (None, None) => Value::Object(serde_json::Map::new()),
    }
}

/// Composed params for `tag` serving `role`: the router's chain —
/// model base params, then the role run block, then the model-side
/// per-role override (each layer wins wholesale per key). Unknown
/// roles and unknown tags compose to an empty object (callers fail
/// closed on the missing model/backend, never on params — and an
/// unknown role must not inherit another role's tuning).
#[must_use]
pub fn compose_role_params(
    models: &ModelsConfig,
    roles: &RolesConfig,
    role: &str,
    tag: &str,
) -> serde_json::Value {
    let role_known = matches!(role, "asr" | "tts" | "chat");
    let Some(entry) = models.models.get(tag).filter(|_| role_known) else {
        return serde_json::Value::Object(serde_json::Map::new());
    };
    let role_params = match role {
        "asr" => roles.asr.params.as_ref(),
        "tts" => roles.tts.params.as_ref(),
        "chat" => roles.chat.params.as_ref(),
        _ => None,
    };
    let mut composed = overlay_params(entry.params.as_ref(), role_params);
    if let Some(over) = entry
        .role_params
        .as_ref()
        .and_then(|overrides| overrides.get(role))
        .and_then(|role_override| role_override.params.as_ref())
    {
        composed = overlay_params(Some(&composed), Some(over));
    }
    composed
}

/// First model listed for `role` (`asr`, `tts`), or `None` when the
/// role is unknown or lists nothing. Unknown roles fail closed at
/// the caller, never a default model.
#[must_use]
pub fn resolve_role_model(roles: &RolesConfig, role: &str) -> Option<String> {
    let list = match role {
        "asr" => &roles.asr.models,
        "tts" => &roles.tts.models,
        "chat" => &roles.chat.models,
        _ => return None,
    };
    list.first().cloned()
}

/// Native session model: the agreed role model when both the ASR
/// and TTS heads are listed and equal, else the `s2s.model` leaf.
/// The chat native branch assures and voices this tag; servability
/// is decided at assurance time (loud remedy on failure), never
/// guessed here. Pure tag logic so the derivation pins without
/// hardware.
#[must_use]
pub fn agreed_s2s_model(config: &OratorConfig) -> String {
    match (
        resolve_role_model(&config.roles, "asr"),
        resolve_role_model(&config.roles, "tts"),
    ) {
        (Some(asr), Some(tts)) if asr == tts => asr,
        _ => config.s2s.model.clone(),
    }
}

/// Whether turns should prefer the native speech-to-speech path:/// the same model heads both the ASR and TTS roles through the
/// audio-server backend (the LFM3-Audio-2B shape: one integrated
/// model voicing every role). Assured at use time — a missing or
/// unloadable server falls back to the cascade with the reason
/// named, never a silent downgrade.
#[must_use]
pub fn should_prefer_s2s(config: &OratorConfig) -> bool {
    let (Some(asr), Some(tts)) = (
        resolve_role_model(&config.roles, "asr"),
        resolve_role_model(&config.roles, "tts"),
    ) else {
        return false;
    };
    if asr != tts {
        return false;
    }
    matches!(
        config
            .models
            .models
            .get(&asr)
            .and_then(ModelSpec::primary_backend),
        Some("audio-server")
    )
}

/// Role head with the configured default as fallback: the task/role
/// default (moonshine ASR, kokoro TTS) wins where listed; the
/// top-level `default` model (the near-complete solution) serves when
/// the role list is empty. Unknown roles still resolve to nothing.
#[must_use]
pub fn resolve_role_model_or_default(
    roles: &RolesConfig,
    role: &str,
    default: &str,
) -> Option<String> {
    if matches!(role, "asr" | "tts" | "chat") {
        if let Some(head) = resolve_role_model(roles, role) {
            return Some(head);
        }
        if !default.is_empty() {
            return Some(default.to_string());
        }
    }
    None
}

/// Router-style weights resolution for a models-table entry: the
/// declared absolute path wins, relative declarations join under
/// `gguf_dir`. `None` when the tag is unknown or the entry declares
/// no weights (backend-owned files).
#[must_use]
pub fn resolve_model_weights(models: &ModelsConfig, gguf_dir: &str, tag: &str) -> Option<PathBuf> {
    let declared = models.models.get(tag)?.weights.as_deref()?;
    if declared.is_empty() {
        return None;
    }
    let raw = Path::new(declared);
    if raw.is_absolute() {
        return Some(raw.to_path_buf());
    }
    if gguf_dir.is_empty() {
        return Some(raw.to_path_buf());
    }
    Some(PathBuf::from(gguf_dir).join(raw))
}

/// Speech-to-text selection.
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, FieldAccess, Describable, Builder,
)]
#[builder(start_fn = new)]
pub struct SttConfig {
    /// STT backend (`audio-server`, `whisper`, `moonshine`).
    #[field(desc = "STT backend", max_len = 32)]
    #[builder(default = "audio-server".to_string())]
    pub backend: String,
    /// Model tag: the roles-table ASR default. The audio-server path
    /// serves its own model and ignores this tag; the whisper path
    /// resolves `ggml-<tag>.bin` from the managed directory and fails
    /// loudly naming the file when absent.
    #[field(desc = "STT model tag", max_len = 64)]
    #[builder(default = DEFAULT_ASR_MODEL.to_string())]
    #[serde(default = "default_stt_model")]
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

fn default_stt_model() -> String {
    DEFAULT_ASR_MODEL.to_string()
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
    #[builder(default = DEFAULT_LLM_MODEL.to_string())]
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
    #[builder(default = format!("{DEFAULT_LLM_MODEL}/latest.gguf"))]
    pub weights: String,
    /// GGUF root prefixing a relative `weights` declaration. Empty
    /// means the shared llama.cpp installation layout.
    #[field(desc = "GGUF directory", max_len = 256)]
    #[builder(default = DEFAULT_GGUF_DIR.to_string())]
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

/// One parsed `host:port` endpoint: scheme plus host plus port, the
/// single grammar every endpoint leaf and every runtime dialer shares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointParts {
    /// URL scheme (`http` default, `https` when declared).
    pub scheme: String,
    /// Host as written (IPv6 keeps its brackets).
    pub host: String,
    /// Explicit port (zero is the supervisor auto-reserve shape).
    pub port: u16,
}

impl EndpointParts {
    /// Base URL for dialing (`scheme://host:port`).
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("{}://{}:{}", self.scheme, self.host, self.port)
    }
}

/// Parse one endpoint leaf (`llm.endpoint`, `s2s.endpoint`): bare
/// `host:port` or `scheme://host:port`, IPv6 in brackets. Refuses
/// garbage with the shape named — set-time (`config set`) and
/// use-time (dialers) call this exact function, so the two never
/// disagree about what an endpoint looks like.
pub fn parse_endpoint(raw: &str) -> Result<EndpointParts, String> {
    const SHAPE: &str = "host:port or scheme://host:port (IPv6 in brackets, e.g. [::1]:8080)";
    let invalid = |raw: &str| format!("invalid endpoint {raw:?}: expected {SHAPE}");
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains(char::is_whitespace) {
        return Err(invalid(raw));
    }
    let (scheme, rest) = match trimmed.split_once("://") {
        Some((scheme, rest)) => {
            if scheme != "http" && scheme != "https" {
                return Err(invalid(raw));
            }
            (scheme.to_string(), rest)
        }
        None => ("http".to_string(), trimmed),
    };
    if rest.is_empty() {
        return Err(invalid(raw));
    }
    let Some((host, port_raw)) = rest.rsplit_once(':') else {
        return Err(invalid(raw));
    };
    if host.is_empty() || port_raw.is_empty() {
        return Err(invalid(raw));
    }
    let Ok(port) = port_raw.parse::<u16>() else {
        return Err(invalid(raw));
    };
    if !host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '[' | ']' | ':'))
    {
        return Err(invalid(raw));
    }
    // Colonic hosts must bracket (bare `::1` would otherwise parse as
    // host `::` port `1`); brackets must come in pairs.
    if host.contains(':') && !(host.starts_with('[') && host.ends_with(']')) {
        return Err(invalid(raw));
    }
    if (host.contains('[') || host.contains(']')) && !(host.starts_with('[') && host.ends_with(']'))
    {
        return Err(invalid(raw));
    }
    Ok(EndpointParts {
        scheme,
        host: host.to_string(),
        port,
    })
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
    /// TTS backend (`kokoro`, `audio-server`, `daemon`).
    #[field(desc = "TTS backend", max_len = 32)]
    #[builder(default = "kokoro".to_string())]
    pub backend: String,
    /// Voice tag: Kokoro voice id (`bf_emma`, …) on the local
    /// fallback path; the audio-server path voices from its own
    /// prompt instead.
    #[field(desc = "Voice tag", max_len = 64)]
    #[builder(default = "bf_emma".to_string())]
    pub voice: String,
    /// GPU offload (explicit opt-in, default off; NVIDIA CUDA only).
    /// When set, the backend attempts a CUDA session and fails
    /// actionably without one — never silently CPU. AMD/ROCm hardware
    /// is not served by the kokoro dependency (CUDA/DirectML/CoreML
    /// only), so opting in there fails naming CPU as the remedy.
    /// Outputs may differ in trailing float bits across devices;
    /// equality holds per device.
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, FieldAccess, Describable, Builder)]
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
    /// Backchannel ceiling in ms: lexicon-only utterances below this
    /// never interrupt and never reach the LLM.
    #[field(desc = "Backchannel ceiling ms", min = 100, max = 10000)]
    #[builder(default = 600)]
    #[serde(default = "default_turn_backchannel_ms")]
    pub backchannel_ms: u64,
    /// Room-tail hold in ms: capture stays suppressed after playout
    /// so the tail decays instead of endpointing as a phantom turn.
    #[field(desc = "Room-tail hold ms", min = 0, max = 10000)]
    #[builder(default = 1000)]
    #[serde(default = "default_turn_tail_hold_ms")]
    pub tail_hold_ms: u16,
    /// Loop poll cadence in ms: line intake, capture drain, and the
    /// barge-in continuity quantum share one tick.
    #[field(desc = "Loop poll cadence ms", min = 5, max = 500)]
    #[builder(default = 20)]
    #[serde(default = "default_turn_poll_ms")]
    pub poll_ms: u16,
    /// Saturated-daemon cooldown in seconds: refused mic turns shed
    /// until this elapses instead of hot-looping the bound.
    #[field(desc = "Saturated-daemon cooldown secs", min = 0, max = 3600)]
    #[builder(default = 2)]
    #[serde(default = "default_turn_saturated_cooldown_secs")]
    pub saturated_cooldown_secs: u64,
    /// Shed-line throttle in seconds: over-cap and saturation sheds
    /// log at most one line per interval, never spam.
    #[field(desc = "Shed-line throttle secs", min = 1, max = 3600)]
    #[builder(default = 10)]
    #[serde(default = "default_turn_shed_notice_secs")]
    pub shed_notice_secs: u64,
    /// Playout duck ramp in ms on barge-in detect.
    #[field(desc = "Barge-in duck ramp ms", min = 0, max = 1000)]
    #[builder(default = 100)]
    #[serde(default = "default_turn_duck_ramp_ms")]
    pub duck_ramp_ms: u64,
    /// Playout cut fade in ms on barge-in cut.
    #[field(desc = "Barge-in cut fade ms", min = 0, max = 1000)]
    #[builder(default = 20)]
    #[serde(default = "default_turn_cut_fade_ms")]
    pub cut_fade_ms: u64,
    /// Playout duck depth in dB on barge-in detect.
    #[field(desc = "Barge-in duck depth dB", min = -48.0, max = 0.0)]
    #[builder(default = -12.0)]
    #[serde(default = "default_turn_duck_depth_db")]
    pub duck_depth_db: f32,
}

fn default_turn_backchannel_ms() -> u64 {
    600
}

fn default_turn_tail_hold_ms() -> u16 {
    1000
}

fn default_turn_poll_ms() -> u16 {
    20
}

fn default_turn_saturated_cooldown_secs() -> u64 {
    2
}

fn default_turn_shed_notice_secs() -> u64 {
    10
}

fn default_turn_duck_ramp_ms() -> u64 {
    100
}

fn default_turn_cut_fade_ms() -> u64 {
    20
}

fn default_turn_duck_depth_db() -> f32 {
    -12.0
}

impl Default for TurnConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

/// Voice activity detection tunables. Thresholds stay silent on
/// digital silence and hiss; timing frames run at the 20 ms gate
/// quantum. The hangover ships extended past the legacy detector
/// default so a brief pause no longer commits the turn early.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, FieldAccess, Describable, Builder)]
#[builder(start_fn = new)]
pub struct VadConfig {
    /// RMS floor for speech, linear amplitude.
    #[field(desc = "VAD RMS floor", min = 0.001, max = 0.5)]
    #[builder(default = 0.02)]
    #[serde(default = "default_vad_floor")]
    pub floor: f32,
    /// Hysteresis band half-width around the floor.
    #[field(desc = "VAD hysteresis half-width", min = 0.0, max = 0.2)]
    #[builder(default = 0.01)]
    #[serde(default = "default_vad_hysteresis")]
    pub hysteresis: f32,
    /// Consecutive hot frames to open speech.
    #[field(desc = "VAD attack frames", min = 1, max = 20)]
    #[builder(default = 3)]
    #[serde(default = "default_vad_attack_frames")]
    pub attack_frames: u32,
    /// Consecutive cold frames to close speech.
    #[field(desc = "VAD hangover frames", min = 1, max = 200)]
    #[builder(default = 25)]
    #[serde(default = "default_vad_hangover_frames")]
    pub hangover_frames: u32,
    /// Zero-crossing rate above which a hot frame reads as noise.
    #[field(desc = "VAD zero-crossing ceiling", min = 0.0, max = 1.0)]
    #[builder(default = 0.45)]
    #[serde(default = "default_vad_max_zero_crossing")]
    pub max_zero_crossing: f32,
    /// Below this overall RMS the input is digital silence.
    #[field(desc = "VAD digital-silence RMS", min = 0.0, max = 0.1)]
    #[builder(default = 0.002)]
    #[serde(default = "default_vad_silence_rms")]
    pub silence_rms: f32,
    /// Gate detector floor: far-mic speech must still open spans.
    #[field(desc = "VAD gate floor", min = 0.001, max = 0.5)]
    #[builder(default = 0.01)]
    #[serde(default = "default_vad_gate_floor")]
    pub gate_floor: f32,
    /// Minimum speech-frame fraction over full frames to commit.
    #[field(desc = "VAD speech fraction", min = 0.0, max = 1.0)]
    #[builder(default = 0.15)]
    #[serde(default = "default_vad_min_speech_fraction")]
    pub min_speech_fraction: f32,
    /// Verdict re-arm cadence in frames: a latched span re-proves on
    /// this cadence so stationary hiss cannot ride one attack win.
    #[field(desc = "VAD re-arm frames", min = 1, max = 1000)]
    #[builder(default = 50)]
    #[serde(default = "default_vad_rearm_frames")]
    pub rearm_frames: u32,
    /// Minimum zero-crossing intervals for a tonal verdict.
    #[field(desc = "VAD tonal intervals", min = 2, max = 64)]
    #[builder(default = 8)]
    #[serde(default = "default_vad_min_tone_intervals")]
    pub min_tone_intervals: u32,
    /// Interval variation at or below this reads as stationary tone.
    #[field(desc = "VAD tonal variation ceiling", min = 0.0, max = 1.0)]
    #[builder(default = 0.1)]
    #[serde(default = "default_vad_tone_max_cv")]
    pub tone_max_cv: f32,
}

fn default_vad_floor() -> f32 {
    0.02
}

fn default_vad_hysteresis() -> f32 {
    0.01
}

fn default_vad_attack_frames() -> u32 {
    3
}

fn default_vad_hangover_frames() -> u32 {
    25
}

fn default_vad_max_zero_crossing() -> f32 {
    0.45
}

fn default_vad_silence_rms() -> f32 {
    0.002
}

fn default_vad_gate_floor() -> f32 {
    0.01
}

fn default_vad_min_speech_fraction() -> f32 {
    0.15
}

fn default_vad_rearm_frames() -> u32 {
    50
}

fn default_vad_min_tone_intervals() -> u32 {
    8
}

fn default_vad_tone_max_cv() -> f32 {
    0.1
}

impl Default for VadConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

/// Chat session starts: the output types and barge-in the `chat`
/// loop uses before any slash command overrides them per session.
/// String leaves name their mode (`voice`: `off`|`short`|`full`;
/// `text`: `short`|`full`; `mic`: `auto`|`ptt`; `narration`:
/// `none`|`intro`|`summary`|`full`); unknown spellings fall back
/// to the defaults below with the loop naming the bad value instead
/// of refusing the session. Old files without the section load
/// today's starts (short voice, full text, auto mic, barge on,
/// narration none).
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, FieldAccess, Describable, Builder,
)]
#[builder(start_fn = new)]
pub struct ChatConfig {
    /// Voice output mode for assistant replies.
    #[field(desc = "Chat voice mode (off|short|full)", max_len = 16)]
    #[builder(default = "short".to_string())]
    #[serde(default = "default_chat_voice")]
    pub voice: String,
    /// Text output mode for assistant replies.
    #[field(desc = "Chat text mode (short|full)", max_len = 16)]
    #[builder(default = "full".to_string())]
    #[serde(default = "default_chat_text")]
    pub text: String,
    /// Mic capture mode for file and live-mic turns.
    #[field(desc = "Chat mic mode (auto|ptt)", max_len = 16)]
    #[builder(default = "auto".to_string())]
    #[serde(default = "default_chat_mic")]
    pub mic: String,
    /// Voice barge-in: talking over a reply cuts the live turn (the
    /// utterance becomes the next turn). Off sheds mic input while a
    /// turn runs instead. Key barge always works either way.
    #[field(desc = "Chat voice barge-in")]
    #[builder(default = true)]
    #[serde(default = "default_chat_barge")]
    pub barge: bool,
    /// Push-to-talk latch for live turns: `on` holds the mic cold
    /// until `/talk` (or `/ptt`) latches it hot. Legacy `mic` stays
    /// as an alias; the two track each other per session.
    #[field(desc = "Chat push-to-talk (off|on)", max_len = 16)]
    #[builder(default = "off".to_string())]
    #[serde(default = "default_chat_ptt")]
    pub ptt: String,
    /// Narration level for assistant replies: how much of the reply
    /// voices while the full text always displays.
    #[field(desc = "Chat narration level (none|intro|summary|full)", max_len = 16)]
    #[builder(default = "none".to_string())]
    #[serde(default = "default_chat_narration")]
    pub narration: String,
}

fn default_chat_voice() -> String {
    "short".to_string()
}

fn default_chat_text() -> String {
    "full".to_string()
}

fn default_chat_mic() -> String {
    "auto".to_string()
}

fn default_chat_ptt() -> String {
    "off".to_string()
}

fn default_chat_barge() -> bool {
    true
}

fn default_chat_narration() -> String {
    "none".to_string()
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

/// The whole daemon configuration. Leaves stay individually editable at
/// runtime; the root loads from disk.
///
/// Selection precedence (single-home authority): the role tables
/// decide; root selection leaves (`stt.model`, `llm.model`,
/// `s2s.model`, `tts.voice`) are typed fallback; code defaults come
/// last. Concretely: role params compose base ← role run block ←
/// model-side per-role override, and the root leaf fills composed
/// values that are absent. The whisper walk tries role heads, then
/// the top-level `default` tag, then the `stt.model` leaf; the ask
/// model is the `roles.chat` head else the `llm.model` leaf; the
/// native session model is the agreed heads else the `s2s.model`
/// leaf. `doctor` names the ignored leaf whenever it differs from
/// the effective value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, FieldAccess, Describable)]
pub struct OratorConfig {
    /// Audio section.
    #[field(nested)]
    #[serde(default)]
    pub audio: AudioConfig,
    /// Speech-to-text section.
    #[field(nested)]
    #[serde(default)]
    pub stt: SttConfig,
    /// Language-model section.
    #[field(nested)]
    #[serde(default)]
    pub llm: LlmEndpointConfig,
    /// Speech-synthesis section.
    #[field(nested)]
    #[serde(default)]
    pub tts: TtsConfig,
    /// Speech endpoint section (llama.cpp audio server: STT, TTS,
    /// speech-to-speech; default port 8080).
    #[field(nested)]
    #[serde(default)]
    pub s2s: S2sConfig,
    /// Turn-taking section.
    #[field(nested)]
    #[serde(default)]
    pub turn: TurnConfig,
    /// Voice activity detection section.
    #[field(nested)]
    #[serde(default)]
    pub vad: VadConfig,
    /// Chat session starts (output modes, mic mode, barge-in).
    #[field(nested)]
    #[serde(default)]
    pub chat: ChatConfig,
    /// Daemon listener section.
    #[field(nested)]
    #[serde(default)]
    pub daemon: DaemonConfig,
    /// Home for GGUF weights (mirrors `coral-router.json` `gguf_dir`).
    /// The models table resolves relative weights declarations under
    /// it; `llm.gguf_dir` overrides it per endpoint when set.
    #[field(desc = "GGUF weights home", max_len = 256)]
    #[serde(default = "default_top_gguf_dir")]
    pub gguf_dir: String,
    /// Servable models table: tag → weights, backends, base params,
    /// per-role overrides. Membership lives on the role side.
    /// Skipped from field access (whole-table JSON through
    /// `config_get`/`config_set`, like the router's skipped tables).
    #[field(skip)]
    #[serde(default)]
    pub models: ModelsConfig,
    /// Capability roles (ordered model preference plus run params).
    /// Skipped from field access (whole-table JSON through
    /// `config_get`/`config_set`).
    #[field(skip)]
    #[serde(default)]
    pub roles: RolesConfig,
    /// Default model tag: the near-complete solution, served through
    /// llama.cpp over the OpenAI-compatible endpoint. Task and role
    /// defaults win where set (moonshine ASR, kokoro TTS); this is
    /// the fallback when a role list is empty or resolves to nothing
    /// servable.
    #[serde(default = "default_top_model")]
    pub default: String,
}

fn default_top_model() -> String {
    DEFAULT_MODEL.to_string()
}

fn default_top_gguf_dir() -> String {
    DEFAULT_GGUF_DIR.to_string()
}

impl Default for OratorConfig {
    fn default() -> Self {
        Self {
            audio: AudioConfig::default(),
            stt: SttConfig::default(),
            llm: LlmEndpointConfig::default(),
            tts: TtsConfig::default(),
            s2s: S2sConfig::default(),
            turn: TurnConfig::default(),
            vad: VadConfig::default(),
            chat: ChatConfig::default(),
            daemon: DaemonConfig::default(),
            gguf_dir: DEFAULT_GGUF_DIR.to_string(),
            models: ModelsConfig::default(),
            roles: RolesConfig::default(),
            default: DEFAULT_MODEL.to_string(),
        }
    }
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
