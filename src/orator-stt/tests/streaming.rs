//! Streaming tests: agreement commits, tails stay provisional, guards
//! hold, aborts surface — all against scripted lattices, no model.

use std::num::NonZeroU32;
use std::sync::Arc;

use fluent_concurrency::stream::StreamAbort;
use orator_core::{Channel, Frame, MediaTime, SttError, SttStream};

use crate::streaming::{BlockDecoder, DecodedWord, StreamingStt};

fn base() -> MediaTime {
    MediaTime::zero(NonZeroU32::new(16_000).expect("test rate"))
}

fn frame() -> Frame {
    Frame::new(Channel::Mic, vec![0.1; 320], 16_000).expect("valid rate builds")
}

fn word(text: &str, start_ms: u64, end_ms: u64) -> DecodedWord {
    DecodedWord {
        text: text.to_string(),
        start_ms,
        end_ms,
        no_speech: 0.1,
    }
}

/// Scripted decoder: cycles lattices per call, ignoring audio. The final
/// lattice repeats forever so long windows keep decoding it.
struct Script {
    lattices: Vec<Vec<DecodedWord>>,
    calls: std::sync::Mutex<usize>,
}

impl Script {
    fn new(lattices: Vec<Vec<DecodedWord>>) -> Self {
        Self {
            lattices,
            calls: std::sync::Mutex::new(0),
        }
    }
}

impl BlockDecoder for Script {
    fn decode(
        &self,
        _samples: &[f32],
        _offset_ms: u64,
        _abort: &StreamAbort,
    ) -> Result<Vec<DecodedWord>, SttError> {
        assert!(!self.lattices.is_empty(), "script needs a lattice");
        let mut calls = self.calls.lock().expect("test lock");
        let index = (*calls).min(self.lattices.len() - 1);
        *calls += 1;
        Ok(self.lattices[index].clone())
    }
}

fn stream(lattices: Vec<Vec<DecodedWord>>) -> StreamingStt {
    let mut stt = StreamingStt::new(Arc::new(Script::new(lattices)), Channel::Mic, base());
    stt.decode_every_frames = 1;
    stt
}

#[test]
fn agreed_prefix_commits_old_words_only() {
    let first = vec![word("hello", 0, 400), word("wor", 400, 2000)];
    let second = vec![word("hello", 0, 400), word("world", 400, 900)];
    let mut stt = stream(vec![first, second]);
    // Window is one frame (20 ms): nothing ends 1500 ms before the end.
    let updates = stt.push(&frame()).expect("decode");
    assert!(updates.iter().all(|update| update.provisional));
    // Grow the window past hello's commit horizon (400 + 1500 ms).
    let mut committed_texts = Vec::new();
    for _ in 0..100 {
        for update in stt.push(&frame()).expect("decode") {
            if !update.provisional {
                committed_texts.push(update.word.text.clone());
            }
        }
    }
    assert!(
        committed_texts.contains(&"hello".to_string()),
        "stable agreed word must commit: {committed_texts:?}"
    );
    assert!(
        !committed_texts.iter().any(|text| text == "wor"),
        "revised tail must never commit: {committed_texts:?}"
    );
}

#[test]
fn finalize_commits_the_remainder() {
    let lattice = vec![word("one", 0, 300), word("two", 300, 600)];
    let mut stt = stream(vec![lattice]);
    stt.push(&frame()).expect("decode");
    let finals = stt.finalize().expect("finalize");
    assert_eq!(finals.len(), 2);
    assert!(finals.iter().all(|update| !update.provisional));
}

#[test]
fn guards_reject_hallucinations() {
    let lattice = vec![
        word("[BLANK_AUDIO]", 0, 300),
        DecodedWord {
            text: "real".to_string(),
            start_ms: 300,
            end_ms: 600,
            no_speech: 0.9,
        },
        word("ok", 600, 900),
        word("ok", 900, 1200),
        word("ok", 1200, 1500),
        word("ok", 1500, 1800),
        word("ok", 1800, 2100),
    ];
    let mut stt = stream(vec![lattice]);
    stt.push(&frame()).expect("decode");
    let finals = stt.finalize().expect("finalize");
    let texts: Vec<&str> = finals.iter().map(|update| update.word.text.as_str()).collect();
    assert!(!texts.iter().any(|text| text.contains("BLANK")), "{texts:?}");
    assert!(!texts.contains(&"real"), "{texts:?}");
    assert!(
        texts.iter().filter(|text| **text == "ok").count() <= 3,
        "{texts:?}"
    );
}

#[test]
fn pre_cancelled_abort_surfaces_without_decoding() {
    struct PanicDecoder;
    impl BlockDecoder for PanicDecoder {
        fn decode(
            &self,
            _samples: &[f32],
            _offset_ms: u64,
            _abort: &StreamAbort,
        ) -> Result<Vec<DecodedWord>, SttError> {
            panic!("cancelled push must never reach the decoder");
        }
    }
    let mut stt = StreamingStt::new(Arc::new(PanicDecoder), Channel::Mic, base());
    let abort = StreamAbort::new();
    abort.cancel();
    let error = stt
        .push_checked(&frame(), &abort)
        .expect_err("cancelled push must fail");
    assert_eq!(error, SttError::Aborted);
}

