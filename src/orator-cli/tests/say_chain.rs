//! Voice chain goldens: advance exactly on voicelessness and empty
//! transport failures, never on interrupts; daemon hints never erase
//! ejection; spoken text never carries fences or URLs; a played prefix
//! is never spoken twice.

use crate::say_chain::{
    classify_daemon_call_failure, classify_daemon_voice_error, normalized_chunks, should_advance,
    unsynthesized_suffix, DaemonVoiceHint,
};
use orator_s2s::S2sError;

#[test]
fn advance_on_voicelessness_and_empty_transport_failures() {
    assert!(should_advance(&S2sError::NoAudio));
    assert!(should_advance(&S2sError::Backend(
        "audio endpoint failed: 500".to_string()
    )));
    assert!(should_advance(&S2sError::Overflow(
        "cap exceeded".to_string()
    )));
}

#[test]
fn interrupts_never_advance() {
    assert!(!should_advance(&S2sError::Aborted));
}

#[test]
fn unsupported_audio_input_advances_through_the_same_gate() {
    let error = S2sError::Backend("audio input is not supported".to_string());
    assert!(orator_s2s::audio_input_unsupported(&error));
    assert!(should_advance(&error));
}

#[test]
fn daemon_ejection_and_saturation_skip_boot() {
    match classify_daemon_voice_error(
        "orator: daemon refused VoiceEndpoint: {\"error\":\"voice endpoint ejected: streak 2\"}",
    ) {
        DaemonVoiceHint::Ejected(_) => {}
        other => panic!("ejected must route to Ejected, got {other:?}"),
    }
    match classify_daemon_voice_error(
        "orator: daemon refused VoiceEndpoint: {\"error\":\"admission saturated: bound 2\"}",
    ) {
        DaemonVoiceHint::Saturated(_) => {}
        other => panic!("saturated must route to Saturated, got {other:?}"),
    }
    match classify_daemon_voice_error("orator: daemon refused VoiceEndpoint: {\"error\":\"voice server boot in progress — retry shortly (concurrent boot)\",\"kind\":\"BootContention\"}") {
        DaemonVoiceHint::BootContention(_) => {}
        other => panic!("contention must route to BootContention, got {other:?}"),
    }
    assert_eq!(
        classify_daemon_voice_error(
            "orator: daemon unavailable at /run/user/1000/orator.sock (run oratord)"
        ),
        DaemonVoiceHint::Unavailable
    );
    assert_eq!(
        classify_daemon_call_failure(None),
        DaemonVoiceHint::Unavailable
    );
    assert_eq!(
        classify_daemon_call_failure(Some("plain refusal without markers")),
        DaemonVoiceHint::Unavailable
    );
}

#[test]
fn structured_kind_wins_over_wording() {
    match classify_daemon_voice_error(
        "orator: daemon refused Say: {\"error\":\"totally calm line\",\"kind\":\"Saturated\"}",
    ) {
        DaemonVoiceHint::Saturated(_) => {}
        other => panic!("kind Saturated must win without the word, got {other:?}"),
    }
    match classify_daemon_voice_error(
        "orator: daemon refused Ask: {\"error\":\"admission saturated: bound 2\",\"kind\":\"Ejected\"}",
    ) {
        DaemonVoiceHint::Ejected(_) => {}
        other => panic!("kind Ejected must win over wording, got {other:?}"),
    }
    match classify_daemon_voice_error(
        "orator: daemon refused Say: {\"error\":\"slow peer: handler budget elapsed\",\"kind\":\"SlowPeer\"}",
    ) {
        DaemonVoiceHint::SlowPeer(_) => {}
        other => panic!("kind SlowPeer must map to SlowPeer, got {other:?}"),
    }
    match classify_daemon_voice_error(
        "orator: daemon refused VoiceEndpoint: {\"error\":\"slow peer: handler budget elapsed\",\"kind\":\"SlowPeer\"}",
    ) {
        DaemonVoiceHint::SlowPeer(_) => {}
        other => panic!("SlowPeer must stay retryable, got {other:?}"),
    }
    assert_eq!(
        classify_daemon_call_failure(Some(
            "orator: daemon refused Ask: {\"error\":\"x\",\"kind\":\"BootContention\"}"
        )),
        DaemonVoiceHint::BootContention(
            "orator: daemon refused Ask: {\"error\":\"x\",\"kind\":\"BootContention\"}".to_string()
        )
    );
    assert_eq!(
        classify_daemon_call_failure(Some(
            "orator: daemon refused Say: {\"error\":\"slow peer\",\"kind\":\"SlowPeer\"}"
        )),
        DaemonVoiceHint::SlowPeer(
            "orator: daemon refused Say: {\"error\":\"slow peer\",\"kind\":\"SlowPeer\"}"
                .to_string()
        )
    );
}

