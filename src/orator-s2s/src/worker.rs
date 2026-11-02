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
/// then a `$PATH` search. `None` when not found.
#[must_use]
pub fn resolve_tts_binary() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os(LLAMA_TTS_ENV) {
        return Some(PathBuf::from(explicit));
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(LLAMA_TTS_BIN);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Synthesize `text` through `binary` over `bundle`: run
/// `llama-tts -m <backbone> -mm <mmproj> -p <text> -o <scratch>`,
/// decode the WAV, delete the scratch. Returns samples plus their
/// rate. Runs on the caller's thread; the whole run (spawn, wait,
/// read) happens inside one bridge so no per-call runtime is built.
/// Standard streams are piped explicitly (never inherited): the
/// failure message carries the run's own stderr tail, so a model
/// that cannot voice names its reason instead of exiting bare.
pub fn synth_blocking(
    binary: &Path,
    bundle: &AudioBundle,
    text: &str,
    timeout: Duration,
) -> Result<(Vec<f32>, u32), TtsWorkerError> {
    validate_tts_inputs(binary, bundle)?;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let out = std::env::temp_dir().join(format!(
        "orator-tts-{}-{}.wav",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _guard = ScratchGuard::new(out.clone());
    let (status, stderr) = common_core::runtime::block_on(async {
        let mut child = tokio::process::Command::new(binary)
            .arg("-m")
            .arg(&bundle.backbone)
            .arg("-mm")
            .arg(&bundle.mmproj)
            .arg("-p")
            .arg(text)
            .arg("-o")
            .arg(&out)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| TtsWorkerError::Failed(format!("spawn: {error}")))?;
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
                    let _ = child.start_kill();
                    let _ = (&mut drain).await;
                    return Err(TtsWorkerError::Failed(format!(
                        "timed out after {}s",
                        timeout.as_secs()
                    )));
                }
            },
            () = orator_core::shutdown_signal() => {
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

/// Validate worker inputs before spawn: the `LLAMA_TTS` override must
/// be a file (executable check is best-effort), and the bundle must be
/// present. Failures fire before any child or scratch file exists.
fn validate_tts_inputs(binary: &Path, bundle: &AudioBundle) -> Result<(), TtsWorkerError> {
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
        return Err(TtsWorkerError::Failed("audio bundle not present".to_string()));
    }
    Ok(())
}

/// Scratch-file owner: deletes the WAV on drop so a panic between
/// spawn and read cannot leak `/tmp/orator-tts-*`.
struct ScratchGuard {
    path: PathBuf,
}

impl ScratchGuard {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
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
        let exit = status.code().map_or("signal".to_string(), |code| code.to_string());
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
    let last = text
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty());
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
