//! Model catalog: what the assistant may download, pinned by size and
//! hash. Entries download through the shared artifact plane, which verifies
//! every byte before atomic rename — an entry without a hash cannot
//! download, only document intent.

use serde::{Deserialize, Serialize};

/// Model families the assistant serves locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelKind {
    /// Speech-to-text weights.
    Whisper,
    /// Language-model weights.
    Llm,
    /// Speech-synthesis weights.
    Tts,
    /// Native speech model bundle (audio encoder + backbone + decoder).
    Audio,
    /// Voice-activity weights.
    Vad,
}

/// One downloadable artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEntry {
    /// Short tag used in configuration (`tiny`, `base`, …).
    pub name: String,
    /// Model family.
    pub kind: ModelKind,
    /// Source repository (`host/org`).
    pub repo: String,
    /// File name inside the repository.
    pub file: String,
    /// Expected bytes. Absent while unpinned.
    pub size: Option<u64>,
    /// Expected SHA-256 hex. Absent while unpinned.
    pub sha256: Option<String>,
}

impl ModelEntry {
    /// Whether the artifact plane may fetch this entry. Unpinned entries
    /// are documented intent, not downloads.
    #[must_use]
    pub fn pinned(&self) -> bool {
        self.size.is_some() && self.sha256.is_some()
    }
}

/// The checked-in catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelManifest {
    /// All entries, in preference order per family.
    pub entries: Vec<ModelEntry>,
}

impl ModelManifest {
    /// Look up one entry by family and tag.
    #[must_use]
    pub fn find(&self, kind: ModelKind, name: &str) -> Option<&ModelEntry> {
        self.entries
            .iter()
            .find(|entry| entry.kind == kind && entry.name == name)
    }
}

/// Whisper catalog. Coordinates re-pinned against the upstream repository
/// at implementation time; entries that could not be verified stay
/// explicitly unpinned instead of carrying stale hashes.
#[must_use]
pub fn whisper_entries() -> Vec<ModelEntry> {
    vec![
        ModelEntry {
            name: "tiny".to_string(),
            kind: ModelKind::Whisper,
            repo: "ggerganov/whisper.cpp".to_string(),
            file: "ggml-tiny.bin".to_string(),
            size: Some(77_691_713),
            sha256: Some(
                "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21".to_string(),
            ),
        },
        ModelEntry {
            name: "base-q5_1".to_string(),
            kind: ModelKind::Whisper,
            repo: "ggerganov/whisper.cpp".to_string(),
            file: "ggml-base-q5_1.bin".to_string(),
            size: None,
            sha256: None,
        },
        ModelEntry {
            name: "small".to_string(),
            kind: ModelKind::Whisper,
            repo: "ggerganov/whisper.cpp".to_string(),
            file: "ggml-small.bin".to_string(),
            size: None,
            sha256: None,
        },
        ModelEntry {
            name: "large-v3-turbo-q5_0".to_string(),
            kind: ModelKind::Whisper,
            repo: "ggerganov/whisper.cpp".to_string(),
            file: "ggml-large-v3-turbo-q5_0.bin".to_string(),
            size: None,
            sha256: None,
        },
    ]
}

/// Language-model catalog. The default chat model is pinned by
/// measured bytes and SHA-256 of the installation file.
#[must_use]
pub fn llm_entries() -> Vec<ModelEntry> {
    vec![
        ModelEntry {
            name: "liquidai/lfm3-audio-2b".to_string(),
            kind: ModelKind::Llm,
            repo: "liquidai/lfm3-audio-2b".to_string(),
            file: "liquidai/lfm3-audio-2b/latest.gguf".to_string(),
            size: Some(1_846_236_704),
            sha256: Some(
                "d4e36902ba8f0ee7b333596030a59d650f88ac76660b4d9ba5ff77c3c2258fc0".to_string(),
            ),
        },
        ModelEntry {
            name: "liquidai/lfm2.5-2.6b".to_string(),
            kind: ModelKind::Llm,
            repo: "LiquidAI/LFM2.5-2.6B".to_string(),
            file: "liquidai/lfm2.5-2.6b-gguf/latest.gguf".to_string(),
            size: Some(1_593_894_944),
            sha256: Some(
                "a247afd6414918eac8e520a9e6137dc271235461ecbe1180462221d5b8d40b03".to_string(),
            ),
        },
    ]
}

/// Pinned SHA-256 per audio-bundle component, in manifest order
/// (backbone, mmproj). Measured from the installation files; a byte
/// change here fails verification loudly. LFM3 embeds the codec in
/// the projector: no separate vocoder/tokenizer files.
pub const AUDIO_COMPONENT_PINS: [(&str, u64, &str); 2] = [
    (
        "backbone",
        1_846_236_704,
        "d4e36902ba8f0ee7b333596030a59d650f88ac76660b4d9ba5ff77c3c2258fc0",
    ),
    (
        "mmproj",
        303_811_840,
        "2b6648b1fd48af16aa0877b0592b144d15070783673e038a379fdbc51d40e365",
    ),
];

/// Total pinned bundle bytes.
pub const AUDIO_BUNDLE_SIZE: u64 = 1_846_236_704 + 303_811_840;

/// Bundle manifest pin: SHA-256 over the concatenated per-component
/// hex digests in [`AUDIO_COMPONENT_PINS`] order.
pub const AUDIO_BUNDLE_SHA256: &str =
    "5029535941ee60bdaf800b821767ade7c8a9a51558aec79dac1fee18b7251c03";

/// Speech-synthesis catalog. Voice packs ride with the model entry;
/// nothing is hash-verified yet, so the entry stays explicitly unpinned.
#[must_use]
pub fn tts_entries() -> Vec<ModelEntry> {
    vec![ModelEntry {
        name: "kokoro-82m".to_string(),
        kind: ModelKind::Tts,
        repo: "onnx-community/Kokoro-82M-v1.0-ONNX".to_string(),
        file: "kokoro-v1.0.onnx".to_string(),
        size: None,
        sha256: None,
    }]
}

/// Native speech-model catalog. The bundle (backbone plus the audio
/// projector) lives in the shared installation layout, so the entry
/// pins the bundle manifest ([`AUDIO_BUNDLE_SHA256`] over the
/// per-component pins) instead of a single download. LFM3-Audio-2B is
/// the default; LFM2.5-Audio-1.5B stays listed as the legacy fallback.
#[must_use]
pub fn audio_entries() -> Vec<ModelEntry> {
    vec![
        ModelEntry {
            name: "liquidai/lfm3-audio-2b".to_string(),
            kind: ModelKind::Audio,
            repo: "LiquidAI/LFM3-Audio-2B".to_string(),
            file: "liquidai/lfm3-audio-2b-gguf".to_string(),
            size: Some(AUDIO_BUNDLE_SIZE),
            sha256: Some(AUDIO_BUNDLE_SHA256.to_string()),
        },
        ModelEntry {
            name: "liquidai/lfm2.5-audio-1.5b".to_string(),
            kind: ModelKind::Audio,
            repo: "LiquidAI/LFM2.5-Audio-1.5B".to_string(),
            file: "liquidai/lfm2.5-audio-1.5b-gguf".to_string(),
            size: None,
            sha256: None,
        },
    ]
}

/// Voice-activity catalog. No VAD weights are pinned in v1 (the
/// energy gate ships in `orator-voice::vad`); the family stays reachable
/// through an explicitly empty list so `list`/`find` remain total over
/// `ModelKind`.
#[must_use]
pub fn vad_entries() -> Vec<ModelEntry> {
    Vec::new()
}

#[cfg(test)]
#[path = "../tests/model.rs"]
mod tests;
