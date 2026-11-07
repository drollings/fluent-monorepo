//! Speech dispatch: daemon, audio-server, and local fallbacks.
//!
//! The `say` command plus the shared audio-turn runner (assured
//! endpoint, contention retry, chain budget). Fallbacks narrate every
//! hop so silence always comes with the exact remedy.

use orator_cli::{
    config::{resolve_backend, s2s_base_url},
    socket::{daemon_call_transport, Transport},
    stream::stream_chunks,
    ExitCode,
};
use orator_core::OratorConfig;

use super::note_unavailable;

/// Shared shutdown releaser: the current endpoint's release callback.
type Releaser = std::sync::Arc<dyn Fn() + Send + Sync>;
/// Slot holding the active releaser for the signal watcher.
type ReleaserSlot = std::sync::Arc<std::sync::Mutex<Option<Releaser>>>;

/// Speak `text` through the daemon and voice the returned audio
/// locally. The daemon synthesizes through its owned server and
/// returns PCM with its rate; this side decodes and drains it. A
/// reply without audio is an error, never a printed count.
pub(crate) fn say_via_daemon(text: &str, transport: Transport) -> ExitCode {
    match daemon_call_transport(transport, "Say", serde_json::json!({ "text": text })) {
        Ok(body) => match orator_cli::say_chain::decode_say_audio(&body) {
            Ok((samples, rate)) => match orator_audio::play_blocking(&samples, rate) {
                Ok(()) => ExitCode::Success,
                Err(error) => {
                    eprintln!("orator: {error}");
                    ExitCode::Error
                }
            },
            Err(error) => {
                eprintln!("orator: {error}");
                ExitCode::Error
            }
        },
        Err(code) => {
            if code == ExitCode::DaemonUnavailable {
                note_unavailable(transport);
            }
            code
        }
    }
}

/// Speak TEXT or stdin: through the daemon by default, or the audio
/// endpoint with local playout under `--backend audio-server` (text
/// falls back to the daemon when the server sends none).
pub(crate) fn say(
    text: &[String],
    backend: &str,
    config: &OratorConfig,
    transport: Transport,
) -> ExitCode {
    let spoken = if text.is_empty() {
        let mut stdin = String::new();
        if std::io::Read::read_to_string(&mut std::io::stdin(), &mut stdin).is_err() {
            eprintln!("orator: failed to read stdin");
            return ExitCode::Error;
        }
        stdin
    } else {
        text.join(" ")
    };
    if spoken.trim().is_empty() {
        eprintln!("orator: nothing to speak");
        return ExitCode::Usage;
    }
    let selected = resolve_backend(backend, &config.tts.backend);
    if selected == "audio-server" {
        return say_audio_server(&spoken, config, transport);
    }
    if selected == "kokoro" {
        // The composed voice for the serving head wins; the leaf
        // fills in when the tables say nothing (defaults: the leaf).
        let tag = orator_core::resolve_role_model(&config.roles, "tts")
            .unwrap_or_else(|| config.tts.voice.clone());
        let voice = orator_cli::fallback::resolve_tts_voice(config, &tag);
        return say_kokoro(&spoken, &voice, config.tts.gpu).0;
    }
    if selected != "daemon" {
        eprintln!("orator: unknown TTS backend: {selected}");
        return ExitCode::Usage;
    }
    say_via_daemon(&spoken, transport)
}

/// Synthesize through the audio endpoint and play locally (blocking
/// cpal output at the vocoder rate). Text-only servers fall back to
/// the daemon so the reply still speaks.
fn say_audio_server(text: &str, config: &OratorConfig, transport: Transport) -> ExitCode {
    match run_audio_turn(config, transport, |session| session.speak_blocking(text)) {
        Ok(samples) => match orator_audio::play_blocking(&samples, orator_s2s::AUDIO_OUT_RATE) {
            Ok(()) => ExitCode::Success,
            Err(error) => {
                eprintln!("orator: {error}");
                ExitCode::Error
            }
        },
        Err(orator_s2s::S2sError::Aborted) => ExitCode::Interrupted,
        Err(error) if orator_s2s::should_advance_chain(&error) => {
            say_fallback(text, config, transport)
        }
        Err(error) => {
            eprintln!("orator: {error}");
            ExitCode::Error
        }
    }
}

