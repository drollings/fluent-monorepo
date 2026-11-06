#![forbid(unsafe_code)]

//! LFM3-Audio model knowledge: the deterministic configuration,
//! processing, and layout layer for LiquidAI's native multimodal
//! model (`Lfm3AudioForConditionalGeneration`).
//!
//! This crate is the config/processing/template home — architecture
//! constants, special tokens, chat-template assembly, generation
//! defaults, and weight-layout verification. It owns no tensor math:
//! the streaming transformer, RVQ codec, and MaskGIT sampler need a
//! ROCm-capable tensor runtime that does not exist in-tree (no
//! candle/safetensors/RoPE/KV-cache infrastructure; `ort` cannot
//! express hybrid shortconv transformers; the local `llama-server`
//! build has no `lfm3` backend), so inference stays with a future
//! LFM-capable server composed over these definitions. Nothing here
//! claims to run the model; the manifest reports exactly what a
//! runtime would still need.

pub mod arch;
pub mod manifest;
pub mod params;
pub mod template;

#[cfg(test)]
#[path = "../tests/reference.rs"]
mod reference_tests;

pub use arch::{
    AUDIO_EOS_CODE, AUDIO_OUT_TOKEN_ID, AUDIO_START_TOKEN_ID, AUDIO_TOKEN_ID,
    AUDIO_VOCAB_SIZE, BOS_TOKEN_ID, CODEBOOKS, EOS_TOKEN_ID, INTERLEAVED_N_AUDIO,
    INTERLEAVED_N_TEXT, MASKGIT_STEPS, MIXED_START_TOKEN_ID, N_MELS, PAD_TOKEN_ID,
    SAMPLE_RATE_IN, SAMPLE_RATE_OUT, TEXT_END_TOKEN_ID,
};
pub use manifest::{LfmLayout, LfmWeights};
pub use params::{AudioGen, GenerationDefaults, TextGen};
pub use template::{assemble_prompt, AudioClip, AudioOutClip, TextTurn};
