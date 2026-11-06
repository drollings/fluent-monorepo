//! Worker tests: stub-binary synthesis round-trips audio; missing
//! binaries, crashes, and stderr tails report actionably. The stub
//! stands in for `llama-tts` (which this stack drives with
//! `-m/-mm/-p/-o`).

use std::os::unix::fs::PermissionsExt as _;
use std::time::Duration;

use crate::worker::{
    resolve_tts_binary, synth_blocking, TtsWorkerError, LLAMA_TTS_BIN, LLAMA_TTS_ENV,
};
use orator_core::AudioBundle;

use crate::ENV_LOCK;

/// Stub `llama-tts`: copies the fixture WAV to its `-o` target (the
/// last argument). Writes nothing else, exits zero. The fixture path
/// is baked into the script so the scrubbed child env needs no
/// pass-through.
fn stub_script(fixture: &std::path::Path, dir: &std::path::Path) -> std::path::PathBuf {
    let script = dir.join("llama-tts");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ncp '{}' \"$8\"\n", fixture.display()),
    )
    .expect("stub writes");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("stub is executable");
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
    let stub = stub_script(&fixture, dir.path());
    let (samples, rate) =
        synth_blocking(&stub, &bundle(), "hello", Duration::from_secs(10)).expect("stub runs");
    assert_eq!(rate, 24_000);
    assert_eq!(samples.len(), 4);
    assert!((samples[1] - 0.5).abs() < 0.01);
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
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("executable");
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
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("executable");
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
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior = std::env::var_os(LLAMA_TTS_ENV);
    std::env::set_var(LLAMA_TTS_ENV, "/opt/llama/bin/llama-tts");
    assert_eq!(
        resolve_tts_binary(),
        Some(std::path::PathBuf::from("/opt/llama/bin/llama-tts"))
    );
    match prior {
        Some(value) => std::env::set_var(LLAMA_TTS_ENV, value),
        None => std::env::remove_var(LLAMA_TTS_ENV),
    }
    let _ = resolve_tts_binary();
}

/// M6-S1 characterization: a relative `LLAMA_TTS` override resolves to
/// nothing — pin before the shared-helper migration.
#[test]
fn relative_tts_override_resolves_to_nothing() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior = std::env::var_os(LLAMA_TTS_ENV);
    std::env::set_var(LLAMA_TTS_ENV, "relative/llama-tts");
    assert_eq!(resolve_tts_binary(), None, "relative explicit never serves");
    match prior {
        Some(value) => std::env::set_var(LLAMA_TTS_ENV, value),
        None => std::env::remove_var(LLAMA_TTS_ENV),
    }
}

/// Writable fixture base outside `/tmp`: the ownership gate refuses
/// anything under `/tmp`, so resolution fixtures live under the test
/// binary's own directory instead.
#[cfg(unix)]
fn fixture_dir() -> tempfile::TempDir {
    let base = std::env::current_exe()
        .expect("exe path")
        .parent()
        .expect("exe parent")
        .to_path_buf();
    tempfile::tempdir_in(base).expect("fixture dir")
}

/// M6-S1 characterization: a world-writable `llama-tts` `$PATH` hit is
/// refused (resolves to nothing) — pin before the shared-helper
/// migration.
#[test]
#[cfg(unix)]
fn world_writable_path_hit_is_refused() {
    use std::os::unix::fs::PermissionsExt as _;
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior_tts = std::env::var_os(LLAMA_TTS_ENV);
    let prior_path = std::env::var_os("PATH");
    std::env::remove_var(LLAMA_TTS_ENV);
    let dir = fixture_dir();
    let candidate = dir.path().join(LLAMA_TTS_BIN);
    std::fs::write(&candidate, "#!/bin/sh\nexit 0\n").expect("stub writes");
    std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o777))
        .expect("stub chmod");
    std::env::set_var("PATH", dir.path().as_os_str());
    assert_eq!(
        resolve_tts_binary(),
        None,
        "group/other-writable searched hits are refused"
    );
    match prior_tts {
        Some(value) => std::env::set_var(LLAMA_TTS_ENV, value),
        None => std::env::remove_var(LLAMA_TTS_ENV),
    }
    match prior_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
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

#[test]
fn scratch_reservation_is_exclusive_and_private() {
    use crate::worker::reserve_scratch_wav;
    let first = reserve_scratch_wav().expect("first reserves");
    let second = reserve_scratch_wav().expect("second reserves");
    assert_ne!(first, second, "reservations never collide");
    assert!(
        first.parent() == second.parent(),
        "both live in the private scratch dir"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(first.parent().expect("parent"))
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "scratch dir is owner-only");
    }
    let _ = std::fs::remove_file(&first);
    let _ = std::fs::remove_file(&second);
}

#[test]
fn synth_scrubs_the_environment_and_pins_path() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = fixture_wav(dir.path());
    let script = dir.path().join("llama-tts");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             if [ -n \"$ORATOR_REDTEAM_SECRET\" ]; then echo 'parent secret leaked' >&2; exit 9; fi\n\
             if [ \"$PATH\" != \"/usr/bin:/bin\" ]; then echo \"PATH not pinned: $PATH\" >&2; exit 10; fi\n\
             if [ \"$LANG\" != \"C.UTF-8\" ]; then echo \"LANG not pinned: $LANG\" >&2; exit 11; fi\n\
             cp '{}' \"$8\"\n",
            fixture.display()
        ),
    )
    .expect("stub writes");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("executable");
    std::env::set_var("ORATOR_REDTEAM_SECRET", "hunter2");
    let result = synth_blocking(&script, &bundle(), "hello", Duration::from_secs(10));
    std::env::remove_var("ORATOR_REDTEAM_SECRET");
    result.expect("scrubbed env with pinned PATH/LANG runs the stub");
}

#[test]
fn synth_timeout_kills_the_whole_group() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("orphan.wav");
    let script = dir.path().join("llama-tts");
    // Background writer outlives the main sleep: killing only the
    // child leaves the grandchild to write the marker; killing the
    // whole group leaves nothing.
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n(sleep 1; echo x > {}) & sleep 30\n",
            marker.display()
        ),
    )
    .expect("stub writes");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("executable");
    let error = synth_blocking(&script, &bundle(), "hi", Duration::from_millis(400))
        .expect_err("budget must fire");
    assert!(
        error.to_string().contains("timed out"),
        "timeout names itself: {error}"
    );
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(!marker.exists(), "group kill leaves no orphan output");
}
