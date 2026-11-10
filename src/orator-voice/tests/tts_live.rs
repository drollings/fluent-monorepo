//! Live synth: real Kokoro weights, real inference. Skips cleanly without
//! the model files or an explicit opt-in — never fails a hermetic gate.

use std::path::PathBuf;

/// Synthesize one sentence with the real backend. Requires `KOKORO_MODEL`
/// (ONNX file) and `KOKORO_VOICES` (voice-pack dir); otherwise skips.
#[ignore = "needs real Kokoro weights; run via the live-ai target"]
#[test]
fn live_kokoro_synthesizes_a_sentence() {
    let model = std::env::var("KOKORO_MODEL").unwrap_or_default();
    let voices = std::env::var("KOKORO_VOICES").unwrap_or_default();
    if model.is_empty() || voices.is_empty() {
        eprintln!("live: KOKORO_MODEL/KOKORO_VOICES unset, skipping");
        return;
    }
    let started = std::time::Instant::now();
    let backend =
        orator_voice::tts::KokoroBackend::new(PathBuf::from(model), PathBuf::from(voices), "af_heart");
    let samples = backend
        .synth_blocking("Hello world, this is a live voice check.")
        .expect("live synth completes");
    let first_audio = started.elapsed();
    assert!(!samples.is_empty(), "live synth must produce audio");
    assert!(
        samples.iter().any(|sample| sample.abs() > 0.02),
        "live synth must voice, not just pad"
    );
    eprintln!(
        "live: first audio in {first_audio:?} for {} samples",
        samples.len()
    );
}

/// Chunked transparency with real weights: one sentence synthesizes as a
/// single chunk identical to the whole. Moved out of the hermetic gate —
/// real ONNX inference never runs in `make smoke`/`make test`.
#[ignore = "needs real Kokoro weights; run via the live-ai target"]
#[test]
fn live_chunked_single_sentence_is_transparent() {
    let Some((model, voices)) = live_weights() else {
        return;
    };
    let backend = orator_voice::tts::KokoroBackend::new(model, voices, "af_heart");
    let whole = backend.synth_blocking("Hello world").expect("synth runs");
    let chunks = backend
        .synth_chunks_blocking("Hello world")
        .expect("chunked runs");
    assert_eq!(chunks.len(), 1, "one sentence is one chunk");
    assert_eq!(chunks.into_iter().next().expect("chunk"), whole);
}

/// Chunked determinism with real weights: multi-sentence input splits and
/// re-synthesizes identically.
#[ignore = "needs real Kokoro weights; run via the live-ai target"]
#[test]
fn live_chunked_multi_sentence_is_deterministic_and_structured() {
    let Some((model, voices)) = live_weights() else {
        return;
    };
    let text =
        "Hello world, this is a test of the voice. How are you today? Quite well, thank you.";
    let backend = orator_voice::tts::KokoroBackend::new(model, voices, "af_heart");
    let first = backend.synth_chunks_blocking(text).expect("chunked runs");
    let second = backend.synth_chunks_blocking(text).expect("chunked reruns");
    assert!(first.len() >= 2, "multi-sentence input splits: {first:?}");
    assert_eq!(first, second, "same weights synth identically per device");
    for chunk in &first {
        assert!(!chunk.is_empty(), "every chunk voices");
    }
    // Per-chunk prosody is per-synthesis-unit by design: the equality
    // claimed here is determinism and structure, never whole-turn
    // sample equality.
}

/// Live weights from the env contract, skipping cleanly without them.
fn live_weights() -> Option<(PathBuf, PathBuf)> {
    let model = std::env::var("KOKORO_MODEL").unwrap_or_default();
    let voices = std::env::var("KOKORO_VOICES").unwrap_or_default();
    if model.is_empty() || voices.is_empty() {
        eprintln!("live: KOKORO_MODEL/KOKORO_VOICES unset, skipping");
        return None;
    }
    Some((PathBuf::from(model), PathBuf::from(voices)))
}
