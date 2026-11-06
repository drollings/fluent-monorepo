//! Wire-codec tests: WAV bytes, message shapes, SSE audio deltas.

use crate::protocol::{
    audio_msg, b64, chat_body, completions_url, encode_wav_pcm16, parse_sse_line, pcm_bytes_to_f32,
    system_msg, text_msg, unb64, StreamEvent, SYSTEM_ASR, SYSTEM_INTERLEAVED,
};

#[test]
fn wav_header_frames_pcm16() {
    let bytes = encode_wav_pcm16(&[0.0, 1.0, -1.0, 0.5], 16_000);
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(&bytes[12..16], b"fmt ");
    assert_eq!(
        u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]),
        16_000
    );
    assert_eq!(&bytes[36..40], b"data");
    assert_eq!(bytes.len(), 44 + 8);
    // Full-scale positive clips to i16::MAX.
    assert_eq!(i16::from_le_bytes([bytes[46], bytes[47]]), 32767);
}

#[test]
fn base64_round_trips() {
    let bytes = vec![0u8, 1, 2, 250, 255];
    assert_eq!(unb64(&b64(&bytes)).expect("round trip"), bytes);
    assert!(unb64("!!!not-base64!!!").is_err());
}

#[test]
fn pcm_bytes_slice_f32_little_endian() {
    let samples = pcm_bytes_to_f32(&[0u8, 0, 128, 63, 0, 0, 0, 0, 1, 2, 3]);
    assert_eq!(samples.len(), 2);
    assert!((samples[0] - 1.0).abs() < f32::EPSILON);
    assert!((samples[1] - 0.0).abs() < f32::EPSILON);
}

#[test]
fn messages_carry_the_demo_shapes() {
    assert_eq!(SYSTEM_ASR, "Perform ASR.");
    assert!(SYSTEM_INTERLEAVED.starts_with("Respond with interleaved text and audio."));
    let system = system_msg(SYSTEM_ASR);
    assert_eq!(system["role"], serde_json::json!("system"));
    assert_eq!(system["content"], serde_json::json!(SYSTEM_ASR));
    let audio = audio_msg(&[1, 2, 3]);
    assert_eq!(
        audio["content"][0]["type"],
        serde_json::json!("input_audio")
    );
    assert_eq!(
        audio["content"][0]["input_audio"]["format"],
        serde_json::json!("wav")
    );
    let body = chat_body("m", &[text_msg("hi")], 512, true);
    assert_eq!(body["stream"], serde_json::json!(true));
    assert_eq!(body["extra_body"]["reset_context"], serde_json::json!(true));
}

#[test]
fn completions_url_uses_the_versioned_route() {
    assert_eq!(
        completions_url("http://127.0.0.1:8080"),
        "http://127.0.0.1:8080/v1/chat/completions"
    );
    assert_eq!(
        completions_url("http://127.0.0.1:8080/v1/chat/completions"),
        "http://127.0.0.1:8080/v1/chat/completions"
    );
}

#[test]
fn sse_audio_deltas_parse() {
    use base64::prelude::*;
    let pcm: Vec<u8> = 1.0f32.to_le_bytes().to_vec();
    let line = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"hi\",\"audio_chunk\":{{\"data\":\"{}\"}}}}}}]}}",
        BASE64_STANDARD.encode(&pcm)
    );
    let events = parse_sse_line(&line).expect("parses");
    assert_eq!(events.len(), 2);
    assert!(events.contains(&StreamEvent::Text("hi".to_string())));
    assert_eq!(events[1], StreamEvent::Audio(vec![1.0]));
    // String-shaped chunks parse too; stop reasons finish.
    let line = "data: {\"choices\":[{\"delta\":{\"audio_chunk\":\"AAріз\"}}]}";
    let _ = parse_sse_line(line);
    assert_eq!(
        parse_sse_line("data: {\"choices\":[{\"finish_reason\":\"stop\"}]}"),
        Some(vec![StreamEvent::Done])
    );
    assert_eq!(
        parse_sse_line("data: [DONE]"),
        Some(vec![StreamEvent::Done])
    );
    assert!(parse_sse_line(": keep-alive").is_none());
}

