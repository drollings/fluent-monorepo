#![forbid(unsafe_code)]

//! orator-models: Model catalog, integrity-checked downloads, and residency management.

pub mod model;
pub mod ops;
pub mod residency;

pub use model::{
    audio_entries, llm_entries, tts_entries, whisper_entries, ModelEntry, ModelKind, ModelManifest,
    AUDIO_BUNDLE_SHA256, AUDIO_BUNDLE_SIZE, AUDIO_COMPONENT_PINS,
};
pub use ops::{
    find_entry, list_models, path_for, pull_plan, remove_entry, resolve_entry_path, verify_bundle,
    verify_bundle_files, verify_entry, ModelOpError, ModelStatus,
};
pub use residency::{ResidencyManager, VoiceLeases, LEASE_SECS, SWEEP_MS};