/// Run one audio turn against an assured endpoint: shared when the
/// model is served, supervised when it is not. A shared endpoint
/// that rejects audio input retries once on a fresh supervised
/// server (verified live: the gateway route has no mmproj, a
/// self-booted one accepts audio). Releases every handle taken.
pub(crate) fn run_audio_turn<T>(
    config: &orator_core::OratorConfig,
    transport: Transport,
    mut run: impl FnMut(&mut orator_s2s::AudioSession) -> Result<T, orator_s2s::S2sError>,
) -> Result<T, orator_s2s::S2sError> {
    let s2s = &config.s2s;
    use orator_s2s::S2sError;
    // One overall budget for the whole chain so a worker timeout plus
    // a stream timeout plus a boot timeout never stack without bound.
    // `shutdown_signal` stays the only preemption; this bound only
    // skips further supervised boots once billed time is spent.
    // Endpoint, model tag, bundle paths, and turn tunables all ride
    // the loaded `s2s` section (default port 8080).
    const CHAIN_BUDGET: std::time::Duration = std::time::Duration::from_secs(600);
    let chain_start = std::time::Instant::now();
    let base = s2s_base_url(s2s);
    let model = s2s.model.clone();
    let bundle = orator_core::bundle_from_s2s(s2s, &config.models, &config.gguf_dir);
    let endpoint = std::sync::Arc::new(
        common_core::runtime::block_on(orator_s2s::ensure_audio_server(
            &base,
            &model,
            &bundle,
            s2s.auto_boot,
            s2s.gpu,
        ))
        .map_err(|error| {
            eprintln!("orator: {error}");
            error
        })?,
    );
    // Current releaser: the watcher runs whatever it holds when the
    // signal lands, so supervised retries booted below stay covered.
    // Every handle here releases idempotently.
    let slot: ReleaserSlot = std::sync::Arc::new(std::sync::Mutex::new(None));
    let releasing = std::sync::Arc::clone(&slot);
    let watch = orator_cli::SignalWatch::arm(move || {
        if let Some(release) = common_core::sync::lock(&releasing).as_ref() {
            release();
        }
    });
    let hold = |release: std::sync::Arc<dyn Fn() + Send + Sync>| {
        *common_core::sync::lock(&slot) = Some(release);
    };
    hold(std::sync::Arc::new({
        let endpoint = std::sync::Arc::clone(&endpoint);
        move || endpoint.release()
    }));
    let interrupted = || -> Result<T, S2sError> {
        if let Some(release) = common_core::sync::lock(&slot).as_ref() {
            release();
        }
        eprintln!("orator: interrupted; endpoint released");
        Err(S2sError::Aborted)
    };
    let mut session = orator_s2s::AudioSession::new(orator_s2s::S2sConfig {
        endpoint: endpoint.base_url().to_string(),
        ..s2s.clone()
    });
    let outcome = run(&mut session);
    if watch.signalled() {
        return interrupted();
    }
    match outcome {
        Ok(value) => {
            endpoint.release();
            Ok(value)
        }
        Err(S2sError::NoAudio) if bundle.present() => {
            endpoint.release();
            eprintln!("orator: shared endpoint sent no audio; seeking the LiquidAI voice server");
            match daemon_voice_endpoint(transport) {
                orator_cli::say_chain::DaemonVoiceHint::Available(url) => {
                    eprintln!("orator: using the daemon-owned voice server");
                    let mut retry = orator_s2s::AudioSession::new(orator_s2s::S2sConfig {
                        endpoint: url,
                        ..s2s.clone()
                    });
                    let outcome = run(&mut retry);
                    if watch.signalled() {
                        return interrupted();
                    }
                    outcome
                }
                orator_cli::say_chain::DaemonVoiceHint::Ejected(reason)
                | orator_cli::say_chain::DaemonVoiceHint::Saturated(reason) => {
                    eprintln!("orator: {reason}; skipping supervised boot");
                    Err(S2sError::NoAudio)
                }
                orator_cli::say_chain::DaemonVoiceHint::BootContention(reason) => {
                    eprintln!(
                        "orator: {reason}; waiting for the daemon boot (progress, not a failure)"
                    );
                    match wait_for_daemon_boot(transport) {
                        orator_cli::say_chain::DaemonVoiceHint::Available(url) => {
                            eprintln!("orator: using the daemon-owned voice server");
                            let mut retry = orator_s2s::AudioSession::new(orator_s2s::S2sConfig {
                                endpoint: url,
                                ..s2s.clone()
                            });
                            let outcome = run(&mut retry);
                            if watch.signalled() {
                                return interrupted();
                            }
                            outcome
                        }
                        other => {
                            eprintln!(
                                "orator: daemon still busy ({}); falling through to the voice chain",
                                match other {
                                    orator_cli::say_chain::DaemonVoiceHint::Ejected(reason)
                                    | orator_cli::say_chain::DaemonVoiceHint::Saturated(reason)
                                    | orator_cli::say_chain::DaemonVoiceHint::BootContention(reason)
                                    | orator_cli::say_chain::DaemonVoiceHint::SlowPeer(reason) => {
                                        reason
                                    }
                                    _ => "unavailable".to_string(),
                                }
                            );
                            Err(S2sError::NoAudio)
                        }
                    }
                }
                orator_cli::say_chain::DaemonVoiceHint::Unavailable
                | orator_cli::say_chain::DaemonVoiceHint::SlowPeer(_) => {
                    if chain_start.elapsed() > CHAIN_BUDGET {
                        eprintln!("orator: chain budget spent; skipping supervised boot");
                        return Err(S2sError::NoAudio);
                    }
                    if orator_s2s::resolve_audio_server().is_none() {
                        eprintln!(
                            "orator: no voice server binary (set LLAMA_AUDIO_SERVER or install llama-liquid-audio-server); falling through to the voice chain"
                        );
                        return Err(S2sError::NoAudio);
                    }
                    eprintln!("orator: booting a supervised voice server");
                    match common_core::runtime::block_on(orator_s2s::boot_audio_server(
                        &bundle,
                        &model,
                        s2s.auto_boot,
                        s2s.gpu,
                    )) {
                        Ok(owned) => {
                            let owned = std::sync::Arc::new(owned);
                            hold(std::sync::Arc::new({
                                let owned = std::sync::Arc::clone(&owned);
                                move || owned.release()
                            }));
                            let mut retry = orator_s2s::AudioSession::new(orator_s2s::S2sConfig {
                                endpoint: owned.base_url().to_string(),
                                ..s2s.clone()
                            });
                            let outcome = run(&mut retry);
                            if watch.signalled() {
                                return interrupted();
                            }
                            let out = outcome;
                            owned.release();
                            out
                        }
                        Err(error) => {
                            eprintln!("orator: voice server failed: {error}");
                            Err(S2sError::NoAudio)
                        }
                    }
                }
            }
        }
        Err(error)
            if orator_s2s::audio_input_unsupported(&error)
                && matches!(&*endpoint, orator_s2s::AudioEndpoint::Shared(_))
                && bundle.present() =>
        {
            endpoint.release();
            eprintln!("orator: endpoint rejects audio input; seeking the LiquidAI voice server");
            match daemon_voice_endpoint(transport) {
                orator_cli::say_chain::DaemonVoiceHint::Available(url) => {
                    eprintln!("orator: using the daemon-owned voice server");
                    let mut retry = orator_s2s::AudioSession::new(orator_s2s::S2sConfig {
                        endpoint: url,
                        ..s2s.clone()
                    });
                    let outcome = run(&mut retry);
                    if watch.signalled() {
                        return interrupted();
                    }
                    outcome
                }
                orator_cli::say_chain::DaemonVoiceHint::Ejected(reason)
                | orator_cli::say_chain::DaemonVoiceHint::Saturated(reason) => {
                    eprintln!("orator: {reason}; skipping supervised boot");
                    Err(S2sError::NoAudio)
                }
                orator_cli::say_chain::DaemonVoiceHint::BootContention(reason) => {
                    eprintln!(
                        "orator: {reason}; waiting for the daemon boot (progress, not a failure)"
                    );
                    match wait_for_daemon_boot(transport) {
                        orator_cli::say_chain::DaemonVoiceHint::Available(url) => {
                            eprintln!("orator: using the daemon-owned voice server");
                            let mut retry = orator_s2s::AudioSession::new(orator_s2s::S2sConfig {
                                endpoint: url,
                                ..s2s.clone()
                            });
                            let outcome = run(&mut retry);
                            if watch.signalled() {
                                return interrupted();
                            }
                            outcome
                        }
                        _ => Err(S2sError::NoAudio),
                    }
                }
                orator_cli::say_chain::DaemonVoiceHint::Unavailable
                | orator_cli::say_chain::DaemonVoiceHint::SlowPeer(_) => {
                    if chain_start.elapsed() > CHAIN_BUDGET {
                        eprintln!("orator: chain budget spent; skipping supervised boot");
                        return Err(S2sError::NoAudio);
                    }
                    eprintln!("orator: booting a supervised voice server");
                    match common_core::runtime::block_on(orator_s2s::boot_audio_server(
                        &bundle,
                        &model,
                        s2s.auto_boot,
                        s2s.gpu,
                    )) {
                        Ok(owned) => {
                            let owned = std::sync::Arc::new(owned);
                            hold(std::sync::Arc::new({
                                let owned = std::sync::Arc::clone(&owned);
                                move || owned.release()
                            }));
                            let mut retry = orator_s2s::AudioSession::new(orator_s2s::S2sConfig {
                                endpoint: owned.base_url().to_string(),
                                ..s2s.clone()
                            });
                            let outcome = run(&mut retry);
                            if watch.signalled() {
                                return interrupted();
                            }
                            let out = outcome;
                            owned.release();
                            out
                        }
                        Err(boot_error) => {
                            eprintln!("orator: voice server failed: {boot_error}");
                            Err(S2sError::NoAudio)
                        }
                    }
                }
            }
        }
        Err(error) => {
            endpoint.release();
            Err(error)
        }
    }
}

