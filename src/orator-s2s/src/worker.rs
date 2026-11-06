//! Supervised vocoder speech: text in, waveform out.
//!
//! When the audio endpoint streams text only, this worker
//! synthesizes locally through the supervised `llama-tts` binary
//! (`-m <backbone> -mm <mmproj> -p <text> -o <wav>`), which runs
//! the bundle's audio head plus vocoder on the GPU. The binary
//! resolves from `LLAMA_TTS` or `$PATH` with a fail-fast actionable
//! error, every run is timeout-bounded, and the scratch WAV never
//! survives the call. A bundle whose projector cannot voice fails
//! fast naming its reason, and the caller falls through to the next
//! voice in the chain.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use thiserror::Error;

use orator_core::AudioBundle;

/// How long one synthesis may run (GPU inference is slow to load).
/// Fits under the CLI chain budget (300 s < 600 s `CHAIN_BUDGET` in
/// `orator-cli`): the vocoder never outlives the turn that owns it.
pub const TTS_TIMEOUT: Duration = Duration::from_secs(300);

/// Env var overriding the `llama-tts` binary path.
pub const LLAMA_TTS_ENV: &str = "LLAMA_TTS";

/// Binary name resolved from `$PATH`.
pub const LLAMA_TTS_BIN: &str = "llama-tts";

/// Worker failures. Every variant names the remedy.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum TtsWorkerError {
    /// No binary to run.
    #[error("{0}")]
    NoBinary(String),
    /// The run failed or timed out.
    #[error("llama-tts failed: {0}")]
    Failed(String),
    /// The WAV did not decode.
    #[error("llama-tts output unreadable: {0}")]
    Unreadable(String),
}

/// Resolve the `llama-tts` binary: the `LLAMA_TTS` override first,
/// then a `$PATH` search. Searched hits pass the ownership gate for
/// the current uid (owner/mode/non-`/tmp`/executable); explicit
/// overrides require absolute paths and log their fingerprint (the
/// user accepts responsibility). `None` when absent or untrusted.
/// Shared homes: `orator_core::paths::ExplicitBinary` for the
/// override arms, `orator_core::paths::resolve_validated_binary` over
/// the first `$PATH` hit for the search (same first-hit the
/// hand-rolled loop found, now via the shared `find_on_path`).
#[must_use]
pub fn resolve_tts_binary() -> Option<PathBuf> {
    let explicit = orator_core::paths::ExplicitBinary::from_env(LLAMA_TTS_ENV);
    match explicit.path.clone() {
        Some(path) if path.is_absolute() => {
            let fingerprint = orator_core::paths::binary_fingerprint(&path);
            if let Err(error) = orator_core::paths::validate_server_binary(
                &path,
                orator_core::paths::current_uid(),
                false,
            ) {
                eprintln!(
                    "orator: explicit tts binary {} ({fingerprint}) bypasses the gate with a warning: {error}",
                    path.display()
                );
            }
            Some(path)
        }
        Some(_) => None,
        None => orator_core::paths::resolve_validated_binary(
            &explicit,
            orator_core::paths::find_on_path(LLAMA_TTS_BIN),
            orator_core::paths::current_uid(),
        ),
    }
}

/// Synthesize `text` through `binary` over `bundle`: run
/// `llama-tts -m <backbone> -mm <mmproj> -p <text> -o <scratch>`,
/// decode the WAV, delete the scratch. Returns samples plus their
/// rate. Runs on the caller's thread; the whole run (spawn, wait,
/// read) happens inside one bridge so no per-call runtime is built.
/// Standard streams are piped explicitly (never inherited): the
/// failure message carries the run's own stderr tail, so a model
/// that cannot voice names its reason instead of exiting bare. The
/// child mirrors the sandbox shape: scrubbed env with a pinned
/// `PATH`/`LANG`, private jail cwd, its own process group, and a
/// whole-group kill on timeout.
pub fn synth_blocking(
    binary: &Path,
    bundle: &AudioBundle,
    text: &str,
    timeout: Duration,
) -> Result<(Vec<f32>, u32), TtsWorkerError> {
    validate_tts_inputs(binary, bundle)?;
    let out = reserve_scratch_wav()?;
    let _guard = ScratchGuard::new(out.clone());
    let cwd = out.parent().map_or_else(
        || {
            orator_core::paths::home_dir().map_or_else(std::env::temp_dir, |home| {
                home.join(".local/share/orator/scratch")
            })
        },
        Path::to_path_buf,
    );
    let (status, stderr) = common_core::runtime::block_on_shared(async {
        // M6-S6 audit: mirrors the supervisor's sandbox prefix
        // (env_clear + pinned PATH/LANG + null stdin/stdout +
        // kill_on_drop + process group) without sharing a builder —
        // stderr stays piped here (drained for error tails) while the
        // supervisor discards it, and the cwd/timeout wrapper differs.
        // See the keep-decision note at `Supervisor::spawn_child`.
        let mut cmd = tokio::process::Command::new(binary);
        cmd.arg("-m")
            .arg(&bundle.backbone)
            .arg("-mm")
            .arg(&bundle.mmproj)
            .arg("-p")
            .arg(text)
            .arg("-o")
            .arg(&out)
            .current_dir(&cwd)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        let mut child = cmd
            .spawn()
            .map_err(|error| TtsWorkerError::Failed(format!("spawn: {error}")))?;
        let pgid = child.id();
        let pipe = child.stderr.take();
        let mut drain = tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = tokio::io::AsyncReadExt::read_to_end(&mut pipe, &mut buf).await;
            }
            buf
        });
        let status = tokio::select! {
            out = tokio::time::timeout(timeout, child.wait()) => match out {
                Ok(Ok(status)) => status,
                Ok(Err(error)) => {
                    let _ = (&mut drain).await;
                    return Err(TtsWorkerError::Failed(format!("wait: {error}")));
                }
                Err(_) => {
                    kill_group(pgid);
                    let _ = child.start_kill();
                    let _ = (&mut drain).await;
                    return Err(TtsWorkerError::Failed(format!(
                        "timed out after {}s",
                        timeout.as_secs()
                    )));
                }
            },
            () = orator_core::shutdown_signal() => {
                kill_group(pgid);
                let _ = child.start_kill();
                let _ = (&mut drain).await;
                return Err(TtsWorkerError::Failed(
                    "interrupted by shutdown signal".to_string(),
                ));
            }
        };
        let stderr = drain.await.unwrap_or_default();
        Ok::<_, TtsWorkerError>((status, stderr))
    })?;
    let samples = read_scratch(&out, status, &stderr).inspect_err(|_| {
        let _ = std::fs::remove_file(&out);
    })?;
    let _ = std::fs::remove_file(&out);
    Ok(samples)
}