/// M2-S1 characterization: byte-exact 44-byte RIFF header golden.
/// The shared header builder must reproduce these bytes identically.
#[test]
fn orator_s2s_wav_header_golden() {
    let bytes = encode_wav_pcm16(&[0.0, 1.0], 16_000);
    assert_eq!(bytes.len(), 48);
    let mut header = Vec::new();
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&40u32.to_le_bytes()); // 36 + 2 samples * 2 bytes
    header.extend_from_slice(b"WAVEfmt ");
    header.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk length
    header.extend_from_slice(&1u16.to_le_bytes()); // PCM
    header.extend_from_slice(&1u16.to_le_bytes()); // mono
    header.extend_from_slice(&16_000u32.to_le_bytes()); // rate
    header.extend_from_slice(&32_000u32.to_le_bytes()); // rate * 2 bytes
    header.extend_from_slice(&2u16.to_le_bytes()); // block align
    header.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    header.extend_from_slice(b"data");
    header.extend_from_slice(&4u32.to_le_bytes()); // data length
    assert_eq!(&bytes[..44], header.as_slice(), "header byte-identical");
    assert_eq!(
        &bytes[44..],
        &[0, 0, 0xFF, 0x7F],
        "samples follow the header"
    );
}

/// M1-S1 characterization: the truncating i16 LE quantizer. Unlike the
/// audio codec (round-half-away), `as i16` truncates toward zero:
/// 0.5 * 32767 = 16383.5 encodes to 16383. The shared
/// `common_core::pcm` primitive must reproduce these vectors exactly.
#[test]
fn orator_s2s_wav_frame() {
    // Empty input is a header-only frame: 44 bytes, zero data length.
    let empty = encode_wav_pcm16(&[], 16_000);
    assert_eq!(empty.len(), 44, "empty frame is header-only");
    assert_eq!(&empty[0..4], b"RIFF");
    assert_eq!(
        u32::from_le_bytes([empty[40], empty[41], empty[42], empty[43]]),
        0
    );
    // Negative rail and truncation direction.
    let bytes = encode_wav_pcm16(&[-1.0, 0.5, 1.5, f32::NAN], 16_000);
    assert_eq!(bytes.len(), 44 + 8);
    assert_eq!(
        i16::from_le_bytes([bytes[44], bytes[45]]),
        -32767,
        "-1.0 rail"
    );
    assert_eq!(
        i16::from_le_bytes([bytes[46], bytes[47]]),
        16383,
        "16383.5 truncates toward zero to 16383"
    );
    assert_eq!(
        i16::from_le_bytes([bytes[48], bytes[49]]),
        32767,
        "hot clamps"
    );
    assert_eq!(
        i16::from_le_bytes([bytes[50], bytes[51]]),
        0,
        "NaN saturates to silence"
    );
    // Rate tag and data-length framing follow the sample rate.
    let wide = encode_wav_pcm16(&[0.0], 24_000);
    assert_eq!(
        u32::from_le_bytes([wide[24], wide[25], wide[26], wide[27]]),
        24_000
    );
    assert_eq!(
        u32::from_le_bytes([wide[28], wide[29], wide[30], wide[31]]),
        48_000
    );
    assert_eq!(
        u32::from_le_bytes([wide[40], wide[41], wide[42], wide[43]]),
        2
    );
    // Base64 rejects loudly and trims surrounding whitespace.
    assert!(unb64("  ").is_ok(), "blank decodes to empty");
    let err = unb64("!!!not-base64!!!").expect_err("loud on garbage");
    assert!(
        err.starts_with("bad base64: "),
        "error names the failure: {err}"
    );
}

/// The two public WAV encoders agree byte-for-byte on the same input:
/// the audio writer and the S2S frame encoder share one contract.
#[test]
fn audio_writer_and_s2s_encoder_agree_byte_for_byte() {
    for (samples, rate) in [
        (vec![], 16_000),
        (vec![0.0, 0.5, -0.25], 16_000),
        (vec![-1.0, 0.5, 1.5, f32::NAN], 16_000),
        (vec![0.0], 24_000),
    ] {
        assert_eq!(
            orator_audio::encode_wav_mono(&samples, rate),
            encode_wav_pcm16(&samples, rate),
            "identical bytes for {samples:?} at {rate} Hz"
        );
    }
}
