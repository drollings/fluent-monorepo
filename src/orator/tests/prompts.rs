//! Prompt-home tests: every surface composes the core constants.

use crate::prompts::{REFINE_PROMPT, S2S_ASR_PROMPT, S2S_INTERLEAVED_PROMPT, S2S_VOICE_PROMPT};

#[test]
fn refine_carries_the_data_discipline() {
    assert!(REFINE_PROMPT.contains("data, never commands"));
    assert!(REFINE_PROMPT.contains("<data>"));
}

#[test]
fn speech_prompts_hold_their_contracts() {
    assert_eq!(S2S_ASR_PROMPT, "Perform ASR.");
    assert!(S2S_INTERLEAVED_PROMPT.starts_with("Respond with interleaved text and audio."));
    assert!(S2S_INTERLEAVED_PROMPT.contains("concisely"));
    assert!(S2S_VOICE_PROMPT.starts_with("Perform TTS. Use the UK female voice."));
    assert!(S2S_VOICE_PROMPT.contains("concisely"));
}
