//! Live decode: real model, real inference. Skips cleanly without the
//! model file or an explicit opt-in — never fails a hermetic gate.

use std::path::PathBuf;

use fluent_concurrency::stream::StreamAbort;
use orator_models::{whisper_entries, ModelKind, ModelManifest};

/// Decode one second of silence with the pinned tiny model. Silence must
/// produce no committed words (hallucination precision), and the call must
/// complete. Requires the model at the manifest path; otherwise skips.
#[ignore = "needs a real whisper model; run via the live-ai target"]
#[test]
fn live_tiny_decodes_silence_to_nothing() {
    let manifest = ModelManifest {
        entries: whisper_entries(),
    };
    let tiny = manifest
        .find(ModelKind::Whisper, "tiny")
        .expect("tiny listed");
    if !tiny.pinned() {
        eprintln!("live: tiny unpinned, skipping");
        return;
    }
    let path: PathBuf = orator_core::models_dir()
        .expect("managed dir resolves")
        .join(&tiny.file);
    if !path.exists() {
        eprintln!("live: model missing at {}, skipping", path.display());
        return;
    }
    let backend = orator_stt::WhisperBackend::new(path, "en");
    let abort = StreamAbort::new();
    let silence = vec![0.0f32; 16_000];
    let segments = backend.decode_blocking(&silence, &abort).expect("live decode");
    let words: usize = segments.iter().map(|segment| segment.text.split_whitespace().count()).sum();
    assert_eq!(words, 0, "silence must decode to no words");
}
