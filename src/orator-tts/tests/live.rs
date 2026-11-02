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
        orator_tts::KokoroBackend::new(PathBuf::from(model), PathBuf::from(voices), "af_heart");
    let samples = backend
        .synth_blocking("Hello world, this is a live voice check.")
        .expect("live synth completes");
    let first_audio = started.elapsed();
    assert!(!samples.is_empty(), "live synth must produce audio");
    assert!(
        samples.iter().any(|sample| sample.abs() > 0.02),
        "live synth must voice, not just pad"
    );
    eprintln!("live: first audio in {first_audio:?} for {} samples", samples.len());
}
