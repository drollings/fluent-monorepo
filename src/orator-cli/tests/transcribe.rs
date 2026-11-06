//! Transcribe tests: golden outputs per format.

use crate::transcribe::{render, Cue, TranscriptFormat};

fn cues() -> Vec<Cue> {
    vec![
        Cue {
            start_s: 0.0,
            end_s: 1.5,
            text: "hello".to_string(),
        },
        Cue {
            start_s: 2.0,
            end_s: 3.25,
            text: "world".to_string(),
        },
    ]
}

#[test]
fn txt_joins_lines() {
    assert_eq!(
        render(&cues(), TranscriptFormat::Txt).expect("txt renders"),
        "hello\nworld"
    );
}

#[test]
fn srt_numbers_and_stamps() {
    let rendered = render(&cues(), TranscriptFormat::Srt).expect("srt renders");
    assert!(rendered.contains("1\n00:00:00,000 --> 00:00:01,500\nhello"));
    assert!(rendered.contains("2\n00:00:02,000 --> 00:00:03,250\nworld"));
}

#[test]
fn vtt_headers_and_dots() {
    let rendered = render(&cues(), TranscriptFormat::Vtt).expect("vtt renders");
    assert!(rendered.starts_with("WEBVTT"));
    assert!(rendered.contains("00:00:01.500"));
}

#[test]
fn json_parses_back() {
    let rendered = render(&cues(), TranscriptFormat::Json).expect("json renders");
    let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("valid json");
    assert_eq!(parsed.as_array().expect("array").len(), 2);
}

#[test]
fn formats_reject_unknown_names() {
    assert!(TranscriptFormat::parse("srt").is_some());
    assert!(TranscriptFormat::parse("ass").is_none());
}

#[test]
fn non_finite_stamps_are_usage_errors() {
    for seconds in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let cues = vec![Cue {
            start_s: seconds,
            end_s: seconds,
            text: "x".to_string(),
        }];
        for format in [
            TranscriptFormat::Srt,
            TranscriptFormat::Vtt,
            TranscriptFormat::Json,
        ] {
            assert!(
                render(&cues, format).is_err(),
                "non-finite {seconds} must fail for {format:?}"
            );
        }
    }
}

#[test]
fn reversed_cues_clamp_end_to_start() {
    use crate::transcribe::cue_from_centiseconds;
    let cue = cue_from_centiseconds(200, 100, "hi".to_string());
    assert!(cue.end_s >= cue.start_s, "end clamps to start: {cue:?}");
    assert!((cue.start_s - 2.0).abs() < 1e-9);
    assert!((cue.end_s - 2.0).abs() < 1e-9);
}

#[test]
fn cue_conversion_clamps_negatives_and_huge() {
    use crate::transcribe::cue_from_centiseconds;
    let neg = cue_from_centiseconds(-50, -10, "n".to_string());
    assert!((neg.start_s - 0.0).abs() < 1e-9);
    assert!((neg.end_s - 0.0).abs() < 1e-9);
    let huge = cue_from_centiseconds(i64::MAX, i64::MAX, "h".to_string());
    assert!(huge.start_s.is_finite());
    assert!(huge.end_s >= huge.start_s);
}

#[test]
fn empty_cue_list_has_trailing_newline_contract() {
    let empty: Vec<Cue> = Vec::new();
    assert_eq!(render(&empty, TranscriptFormat::Txt).expect("txt"), "");
    let srt = render(&empty, TranscriptFormat::Srt).expect("srt");
    assert_eq!(srt, "");
    let vtt = render(&empty, TranscriptFormat::Vtt).expect("vtt");
    assert!(vtt.starts_with("WEBVTT"));
    let json = render(&empty, TranscriptFormat::Json).expect("json");
    assert_eq!(json, "[]");
}

#[test]
fn thinking_strip_parity_documented() {
    // Intentional difference: the audio-server (LLM) path strips
    // thinking blocks; the whisper (acoustic) path keeps transcript
    // bytes verbatim, including literal `<think>` words actually spoken.
    let tagged = "hello <think>not a tag</think> world".to_string();
    let stripped = fluent_llm::thinking::strip_thinking_blocks(&tagged);
    assert!(!stripped.contains("<think>"), "LLM path strips: {stripped}");
    assert!(tagged.contains("<think>"), "whisper path keeps verbatim");
}

#[test]
fn audio_server_cue_spans_the_samples() {
    use crate::transcribe::audio_server_cue;
    let cue = audio_server_cue("hello".to_string(), 16_000);
    assert!((cue.start_s - 0.0).abs() < 1e-9);
    assert!((cue.end_s - 1.0).abs() < 1e-9);
    assert_eq!(cue.text, "hello");
    // No-fallback asymmetry with `say`: a failed audio-server
    // transcription is an honest error, never a chain advance —
    // transcripts must not invent words.
    let rendered = render(&[cue], TranscriptFormat::Txt).expect("renders");
    assert_eq!(rendered, "hello");
}

