//! Supervised voice server: stock `llama-server` over the native
//! audio bundle.
//!
//! Current `llama-server` builds voice LFM3-Audio from backbone +
//! projector alone (the codec lives in the projector; `/tts` voices,
//! `input_audio` transcribes, `/v1/realtime` converses). The legacy
//! LiquidAI audio server (`-mv` vocoder, `--tts-speaker-file`
//! tokenizer) stays supported when installed, but nothing requires
//! it. The binary resolves from `LLAMA_AUDIO_SERVER`, then the legacy
//! name on `$PATH`/managed home, then stock `llama-server`
//! (`LLAMA_SERVER`/`$PATH`); readiness is an `OPTIONS` probe against
//! `/v1/chat/completions`. The child is owned by
//! `orator_llm::Supervisor` — the single supervised-child
//! implementation — configured with the audio probe; this module keeps
//! only the audio-specific argv/probe wrapper.

use std::path::PathBuf;
use std::time::Duration;

use orator_llm::{HealthProbe, SupervisorConfig};

use crate::session::S2sError;
use orator_core::AudioBundle;
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
    orator_core::paths::home_dir().map(|home| home.join(".local/share/orator/servers/liquid-audio"))
}

/// Voice-server search order (unvalidated): the legacy audio
/// binary's `$PATH` hits, the managed-home binary when present, then
/// the stock `llama-server` candidates. Per-caller order table —
/// stays local; the gate filter is shared.
fn audio_server_search_table() -> Vec<PathBuf> {
    let mut table = Vec::new();
    table.extend(orator_core::paths::find_all_on_path(AUDIO_SERVER_BIN));
    if let Some(home) = audio_server_home().map(|home| home.join(AUDIO_SERVER_BIN)) {
        if home.is_file() {
            table.push(home);
        }
    }
    table.extend(orator_llm::resolve_binary_candidates());
    table
}

/// Every voice-server candidate binary in try order: the
/// `LLAMA_AUDIO_SERVER` override alone when set, else the validated
/// search table. Boot tries each until one answers readiness: a
/// pre-LFM3 install fails fast on the bundle and the next serves,
/// instead of one shadowed binary vetoing voice.
/// Shared filter home: `orator_core::paths::filter_validated_binaries`;
/// the override classification is `orator_core::paths::ExplicitBinary`.
#[must_use]
pub fn resolve_audio_server_candidates() -> Vec<PathBuf> {
    let explicit = orator_core::paths::ExplicitBinary::from_env(AUDIO_SERVER_ENV);
    match explicit.path {
        Some(path) if path.is_absolute() => vec![path],
        Some(_) => Vec::new(),
        None => orator_core::paths::filter_validated_binaries(
            audio_server_search_table(),
            orator_core::paths::current_uid(),
        ),
    }
}

/// Resolve the voice-server binary: the first of
/// [`resolve_audio_server_candidates`]. Searched hits pass the
/// ownership gate for the current uid and are refused (`None`) when
/// untrusted; explicit overrides log their owner/mode/fingerprint
/// (the user accepts responsibility). `None` when installed nowhere
/// trusted. Prefer the candidate list plus a boot attempt wherever a
/// turn is at stake: the first candidate may be a pre-LFM3 install
/// that fails the bundle fast while a later one serves.
#[must_use]
pub fn resolve_audio_server() -> Option<PathBuf> {
    let candidates = resolve_audio_server_candidates();
    if candidates.len() == 1 && std::env::var_os(AUDIO_SERVER_ENV).is_some() {
        let path = &candidates[0];
        let fingerprint = orator_core::paths::binary_fingerprint(path);
        if let Err(error) = orator_core::paths::validate_server_binary(
            path,
            orator_core::paths::current_uid(),
            false,
        ) {
            eprintln!(
                "orator: explicit audio server {} ({fingerprint}) bypasses the gate with a warning: {error}",
                path.display()
            );
        }
    }
    candidates.into_iter().next()
}

/// Render the exact argv for a voice-server spawn. Unit-testable, no
/// side effects. Loopback-only; stock `llama-server` shape (`-m`
/// backbone, `--mmproj` projector) plus the legacy `-mv`/
/// `--tts-speaker-file` pair only when the bundle carries them
/// (LFM2.5). The supervised spawn renders the same bytes through
/// [`orator_llm::build_args`] over [`audio_supervisor_config`].
#[must_use]
pub fn voice_server_args(bundle: &AudioBundle, port: u16) -> Vec<String> {
    let mut args = vec![
        "-m".to_string(),
        bundle.backbone.to_string_lossy().into_owned(),
        "--mmproj".to_string(),
        bundle.mmproj.to_string_lossy().into_owned(),
    ];
    if !bundle.vocoder.as_os_str().is_empty() && bundle.vocoder.exists() {
        args.push("-mv".to_string());
        args.push(bundle.vocoder.to_string_lossy().into_owned());
    }
    if !bundle.tokenizer.as_os_str().is_empty() && bundle.tokenizer.exists() {
        args.push("--tts-speaker-file".to_string());
        args.push(bundle.tokenizer.to_string_lossy().into_owned());
    }
    args.push("--host".to_string());
    args.push("127.0.0.1".to_string());
    args.push("--port".to_string());
    args.push(port.to_string());
    args
}

/// Audio `SupervisorConfig`: the bundle as weights/projector on a
/// loopback port, readied by the completions `OPTIONS` probe. The
/// legacy vocoder/speaker pair rides along only when present
/// (LFM2.5); LFM3 voices from backbone + projector alone. The
/// served-model tag stays empty here — callers that know it set
/// `config.model` themselves.
#[must_use]
pub fn audio_supervisor_config(bundle: &AudioBundle, port: u16) -> SupervisorConfig {
    let vocoder = (!bundle.vocoder.as_os_str().is_empty() && bundle.vocoder.exists())
        .then(|| bundle.vocoder.clone());
    let speaker = (!bundle.tokenizer.as_os_str().is_empty() && bundle.tokenizer.exists())
        .then(|| bundle.tokenizer.clone());
    SupervisorConfig {
        model: String::new(),
        weights: Some(bundle.backbone.clone()),
        mmproj: Some(bundle.mmproj.clone()),
        vocoder,
        tts_speaker_file: speaker,
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
