//! Wire-codec tests: WAV bytes, message shapes, SSE audio deltas.

use crate::protocol::{
    audio_msg, b64, chat_body, completions_url, encode_wav_pcm16, parse_sse_line,
    pcm_bytes_to_f32, system_msg, text_msg, unb64, StreamEvent, SYSTEM_ASR, SYSTEM_INTERLEAVED,
};

#[test]
fn wav_header_frames_pcm16() {
    let bytes = encode_wav_pcm16(&[0.0, 1.0, -1.0, 0.5], 16_000);
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(&bytes[12..16], b"fmt ");
    assert_eq!(u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]), 16_000);
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
    assert_eq!(SYSTEM_INTERLEAVED, "Respond with interleaved text and audio.");
    let system = system_msg(SYSTEM_ASR);
    assert_eq!(system["role"], serde_json::json!("system"));
    assert_eq!(system["content"], serde_json::json!(SYSTEM_ASR));
    let audio = audio_msg(&[1, 2, 3]);
    assert_eq!(audio["content"][0]["type"], serde_json::json!("input_audio"));
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
    assert_eq!(parse_sse_line("data: [DONE]"), Some(vec![StreamEvent::Done]));
    assert!(parse_sse_line(": keep-alive").is_none());
}