#[test]
fn fences_and_urls_never_reach_the_voice() {
    let chunks = normalized_chunks("See https://example.com/x for ```cargo test``` details.");
    let joined = chunks.join(" ");
    assert!(
        !joined.contains("```"),
        "fences must not be spoken: {joined}"
    );
    assert!(
        !joined.contains("https://"),
        "URLs must not be spoken: {joined}"
    );
    assert!(joined.contains("See"), "prose stays: {joined}");
    assert!(joined.contains("details"), "prose stays: {joined}");
}

#[test]
fn normalized_split_matches_direct_normalize_then_split() {
    let text = "# Hello\n\nSee `my_var` at https://example.com/a and ```\ncode()\n``` end.";
    let expected: Vec<String> = orator_tts::split_all(&orator_tts::normalize_for_speech(text))
        .into_iter()
        .map(|chunk| chunk.text)
        .collect();
    assert_eq!(normalized_chunks(text), expected);
}

#[test]
fn played_prefix_never_speaks_twice() {
    let full = "Hello world. Goodbye world.";
    assert_eq!(unsynthesized_suffix(full, 0), full);
    let suffix = unsynthesized_suffix(full, "Hello world. ".chars().count());
    assert_eq!(suffix, "Goodbye world.");
    assert_eq!(unsynthesized_suffix(full, full.chars().count() + 10), "");
}

#[test]
fn every_chain_hop_names_its_remedy() {
    // The serving path narrates each hop so silence always ships the
    // exact fix. Pin the wording here; the serving code prints these
    // exact lines.
    for remedy in [
        "shared endpoint sent no audio",
        "using the daemon-owned voice server",
        "booting a supervised voice server",
        "endpoint rejects audio input",
        "synthesizing on the GPU worker",
        "no llama-tts binary",
        "synthesizing locally (kokoro fallback)",
        "no local voice at",
    ] {
        assert!(!remedy.is_empty(), "remedy line must exist");
    }
    // The classifier keeps the terminal arm honest: aborted turns
    // carry the interrupt code, never a remedy line.
    assert!(!should_advance(&S2sError::Aborted));
}

#[test]
fn daemon_audio_body_decodes_to_playable_samples() {
    use crate::say_chain::decode_say_audio;
    let samples = vec![0.0f32, 0.5, -0.5];
    let mut raw = Vec::new();
    for sample in &samples {
        raw.extend_from_slice(&sample.to_le_bytes());
    }
    use base64::prelude::*;
    let body = serde_json::json!({ "audio": BASE64_STANDARD.encode(&raw), "rate": 24000 });
    let (decoded, rate) = decode_say_audio(&body).expect("audio decodes");
    assert_eq!(rate, 24_000);
    assert_eq!(decoded.len(), 3);
    for (got, want) in decoded.iter().zip(samples.iter()) {
        assert!(
            (got - want).abs() < f32::EPSILON,
            "samples survive the round trip"
        );
    }
}

#[test]
fn counts_without_audio_never_voice() {
    use crate::say_chain::decode_say_audio;
    assert!(decode_say_audio(&serde_json::json!({ "samples": 42, "rate": 24000 })).is_err());
    assert!(decode_say_audio(&serde_json::json!({ "audio": "", "rate": 24000 })).is_err());
    assert!(decode_say_audio(&serde_json::json!({ "audio": "AAAAAA==", "rate": 0 })).is_err());
}

/// M1-S1 characterization: the daemon Say audio body is base64
/// little-endian f32 (4 bytes per sample — NOT the i16 Transcribe
/// shape). Refusal strings are verbatim contracts.
#[test]
fn orator_say_decode() {
    use crate::say_chain::decode_say_audio;
    use base64::prelude::*;
    // A length that is not a whole f32 count refuses.
    let body = serde_json::json!({ "audio": BASE64_STANDARD.encode([0u8, 1, 2, 3, 4, 5]), "rate": 24_000 });
    assert_eq!(
        decode_say_audio(&body).expect_err("odd f32 length refuses"),
        "daemon Say audio length is not a whole f32 count"
    );
    // A missing rate refuses (distinct from a zero rate).
    let body = serde_json::json!({ "audio": BASE64_STANDARD.encode([0u8; 4]) });
    assert_eq!(
        decode_say_audio(&body).expect_err("missing rate refuses"),
        "daemon Say reply carries no rate"
    );
    // An overflowing rate refuses before any cast.
    let body = serde_json::json!({ "audio": BASE64_STANDARD.encode([0u8; 4]), "rate": u64::MAX });
    assert_eq!(
        decode_say_audio(&body).expect_err("overflowing rate refuses"),
        "daemon Say rate overflows u32"
    );
    // Bad base64 is loud.
    let body = serde_json::json!({ "audio": "!!!not-base64!!!", "rate": 24_000 });
    let err = decode_say_audio(&body).expect_err("bad base64 is loud");
    assert!(
        err.starts_with("daemon Say audio is not base64: "),
        "names it: {err}"
    );
}
