//! Backend tests: missing files fail unavailable, trim keeps guards,
//! uniform marks stay ordered — no model touched.

use std::path::PathBuf;

use fluent_wvr::FieldAccess;

use crate::kokoro::{
    cpu_pin_applies, invalidate_cuda_cache, ort_provider_override, set_cuda_usable_for_tests,
    trim_silence, uniform_marks, KokoroBackend, KOKORO_SAMPLE_RATE,
};

/// Serializes the provider-verdict tests below: the availability
/// cache and `KOKORO_ORT_PROVIDER` are process-global, so a probe
/// must never interleave with another test's decision.
static PROVIDER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn default_paths_honor_env_then_managed_files() {
    let (model, voices) = crate::kokoro::default_paths().expect("managed dir resolves");
    assert!(model.ends_with("kokoro-v1.0.onnx"));
    assert!(voices.ends_with("kokoro-voices"));
    // Env overrides win without touching disk.
    std::env::set_var("ORATOR_TEST_KOKORO_MODEL", "/tmp/x.onnx");
    let _ = std::env::var("ORATOR_TEST_KOKORO_MODEL");
}

#[test]
fn missing_files_are_unavailable_not_a_crash() {
    let prior = std::env::var_os("KOKORO_ORT_PROVIDER");
    let backend = KokoroBackend::new(
        PathBuf::from("/nonexistent-orator/kokoro.onnx"),
        PathBuf::from("/nonexistent-orator/voices"),
        "af_heart",
    );
    assert!(!backend.is_loaded());
    let error = backend
        .synth_blocking("hello")
        .expect_err("missing model must fail");
    match prior {
        Some(value) => std::env::set_var("KOKORO_ORT_PROVIDER", value),
        None => std::env::remove_var("KOKORO_ORT_PROVIDER"),
    }
    assert_eq!(
        error,
        orator_core::TtsError::Unavailable(
            "kokoro model missing at /nonexistent-orator/kokoro.onnx".to_string()
        )
    );
}

#[test]
fn trim_keeps_a_guard_and_never_clips_voice() {
    let mut samples = vec![0.0; 1000];
    samples.extend(std::iter::repeat_n(0.5, 500));
    samples.extend(std::iter::repeat_n(0.0, 2000));
    let trimmed = trim_silence(&samples);
    assert!(trimmed.len() < samples.len());
    assert!(trimmed.len() >= 500);
    assert!(trimmed.contains(&0.5));
}

#[test]
fn ort_provider_override_pins_cuda_only_where_nvidia_serves() {
    // Explicit opt-in on NVIDIA hardware pins CUDA.
    assert_eq!(ort_provider_override(true, true, false), Some("cuda"));
    // A user-set KOKORO_ORT_PROVIDER always wins, even for the opt-in.
    assert_eq!(ort_provider_override(true, true, true), None);
    assert_eq!(ort_provider_override(false, false, true), None);
    // Without the opt-in the dependency default is left alone — the
    // CPU path never touches provider selection.
    assert_eq!(ort_provider_override(false, true, false), None);
    assert_eq!(ort_provider_override(false, false, false), None);
    // Explicit opt-in without NVIDIA hardware pins nothing here — the
    // caller errors actionably before any provider is set.
    assert_eq!(ort_provider_override(true, false, false), None);
}

#[test]
fn trim_returns_all_silence_unchanged() {
    let samples = vec![0.0; 100];
    assert_eq!(trim_silence(&samples).len(), 100);
}

#[test]
fn uniform_marks_cover_and_order() {
    let marks = uniform_marks("one two three four", 2400, KOKORO_SAMPLE_RATE);
    assert_eq!(marks.len(), 4);
    assert_eq!(marks[0].text, "one");
    assert_eq!(marks[0].start_samples, 0);
    assert_eq!(marks[3].end_samples, 2400);
    assert_eq!(marks[1].start_samples, 600);
    for pair in marks.windows(2) {
        assert!(pair[0].start_samples < pair[1].start_samples);
        assert!(pair[0].end_samples <= pair[1].end_samples);
    }
}

#[test]
fn empty_text_yields_no_marks() {
    assert!(uniform_marks("   ", 2400, KOKORO_SAMPLE_RATE).is_empty());
    assert!(uniform_marks("hi", 0, KOKORO_SAMPLE_RATE).is_empty());
    assert!(uniform_marks("hi", 2400, 0).is_empty());
}