/// Wait briefly for a contended daemon boot, then look once more.
/// Both voice arms share this: contention always costs one 2 s wait
/// plus one re-lookup, never a silent fall-through and never an
/// unbounded wait. The chain budget still bounds the supervised boot
/// that follows.
fn wait_for_daemon_boot(transport: Transport) -> orator_cli::say_chain::DaemonVoiceHint {
    std::thread::sleep(std::time::Duration::from_secs(2));
    daemon_voice_endpoint(transport)
}

/// Ask the daemon for its owned voice-server URL.
///
/// The daemon distinguishes absence (caller may boot its own server)
/// from ejection and saturation (caller must not boot; fall through
/// to the voice chain instead). The quiet transport core preserves
/// the body text so those two cases stay distinguishable.
fn daemon_voice_endpoint(transport: Transport) -> orator_cli::say_chain::DaemonVoiceHint {
    use orator_cli::socket::call_transport;
    match call_transport(transport, "VoiceEndpoint", serde_json::json!({})) {
        Ok(body) => body
            .get("url")
            .and_then(|url| url.as_str())
            .map_or(orator_cli::say_chain::DaemonVoiceHint::Unavailable, |url| {
                orator_cli::say_chain::DaemonVoiceHint::Available(url.to_string())
            }),
        Err(failure) => {
            if let Some(text) = failure.text.as_deref() {
                eprintln!("{text}");
            }
            orator_cli::say_chain::classify_daemon_call_failure(failure.text.as_deref())
        }
    }
}