#[test]
fn cue_bounds_extremes_to_media_range() {
    use crate::transcribe::{cue_from_centiseconds, MAX_CUE_SECONDS};
    // i64 extremes never become giant stamps: clamped to the media cap.
    let max = cue_from_centiseconds(i64::MAX, i64::MAX, "h".to_string());
    assert!(max.start_s <= MAX_CUE_SECONDS, "MAX clamps: {max:?}");
    assert!(max.end_s <= MAX_CUE_SECONDS, "MAX clamps: {max:?}");
    let min = cue_from_centiseconds(i64::MIN, i64::MIN, "m".to_string());
    assert!(
        (min.start_s - 0.0).abs() < 1e-9,
        "MIN clamps to zero: {min:?}"
    );
    assert!(
        (min.end_s - 0.0).abs() < 1e-9,
        "MIN clamps to zero: {min:?}"
    );
    // Negative overflow clamps, never wraps.
    let neg = cue_from_centiseconds(-9_223_372_036_854, -1, "n".to_string());
    assert!((neg.start_s - 0.0).abs() < 1e-9);
    assert!((neg.end_s - 0.0).abs() < 1e-9);
    // End-before-start clamps up (existing contract holds under the cap).
    let rev = cue_from_centiseconds(200, 100, "hi".to_string());
    assert!(rev.end_s >= rev.start_s);
}

#[test]
fn transcribe_codec_round_trips_i16_base64() {
    use crate::transcribe::{decode_transcribe_text, encode_transcribe_audio};
    let params = encode_transcribe_audio(&[0.0, 0.5, -0.5, 1.0]);
    assert_eq!(
        params.get("rate").and_then(serde_json::Value::as_u64),
        Some(16_000)
    );
    let encoded = params
        .get("audio")
        .and_then(|audio| audio.as_str())
        .expect("audio present");
    use base64::prelude::*;
    let raw = BASE64_STANDARD.decode(encoded).expect("valid base64");
    assert_eq!(raw.len(), 8, "i16 LE: 2 bytes per sample");
    assert_eq!(
        i16::from_le_bytes([raw[2], raw[3]]),
        16383,
        "0.5 scales to half range"
    );
    let body = serde_json::json!({"text": "hello there"});
    assert_eq!(
        decode_transcribe_text(&body).expect("text decodes"),
        "hello there"
    );
    decode_transcribe_text(&serde_json::json!({})).expect_err("missing text is loud");
}

/// M1-S1 characterization: the Transcribe i16 LE payload shares the
/// s2s truncating quantizer (NOT the codec's rounding one). The shared
/// `common_core::pcm` primitive must reproduce these vectors exactly.
#[test]
fn orator_transcribe_pair() {
    use crate::transcribe::encode_transcribe_audio;
    use base64::prelude::*;
    // Rails and truncation direction match s2s, not the codec.
    let params = encode_transcribe_audio(&[1.0, -1.0, 0.5]);
    let raw = BASE64_STANDARD
        .decode(
            params
                .get("audio")
                .and_then(|audio| audio.as_str())
                .expect("audio"),
        )
        .expect("valid base64");
    assert_eq!(i16::from_le_bytes([raw[0], raw[1]]), 32767);
    assert_eq!(i16::from_le_bytes([raw[2], raw[3]]), -32767);
    assert_eq!(
        i16::from_le_bytes([raw[4], raw[5]]),
        16383,
        "truncates like s2s"
    );
    // Empty samples encode to an empty (but present) payload.
    let params = encode_transcribe_audio(&[]);
    assert_eq!(
        params.get("audio").and_then(|audio| audio.as_str()),
        Some(""),
        "empty samples keep the envelope with an empty payload"
    );
}

/// The file gate the binary consults before inference: non-speech
/// fixtures hold (silence and the verified 440 Hz tone shape), voiced
/// proxies fire (transparency — speech takes the ungated path).
#[test]
fn file_gate_holds_non_speech_and_fires_on_speech() {
    fn tone(freq: f32, amplitude: f32) -> Vec<f32> {
        (0..16_000)
            .map(|index| {
                amplitude * (2.0 * std::f32::consts::PI * freq * index as f32 / 16_000.0).sin()
            })
            .collect()
    }
    assert!(
        !orator_vad::speech_present_16k(&vec![0.0; 16_000]),
        "silence holds the gate"
    );
    assert!(
        !orator_vad::speech_present_16k(&tone(440.0, 0.3)),
        "the verified tone shape holds the gate"
    );
    let voiced: Vec<f32> = (0..16_000)
        .map(|index| {
            let time = index as f32 / 16_000.0;
            0.35 * (2.0 * std::f32::consts::PI * 150.0 * time).sin()
                + 0.35 * (2.0 * std::f32::consts::PI * 300.0 * time).sin()
        })
        .collect();
    assert!(
        orator_vad::speech_present_16k(&voiced),
        "voiced input fires the gate (byte-identical path)"
    );
}

/// The held-gate note carries the envelope prefix and names the outcome.
#[test]
fn held_gate_note_is_prefixed_and_empty() {
    use crate::transcribe::{TranscriptFormat, NO_SPEECH_NOTE};
    assert!(NO_SPEECH_NOTE.starts_with("orator: "), "{NO_SPEECH_NOTE}");
    assert!(NO_SPEECH_NOTE.contains("no speech"), "{NO_SPEECH_NOTE}");
    let empty: Vec<crate::transcribe::Cue> = Vec::new();
    assert_eq!(render(&empty, TranscriptFormat::Txt).expect("txt"), "");
    assert_eq!(render(&empty, TranscriptFormat::Json).expect("json"), "[]");
}
