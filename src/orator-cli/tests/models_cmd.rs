//! Characterization for the cross-family catalog lookup. The lookup walks
//! families in a fixed order, so a tag listed by more than one family resolves
//! to the first family that declares it.

use orator_models::ModelKind;

use crate::models_cmd::find_model_entry;

#[test]
fn unknown_and_empty_names_return_none() {
    assert!(find_model_entry("").is_none());
    assert!(find_model_entry("no-such-model").is_none());
}

#[test]
fn whisper_family_resolves_before_the_other_families() {
    let entry = find_model_entry("tiny").expect("whisper tag");
    assert_eq!(entry.kind, ModelKind::Whisper);
}

#[test]
fn every_unique_family_resolves() {
    assert_eq!(
        find_model_entry("liquidai/lfm2.5-2.6b").map(|entry| entry.kind),
        Some(ModelKind::Llm)
    );
    assert_eq!(
        find_model_entry("kokoro-82m").map(|entry| entry.kind),
        Some(ModelKind::Tts)
    );
    assert_eq!(
        find_model_entry("liquidai/lfm2.5-audio-1.5b").map(|entry| entry.kind),
        Some(ModelKind::Audio)
    );
}

#[test]
fn duplicate_tag_resolves_to_the_first_matching_family() {
    // `liquidai/lfm3-audio-2b` is listed by both the LLM and the audio
    // families; the LLM family is walked first.
    let entry = find_model_entry("liquidai/lfm3-audio-2b").expect("duplicate tag");
    assert_eq!(entry.kind, ModelKind::Llm);
}
