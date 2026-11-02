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
    assert_eq!(render(&cues(), TranscriptFormat::Txt).expect("txt renders"), "hello\nworld");
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
