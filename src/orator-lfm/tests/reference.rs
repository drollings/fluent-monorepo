//! Reference anchoring: every expectation below was RECORDED from
//! the real HuggingFace processor (`fixtures/reference_expansion.json`,
//! produced by `fixtures/probe_reference.py` against the reference
//! tree) — never hand-written. The hermetic goldens pin our layer
//! against those vectors byte-for-byte; the `live` target re-runs
//! the probe to guard fixture staleness.
//!
//! What stays unvalidated hermetically, and why a scripted
//! assertion there would be a false measurement:
//! - Tensor math (STFT spectra, mel values, codec latents): we pin
//!   frame/token COUNTS, never spectra. A scripted encoder ignores
//!   audio by contract, so a 0.0 spectral delta would measure the
//!   stub, not the model (the same reason scripted-decoders stay
//!   out of the WER harness in `doc/orator/CALIBRATION.md`).
//! - MaskGIT sampling and codec audio: the reveal schedule SHAPE
//!   (`[4,4,4]` covering 12 codebooks) and the frames+1 EOS rule
//!   pin; the sampling distribution and waveform fidelity need the
//!   weights and ears, not assertions.
//! - Multi-turn conversation assembly: the assembler covers one
//!   turn; bos handling, generation prompts, and truncation refusals
//!   execute server-side through the reference template.
//! - The recording itself used fixed deterministic waveforms (sine
//!   at exact sample counts) — no randomness, no network,
//!   reproducible with the probe.

use crate::arch::{
    AUDIO_EOS_CODE, AUDIO_OUT_TOKEN_ID, AUDIO_TOKEN_ID, BOS_TOKEN_ID, DOWNSAMPLE_STRIDE,
    EOS_TOKEN_ID, MEL_HOP, MEL_WIN, N_MELS, PAD_TOKEN_ID, SAMPLE_RATE_IN,
};
use crate::template::{assemble_prompt, AudioClip, AudioOutClip, TextTurn};

/// The frozen reference vectors, recorded by the probe.
fn recorded() -> serde_json::Value {
    serde_json::from_str(include_str!("fixtures/reference_expansion.json"))
        .expect("fixtures are valid JSON")
}

/// JSON numbers arrive as u64; our counts are usize. The
/// conversion is exact on every supported target (values are tiny).
fn as_usize(value: &serde_json::Value) -> Option<usize> {
    value.as_u64().and_then(|v| usize::try_from(v).ok())
}

#[test]
fn mel_contract_matches_the_reference() {
    let contract = &recorded()["mel_contract"];
    assert_eq!(as_usize(&contract["feature_size"]), Some(N_MELS));
    assert_eq!(contract["sampling_rate"].as_u64(), Some(u64::from(SAMPLE_RATE_IN)));
    assert_eq!(as_usize(&contract["n_fft"]), Some(MEL_WIN));
    assert_eq!(as_usize(&contract["hop_length"]), Some(MEL_HOP));
    assert_eq!(as_usize(&contract["win_length"]), Some(MEL_WIN));
    assert_eq!(as_usize(&contract["encoder_stride"]), Some(DOWNSAMPLE_STRIDE));
}

#[test]
fn token_ids_match_the_reference() {
    let ids = &recorded()["token_ids"];
    assert_eq!(ids["audio"].as_u64(), Some(u64::from(AUDIO_TOKEN_ID)));
    assert_eq!(ids["audio_out"].as_u64(), Some(u64::from(AUDIO_OUT_TOKEN_ID)));
    assert_eq!(ids["bos"].as_u64(), Some(u64::from(BOS_TOKEN_ID)));
    assert_eq!(ids["eos"].as_u64(), Some(u64::from(EOS_TOKEN_ID)));
    assert_eq!(ids["pad"].as_u64(), Some(u64::from(PAD_TOKEN_ID)));
}

#[test]
fn text_turn_framing_matches_the_reference() {
    let framed = recorded()["framed_text_turn"].as_str().expect("framed string").to_string();
    let turn = TextTurn {
        role: "user".to_string(),
        text: "hello".to_string(),
        audio: None,
        audio_out: None,
    };
    let (text, ids) = assemble_prompt(&turn).expect("assembles");
    assert_eq!(text, framed, "byte-identical framing");
    assert!(ids.is_empty(), "no placeholders, no ids");
}

#[test]
fn encoder_counts_match_the_reference_on_all_fixtures() {
    let fixtures = recorded()["fixtures"].as_array().expect("fixtures").clone();
    assert!(fixtures.len() >= 3, "two mel fixtures plus the edge: {fixtures:?}");
    for fixture in &fixtures {
        let mel = fixture["mel_frames"].as_u64().and_then(|v| usize::try_from(v).ok()).expect("mel frames");
        let tokens = fixture["encoder_tokens"].as_u64().and_then(|v| usize::try_from(v).ok()).expect("tokens");
        assert_eq!(
            crate::arch::encoder_tokens_for_frames(mel),
            tokens,
            "fixture `{}`: ceil({mel}/8)",
            fixture["name"].as_str().unwrap_or("?")
        );
    }
}

#[test]
fn audio_expansion_matches_the_reference_end_to_end() {
    let expansion = &recorded()["audio_expansion"];
    let mel = expansion["mel_frames"].as_u64().and_then(|v| usize::try_from(v).ok()).expect("mel frames");
    let tokens = expansion["encoder_tokens"].as_u64().and_then(|v| usize::try_from(v).ok()).expect("tokens");
    assert_eq!(
        expansion["audio_token_id"].as_u64(),
        Some(u64::from(AUDIO_TOKEN_ID))
    );
    let turn = TextTurn {
        role: "user".to_string(),
        text: expansion["text"].as_str().expect("text").to_string(),
        audio: Some(AudioClip { mel_frames: mel }),
        audio_out: None,
    };
    let (text, ids) = assemble_prompt(&turn).expect("assembles");
    assert_eq!(
        text,
        expansion["framed_turn"].as_str().expect("framed turn"),
        "framing and expansion byte-identical"
    );
    assert_eq!(ids, vec![AUDIO_TOKEN_ID; tokens], "one id per encoder token");
}

#[test]
fn audio_out_expansion_matches_the_reference_end_to_end() {
    let out = &recorded()["audio_out"];
    let frames = out["codec_frames"].as_u64().and_then(|v| usize::try_from(v).ok()).expect("codec frames");
    let placeholders = out["placeholders"].as_u64().and_then(|v| usize::try_from(v).ok()).expect("placeholders");
    assert_eq!(placeholders, frames + 1, "every clip carries the EOS frame");
    assert_eq!(out["eos_code"].as_u64(), Some(u64::from(AUDIO_EOS_CODE)));
    assert_eq!(
        out["audio_out_token_id"].as_u64(),
        Some(u64::from(AUDIO_OUT_TOKEN_ID))
    );
    let turn = TextTurn {
        role: "assistant".to_string(),
        text: out["text"].as_str().expect("text").to_string(),
        audio: None,
        audio_out: Some(AudioOutClip { frames }),
    };
    let (text, ids) = assemble_prompt(&turn).expect("assembles");
    assert_eq!(
        text,
        out["framed_turn"].as_str().expect("framed turn"),
        "frames plus EOS frame, byte-identical"
    );
    assert_eq!(ids, vec![AUDIO_OUT_TOKEN_ID; placeholders]);
}
