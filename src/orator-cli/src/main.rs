#![forbid(unsafe_code)]

//! `orator`: thin client over the `oratord` daemon. Parses one command and
//! either runs it locally or forwards it over the socket.

use std::process::ExitCode as ProcessExit;
use std::time::{Duration, Instant};

use clap::Parser;
use fluent_concurrency::stream::StreamAbort;
use orator_audio::{resample_linear, CpalBackend};
use orator_cli::{
    config::{audio_base_url, gpu_llm_args, resolve_backend, resolve_llm_base},
    doctor::{full_report, render_full},
    shell::{init_script, write_completions, Shell},
    socket::{
        daemon_call_transport, parse_transport, unavailable_socket_text, unavailable_ws_text,
        Transport,
    },
    stream::stream_chunks,
    transcribe::{audio_server_cue, cue_from_centiseconds, render, Cue, TranscriptFormat},
    Cli, Command, ExitCode, ModelsAction,
};
use orator_core::{models_dir, LlmEndpointConfig, LlmSession, OratorConfig, SttConfig};
use orator_desktop::{ExternalClipboard, LadderMemory, NoChord};
use orator_llm::{probe_models, HttpSession, Supervisor, SupervisorConfig};
use orator_stt::WhisperBackend;

/// Shared shutdown releaser: the current endpoint's release callback.
type Releaser = std::sync::Arc<dyn Fn() + Send + Sync>;
/// Slot holding the active releaser for the signal watcher.
type ReleaserSlot = std::sync::Arc<std::sync::Mutex<Option<Releaser>>>;

fn main() -> ProcessExit {
    match run(Cli::parse()) {
        ExitCode::Success => ProcessExit::from(0),
        code => ProcessExit::from(code.code() as u8),
    }
}

/// Dispatch one parsed command. Implemented arms run; the rest report
/// unimplemented with a named reason. The match stays exhaustive
/// over the command surface.
fn run(cli: Cli) -> ExitCode {
    let transport = match parse_transport(&cli.transport) {
        Ok(transport) => transport,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::Usage;
        }
    };
    let (cfg, config_source) = orator_core::config::load_hierarchical();
    match cli.command {
        Command::Doctor => {
            print!("{}", render_full(&full_report(&cfg)));
            println!("config: {}", config_source.display());
            ExitCode::Success
        }
        Command::Models { action } => models(action),
        Command::ShellInit { shell } => {
            let Some(parsed) = Shell::parse(&shell) else {
                eprintln!("orator: unknown shell: {shell}");
                return ExitCode::Usage;
            };
            print!("{}", init_script(parsed));
            ExitCode::Success
        }
        Command::Completions { shell } => {
            let mut out = std::io::stdout().lock();
            match write_completions(&shell, &mut out) {
                Ok(()) => ExitCode::Success,
                Err(error) => {
                    eprintln!("orator: {error}");
                    ExitCode::Usage
                }
            }
        }
        Command::Transcribe { file, format, backend } => {
            transcribe(&file, &format, &backend, &cfg, transport)
        }
        Command::Ask { question, speak } => ask(&question, speak, &cfg, transport),
        Command::Say { text, backend } => say(&text, &backend, &cfg, transport),
        Command::Listen { sys, mic, json } => listen(sys, mic, json, transport),
        Command::Chat => chat(&cfg),
        Command::Status { selector } => {
            if !selector.trim().is_empty() {
                eprintln!("orator: status takes no selector (got {selector:?})");
                return ExitCode::Usage;
            }
            daemon_passthrough("GetState", serde_json::json!({}), transport)
        }
        Command::Ptt { action } => ptt(&action, transport),
        Command::Meeting { action, id } => meeting(&action, &id, transport),
        Command::Cmd { request } => propose(&request),
        Command::Approve { id, deny } => approve(&id, deny, transport),
        Command::Stats { file, selector } => {
            if !selector.trim().is_empty() {
                eprintln!("orator: stats takes no selector (got {selector:?})");
                return ExitCode::Usage;
            }
            stats(&file, transport)
        }
        Command::Config { action, key, value } => config(&action, &key, &value),
        Command::Daemon { action } => daemon_lifecycle(&action, transport),
        Command::Tui => orator_cli::tui::run_live(transport, &cfg),
        Command::Dictate {
            once,
            seconds,
            stdout,
            backend,
        } => {
            if once {
                dictate_once(seconds, stdout, &backend, &cfg, transport)
            } else {
                eprintln!("orator: hold-to-talk lands with the live hotkey");
                ExitCode::Error
            }
        }
    }
}