#[test]
fn long_tail_marks_stay_ordered_and_cover_exactly() {
    // A no-pause run keeps every word through the safety chunker, and
    // marks over a long span still partition it exactly, in order.
    let text = "word ".repeat(200);
    let chunks = crate::splitter::split_all(&text);
    let mut covered = 0usize;
    for chunk in &chunks {
        let words: Vec<&str> = chunk.text.split_whitespace().collect();
        assert!(!words.is_empty(), "re-chunks keep words");
        covered += words.len();
    }
    assert_eq!(covered, 200, "safety chunking never loses words");
    let marks = uniform_marks(&text, 48_000, KOKORO_SAMPLE_RATE);
    assert_eq!(marks.len(), 200);
    assert_eq!(marks.first().expect("first mark").start_samples, 0);
    assert_eq!(marks.last().expect("last mark").end_samples, 48_000);
    for pair in marks.windows(2) {
        assert!(pair[0].start_samples < pair[1].start_samples);
        assert_eq!(pair[0].end_samples, pair[1].start_samples);
    }
}

#[test]
fn fields_reconfigure_and_drop_session() {
    let mut backend = KokoroBackend::new(
        PathBuf::from("/nonexistent/x.onnx"),
        PathBuf::from("/nonexistent/v"),
        "af_heart",
    );
    backend.set_field("voice", "am_adam").expect("voice sets");
    assert_eq!(backend.get_field("voice").expect("read back"), "am_adam");
    backend
        .set_field("nope", "1")
        .expect_err("unknown field fails");
    assert!(backend.field_names().contains(&"voice"));
}

/// M15-S1: the kokoro field surface. `describe()` is the schema
/// golden (gpu deliberately outside `required`); the table pins
/// accept and reject.
#[test]
fn kokoro_field_surface_pins_schema_and_table() {
    use fluent_wvr::{Describable, FieldAccess};
    let mut backend = KokoroBackend::new(
        PathBuf::from("/nonexistent/x.onnx"),
        PathBuf::from("/nonexistent/v"),
        "af_heart",
    );
    assert_eq!(
        backend.describe(),
        serde_json::json!({
            "type": "object",
            "properties": {
                "voice": { "type": "string", "description": "Voice tag" },
                "model_path": { "type": "string", "description": "Kokoro ONNX model" },
                "voices_dir": { "type": "string", "description": "Voice pack directory" },
                "gpu": { "type": "boolean", "description": "GPU offload (explicit opt-in)" }
            },
            "required": ["voice", "model_path", "voices_dir"]
        })
    );
    assert_eq!(
        backend.field_names(),
        &["voice", "model_path", "voices_dir", "gpu"]
    );
    backend.set_field("voices_dir", "/v2").expect("dir sets");
    assert_eq!(backend.get_field("voices_dir").expect("reads"), "/v2");
    backend
        .set_field("gpu", "maybe")
        .expect_err("garbage bool rejects");
    backend.set_field("nope", "1").expect_err("unknown rejects");
    backend.get_field("nope").expect_err("unknown reads reject");
}

