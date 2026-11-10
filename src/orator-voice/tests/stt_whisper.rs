//! Backend tests: unloaded backends fail cleanly, word splitting is
//! uniform, fields reconfigure. No model file is touched.

use std::path::PathBuf;

use fluent_concurrency::stream::StreamAbort;
use fluent_wvr::FieldAccess;

use crate::stt::whisper::{split_segment_words, RawSegment, WhisperBackend};

#[test]
fn missing_model_is_unavailable_not_a_crash() {
    let backend = WhisperBackend::new(PathBuf::from("/nonexistent-orator/ggml-tiny.bin"), "en");
    assert!(!backend.is_loaded());
    let abort = StreamAbort::new();
    let error = backend
        .decode_blocking(&vec![0.0; 320], &abort)
        .expect_err("missing model must fail");
    assert_eq!(
        error,
        orator::SttError::Unavailable(
            "whisper model missing at /nonexistent-orator/ggml-tiny.bin".to_string()
        )
    );
}

#[test]
fn segments_split_into_uniform_words() {
    let segment = RawSegment {
        text: "one two three four".to_string(),
        start_cs: 0,
        end_cs: 80,
        no_speech: 0.1,
    };
    let words = split_segment_words(&segment);
    assert_eq!(words.len(), 4);
    assert_eq!(words[0].text, "one");
    assert_eq!(words[0].start_ms, 0);
    assert_eq!(words[3].end_ms, 800);
    // Uniform span: each word owns a quarter of 800 ms.
    assert_eq!(words[1].start_ms, 200);
    assert!(words
        .iter()
        .all(|word| (word.no_speech - 0.1).abs() < f32::EPSILON));
}

#[test]
fn empty_segments_yield_no_words() {
    let segment = RawSegment {
        text: "   ".to_string(),
        start_cs: 0,
        end_cs: 10,
        no_speech: 0.0,
    };
    assert!(split_segment_words(&segment).is_empty());
}

#[test]
fn fields_reconfigure_and_drop_context() {
    let mut backend = WhisperBackend::new(PathBuf::from("/nonexistent/x.bin"), "en");
    backend.set_field("language", "de").expect("language sets");
    assert_eq!(backend.get_field("language").expect("read back"), "de");
    backend
        .set_field("nope", "1")
        .expect_err("unknown field fails");
    assert!(backend.field_names().contains(&"language"));
}

/// M15-S1: the whisper field surface. `describe()` is the schema
/// golden (gpu deliberately outside `required`); the table pins
/// accept and reject.
#[test]
fn whisper_field_surface_pins_schema_and_table() {
    use fluent_wvr::{Describable, FieldAccess};
    let mut backend = WhisperBackend::new(PathBuf::from("/nonexistent/x.bin"), "en");
    assert_eq!(
        backend.describe(),
        serde_json::json!({
            "type": "object",
            "properties": {
                "language": { "type": "string", "description": "Recognition language" },
                "model_path": { "type": "string", "description": "Whisper model file" },
                "gpu": { "type": "boolean", "description": "GPU offload (explicit opt-in)" }
            },
            "required": ["language", "model_path"]
        })
    );
    assert_eq!(backend.field_names(), &["language", "model_path", "gpu"]);
    backend
        .set_field("model_path", "/m.gguf")
        .expect("path sets");
    assert_eq!(
        backend.get_field("model_path").expect("path reads"),
        "/m.gguf"
    );
    backend
        .set_field("gpu", "maybe")
        .expect_err("garbage bool rejects");
    backend.get_field("nope").expect_err("unknown reads reject");
}

#[test]
fn gpu_defaults_off_and_round_trips() {
    let mut backend = WhisperBackend::new(PathBuf::from("/nonexistent/x.bin"), "en");
    assert_eq!(backend.get_field("gpu").expect("readable"), "false");
    backend.set_field("gpu", "true").expect("true parses");
    assert_eq!(backend.get_field("gpu").expect("read back"), "true");
    backend
        .set_field("gpu", "maybe")
        .expect_err("garbage must fail");
    assert!(backend.field_names().contains(&"gpu"));
}

#[test]
fn gpu_without_a_device_fails_actionably() {
    if orator::gpu::device_present() {
        eprintln!("SKIP gpu-without-a-device: GPU device present, nothing to refuse");
        return;
    }
    let backend =
        WhisperBackend::new(PathBuf::from("/nonexistent-orator/ggml-tiny.bin"), "en").with_gpu();
    let abort = StreamAbort::new();
    let error = backend
        .decode_blocking(&vec![0.0; 320], &abort)
        .expect_err("GPU without a device must fail");
    assert!(
        matches!(error, orator::SttError::Unavailable(_)),
        "refusal is unavailable, never silent CPU: {error:?}"
    );
    assert!(
        error.to_string().contains("GPU"),
        "names the remedy: {error}"
    );
}

#[test]
fn cpu_gpu_decode_equality_skips_without_device_or_model() {
    if !orator::gpu::device_present() {
        eprintln!("SKIP cpu-gpu-equality: no GPU device on this machine");
        return;
    }
    let Some(dir) = orator::models_dir() else {
        eprintln!("SKIP cpu-gpu-equality: no managed model directory");
        return;
    };
    let path = dir.join("ggml-large-v3-turbo-q5_0.bin");
    if !path.exists() {
        eprintln!("SKIP cpu-gpu-equality: test model absent");
        return;
    }
    let abort = StreamAbort::new();
    let samples = vec![0.0; 16_000];
    let cpu = WhisperBackend::new(path.clone(), "en")
        .decode_blocking(&samples, &abort)
        .expect("CPU decodes");
    let gpu = WhisperBackend::new(path, "en")
        .with_gpu()
        .decode_blocking(&samples, &abort)
        .expect("GPU decodes");
    assert_eq!(cpu, gpu, "same weights decode identically per device");
}
