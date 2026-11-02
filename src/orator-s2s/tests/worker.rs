//! Worker tests: stub-binary synthesis round-trips audio; missing
//! binaries, crashes, and stderr tails report actionably. The stub
//! stands in for `llama-tts` (which this stack drives with
//! `-m/-mm/-p/-o`).

use std::os::unix::fs::PermissionsExt as _;
use std::time::Duration;

use orator_core::AudioBundle;
use crate::worker::{resolve_tts_binary, synth_blocking, TtsWorkerError};

/// Stub `llama-tts`: copies the fixture WAV to its `-o` target (the
/// last argument). Writes nothing else, exits zero.
fn stub_script(fixture: &std::path::Path, dir: &std::path::Path) -> std::path::PathBuf {
    let script = dir.join("llama-tts");
    std::fs::write(&script, "#!/bin/sh\ncp \"$ORATOR_S2S_STUB_WAV\" \"$8\"\n")
        .expect("stub writes");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("stub is executable");
    let _ = fixture;
    script
}

fn fixture_wav(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("fixture.wav");
    let bytes = crate::protocol::encode_wav_pcm16(&[0.0, 0.5, -0.5, 0.25], 24_000);
    std::fs::write(&path, &bytes).expect("fixture writes");
    path
}

fn bundle() -> AudioBundle {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = |name: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, b"weights").expect("weights fixture");
        // Leak the dir so component files survive the call.
        path
    };
    let bundle = AudioBundle {
        backbone: file("backbone.gguf"),
        mmproj: file("mmproj.gguf"),
        vocoder: file("vocoder.gguf"),
        tokenizer: file("tokenizer.gguf"),
    };
    std::mem::forget(dir);
    bundle
}

#[test]
fn stub_synthesis_round_trips_samples() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = fixture_wav(dir.path());
    std::env::set_var("ORATOR_S2S_STUB_WAV", &fixture);
    let stub = stub_script(&fixture, dir.path());
    let (samples, rate) =
        synth_blocking(&stub, &bundle(), "hello", Duration::from_secs(10)).expect("stub runs");
    assert_eq!(rate, 24_000);
    assert_eq!(samples.len(), 4);
    assert!((samples[1] - 0.5).abs() < 0.01);
    std::env::remove_var("ORATOR_S2S_STUB_WAV");
}

#[test]
fn missing_binary_fails_actionably() {
    let missing = std::path::PathBuf::from("/nonexistent-orator/llama-tts");
    let error =
        synth_blocking(&missing, &bundle(), "hi", Duration::from_secs(5)).expect_err("must fail");
    assert!(matches!(error, TtsWorkerError::NoBinary(_)), "{error:?}");
}

#[test]
fn crashing_binary_reports_its_exit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("llama-tts");
    std::fs::write(&script, "#!/bin/sh\nexit 3\n").expect("stub writes");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("executable");
    let error =
        synth_blocking(&script, &bundle(), "hi", Duration::from_secs(10)).expect_err("must fail");
    assert_eq!(error, TtsWorkerError::Failed("exit 3".to_string()));
}

#[test]
fn failing_binary_reports_its_stderr_tail() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("llama-tts");
    std::fs::write(
        &script,
        "#!/bin/sh\necho 'mmproj does not support audio generation' >&2\nexit 1\n",
    )
    .expect("stub writes");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("executable");
    let error =
        synth_blocking(&script, &bundle(), "hi", Duration::from_secs(10)).expect_err("must fail");
    let message = error.to_string();
    assert!(message.contains("exit 1"), "{message}");
    assert!(
        message.contains("mmproj does not support audio generation"),
        "{message}"
    );
}

#[test]
fn binary_resolution_prefers_the_override() {
    std::env::set_var("LLAMA_TTS", "/opt/llama/bin/llama-tts");
    assert_eq!(
        resolve_tts_binary(),
        Some(std::path::PathBuf::from("/opt/llama/bin/llama-tts"))
    );
    std::env::remove_var("LLAMA_TTS");
    let _ = resolve_tts_binary();
}

#[test]
fn missing_binary_and_bundle_fail_before_spawn() {
    let bundle = bundle();
    let missing = std::path::Path::new("/nonexistent/llama-tts");
    let error = synth_blocking(missing, &bundle, "hi", Duration::from_secs(5))
        .expect_err("missing binary refuses");
    assert!(
        matches!(error, TtsWorkerError::NoBinary(_)),
        "names the remedy: {error}"
    );
}

#[test]
fn non_executable_binary_refuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let binary = dir.path().join("llama-tts");
    std::fs::write(&binary, "#!/bin/sh\nexit 0\n").expect("stub writes");
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o644))
        .expect("non-executable");
    let bundle = bundle();
    let error = synth_blocking(&binary, &bundle, "hi", Duration::from_secs(5))
        .expect_err("non-executable refuses");
    assert!(
        matches!(error, TtsWorkerError::NoBinary(_)),
        "names the remedy: {error}"
    );
}

#[test]
fn scratch_guard_deletes_on_drop() {
    use crate::worker::ScratchGuard;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("orator-tts-drop.wav");
    std::fs::write(&path, b"scratch").expect("scratch writes");
    {
        let _guard = ScratchGuard::new(path.clone());
    }
    assert!(!path.exists(), "drop reaps the scratch file");
}