/// Whether the daemon currently refuses voice work with a named
/// ejection or saturation is decided by the caller matching on
/// [`orator_cli::say_chain::DaemonVoiceHint`]; no separate helper
/// exists so the decision stays in one place.
/// Voice fallback chain after a text-only server turn: the GPU
/// vocoder worker, then the local Kokoro voice, then the daemon.
/// LiquidAI stays the default everywhere; the worker voices on the
/// GPU whenever the bundle's projector supports generation, Kokoro
/// covers the CPU fallback, and the daemon is the last resort.
/// Every hop narrates, so silence always comes with the exact
/// remedy.
fn say_fallback(text: &str, config: &OratorConfig, transport: Transport) -> ExitCode {
    if let Some(binary) = orator_s2s::resolve_tts_binary() {
        eprintln!("orator: server sent no audio; synthesizing on the GPU worker");
        let bundle = orator_core::bundle_from_s2s(&config.s2s, &config.models, &config.gguf_dir);
        match orator_s2s::synth_blocking(&binary, &bundle, text, orator_s2s::TTS_TIMEOUT) {
            Ok((samples, rate)) => {
                return match orator_audio::play_blocking(&samples, rate) {
                    Ok(()) => ExitCode::Success,
                    Err(error) => {
                        eprintln!("orator: {error}");
                        ExitCode::Error
                    }
                };
            }
            Err(error) => eprintln!("orator: GPU worker failed: {error}"),
        }
    } else {
        eprintln!(
            "orator: server sent no audio; no llama-tts binary (set LLAMA_TTS or add it to $PATH)"
        );
    }
    let resolved = orator_tts::default_paths();
    let kokoro_ready = resolved
        .as_ref()
        .is_some_and(|(model, voices)| model.exists() && voices.exists());
    if kokoro_ready {
        eprintln!("orator: synthesizing locally (kokoro fallback)");
        let (code, chunks, played) = say_kokoro(text, &config.tts.voice, config.tts.gpu);
        match code {
            ExitCode::Success => return ExitCode::Success,
            ExitCode::Error if played > 0 && played < chunks.len() => {
                let remaining: Vec<&str> = chunks.iter().skip(played).map(String::as_str).collect();
                let suffix = remaining.join(" ");
                if suffix.trim().is_empty() {
                    eprintln!(
                        "orator: local voice played {played}/{} chunks; remainder empty, not re-speaking",
                        chunks.len()
                    );
                    return ExitCode::Error;
                }
                eprintln!(
                    "orator: local voice played {played}/{} chunks; continuing with the daemon for the remainder",
                    chunks.len()
                );
                return say_via_daemon(&suffix, transport);
            }
            ExitCode::Error if played > 0 => {
                eprintln!(
                    "orator: local voice played {played}/{} chunks; output already heard, not re-speaking",
                    chunks.len()
                );
                return ExitCode::Error;
            }
            ExitCode::Error => {}
            code => return code,
        }
    } else {
        let hint = resolved.map_or_else(
            || "the managed model directory".to_string(),
            |(model, _)| model.display().to_string(),
        );
        eprintln!("orator: no local voice at {hint} (fetch kokoro-v1.0.onnx plus a voice pack)");
    }
    say_via_daemon(text, transport)
}