/// Validate worker inputs before spawn: the binary must be absolute
/// and a file (executable check is best-effort); the bundle must be
/// present. Ownership of searched hits is gated in
/// `resolve_tts_binary`; explicit overrides bypass it as the user's
/// responsibility. Failures fire before any child or scratch file
/// exists.
fn validate_tts_inputs(binary: &Path, bundle: &AudioBundle) -> Result<(), TtsWorkerError> {
    if !binary.is_absolute() {
        return Err(TtsWorkerError::NoBinary(format!(
            "tts binary must be absolute: {}",
            binary.display()
        )));
    }
    if !binary.is_file() {
        return Err(TtsWorkerError::NoBinary(format!(
            "tts binary missing: {}",
            binary.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Ok(metadata) = std::fs::metadata(binary) {
            if metadata.permissions().mode() & 0o111 == 0 {
                return Err(TtsWorkerError::NoBinary(format!(
                    "tts binary not executable: {}",
                    binary.display()
                )));
            }
        }
    }
    if !bundle.present() {
        return Err(TtsWorkerError::Failed(
            "audio bundle not present".to_string(),
        ));
    }
    Ok(())
}

/// Kill a whole process group started by `synth_blocking`: the child
/// leads its own group (`process_group(0)`), so `kill -KILL -pgid`
/// takes grandchildren with it instead of orphaning them. Best-effort:
/// a missing `kill` binary still leaves `kill_on_drop` plus the
/// explicit `start_kill` at the call site.
fn kill_group(pgid: Option<u32>) {
    let Some(id) = pgid else {
        return;
    };
    let program = (["/bin/kill", "/usr/bin/kill"])
        .into_iter()
        .find(|candidate| std::path::Path::new(candidate).is_file())
        .unwrap_or("kill");
    let _ = std::process::Command::new(program)
        .args(["-KILL", "--", &format!("-{id}")])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Scratch-file owner: deletes the WAV on drop so a panic between
/// spawn and read cannot leak the scratch file.
struct ScratchGuard {
    path: PathBuf,
}

impl ScratchGuard {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

/// Reserve a scratch WAV path with `O_EXCL` semantics inside a private
/// directory: the file is created (and immediately closed) before the
/// child ever sees the path, so a pre-planted symlink cannot redirect
/// the run's output.
fn reserve_scratch_wav() -> Result<PathBuf, TtsWorkerError> {
    use std::io::ErrorKind;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let mut dir = std::env::temp_dir();
    dir.push(format!("orator-tts-{}", orator_core::paths::current_uid()));
    std::fs::create_dir_all(&dir)
        .map_err(|error| TtsWorkerError::Failed(format!("scratch dir: {error}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    for _ in 0..15 {
        let candidate = dir.join(format!(
            "orator-tts-{}-{}-{}.wav",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.subsec_nanos())
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(error) if error.kind() != ErrorKind::AlreadyExists => {
                return Err(TtsWorkerError::Failed(format!("scratch reserve: {error}")));
            }
            Err(_) => {}
        }
    }
    Err(TtsWorkerError::Failed(
        "scratch reserve: too many collisions".to_string(),
    ))
}

impl Drop for ScratchGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn read_scratch(
    out: &Path,
    status: std::process::ExitStatus,
    stderr: &[u8],
) -> Result<(Vec<f32>, u32), TtsWorkerError> {
    if !status.success() {
        let exit = status
            .code()
            .map_or("signal".to_string(), |code| code.to_string());
        let tail = stderr_tail(stderr);
        let detail = if tail.is_empty() {
            format!("exit {exit}")
        } else {
            format!("exit {exit}: {tail}")
        };
        return Err(TtsWorkerError::Failed(detail));
    }
    orator_audio::read_wav_mono(out)
        .map(|wav| (wav.samples, wav.sample_rate))
        .map_err(|error| TtsWorkerError::Unreadable(error.to_string()))
}

/// Last non-empty stderr line, flattened to one line and capped, so
/// a failing run reports its own reason without flooding.
fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let last = text.lines().map(str::trim).rfind(|line| !line.is_empty());
    match last {
        Some(line) => {
            let flat: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
            if flat.chars().count() <= 300 {
                flat
            } else {
                flat.chars().take(300).collect::<String>() + "…"
            }
        }
        None => String::new(),
    }
}

#[cfg(test)]
#[path = "../tests/worker.rs"]
mod tests;