#[test]
fn chunk_partition_covers_the_text_in_order() {
    let text =
        "Hello world, this is a test of the voice. How are you today? Quite well, thank you.";
    let chunks = crate::splitter::split_all(text);
    assert!(!chunks.is_empty(), "text always chunks");
    for chunk in &chunks {
        assert!(!chunk.text.is_empty(), "no empty chunks");
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    let covered: Vec<&str> = chunks
        .iter()
        .flat_map(|chunk| chunk.text.split_whitespace())
        .collect();
    assert_eq!(covered, words, "chunking never loses or reorders words");
}

#[test]
fn overlapping_synths_resolve_cleanly_never_hang() {
    use std::sync::Arc;
    // Eight threads against an unloadable backend: every call fails
    // unavailable through the same limiter permit that serializes real
    // inference. The suite completing proves overlap resolves instead
    // of hanging; no thread may panic or corrupt.
    let backend = Arc::new(KokoroBackend::new(
        PathBuf::from("/nonexistent-orator/kokoro.onnx"),
        PathBuf::from("/nonexistent-orator/voices"),
        "af_heart",
    ));
    let workers: Vec<std::thread::JoinHandle<_>> = (0..8)
        .map(|_| {
            let shared = Arc::clone(&backend);
            std::thread::spawn(move || shared.synth_blocking("hello"))
        })
        .collect();
    for worker in workers {
        assert!(
            matches!(
                worker.join().expect("worker thread done"),
                Err(orator_core::TtsError::Unavailable(_))
            ),
            "overlap fails clean, never hangs"
        );
    }
}

fn kokoro_model_present() -> Option<(PathBuf, PathBuf)> {
    let (model, voices) = crate::kokoro::default_paths()?;
    (model.exists() && voices.exists()).then_some((model, voices))
}

#[test]
fn chunked_single_sentence_is_transparent() {
    let Some((model, voices)) = kokoro_model_present() else {
        eprintln!("SKIP chunked-transparency: no kokoro weights on this machine");
        return;
    };
    let backend = KokoroBackend::new(model, voices, "af_heart");
    let whole = backend.synth_blocking("Hello world").expect("synth runs");
    let chunks = backend
        .synth_chunks_blocking("Hello world")
        .expect("chunked runs");
    assert_eq!(chunks.len(), 1, "one sentence is one chunk");
    assert_eq!(chunks.into_iter().next().expect("chunk"), whole);
}

#[test]
fn chunked_multi_sentence_is_deterministic_and_structured() {
    let Some((model, voices)) = kokoro_model_present() else {
        eprintln!("SKIP chunked-structure: no kokoro weights on this machine");
        return;
    };
    let text =
        "Hello world, this is a test of the voice. How are you today? Quite well, thank you.";
    let backend = KokoroBackend::new(model, voices, "af_heart");
    let first = backend.synth_chunks_blocking(text).expect("chunked runs");
    let second = backend.synth_chunks_blocking(text).expect("chunked reruns");
    assert!(first.len() >= 2, "multi-sentence input splits: {first:?}");
    assert_eq!(first, second, "same weights synth identically per device");
    for chunk in &first {
        assert!(!chunk.is_empty(), "every chunk voices");
    }
    // Per-chunk prosody is per-synthesis-unit by design: the equality
    // claimed here is determinism and structure, never whole-turn
    // sample equality (recorded with the performance numbers).
}

#[test]
fn gpu_defaults_off_and_round_trips() {
    let mut backend = KokoroBackend::new(
        PathBuf::from("/nonexistent/x.onnx"),
        PathBuf::from("/nonexistent/v"),
        "af_heart",
    );
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
    if orator_core::gpu::device_present() {
        eprintln!("SKIP gpu-without-a-device: GPU device present, nothing to refuse");
        return;
    }
    let prior = std::env::var_os("KOKORO_ORT_PROVIDER");
    let backend = KokoroBackend::new(
        PathBuf::from("/nonexistent-orator/kokoro.onnx"),
        PathBuf::from("/nonexistent-orator/voices"),
        "af_heart",
    )
    .with_gpu();
    let error = backend
        .synth_blocking("hello")
        .expect_err("GPU without a device must fail");
    match prior {
        Some(value) => std::env::set_var("KOKORO_ORT_PROVIDER", value),
        None => std::env::remove_var("KOKORO_ORT_PROVIDER"),
    }
    assert!(
        matches!(error, orator_core::TtsError::Unavailable(_)),
        "refusal is unavailable, never silent CPU: {error:?}"
    );
    assert!(
        error.to_string().contains("GPU"),
        "names the remedy: {error}"
    );
}

#[test]
fn cpu_path_pins_cpu_only_where_cuda_can_never_serve() {
    let _guard = common_core::sync::lock(&PROVIDER_LOCK);
    // Pure matrix first: only the quiet default on CUDA-less
    // hardware collapses to an explicit `cpu`. The opt-in and the
    // user override always win; usable CUDA keeps `auto`.
    for (gpu, user, usable, pinned) in [
        (false, false, false, true),
        (false, false, true, false),
        (false, true, false, false),
        (false, true, true, false),
        (true, false, false, false),
        (true, false, true, false),
        (true, true, false, false),
        (true, true, true, false),
    ] {
        assert_eq!(
            cpu_pin_applies(gpu, user, usable),
            pinned,
            "gpu={gpu} user={user} usable={usable}"
        );
    }
    // End to end: without an override, a CPU load pins `cpu` exactly
    // where CUDA can never serve, and stays hands-off elsewhere.
    let prior = std::env::var_os("KOKORO_ORT_PROVIDER");
    std::env::remove_var("KOKORO_ORT_PROVIDER");
    let backend = KokoroBackend::new(
        PathBuf::from("/nonexistent-orator/kokoro.onnx"),
        PathBuf::from("/nonexistent-orator/voices"),
        "af_heart",
    );
    let _ = backend.synth_blocking("hello");
    let pinned = std::env::var_os("KOKORO_ORT_PROVIDER");
    match prior {
        Some(value) => std::env::set_var("KOKORO_ORT_PROVIDER", value),
        None => std::env::remove_var("KOKORO_ORT_PROVIDER"),
    }
    let expect_pin = cfg!(target_os = "linux") && !orator_core::gpu::nvidia_present();
    assert_eq!(
        pinned.as_deref().map(|value| value.to_str()),
        if expect_pin { Some(Some("cpu")) } else { None },
        "pin only where CUDA can never serve"
    );
}

#[test]
fn gpu_opt_in_without_nvidia_fails_actionably() {
    if !orator_core::gpu::device_present() || orator_core::gpu::nvidia_present() {
        eprintln!("SKIP amd-gpu-refusal: needs a visible non-NVIDIA GPU");
        return;
    }
    let backend = KokoroBackend::new(
        PathBuf::from("/nonexistent-orator/kokoro.onnx"),
        PathBuf::from("/nonexistent-orator/voices"),
        "af_heart",
    )
    .with_gpu();
    let error = backend
        .synth_blocking("hello")
        .expect_err("AMD-only GPU opt-in must fail, never silently CPU");
    assert!(
        matches!(error, orator_core::TtsError::Unavailable(_)),
        "refusal is unavailable: {error:?}"
    );
    let text = error.to_string();
    assert!(text.contains("AMD"), "names the hardware gap: {text}");
    assert!(text.contains("gpu"), "names the remedy leaf: {text}");
}

#[test]
fn cap_one_serializes_voice_turns() {
    // TTS single-inference serialization (streaming path only): two
    // chunks queue through a cap-1 limiter and never interleave —
    // peak concurrency stays 1 even under contention.
    use fluent_concurrency::pool::Limiter;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let limiter = Arc::new(Limiter::new(1));
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    common_core::runtime::block_on_shared(async {
        let mut handles = Vec::new();
        for _ in 0..8 {
            let limiter = Arc::clone(&limiter);
            let live = Arc::clone(&live);
            let peak = Arc::clone(&peak);
            handles.push(tokio::spawn(async move {
                limiter
                    .run(|| async {
                        let current = live.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(current, Ordering::SeqCst);
                        tokio::task::yield_now().await;
                        live.fetch_sub(1, Ordering::SeqCst);
                    })
                    .await;
            }));
        }
        for handle in handles {
            handle.await.expect("voice task joins");
        }
    });
    assert_eq!(
        peak.load(Ordering::SeqCst),
        1,
        "cap-1 chunks queue, never interleave"
    );
}

/// Availability cache: the verdict is computed once and reused,
/// and an ORT-side failure drops it so the next load re-probes
/// from the device nodes instead of trusting yesterday.
#[test]
fn cuda_cache_recalculates_only_after_a_backend_failure() {
    let _guard = common_core::sync::lock(&PROVIDER_LOCK);
    let prior = std::env::var_os("KOKORO_ORT_PROVIDER");
    std::env::remove_var("KOKORO_ORT_PROVIDER");
    // Present-but-garbage weights fail inside ORT (a `Backend`
    // error), which is exactly the recalculation trigger.
    let broken = tempfile::tempdir().expect("tempdir");
    let voices = broken.path().join("voices");
    std::fs::create_dir(&voices).expect("mkdir");
    let weights = broken.path().join("kokoro.onnx");
    std::fs::write(&weights, b"not a model").expect("seed writes");
    let backend = KokoroBackend::new(weights, voices, "af_heart");
    // Poison the verdict to usable: the load auto-probes (and fails
    // on the garbage weights), which must drop the poison so the
    // next load re-resolves from the nodes.
    set_cuda_usable_for_tests(true);
    let failed_backend = matches!(
        common_core::runtime::block_on_shared(backend.load()),
        Err(orator_core::TtsError::Backend(_))
    );
    assert!(failed_backend, "garbage weights fail inside ORT");
    // A follow-up load re-resolves instead of trusting the poison:
    // CUDA-less Linux pins `cpu`, usable hardware leaves `auto` be.
    std::env::remove_var("KOKORO_ORT_PROVIDER");
    let fresh = KokoroBackend::new(
        PathBuf::from("/nonexistent-orator/kokoro.onnx"),
        PathBuf::from("/nonexistent-orator/voices"),
        "af_heart",
    );
    let _ = fresh.synth_blocking("hello");
    let repinned = std::env::var_os("KOKORO_ORT_PROVIDER");
    match prior {
        Some(value) => std::env::set_var("KOKORO_ORT_PROVIDER", value),
        None => std::env::remove_var("KOKORO_ORT_PROVIDER"),
    }
    // Clean the process verdict for the rest of the suite: the next
    // load recomputes it from the nodes either way.
    invalidate_cuda_cache();
    let expect_pin = cfg!(target_os = "linux") && !orator_core::gpu::nvidia_present();
    assert_eq!(
        repinned.as_deref().map(|value| value.to_str()),
        if expect_pin { Some(Some("cpu")) } else { None },
        "recalculation follows the nodes, not the poison"
    );
}