/// Speak `text` through the local Kokoro voice and play it on the
/// default output device. Weights missing or device absent fail
/// actionably; synthesis errors never fall through silently. GPU is
/// explicit opt-in and fails without a device instead of running CPU.
/// Text normalizes once (fences and URLs never spoken) then splits on
/// sentence boundaries; chunk N plays while chunk N+1 synthesizes, so
/// first audio lands after the first chunk. Returns the exit code plus
/// the chunk list and the count already played, so the caller speaks
/// only the unsynthesized suffix onward and never repeats a prefix.
fn say_kokoro(text: &str, voice: &str, gpu: bool) -> (ExitCode, Vec<String>, usize) {
    let Some((model, voices)) = orator_tts::default_paths() else {
        eprintln!("orator: no managed model directory (HOME and XDG_DATA_HOME are unset)");
        return (ExitCode::Error, Vec::new(), 0);
    };
    let backend = std::sync::Arc::new(if gpu {
        orator_tts::KokoroBackend::new(model, voices, voice.to_string()).with_gpu()
    } else {
        orator_tts::KokoroBackend::new(model, voices, voice.to_string())
    });
    let chunks: Vec<String> = orator_cli::say_chain::normalized_chunks(text);
    if chunks.iter().all(|chunk| chunk.trim().is_empty()) {
        return (ExitCode::Success, chunks, 0);
    }
    let player = match orator_audio::StreamPlayer::open(
        orator_tts::KOKORO_SAMPLE_RATE,
    ) {
        Ok(player) => player,
        Err(error) => {
            eprintln!("orator: {error}");
            return (ExitCode::Error, Vec::new(), 0);
        }
    };
    let mut played_chunks = 0usize;
    let outcome = stream_chunks(
        chunks.clone(),
        {
            let backend = std::sync::Arc::clone(&backend);
            move |chunk: &str| {
                backend
                    .synth_blocking(chunk)
                    .map_err(|error| error.to_string())
            }
        },
        |samples: &[f32]| {
            player.push(samples);
            played_chunks += 1;
            Ok::<(), String>(())
        },
    );
    // Drain whatever queued even on failure: committed audio is
    // already on the device, and the stream must stop cleanly.
    if let Err(error) = player.finish() {
        eprintln!("orator: {error}");
        return (ExitCode::Error, chunks, played_chunks);
    }
    match outcome {
        Ok(()) => (ExitCode::Success, chunks, played_chunks),
        Err(error) => {
            eprintln!("orator: local voice failed: {error}");
            (ExitCode::Error, chunks, played_chunks)
        }
    }
}
