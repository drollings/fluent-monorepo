//! Model catalog tests: structure, lookup, pin discipline.

use crate::{
    audio_entries, llm_entries, tts_entries, whisper_entries, ModelKind, ModelManifest,
    AUDIO_BUNDLE_SHA256, AUDIO_BUNDLE_SIZE,
};

#[test]
fn entries_carry_repos_and_files() {
    let entries = whisper_entries();
    assert!(entries.len() >= 4);
    for entry in &entries {
        assert_eq!(entry.kind, ModelKind::Whisper);
        assert!(!entry.repo.is_empty());
        assert!(entry.file.starts_with("ggml-"));
    }
}

#[test]
fn manifest_finds_by_family_and_tag() {
    let manifest = ModelManifest {
        entries: whisper_entries(),
    };
    let tiny = manifest.find(ModelKind::Whisper, "tiny").expect("tiny listed");
    assert_eq!(tiny.file, "ggml-tiny.bin");
    assert!(manifest.find(ModelKind::Llm, "tiny").is_none());
}

#[test]
fn pins_are_explicit_never_stale() {
    // Every entry is either fully pinned (size + hash verified against the
    // upstream repository) or explicitly unpinned. A stale hash is worse
    // than none: the artifact plane would reject every download.
    for entry in whisper_entries() {
        if entry.sha256.is_some() {
            assert!(
                entry.size.is_some_and(|size| size > 0),
                "hash without size is not a pin: {}",
                entry.name
            );
        }
    }
    let manifest = ModelManifest {
        entries: whisper_entries(),
    };
    let tiny = manifest.find(ModelKind::Whisper, "tiny").expect("tiny listed");
    assert!(tiny.pinned(), "tiny must be fully pinned");
}

#[test]
fn manifest_round_trips_json() {
    let manifest = ModelManifest {
        entries: whisper_entries(),
    };
    let json = serde_json::to_string(&manifest).expect("serializable");
    let back: ModelManifest = serde_json::from_str(&json).expect("deserializable");
    assert_eq!(back, manifest);
}

#[test]
fn language_model_default_resolves_as_documented_intent() {
    let manifest = ModelManifest {
        entries: llm_entries(),
    };
    let default = manifest
        .find(ModelKind::Llm, "bartowski/lfm3-1.7b")
        .expect("default tag listed");
    assert_eq!(default.kind, ModelKind::Llm);
    assert!(default.file.ends_with(".gguf"));
    assert!(
        default.pinned(),
        "the default chat weights are measured and pinned"
    );
    assert!(default.size.is_some_and(|size| size > 0));
}

#[test]
fn audio_bundle_entry_pins_the_manifest() {
    let manifest = ModelManifest {
        entries: audio_entries(),
    };
    let bundle = manifest
        .find(ModelKind::Audio, "liquidai/lfm3-audio-2b")
        .expect("audio bundle listed");
    assert!(bundle.pinned());
    assert_eq!(bundle.size, Some(AUDIO_BUNDLE_SIZE));
    assert_eq!(bundle.sha256.as_deref(), Some(AUDIO_BUNDLE_SHA256));
}

#[test]
fn voice_default_resolves_as_documented_intent() {
    let manifest = ModelManifest {
        entries: tts_entries(),
    };
    let default = manifest
        .find(ModelKind::Tts, "kokoro-82m")
        .expect("default voice listed");
    assert_eq!(default.kind, ModelKind::Tts);
    assert!(default.file.ends_with(".onnx"));
    assert!(
        !default.pinned(),
        "no voice weights are hash-verified yet: the entry must stay explicitly unpinned"
    );
}
