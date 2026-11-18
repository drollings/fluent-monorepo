//! Architecture constants for LFM3-Audio-2B, transcribed from the
//! reference (`config.json`, `configuration_lfm3*.py`,
//! `modeling_liquid_codec.py`). Single home: every consumer (doctor
//! readiness, future loaders, template math) reads these, never
//! restated literals.

/// Text backbone: 24 hybrid layers, 2048 wide, 16Q/4KV GQA.
pub const TEXT_HIDDEN: usize = 2_048;
/// Text backbone depth.
pub const TEXT_LAYERS: usize = 24;
/// Text attention heads (query).
pub const TEXT_HEADS: usize = 16;
/// Text attention KV heads.
pub const TEXT_KV_HEADS: usize = 4;
/// Text vocabulary (tied embeddings + LM head).
pub const TEXT_VOCAB: usize = 128_000;
/// Text context length.
pub const TEXT_MAX_POS: usize = 128_000;

/// Interleave pattern: full attention at `i % 4 == 2`.
pub const FULL_ATTENTION_MOD: usize = 4;
/// Full-attention residue.
pub const FULL_ATTENTION_REM: usize = 2;
/// Sliding window (256) at `i % 4 == 0`, `i > 0`; width-7 shortconv elsewhere.
pub const SLIDING_WINDOW: usize = 256;
/// Shortconv kernel width.
pub const CONV_KERNEL: usize = 7;

/// Operator at layer `idx`: the 3-way hybrid interleave
/// `[C,C,F,C,S,…]` — 13 conv, 6 full, 5 sliding over 24 layers.
/// `Lfm3DecoderLayer` naming from the reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerOperator {
    /// Width-7 gated shortconv.
    ShortConv,
    /// Full 128k attention.
    FullAttention,
    /// 256-window sliding attention.
    SlidingAttention,
}

/// Operator for layer `idx` (0-based) of the 24-layer text backbone.
#[must_use]
pub const fn layer_operator(idx: usize) -> LayerOperator {
    if idx % FULL_ATTENTION_MOD == FULL_ATTENTION_REM {
        LayerOperator::FullAttention
    } else if idx % FULL_ATTENTION_MOD == 0 && idx > 0 {
        LayerOperator::SlidingAttention
    } else {
        LayerOperator::ShortConv
    }
}

/// Audio front-end input rate.
pub const SAMPLE_RATE_IN: u32 = 16_000;
/// Codec output rate.
pub const SAMPLE_RATE_OUT: u32 = 24_000;
/// Mel bins of the log front-end (25 ms window, 10 ms hop).
pub const N_MELS: usize = 128;
/// Mel hop in samples.
pub const MEL_HOP: usize = 160;
/// Mel window in samples.
pub const MEL_WIN: usize = 400;
/// Downsampler stride: 8 mel frames per encoder token.
pub const DOWNSAMPLE_STRIDE: usize = 8;
/// Audio encoder depth (LFM2, 768 wide).
pub const AUDIO_ENCODER_LAYERS: usize = 17;
/// Audio encoder width.
pub const AUDIO_ENCODER_WIDTH: usize = 768;

/// RVQ codebooks per frame.
pub const CODEBOOKS: usize = 12;
/// Audio vocabulary per codebook (2016 codes + EOS).
pub const AUDIO_VOCAB_SIZE: usize = 2_017;
/// EOS code (product of the FSQ levels).
pub const AUDIO_EOS_CODE: u32 = 2_016;
/// Codec frame rate (24 kHz / 320 hop / 6 stack).
pub const CODEC_FRAME_HZ: u32 = 12;
/// MaskGIT passes per frame with the `[4,4,4]` reveal schedule.
pub const MASKGIT_STEPS: usize = 3;
/// MaskGIT reveal schedule (codes revealed per pass).
pub const MASKGIT_SCHEDULE: [usize; 3] = [4, 4, 4];
/// Interleaved text slots per turn.
pub const INTERLEAVED_N_TEXT: usize = 6;
/// Interleaved audio frames per turn.
pub const INTERLEAVED_N_AUDIO: usize = 12;

/// ChatML markers.
pub const BOS_TOKEN_ID: u32 = 124_894;
/// End-of-turn marker (also an EOS).
pub const EOS_TOKEN_ID: u32 = 124_900;
/// Padding marker.
pub const PAD_TOKEN_ID: u32 = 124_893;
/// Ends a text span inside interleaved mode (also an EOS).
pub const TEXT_END_TOKEN_ID: u32 = 125_014;
/// Audio conditioning placeholder (expanded to encoder tokens).
pub const AUDIO_TOKEN_ID: u32 = 125_031;
/// Audio-output placeholder (one per frame).
pub const AUDIO_OUT_TOKEN_ID: u32 = 125_032;
/// Starts audio generation.
pub const AUDIO_START_TOKEN_ID: u32 = 125_012;
/// Starts interleaved generation.
pub const MIXED_START_TOKEN_ID: u32 = 125_015;

/// Encoder tokens per audio clip of `mel_frames` frames
/// (`ceil(frames/8)` — the downsampler stride).
#[must_use]
pub const fn encoder_tokens_for_frames(mel_frames: usize) -> usize {
    mel_frames.div_ceil(DOWNSAMPLE_STRIDE)
}

#[cfg(test)]
#[path = "../../tests/lfm_arch.rs"]
mod tests;