/// Forward one control call to the daemon; absence is exit 3.
fn daemon_passthrough(
    method: &str,
    params: serde_json::Value,
    transport: Transport,
) -> ExitCode {
    match daemon_call_transport(transport, method, params) {
        Ok(body) => {
            println!("{body}");
            ExitCode::Success
        }
        Err(code) => {
            if code == ExitCode::DaemonUnavailable {
                match transport {
                    Transport::Socket => {
                        eprintln!(
                            "{}",
                            unavailable_socket_text(
                                orator_core::socket_path().as_deref()
                            )
                        );
                    }
                    Transport::Ws => {
                        let (config, _) = orator_core::config::load_hierarchical();
                        eprintln!("{}", unavailable_ws_text(&config.daemon.ws_listen));
                    }
                }
            }
            code
        }
    }
}

/// Metrics: render a stat file locally, or dial the daemon.
fn stats(file: &str, transport: Transport) -> ExitCode {
    if file.is_empty() {
        daemon_passthrough("GetState", serde_json::json!({}), transport)
    } else {
        match orator_bench::BenchLog::read_stat_file(std::path::Path::new(file)) {
            Ok(log) => {
                print!("{}", orator_bench::render_stats(&orator_bench::histograms(&log)));
                ExitCode::Success
            }
            Err(error) => {
                eprintln!("orator: {error}");
                ExitCode::Error
            }
        }
    }
}

/// Speak TEXT or stdin: through the daemon by default, or the audio
/// endpoint with local playout under `--backend audio-server` (text
/// falls back to the daemon when the server sends none).
fn say(text: &[String], backend: &str, config: &OratorConfig, transport: Transport) -> ExitCode {
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
        return say_kokoro(&spoken, &config.tts.voice, config.tts.gpu).0;
    }
    if selected != "daemon" {
        eprintln!("orator: unknown TTS backend: {selected}");
        return ExitCode::Usage;
    }
    daemon_passthrough("Say", serde_json::json!({ "text": spoken }), transport)
}