#[test]
fn timestamps_follow_the_utterance_base() {
    let lattice = vec![word("hi", 1000, 1400)];
    let mut stt = stream(vec![lattice]);
    stt.push(&frame()).expect("decode");
    let finals = stt.finalize().expect("finalize");
    assert_eq!(finals.len(), 1);
    let word = &finals[0].word;
    assert!((word.start.as_secs_f64() - 1.0).abs() < 1e-9);
    assert!((word.end.as_secs_f64() - 1.4).abs() < 1e-9);
    assert_eq!(word.channel, Channel::Mic);
}

#[test]
fn incremental_matches_full_decode_word_for_word() {
    let lattice = vec![
        word("hello", 0, 400),
        word("world", 400, 900),
        word("today", 900, 1400),
    ];
    // Incremental: decode every frame, committing as words age.
    let mut incremental = stream(vec![lattice.clone()]);
    let mut committed_texts = Vec::new();
    for _ in 0..200 {
        for update in incremental.push(&frame()).expect("decode") {
            if !update.provisional {
                committed_texts.push(update.word.text.clone());
            }
        }
    }
    assert_eq!(committed_texts, vec!["hello", "world", "today"]);
    // Eviction fired: the window holds only the tail, and committed
    // audio left the buffer.
    assert!(incremental.evicted_samples() > 0, "commits must evict");
    assert!(
        (incremental.window_samples() as u64) < 200 * 320,
        "window stops growing with utterance length"
    );
    // Full: one decode of the whole utterance, then finalize.
    let mut full = StreamingStt::new(Arc::new(Script::new(vec![lattice])), Channel::Mic, base());
    full.decode_every_frames = 10_000;
    for _ in 0..200 {
        full.push(&frame()).expect("buffer");
    }
    let finals: Vec<String> = full
        .finalize()
        .expect("finalize")
        .into_iter()
        .map(|update| update.word.text)
        .collect();
    assert_eq!(finals, committed_texts, "incremental == full decode");
}

#[test]
fn revised_every_pass_never_commits() {
    // A word that never survives two identical passes stays
    // provisional no matter how old it gets.
    struct FlipFlop {
        calls: std::sync::Mutex<usize>,
    }
    impl BlockDecoder for FlipFlop {
        fn decode(
            &self,
            _samples: &[f32],
            _offset_ms: u64,
            _abort: &StreamAbort,
        ) -> Result<Vec<DecodedWord>, SttError> {
            let mut calls = self.calls.lock().expect("test lock");
            *calls += 1;
            let text = if *calls % 2 == 0 { "alpha" } else { "beta" };
            Ok(vec![word(text, 0, 400)])
        }
    }
    let mut stt = StreamingStt::new(Arc::new(FlipFlop { calls: std::sync::Mutex::new(0) }), Channel::Mic, base());
    stt.decode_every_frames = 1;
    for _ in 0..100 {
        for update in stt.push(&frame()).expect("decode") {
            assert!(
                update.provisional,
                "an ever-revised word must never commit"
            );
        }
    }
    assert_eq!(stt.committed(), 0);
}

#[test]
fn silence_and_noise_commit_nothing() {
    struct Noisy {
        calls: std::sync::Mutex<usize>,
    }
    impl BlockDecoder for Noisy {
        fn decode(
            &self,
            _samples: &[f32],
            _offset_ms: u64,
            _abort: &StreamAbort,
        ) -> Result<Vec<DecodedWord>, SttError> {
            let mut calls = self.calls.lock().expect("test lock");
            *calls += 1;
            if *calls % 2 == 0 {
                // High no-speech: the guards strip it before agreement.
                Ok(vec![DecodedWord {
                    text: "um".to_string(),
                    start_ms: 0,
                    end_ms: 400,
                    no_speech: 0.9,
                }])
            } else {
                Ok(Vec::new())
            }
        }
    }
    let mut stt = StreamingStt::new(Arc::new(Noisy { calls: std::sync::Mutex::new(0) }), Channel::Mic, base());
    stt.decode_every_frames = 1;
    for _ in 0..100 {
        for update in stt.push(&frame()).expect("decode") {
            assert!(update.provisional, "noise must never commit");
        }
    }
    let finals = stt.finalize().expect("finalize");
    assert!(finals.is_empty(), "noise finalizes to nothing: {finals:?}");
}

#[test]
fn fired_abort_mid_window_aborts_without_partial_commit() {
    let lattice = vec![word("hello", 0, 400), word("world", 400, 900)];
    let mut stt = stream(vec![lattice]);
    // Age hello to its commit horizon first.
    for _ in 0..100 {
        let _ = stt.push(&frame()).expect("decode");
    }
    assert_eq!(stt.committed(), 1);
    let abort = StreamAbort::new();
    abort.cancel();
    let error = stt
        .push_checked(&frame(), &abort)
        .expect_err("fired abort must fail");
    assert_eq!(error, SttError::Aborted);
    // No partial commit persisted through the abort.
    assert_eq!(stt.committed(), 1);
    // The stream resumes cleanly: no duplicate hello after re-arm.
    let mut seen = Vec::new();
    for _ in 0..60 {
        for update in stt.push(&frame()).expect("decode") {
            if !update.provisional {
                seen.push(update.word.text.clone());
            }
        }
    }
    assert!(!seen.contains(&"hello".to_string()), "no recommit: {seen:?}");
}
