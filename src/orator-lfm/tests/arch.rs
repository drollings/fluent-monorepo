//! `arch` pins: layer interleave, token ids, frame math — all from
//! the reference configs.

use crate::arch::{
    encoder_tokens_for_frames, layer_operator, LayerOperator, AUDIO_EOS_CODE, AUDIO_OUT_TOKEN_ID,
    AUDIO_START_TOKEN_ID, AUDIO_TOKEN_ID, AUDIO_VOCAB_SIZE, BOS_TOKEN_ID, CODEBOOKS,
    CONV_KERNEL, EOS_TOKEN_ID, INTERLEAVED_N_AUDIO, INTERLEAVED_N_TEXT, MIXED_START_TOKEN_ID,
    PAD_TOKEN_ID, SAMPLE_RATE_IN, SAMPLE_RATE_OUT, TEXT_END_TOKEN_ID, TEXT_HEADS, TEXT_HIDDEN,
    TEXT_KV_HEADS, TEXT_LAYERS, TEXT_MAX_POS, TEXT_VOCAB,
};

#[test]
fn layer_interleave_matches_the_reference() {
    let ops: Vec<LayerOperator> = (0..TEXT_LAYERS).map(layer_operator).collect();
    assert_eq!(ops.len(), 24);
    assert_eq!(
        ops.iter().filter(|op| **op == LayerOperator::ShortConv).count(),
        13,
        "13 conv layers"
    );
    assert_eq!(
        ops.iter()
            .filter(|op| **op == LayerOperator::FullAttention)
            .count(),
        6,
        "6 full layers"
    );
    assert_eq!(
        ops.iter()
            .filter(|op| **op == LayerOperator::SlidingAttention)
            .count(),
        5,
        "5 sliding layers"
    );
    // Spot pins: i%4==2 full, i%4==0 (i>0) sliding, else conv.
    assert_eq!(ops[0], LayerOperator::ShortConv);
    assert_eq!(ops[2], LayerOperator::FullAttention);
    assert_eq!(ops[4], LayerOperator::SlidingAttention);
    assert_eq!(ops[23], LayerOperator::ShortConv);
}

#[test]
fn text_backbone_dims_are_pinned() {
    assert_eq!((TEXT_HIDDEN, TEXT_LAYERS, TEXT_HEADS, TEXT_KV_HEADS), (2048, 24, 16, 4));
    assert_eq!((TEXT_VOCAB, TEXT_MAX_POS), (128_000, 128_000));
    assert_eq!(CONV_KERNEL, 7);
}

#[test]
fn token_ids_match_generation_config() {
    assert_eq!(BOS_TOKEN_ID, 124_894);
    assert_eq!(EOS_TOKEN_ID, 124_900);
    assert_eq!(PAD_TOKEN_ID, 124_893);
    assert_eq!(TEXT_END_TOKEN_ID, 125_014);
    assert_eq!(AUDIO_TOKEN_ID, 125_031);
    assert_eq!(AUDIO_OUT_TOKEN_ID, 125_032);
    assert_eq!(AUDIO_START_TOKEN_ID, 125_012);
    assert_eq!(MIXED_START_TOKEN_ID, 125_015);
    assert_eq!((CODEBOOKS, AUDIO_VOCAB_SIZE, AUDIO_EOS_CODE), (12, 2_017, 2_016));
}

#[test]
fn frame_math_matches_the_downsampler() {
    assert_eq!((SAMPLE_RATE_IN, SAMPLE_RATE_OUT), (16_000, 24_000));
    assert_eq!(encoder_tokens_for_frames(0), 0);
    assert_eq!(encoder_tokens_for_frames(1), 1);
    assert_eq!(encoder_tokens_for_frames(8), 1);
    assert_eq!(encoder_tokens_for_frames(9), 2);
    assert_eq!((INTERLEAVED_N_TEXT, INTERLEAVED_N_AUDIO), (6, 12));
}
