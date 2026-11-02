//! Supervised LiquidAI voice server: the custom audio-capable
//! server binary over the native bundle.
//!
//! Stock `llama-server` builds cannot voice this bundle (their
//! projector handling has no generation pipeline for it), so voice
//! turns boot the LiquidAI audio server instead: `-m` backbone,
//! `-mm` projector, `-mv` vocoder, `--tts-speaker-file` tokenizer
//! — the file mapping the reference server runs. The binary
//! resolves from `LLAMA_AUDIO_SERVER`, then `$PATH`, then the
//! managed servers directory; readiness is an `OPTIONS` probe
//! against `/v1/chat/completions` (this build serves no `/health`
//! or `/models`). The child is owned by `orator_llm::Supervisor`
//! — the single supervised-child implementation — configured with
//! the audio probe; this module keeps only the audio-specific
//! argv/probe wrapper.

use std::path::PathBuf;
use std::time::Duration;

use orator_llm::{HealthProbe, SupervisorConfig};

use orator_core::AudioBundle;
use crate::session::S2sError;
/// Server binary name resolved from `$PATH` and the managed dir.
pub const AUDIO_SERVER_BIN: &str = "llama-liquid-audio-server";

/// Env var overriding the voice-server binary path.
pub const AUDIO_SERVER_ENV: &str = "LLAMA_AUDIO_SERVER";

/// How long a fresh voice server has to answer its readiness probe
/// (a gigabyte of weights loads before the first accept).
pub const VOICE_BOOT_TIMEOUT: Duration = Duration::from_secs(240);

/// Managed home holding the voice-server binary plus its libraries
/// (kept together: the binary finds its `.so` siblings by runpath).
/// `None` when no home base resolves.
#[must_use]
pub fn audio_server_home() -> Option<PathBuf> {
    orator_core::paths::home_dir()
        .map(|home| home.join(".local/share/orator/servers/liquid-audio"))
}

/// Resolve the voice-server binary: the `LLAMA_AUDIO_SERVER`
/// override first, then a `$PATH` search, then the managed home.
/// Searched hits pass the ownership gate for the current uid and are
/// refused (`None`) when untrusted; the explicit override bypasses
/// the gate. `None` when installed nowhere trusted.
#[must_use]
pub fn resolve_audio_server() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os(AUDIO_SERVER_ENV) {
        return Some(PathBuf::from(explicit));
    }
    let searched = orator_core::paths::find_on_path(AUDIO_SERVER_BIN).or_else(|| {
        audio_server_home()
            .map(|home| home.join(AUDIO_SERVER_BIN))
            .filter(|candidate| candidate.is_file())
    })?;
    orator_core::paths::validate_server_binary(
        &searched,
        orator_core::paths::current_uid(),
        false,
    )
    .ok()?;
    Some(searched)
}

/// Render the exact argv for a voice-server spawn. Unit-testable, no
/// side effects. Loopback-only; the reference file mapping, nothing
/// the stock arg shape would add (this build takes no `--alias`).
/// The supervised spawn renders the same bytes through
/// [`orator_llm::build_args`] over [`audio_supervisor_config`].
#[must_use]
pub fn voice_server_args(bundle: &AudioBundle, port: u16) -> Vec<String> {
    vec![
        "-m".to_string(),
        bundle.backbone.to_string_lossy().into_owned(),
        "-mm".to_string(),
        bundle.mmproj.to_string_lossy().into_owned(),
        "-mv".to_string(),
        bundle.vocoder.to_string_lossy().into_owned(),
        "--tts-speaker-file".to_string(),
        bundle.tokenizer.to_string_lossy().into_owned(),
        "--host".to_string(),
        "127.0.0.1".to_string(),
        "--port".to_string(),
        port.to_string(),
    ]
}

/// Audio `SupervisorConfig`: the bundle as weights/projector/vocoder/
/// speaker file on a loopback port, readied by the completions
/// `OPTIONS` probe. The served-model tag stays empty here — callers
/// that know it set `config.model` themselves (no `--alias` is ever
/// rendered for this build).
#[must_use]
pub fn audio_supervisor_config(bundle: &AudioBundle, port: u16) -> SupervisorConfig {
    SupervisorConfig {
        model: String::new(),
        weights: Some(bundle.backbone.clone()),
        mmproj: Some(bundle.mmproj.clone()),
        vocoder: Some(bundle.vocoder.clone()),
        tts_speaker_file: Some(bundle.tokenizer.clone()),
        port,
        health_probe: HealthProbe::OptionsCompletions,
        health_timeout: VOICE_BOOT_TIMEOUT,
        ..SupervisorConfig::default()
    }
}

/// Wait until `OPTIONS {base}/v1/chat/completions` answers 2xx or
/// `timeout` elapses. This build serves no `/health` or `/models`,
/// so the completions route itself is the readiness signal; the
/// probe costs no inference.
pub async fn wait_ready(base: &str, timeout: Duration) -> Result<(), S2sError> {
    let probe = orator_llm::Supervisor::attach_with_probe(
        base.to_string(),
        HealthProbe::OptionsCompletions,
    );
    probe.wait_healthy_timeout(timeout).await.map_err(|_| {
        S2sError::Backend(format!(
            "voice server at {base} not ready after {}s",
            timeout.as_secs()
        ))
    })
}

/// Managed-home binary path, for diagnostics. `None` when no home
/// base resolves.
#[must_use]
pub fn managed_binary() -> Option<PathBuf> {
    audio_server_home().map(|home| home.join(AUDIO_SERVER_BIN))
}

#[cfg(test)]
#[path = "../tests/voice.rs"]
mod tests;