/// Synthesize through the audio endpoint and play locally (blocking
/// cpal output at the vocoder rate). Text-only servers fall back to
/// the daemon so the reply still speaks.
fn say_audio_server(text: &str, config: &OratorConfig, transport: Transport) -> ExitCode {
    match run_audio_turn(&audio_base_url(&config.llm), transport, |session| {
        session.speak_blocking(text)
    }) {
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
fn run_audio_turn<T>(
    base: &str,
    transport: Transport,
    mut run: impl FnMut(&mut orator_s2s::AudioSession) -> Result<T, orator_s2s::S2sError>,
) -> Result<T, orator_s2s::S2sError> {
    use orator_s2s::S2sError;
    // One overall budget for the whole chain so a worker timeout plus
    // a stream timeout plus a boot timeout never stack without bound.
    // `shutdown_signal` stays the only preemption; this bound only
    // skips further supervised boots once billed time is spent.
    const CHAIN_BUDGET: std::time::Duration = std::time::Duration::from_secs(600);
    let chain_start = std::time::Instant::now();
    let model = orator_s2s::S2sConfig::default().model.clone();
    let bundle = orator_core::default_bundle();
    let endpoint = std::sync::Arc::new(
        common_core::runtime::block_on(orator_s2s::ensure_audio_server(base, &model, &bundle))
            .map_err(|error| {
                eprintln!("orator: {error}");
                error
            })?,
    );
    // Current releaser: the watcher runs whatever it holds when the
    // signal lands, so supervised retries booted below stay covered.
    // Every handle here releases idempotently.
    let slot: ReleaserSlot =
        std::sync::Arc::new(std::sync::Mutex::new(None));
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
        ..orator_s2s::S2sConfig::default()
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
                        ..orator_s2s::S2sConfig::default()
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
                    eprintln!("orator: {reason}; waiting for the daemon boot (progress, not a failure)");
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    match daemon_voice_endpoint(transport) {
                        orator_cli::say_chain::DaemonVoiceHint::Available(url) => {
                            eprintln!("orator: using the daemon-owned voice server");
                            let mut retry = orator_s2s::AudioSession::new(orator_s2s::S2sConfig {
                                endpoint: url,
                                ..orator_s2s::S2sConfig::default()
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
                                    | orator_cli::say_chain::DaemonVoiceHint::BootContention(reason) => {
                                        reason
                                    }
                                    _ => "unavailable".to_string(),
                                }
                            );
                            Err(S2sError::NoAudio)
                        }
                    }
                }
                orator_cli::say_chain::DaemonVoiceHint::Unavailable => {
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
                    match common_core::runtime::block_on(orator_s2s::boot_audio_server(&bundle, &model))
                    {
                        Ok(owned) => {
                            let owned = std::sync::Arc::new(owned);
                            hold(std::sync::Arc::new({
                                let owned = std::sync::Arc::clone(&owned);
                                move || owned.release()
                            }));
                            let mut retry = orator_s2s::AudioSession::new(orator_s2s::S2sConfig {
                                endpoint: owned.base_url().to_string(),
                                ..orator_s2s::S2sConfig::default()
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
                        ..orator_s2s::S2sConfig::default()
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
                    eprintln!("orator: {reason}; waiting for the daemon boot (progress, not a failure)");
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    Err(S2sError::NoAudio)
                }
                orator_cli::say_chain::DaemonVoiceHint::Unavailable => {
                    if chain_start.elapsed() > CHAIN_BUDGET {
                        eprintln!("orator: chain budget spent; skipping supervised boot");
                        return Err(S2sError::NoAudio);
                    }
                    eprintln!("orator: booting a supervised voice server");
                    match common_core::runtime::block_on(orator_s2s::boot_audio_server(&bundle, &model)) {
                        Ok(owned) => {
                            let owned = std::sync::Arc::new(owned);
                            hold(std::sync::Arc::new({
                                let owned = std::sync::Arc::clone(&owned);
                                move || owned.release()
                            }));
                            let mut retry = orator_s2s::AudioSession::new(orator_s2s::S2sConfig {
                                endpoint: owned.base_url().to_string(),
                                ..orator_s2s::S2sConfig::default()
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

/// Ask the daemon for its owned voice-server URL.
///
/// The daemon distinguishes absence (caller may boot its own server)
/// from ejection and saturation (caller must not boot; fall through
/// to the voice chain instead). The quiet transport core preserves
/// the body text so those two cases stay distinguishable.
fn daemon_voice_endpoint(
    transport: Transport,
) -> orator_cli::say_chain::DaemonVoiceHint {
    use orator_cli::socket::call_transport;
    match call_transport(transport, "VoiceEndpoint", serde_json::json!({})) {
        Ok(body) => body
            .get("url")
            .and_then(|url| url.as_str())
            .map_or(
                orator_cli::say_chain::DaemonVoiceHint::Unavailable,
                |url| orator_cli::say_chain::DaemonVoiceHint::Available(url.to_string()),
            ),
        Err(failure) => {
            if let Some(text) = failure.text.as_deref() {
                eprintln!("{text}");
            }
            orator_cli::say_chain::classify_daemon_call_failure(
                failure.text.as_deref(),
            )
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
        let bundle = orator_core::default_bundle();
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
        eprintln!("orator: server sent no audio; no llama-tts binary (set LLAMA_TTS or add it to $PATH)");
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
                let remaining: Vec<&str> =
                    chunks.iter().skip(played).map(String::as_str).collect();
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
                return daemon_passthrough(
                    "Say",
                    serde_json::json!({ "text": suffix }),
                    transport,
                );
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
        eprintln!(
            "orator: no local voice at {hint} (fetch kokoro-v1.0.onnx plus a voice pack)"
        );
    }
    daemon_passthrough("Say", serde_json::json!({ "text": text }), transport)
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
    let player = match orator_audio::StreamPlayer::open(orator_tts::KOKORO_SAMPLE_RATE) {
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
            move |chunk: &str| backend.synth_blocking(chunk).map_err(|error| error.to_string())
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

/// Stream the live transcript selector to the daemon.
fn listen(sys: bool, mic: bool, json: bool, transport: Transport) -> ExitCode {
    if sys || mic || json {
        eprintln!("orator: listen takes no selectors in v1 (got sys={sys} mic={mic} json={json}); run bare `listen`");
        return ExitCode::Usage;
    }
    daemon_passthrough(
        "ListSessions",
        serde_json::json!({}),
        transport,
    )
}

/// Interactive voice chat over the audio endpoint (interleaved mode):
/// typed text plus `/wav <path>` voice turns, `/reset` to start fresh.
/// Spoken replies play locally; transcripts print so the turn stays
/// checkable without audio.
fn chat(config: &OratorConfig) -> ExitCode {
    use std::io::{BufRead, Write as _};
    let model = orator_s2s::S2sConfig::default().model.clone();
    let bundle = orator_core::default_bundle();
    let assured = common_core::runtime::block_on(orator_s2s::ensure_audio_server(
        &audio_base_url(&config.llm),
        &model,
        &bundle,
    ));
    let endpoint = std::sync::Arc::new(match assured {
        Ok(endpoint) => endpoint,
        Err(error) => {
            eprintln!("orator: {error}");
            return ExitCode::Error;
        }
    });
    // A shutdown mid-chat releases the endpoint; the loop below
    // observes the watch between turns and exits interrupted.
    let watch = {
        let endpoint = std::sync::Arc::clone(&endpoint);
        orator_cli::SignalWatch::arm(move || endpoint.release())
    };
    let mut session = orator_s2s::AudioSession::new(orator_s2s::S2sConfig {
        endpoint: endpoint.base_url().to_string(),
        ..orator_s2s::S2sConfig::default()
    });
    let abort = StreamAbort::new();
    println!("orator chat (interleaved voice). /wav <path> speaks a file, /reset restarts, /quit exits.");
    let stdin = std::io::stdin();
    // Pipelined turns: the previous reply plays while the next turn
    // infers, so the gap shrinks without reordering transcripts.
    // `pending_audio` holds at most one reply (moved, never copied).
    // The abort handle cancels the in-flight turn on shutdown signal.
    let mut pending_audio: Option<Vec<f32>> = None;
    for line in stdin.lock().lines() {
        if watch.signalled() {
            abort.cancel();
            endpoint.release();
            return ExitCode::Interrupted;
        }
        let Ok(line) = line else {
            break;
        };
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        if input == "/quit" || input == "/exit" {
            break;
        }
        if input == "/reset" {
            if let Some(samples) = pending_audio.take() {
                if let Err(error) =
                    orator_audio::play_blocking(&samples, orator_s2s::AUDIO_OUT_RATE)
                {
                    eprintln!("orator: {error}");
                    break;
                }
            }
            session.reset();
            println!("context reset");
            continue;
        }
        let (text, audio) = if let Some(path) = input.strip_prefix("/wav ") {
            let wav = match orator_audio::read_wav_mono(std::path::Path::new(path.trim())) {
                Ok(wav) => wav,
                Err(error) => {
                    eprintln!("orator: {error}");
                    continue;
                }
            };
            let samples = if wav.sample_rate == 16_000 {
                wav.samples
            } else {
                resample_linear(&wav.samples, wav.sample_rate, 16_000)
            };
            (None, Some(samples))
        } else {
            (Some(input.to_string()), None)
        };
        // Play the previous reply on a worker while this turn infers
        // on the calling thread; join before printing so order holds.
        let playback = pending_audio.take().map(|samples| {
            std::thread::spawn(move || {
                orator_audio::play_blocking(&samples, orator_s2s::AUDIO_OUT_RATE)
            })
        });
        let turn = session.chat_turn(text.as_deref(), audio.as_deref(), &abort);
        if let Some(worker) = playback {
            match worker.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    eprintln!("orator: {error}");
                    break;
                }
                Err(_) => {
                    eprintln!("orator: playback worker failed");
                    break;
                }
            }
        }
        match turn {
            Ok(turn) => {
                if !turn.text.is_empty() {
                    println!("{}", turn.text);
                }
                if turn.voiced {
                    print!("♪ ");
                    let _ = std::io::stdout().flush();
                    pending_audio = Some(turn.audio);
                }
            }
            Err(error) => {
                eprintln!("orator: {error}");
                endpoint.release();
                return ExitCode::Error;
            }
        }
    }
    if let Some(samples) = pending_audio {
        if let Err(error) = orator_audio::play_blocking(&samples, orator_s2s::AUDIO_OUT_RATE) {
            eprintln!("orator: {error}");
        }
    }
    println!("goodbye");
    endpoint.release();
    ExitCode::Success
}

/// Push-to-talk: start/stop dispatch to session control.
fn ptt(action: &str, transport: Transport) -> ExitCode {
    match action {
        "start" => daemon_passthrough("StartSession", serde_json::json!({ "kind": "dictation" }), transport),
        "stop" => daemon_passthrough("StopSession", serde_json::json!({}), transport),
        _ => {
            eprintln!("orator: ptt expects `start` or `stop`");
            ExitCode::Usage
        }
    }
}

/// Meeting lifecycle: local validation, daemon execution.
fn meeting(action: &str, id: &str, transport: Transport) -> ExitCode {
    match action {
        "start" => daemon_passthrough("StartSession", serde_json::json!({ "kind": "meeting" }), transport),
        "stop" => {
            if id.trim().is_empty() {
                eprintln!("orator: meeting stop needs an id (run `meeting list` first)");
                return ExitCode::Usage;
            }
            daemon_passthrough("StopSession", serde_json::json!({ "id": id }), transport)
        }
        "list" => {
            if !id.trim().is_empty() {
                eprintln!("orator: meeting list takes no id");
                return ExitCode::Usage;
            }
            daemon_passthrough("ListSessions", serde_json::json!({}), transport)
        }
        "status" | "show" | "export" => {
            eprintln!("orator: meeting {action} is not implemented; supported: start/stop/list");
            ExitCode::Usage
        }
        _ => {
            eprintln!("orator: meeting expects start/stop/list (show/export/status are not implemented)");
            ExitCode::Usage
        }
    }
}

/// Propose a shell command from words. Printed, never executed.
fn propose(request: &[String]) -> ExitCode {
    if request.is_empty() {
        eprintln!("orator: cmd needs a request to propose from");
        return ExitCode::Usage;
    }
    println!("# proposed (review before running):");
    println!("{}", request.join(" "));
    ExitCode::Success
}

/// Answer a pending approval through the daemon. Denials are ordinary.
fn approve(id: &str, deny: bool, transport: Transport) -> ExitCode {
    if id.is_empty() {
        eprintln!("orator: approve needs an approval id");
        return ExitCode::Usage;
    }
    if deny {
        eprintln!("orator: approve --deny is not served by the daemon; denial stays local and never maps to Interrupt");
        return ExitCode::Usage;
    }
    daemon_passthrough("Interrupt", serde_json::json!({ "id": id }), transport)
}

/// Local configuration administration.
fn config(action: &str, key: &str, value: &str) -> ExitCode {
    use orator_cli::config::{config_get, config_set};
    match action {
        "path" => {
            let mut candidates = vec![std::path::PathBuf::from("orator.json")];
            candidates.extend(orator_core::config_path());
            let Some(path) = common_core::config::find_hierarchical(&candidates)
                .or_else(orator_core::config_path)
            else {
                eprintln!("orator: no config path (HOME and XDG_DATA_HOME are unset)");
                return ExitCode::Error;
            };
            println!("{}", path.display());
            ExitCode::Success
        }
        "get" => {
            if key.is_empty() {
                eprintln!("orator: config get needs a key");
                return ExitCode::Usage;
            }
            let (loaded, _) = orator_core::config::load_hierarchical();
            match config_get(&loaded, key) {
                Ok(found) => {
                    println!("{found}");
                    ExitCode::Success
                }
                Err(error) => {
                    eprintln!("{error}");
                    ExitCode::Usage
                }
            }
        }
        "set" => {
            if key.is_empty() || value.is_empty() {
                eprintln!("orator: config set needs a key and a value");
                return ExitCode::Usage;
            }
            let mut candidates = vec![std::path::PathBuf::from("orator.json")];
            candidates.extend(orator_core::config_path());
            let Some(path) = common_core::config::find_hierarchical(&candidates)
                .or_else(orator_core::config_path)
            else {
                eprintln!("orator: no config path (HOME and XDG_DATA_HOME are unset)");
                return ExitCode::Error;
            };
            // Missing files start from defaults; malformed files fail
            // loudly instead of silently resetting on write.
            let mut loaded = if path.exists() {
                match common_core::config::load_json::<OratorConfig>(&path) {
                    Ok(config) => config,
                    Err(error) => {
                        eprintln!("orator: cannot parse {}: {error}", path.display());
                        return ExitCode::Error;
                    }
                }
            } else {
                OratorConfig::default()
            };
            if let Err(error) = config_set(&mut loaded, key, value) {
                eprintln!("{error}");
                return ExitCode::Usage;
            }
            let text = match serde_json::to_string_pretty(&loaded) {
                Ok(text) => text,
                Err(error) => {
                    eprintln!("orator: cannot encode config: {error}");
                    return ExitCode::Error;
                }
            };
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    if let Err(error) = std::fs::create_dir_all(parent) {
                        eprintln!("orator: cannot create {}: {error}", parent.display());
                        return ExitCode::Error;
                    }
                }
            }
            match common_core::io::write_atomic(&path, text.as_bytes()) {
                Ok(()) => {
                    println!("{key}={value}");
                    ExitCode::Success
                }
                Err(error) => {
                    eprintln!("orator: cannot write {}: {error}", path.display());
                    ExitCode::Error
                }
            }
        }
        _ => {
            eprintln!("orator: config expects get/set/path");
            ExitCode::Usage
        }
    }
}

/// Daemon lifecycle: run in the foreground for debugging; stop and
/// reload go through the socket.
fn daemon_lifecycle(action: &str, transport: Transport) -> ExitCode {
    match action {
        "run" => {
            eprintln!("orator: `daemon run` starts oratord in the foreground");
            ExitCode::Error
        }
        "stop" => daemon_passthrough("StopDaemon", serde_json::json!({}), transport),
        "reload" => {
            eprintln!("orator: daemon reload is not implemented; restart oratord to reload config");
            ExitCode::Usage
        }
        _ => {
            eprintln!("orator: daemon expects run/stop (reload is not implemented)");
            ExitCode::Usage
        }
    }
}

fn stt_backend(config: &SttConfig) -> Result<WhisperBackend, ExitCode> {
    let Some(dir) = models_dir() else {
        eprintln!("orator: no managed model directory (HOME and XDG_DATA_HOME are unset)");
        return Err(ExitCode::Error);
    };
    let path = dir.join(format!("ggml-{}.bin", config.model));
    if !path.exists() {
        eprintln!(
            "orator: model missing at {} (fetch it with `orator models pull`)",
            path.display()
        );
        return Err(ExitCode::Error);
    }
    let backend = WhisperBackend::new(path, config.language.clone());
    Ok(if config.gpu {
        backend.with_gpu()
    } else {
        backend
    })
}

fn transcribe(
    file: &std::path::Path,
    format: &str,
    backend: &str,
    config: &OratorConfig,
    transport: Transport,
) -> ExitCode {
    let Some(format) = TranscriptFormat::parse(format) else {
        eprintln!("orator: unknown format: {format}");
        return ExitCode::Usage;
    };
    let wav = match orator_audio::read_wav_mono(file) {
        Ok(wav) => wav,
        Err(error) => {
            eprintln!("orator: {error}");
            return ExitCode::Error;
        }
    };
    let samples = if wav.sample_rate == 16_000 {
        wav.samples
    } else {
        resample_linear(&wav.samples, wav.sample_rate, 16_000)
    };
    let selected = resolve_backend(backend, &config.stt.backend);
    if selected == "audio-server" {
        return transcribe_audio_server(&samples, format, config, transport);
    }
    if selected != "whisper" {
        eprintln!("orator: unknown STT backend: {selected}");
        return ExitCode::Usage;
    }
    let backend = match stt_backend(&config.stt) {
        Ok(backend) => backend,
        Err(code) => return code,
    };
    let segments = match backend.decode_blocking(&samples, &StreamAbort::new()) {
        Ok(segments) => segments,
        Err(error) => {
            eprintln!("orator: {error}");
            return ExitCode::Error;
        }
    };
    let cues: Vec<Cue> = segments
        .iter()
        .map(|segment| {
            cue_from_centiseconds(segment.start_cs, segment.end_cs, segment.text.clone())
        })
        .collect();
    // The audio-server path strips LLM thinking blocks; the whisper
    // path keeps acoustic bytes verbatim (a spoken "<think>" stays).
    match render(&cues, format) {
        Ok(text) => {
            print!("{text}");
            ExitCode::Success
        }
        Err(error) => {
            eprintln!("orator: {error}");
            ExitCode::Usage
        }
    }
}

/// Transcribe 16 kHz samples through the audio endpoint (single-shot
/// ASR). Falls back to nothing: endpoint errors report actionably.
fn transcribe_audio_server(
    samples: &[f32],
    format: TranscriptFormat,
    config: &OratorConfig,
    transport: Transport,
) -> ExitCode {
    match run_audio_turn(&audio_base_url(&config.llm), transport, |session| {
        session.transcribe_blocking(samples)
    }) {
        Ok(text) => {
            let text = fluent_llm::thinking::strip_thinking_blocks(&text);
            let cues = vec![audio_server_cue(text, samples.len())];
            match render(&cues, format) {
                Ok(rendered) => {
                    print!("{rendered}");
                    ExitCode::Success
                }
                Err(error) => {
                    eprintln!("orator: {error}");
                    ExitCode::Usage
                }
            }
        }
        Err(orator_s2s::S2sError::Aborted) => ExitCode::Interrupted,
        Err(error) => {
            eprintln!("orator: {error}");
            ExitCode::Error
        }
    }
}

fn dictate_once(seconds: u64, echo_stdout: bool, backend: &str, config: &OratorConfig, transport: Transport) -> ExitCode {
    let duration = match orator_cli::dictate::validate_seconds(seconds) {
        Ok(duration) => duration,
        Err(error) => {
            eprintln!("orator: {error}");
            return ExitCode::Usage;
        }
    };
    let choice = match orator_cli::dictate::capture_choice(&config.audio.backend, &config.audio.mic) {
        Ok(choice) => choice,
        Err(error) => {
            eprintln!("orator: {error}");
            return ExitCode::Usage;
        }
    };
    let mut capture = match match choice {
        orator_cli::dictate::CaptureChoice::Default => CpalBackend::open_default(),
        orator_cli::dictate::CaptureChoice::Named(name) => CpalBackend::open_named(&name),
    } {
        Ok(backend) => backend,
        Err(error) => {
            eprintln!("orator: {error}");
            return ExitCode::Error;
        }
    };
    let rate = capture.sample_rate;
    let deadline = match orator_cli::dictate::checked_deadline(Instant::now(), duration) {
        Ok(deadline) => deadline,
        Err(error) => {
            eprintln!("orator: {error}");
            return ExitCode::Usage;
        }
    };
    let watch = orator_cli::SignalWatch::arm(|| {});
    let mut samples = Vec::with_capacity(orator_cli::dictate::reserve_for(seconds, rate));
    while Instant::now() < deadline {
        if watch.signalled() {
            return ExitCode::Interrupted;
        }
        while let Ok(sample) = capture.consumer.pop() {
            if samples.len() >= orator_cli::dictate::MAX_DICTATE_SAMPLES {
                eprintln!(
                    "orator: capture over the {}-sample bound; truncating",
                    orator_cli::dictate::MAX_DICTATE_SAMPLES
                );
                break;
            }
            samples.push(sample);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let dropped = capture.dropped();
    if dropped > 0 {
        eprintln!("orator: capture dropped {dropped} ring sample(s)");
    }
    // Intentional parity note: whisper keeps acoustic bytes verbatim
    // while the audio-server path strips LLM thinking blocks (see
    // `transcribe` above). Dictation carries what was said.
    let samples = if rate == 16_000 {
        samples
    } else {
        resample_linear(&samples, rate, 16_000)
    };
    let selected = resolve_backend(backend, &config.stt.backend);
    let text = if selected == "audio-server" {
        match dictate_audio_server(&samples, config, transport) {
            Ok(text) => text,
            Err(code) => return code,
        }
    } else if selected == "whisper" {
        match dictate_whisper(&samples, &config.stt) {
            Ok(text) => text,
            Err(code) => return code,
        }
    } else {
        eprintln!("orator: unknown STT backend: {selected}");
        return ExitCode::Usage;
    };
    if echo_stdout {
        println!("{text}");
    }
    let clip = ExternalClipboard::new();
    // Single-print sink report: `OutputOnly` already printed via the
    // ladder, so `--stdout` only prints for real injections. Headless
    // runs report the stdout sink instead of false `Injected` success.
    // `--once` joins with single spaces; selection capture stays
    // document-mode while continuation spacing joins in-window repeats
    // via the ladder memory (see ladder tests). The refine chain
    // records clean text per turn.
    let mut memory = LadderMemory::default();
    let mut history = Vec::new();
    match orator_cli::dictate::finish_turn(&text, &mut memory, &mut history, &clip, &NoChord) {
        Ok(orator_desktop::InjectOutcome::Injected(_)) => {
            if echo_stdout {
                println!("{text}");
            }
            ExitCode::Success
        }
        Ok(orator_desktop::InjectOutcome::OutputOnly(_)) => ExitCode::Success,
        Err(error) => {
            eprintln!("orator: {error}");
            ExitCode::Error
        }
    }
}

/// Dictate through whisper: committed words joined as one injection.
fn dictate_whisper(samples: &[f32], stt: &SttConfig) -> Result<String, ExitCode> {
    let engine = stt_backend(stt)?;
    let segments = engine.decode_blocking(samples, &StreamAbort::new()).map_err(|error| {
        eprintln!("orator: {error}");
        ExitCode::Error
    })?;
    Ok(segments.iter().map(|segment| segment.text.clone()).collect::<Vec<_>>().join(" "))
}

/// Dictate through the audio endpoint. Chat-model thinking blocks are
/// stripped before injection: dictation carries what was said, never
/// how the model thought about it.
fn dictate_audio_server(samples: &[f32], config: &OratorConfig, transport: Transport) -> Result<String, ExitCode> {
    run_audio_turn(&audio_base_url(&config.llm), transport, |session| {
        session.transcribe_blocking(samples)
    })
        .map_err(|error| {
            if error == orator_s2s::S2sError::Aborted {
                return ExitCode::Interrupted;
            }
            eprintln!("orator: {error}");
            ExitCode::Error
        })
        .map(|text| fluent_llm::thinking::strip_thinking_blocks(&text))
}

/// One question through the supervised endpoint: probe the configured
/// address, boot a supervised server when nothing answers, ask, print.
/// One-shot CLI use keeps one-question ownership only when it booted the
/// server itself; the per-process model-load cost is retained by design
/// (no cross-command server lease in v1).
fn ask(question: &str, speak: bool, config: &OratorConfig, transport: Transport) -> ExitCode {
    let llm = config.llm.clone();
    let explicit = std::env::var("LLM_BASE_URL").unwrap_or_default();
    let (base, model) = resolve_llm_base(&llm);
    common_core::runtime::block_on_shared(async {
        // A live endpoint is used as-is; otherwise this command owns a
        // supervised server for exactly one question.
        let supervised: Option<std::sync::Arc<Supervisor>> = if probe_models(
            &base,
            common_core::http::shared_http_client(),
        )
        .await
        .is_err()
            && explicit.is_empty()
        {
            match boot_supervised(&llm).await {
                Ok(server) => Some(std::sync::Arc::new(server)),
                Err(orator_cli::ask::AskFail::Usage(message)) => {
                    eprintln!("{message}");
                    return ExitCode::Usage;
                }
                Err(orator_cli::ask::AskFail::Error(message)) => {
                    eprintln!("{message}");
                    return ExitCode::Error;
                }
            }
        } else {
            None
        };
        // A shutdown mid-question stops the owned server; the step
        // below then fails fast instead of hanging on a dead child.
        let watch = supervised.clone().map(|server| {
            orator_cli::SignalWatch::arm(move || server.stop())
        });
        let live_base = supervised
            .as_ref()
            .map(|server| server.base_url().to_string())
            .unwrap_or(base);
        let mut session = HttpSession::open(
            &live_base,
            &model,
            usize::from(llm.ctx),
            usize::from(llm.n_batch),
        );
        session.push_text(question);
        if watch.as_ref().is_some_and(orator_cli::SignalWatch::signalled) {
            return ExitCode::Interrupted;
        }
        let code = match session.step(&mut orator_core::Sampler::default()) {
            Ok(out) => {
                let reply = out.delta.trim_end().to_string();
                println!("{reply}");
                if speak {
                    match daemon_call_transport(transport, "Say", serde_json::json!({ "text": reply })) {
                        Ok(_) => ExitCode::Success,
                        Err(code) => {
                            eprintln!("{}", orator_cli::ask::speak_failure_note(code));
                            code
                        }
                    }
                } else {
                    ExitCode::Success
                }
            }
            Err(error) => {
                eprintln!("orator: {error}");
                ExitCode::Error
            }
        };
        if watch.as_ref().is_some_and(orator_cli::SignalWatch::signalled) {
            return ExitCode::Interrupted;
        }
        if let Some(server) = supervised {
            server.stop();
        }
        code
    })
}

/// Boot the configured endpoint under supervision: weights resolve from
/// the model home when present, otherwise the boot refuses fast naming
/// the expected path (no 60 s health burn on known-missing files). A
/// GPU opt-in without a visible device refuses before boot; with one
/// it requests full layer offload. Garbage ports are usage errors and
/// never silently auto-reserve elsewhere.
async fn boot_supervised(endpoint: &LlmEndpointConfig) -> Result<Supervisor, orator_cli::ask::AskFail> {
    use orator_cli::ask::AskFail;
    if endpoint.gpu {
        orator_core::gpu::require_gpu("llama-server").map_err(AskFail::Error)?;
    }
    let weights =
        orator_cli::ask::check_ask_weights(endpoint).map_err(AskFail::Error)?;
    let port = orator_cli::ask::parse_ask_port(&endpoint.endpoint).map_err(AskFail::Usage)?;
    let mut config = SupervisorConfig {
        model: endpoint.model.clone(),
        weights: Some(weights),
        ctx: endpoint.ctx,
        n_batch: endpoint.n_batch,
        port,
        ..SupervisorConfig::default()
    };
    config.extra_args.extend(gpu_llm_args(endpoint.gpu));
    config.health_timeout = std::time::Duration::from_secs(60);
    Supervisor::boot(config)
        .await
        .map_err(|error| AskFail::Error(error.to_string()))
}

/// Model management: list, pull, rm, verify. Pulls of unpinned entries
/// refuse before any network; verify re-hashes with SHA-256.
fn models(action: ModelsAction) -> ExitCode {
    match action {
        ModelsAction::List => {
            for row in orator_models::list_models() {
                let state = if row.present {
                    match row.verified {
                        Some(true) => "present verified",
                        Some(false) => "present MISMATCH",
                        None => "present",
                    }
                } else {
                    "missing"
                };
                println!("{} ({}) {state}", row.name, row.kind);
            }
            ExitCode::Success
        }
        ModelsAction::Verify { name } => {
            let rows = orator_models::list_models();
            let report = orator_cli::models::verify_report(&rows, &name, |row_name| {
                let Some(entry) = find_model_entry(row_name) else {
                    return Err(format!("no catalog entry for {row_name}"));
                };
                orator_models::verify_entry(&entry).map_err(|error| error.to_string())
            });
            if report.unknown {
                eprintln!("orator: unknown model: {name}");
                return ExitCode::Usage;
            }
            for line in &report.lines {
                println!("{line}");
            }
            if report.failed { ExitCode::Error } else { ExitCode::Success }
        }
        ModelsAction::Rm { name } => {
            let Some(entry) = find_model_entry(&name) else {
                eprintln!("orator: unknown model: {name}");
                return ExitCode::Usage;
            };
            match orator_models::remove_entry(&entry) {
                Ok(true) => {
                    println!("removed {name}");
                    ExitCode::Success
                }
                Ok(false) => {
                    println!("{name}: not present");
                    ExitCode::Success
                }
                Err(error) => {
                    eprintln!("orator: {error}");
                    ExitCode::Error
                }
            }
        }
        ModelsAction::Pull { name } => {
            let Some(entry) = find_model_entry(&name) else {
                eprintln!("orator: unknown model: {name}");
                return ExitCode::Usage;
            };
            match orator_models::pull_plan(&entry) {
                Ok((repo, file, dest)) => {
                    if dest.exists() && orator_models::verify_entry(&entry) == Ok(true) {
                        println!("{name}: already present and verified");
                        return ExitCode::Success;
                    }
                    eprintln!(
                        "orator: download {repo}/{file} to {} via `orator models pull` once pinned (artifact plane)",
                        dest.display()
                    );
                    ExitCode::Error
                }
                Err(error) => {
                    eprintln!("orator: {error}");
                    ExitCode::Error
                }
            }
        }
    }
}

/// Look up a catalog entry by tag across families.
fn find_model_entry(name: &str) -> Option<orator_models::ModelEntry> {
    use orator_models::ModelKind;
    for kind in [ModelKind::Whisper, ModelKind::Llm, ModelKind::Tts, ModelKind::Audio, ModelKind::Vad] {
        if let Some(entry) = orator_models::find_entry(kind, name) {
            return Some(entry);
        }
    }
    None
}
