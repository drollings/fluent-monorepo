//! Live decode: real model, real inference. Skips cleanly without the
//! model file or an explicit opt-in — never fails a hermetic gate.

use std::path::PathBuf;

use fluent_concurrency::stream::StreamAbort;
use orator_models::{whisper_entries, ModelKind, ModelManifest};

/// Moonshine medium decodes real weights end to end: the default ASR
/// tag resolves through the models table, the backend loads the
/// GGUF, and a one-second tone completes inside the local-op
/// budget. No content assertion on non-speech (tones are not words);
/// the transcript prints for the operator. Skips cleanly without
/// the weights file — never fails a hermetic gate.
#[ignore = "needs real moonshine weights; run via the live-ai target"]
#[test]
fn live_moonshine_medium_decodes_tone_to_completion() {
    let config = orator_core::OratorConfig::default();
    let path = orator_core::resolve_model_weights(
        &config.models,
        &config.gguf_dir,
        orator_core::DEFAULT_ASR_MODEL,
    );
    let Some(path) = path.filter(|path| path.exists()) else {
        eprintln!("live: moonshine weights missing, skipping");
        return;
    };
    let backend = orator_stt::MoonshineBackend::new(&path);
    let abort = StreamAbort::new();
    let tone: Vec<f32> = (0..16_000)
        .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16_000.0).sin() * 0.3)
        .collect();
    let started = std::time::Instant::now();
    let segments = backend.decode_blocking(&tone, &abort).expect("live decode");
    let elapsed = started.elapsed();
    let text: String = segments
        .iter()
        .map(|segment| segment.text.clone())
        .collect();
    eprintln!("live: moonshine decoded 1 s tone in {elapsed:?}: {text:?}");
    assert!(
        elapsed.as_secs() < 10,
        "local decode stays inside the 10 s op budget: {elapsed:?}"
    );
}

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
    let segments = backend
        .decode_blocking(&silence, &abort)
        .expect("live decode");
    let words: usize = segments
        .iter()
        .map(|segment| segment.text.split_whitespace().count())
        .sum();
    assert_eq!(words, 0, "silence must decode to no words");
}

/// WER per tier over the checked-in corpus. For every pinned whisper
/// tier present on disk and every `tests/corpus/name.{wav,txt}` pair,
/// decodes and scores; the table prints per tier. No budgets gate —
/// budgets land in the calibration register once measured on real
/// audio. Skips cleanly without models or corpus (never fails a
/// hermetic gate; run via the live-ai target).
#[ignore = "needs real whisper models + corpus; run via the live-ai target"]
#[test]
fn live_wer_per_tier() {
    use std::collections::BTreeMap;

    let entries = whisper_entries();
    let Some(managed) = orator_core::models_dir() else {
        eprintln!("live: no managed dir, skipping");
        return;
    };
    let mut tiers = Vec::new();
    for entry in &entries {
        if entry.kind == ModelKind::Whisper && entry.pinned() && managed.join(&entry.file).exists()
        {
            tiers.push(entry.clone());
        }
    }
    if tiers.is_empty() {
        eprintln!("live: no whisper models on disk, skipping");
        return;
    }
    let corpus_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut pairs: Vec<(std::path::PathBuf, String)> = Vec::new();
    if let Ok(dir) = std::fs::read_dir(&corpus_dir) {
        for item in dir.filter_map(Result::ok) {
            let wav = item.path();
            if wav.extension().is_some_and(|ext| ext == "wav") {
                let label = wav.with_extension("txt");
                if let Ok(text) = std::fs::read_to_string(&label) {
                    pairs.push((wav, text));
                }
            }
        }
    }
    pairs.sort();
    if pairs.is_empty() {
        eprintln!("live: corpus empty (see tests/corpus/README.md), skipping");
        return;
    }
    let mut table: BTreeMap<String, Vec<(String, f64)>> = BTreeMap::new();
    for tier in &tiers {
        let backend = orator_stt::WhisperBackend::new(managed.join(&tier.file), "en");
        let abort = StreamAbort::new();
        for (wav, reference) in &pairs {
            let pcm = orator_audio::read_wav_mono(wav).expect("corpus wav reads");
            let samples = if pcm.sample_rate == 16_000 {
                pcm.samples.clone()
            } else {
                orator_audio::resample_linear(&pcm.samples, pcm.sample_rate, 16_000)
            };
            let segments = backend
                .decode_blocking(&samples, &abort)
                .expect("live decode");
            let hypothesis = segments
                .iter()
                .map(|segment| segment.text.clone())
                .collect::<Vec<_>>()
                .join(" ");
            let score = orator_stt::wer(reference, &hypothesis);
            assert!(score.is_finite(), "WER must score finite");
            table
                .entry(tier.name.clone())
                .or_default()
                .push((wav.display().to_string(), score));
        }
    }
    for (tier, rows) in &table {
        for (file, score) in rows {
            eprintln!("live wer: tier={tier} file={file} wer={score:.3}");
        }
    }
    assert!(!table.is_empty(), "measured at least one tier");
}
