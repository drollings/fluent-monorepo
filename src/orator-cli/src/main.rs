#![forbid(unsafe_code)]

//! `orator`: thin client over the `oratord` daemon. Parses one command and
//! either runs it locally or forwards it over the socket.

use std::process::ExitCode as ProcessExit;
use std::time::{Duration, Instant};

use clap::Parser;
use fluent_concurrency::stream::StreamAbort;
use orator_audio::{resample_linear, CpalBackend};
use orator_cli::{
    config::resolve_backend,
    doctor::{full_report, render_full},
    shell::{init_script, write_completions, Shell},
    socket::{
        daemon_call_transport, parse_transport, unavailable_socket_text, unavailable_ws_text,
        Transport,
    },
    transcribe::{audio_server_cue, cue_from_centiseconds, render, Cue, TranscriptFormat},
    Cli, Command, ExitCode,
};
use orator_core::{models_dir, OratorConfig, SttConfig};
use orator_desktop::{ExternalClipboard, LadderMemory, NoChord};
use orator_stt::WhisperBackend;

mod ask_cmd;
mod models_cmd;
mod say;

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
    if cli.insecure {
        // Explicit only: the socket preflight refuses foreign sockets
        // unless this flag (or the env it sets) opts out.
        std::env::set_var("ORATOR_INSECURE", "1");
    }
    let (cfg, config_source) = orator_core::config::load_hierarchical();
    // Removed audio keys refuse loudly: a config that sets them names
    // the removal instead of running with silently ignored fields.
    // `load_hierarchical` falls back to the canonical path when no
    // file exists, so only a real file refusal counts.
    if config_source.is_file() {
        let removed = orator_core::config::removed_audio_keys(&config_source);
        if !removed.is_empty() {
            eprintln!(
                "orator: audio.{} removed from the config (no capture path or AEC gate ever read it); delete the key",
                removed.join(", audio.")
            );
            return ExitCode::Usage;
        }
    }
    match cli.command {
        Command::Doctor => {
            print!("{}", render_full(&full_report(&cfg)));
            println!("config: {}", config_source.display());
            ExitCode::Success
        }
        Command::Models { action } => models_cmd::models(action),
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
        Command::Ask { question, speak } => ask_cmd::ask(&question, speak, &cfg, transport),
        Command::Say { text, backend } => say::say(&text, &backend, &cfg, transport),
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
        Command::Cmd { request } => propose(&request, &cfg, transport),
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
                note_unavailable(transport);
            }
            code
        }
    }
}

/// Name the missing daemon for the selected transport on stderr.
pub(crate) fn note_unavailable(transport: Transport) {
    match transport {
        Transport::Socket => {
            eprintln!(
                "{}",
                unavailable_socket_text(orator_core::socket_path().as_deref())
            );
        }
        Transport::Ws => {
            let (config, _) = orator_core::config::load_hierarchical();
            eprintln!("{}", unavailable_ws_text(&config.daemon.ws_listen));
        }
    }
}

/// Metrics: render a stat file locally, or dial the daemon.
fn stats(file: &str, transport: Transport) -> ExitCode {
    if file.is_empty() {
        daemon_passthrough("GetState", serde_json::json!({}), transport)
    } else {
        match orator_core::bench::BenchLog::read_stat_file(std::path::Path::new(file)) {
            Ok(log) => {
                print!("{}", orator_core::bench::render_stats(&orator_core::bench::histograms(&log)));
                ExitCode::Success
            }
            Err(error) => {
                eprintln!("orator: {error}");
                ExitCode::Error
            }
        }
    }
}

/// Listen-in: capture the default input, transcribe utterances
/// through the daemon, print timestamped lines. Never speaks, never
/// acts — transcripts are untrusted data by construction. `--sys`
/// and `--mic` both select the default input in v1 (separate system
/// tap lands later); `--json` emits JSON lines. Ctrl-C stops.
fn listen(sys: bool, mic: bool, json: bool, transport: Transport) -> ExitCode {
    use orator_cli::transcribe::{decode_transcribe_text, encode_transcribe_audio};
    let _ = (sys, mic);
    let mut capture = match CpalBackend::open_default() {
        Ok(backend) => backend,
        Err(error) => {
            eprintln!("orator: {error}");
            return ExitCode::Error;
        }
    };
    let rate = capture.sample_rate;
    let mut gate = orator_cli::tui::MicGate::new();
    let mut buffered: Vec<f32> = Vec::new();
    let watch = orator_cli::SignalWatch::arm(|| {});
    eprintln!("orator: listening on the default input at {rate} Hz (Ctrl-C stops; transcripts only, never actions)");
    let started = std::time::Instant::now();
    let mut frames_seen: u64 = 0;
    let mut utterances: u64 = 0;
    let mut last_nudge = started;
    loop {
        if watch.signalled() {
            eprintln!("orator: listen stopped after {utterances} utterance(s)");
            return ExitCode::Interrupted;
        }
        let mut batch = Vec::new();
        while let Ok(sample) = capture.consumer.pop() {
            batch.push(sample);
        }
        if batch.is_empty() {
            // No device data at all: after 5 s the input is dead or
            // absent — fail loudly instead of spinning silently
            // forever. Silence (frames flowing, gate closed) is fine
            // and waits below.
            if frames_seen == 0 && started.elapsed() > std::time::Duration::from_secs(5) {
                eprintln!("orator: capture delivered no frames in 5 s (no live input on the default device)");
                return ExitCode::Error;
            }
            if frames_seen > 0 && last_nudge.elapsed() > std::time::Duration::from_secs(30) {
                last_nudge = std::time::Instant::now();
                eprintln!("orator: still listening ({utterances} utterance(s) so far; speak to transcribe)");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            continue;
        }
        frames_seen += 1;
        let samples_16k = if rate == 16_000 {
            batch
        } else {
            resample_linear(&batch, rate, 16_000)
        };
        // Frame the resample output through the mic gate (20 ms
        // frames); hot spans buffer, gate close commits one utterance.
        let mut committed = false;
        for frame in samples_16k.chunks(320) {
            if gate.push(frame) {
                buffered.extend_from_slice(frame);
                if buffered.len() > orator_cli::tui::MAX_PENDING_UTTERANCE {
                    let excess = buffered.len() - orator_cli::tui::MAX_PENDING_UTTERANCE;
                    buffered.drain(..excess);
                }
            } else if !buffered.is_empty() {
                committed = true;
                break;
            }
        }
        if !committed || buffered.is_empty() {
            continue;
        }
        let utterance = std::mem::take(&mut buffered);
        let params = encode_transcribe_audio(&utterance);
        let text = match orator_cli::socket::call_transport(transport, "Transcribe", params) {
            Ok(body) => match decode_transcribe_text(&body) {
                Ok(text) => text,
                Err(error) => {
                    eprintln!("orator: {error}");
                    continue;
                }
            },
            Err(failure) => {
                if let Some(line) = failure.text {
                    eprintln!("{line}");
                }
                continue;
            }
        };
        if text.trim().is_empty() {
            continue;
        }
        utterances += 1;
        last_nudge = std::time::Instant::now();
        if json {
            println!(
                "{}",
                serde_json::json!({ "text": text, "samples": utterance.len() })
            );
        } else {
            println!("{text}");
        }
    }
}

/// Interactive voice chat over the audio endpoint (interleaved mode):
/// typed text plus `/wav <path>` voice turns, `/reset` to start fresh.
/// Spoken replies play locally; transcripts print so the turn stays
/// checkable without audio.
fn chat(config: &OratorConfig) -> ExitCode {
    use std::io::{BufRead, Write as _};
    use orator_cli::config::s2s_base_url;
    let s2s = config.s2s.clone();
    let bundle = orator_core::bundle_from_s2s(&s2s);
    let assured = common_core::runtime::block_on(orator_s2s::ensure_audio_server(
        &s2s_base_url(&s2s),
        &s2s.model,
        &bundle,
        s2s.auto_boot,
    ));
    let endpoint = std::sync::Arc::new(match assured {
        Ok(endpoint) => endpoint,
        Err(error) => {
            eprintln!("orator: {error}");
            return ExitCode::Error;
        }
    });
    // A shutdown mid-chat releases the endpoint and cancels the
    // in-flight turn: the watch closure owns both, so a mid-turn
    // Ctrl-C interrupts `chat_turn` instead of waiting out the
    // stream timeout. The loop below still observes the watch
    // between turns and exits interrupted.
    let abort = StreamAbort::new();
    let watch = {
        let endpoint = std::sync::Arc::clone(&endpoint);
        let turn_abort = abort.clone();
        orator_cli::SignalWatch::arm(move || {
            turn_abort.cancel();
            endpoint.release();
        })
    };
    let mut session = orator_s2s::AudioSession::new(orator_s2s::S2sConfig {
        endpoint: endpoint.base_url().to_string(),
        ..s2s
    });
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
        // Status/show read the live registry: same sessions the
        // daemon serves, no second store. Export needs the diarized
        // transcript store (orator-diar over the event log) — not
        // yet served over the socket, so it names the gap instead of
        // printing an empty file.
        "status" | "show" => {
            if !id.trim().is_empty() {
                eprintln!("orator: meeting {action} takes no id; run `meeting list`");
                return ExitCode::Usage;
            }
            daemon_passthrough("GetState", serde_json::json!({}), transport)
        }
        "export" => {
            eprintln!("orator: meeting export needs a transcript store over the socket (run `meeting list`, then `orator transcribe <archive.wav>`); refusing to write an empty file");
            ExitCode::Usage
        }
        _ => {
            eprintln!("orator: meeting expects start/stop/list/status/show/export");
            ExitCode::Usage
        }
    }
}

/// Propose a shell command from words via the local LLM. Printed,
/// never executed. Falls back to echoing the request when no
/// endpoint answers (honest, never a silent success).
fn propose(request: &[String], config: &OratorConfig, transport: Transport) -> ExitCode {
    if request.is_empty() {
        eprintln!("orator: cmd needs a request to propose from");
        return ExitCode::Usage;
    }
    let want = request.join(" ");
    // Prefer the daemon (shared endpoint, no per-command boot) when
    // it answers; otherwise boot one supervised question like `ask`.
    let daemon_try = match transport {
        Transport::Socket | Transport::Ws => orator_cli::socket::call_transport(
            transport,
            "Ask",
            serde_json::json!({
                "question": format!(
                    "Propose a shell command for this request. Reply with only the command, no explanation, no fences: {want}"
                ),
            }),
        ),
    };
    let proposed = match daemon_try {
        Ok(body) => body
            .get("reply")
            .and_then(|reply| reply.as_str())
            .unwrap_or("")
            .to_string(),
        Err(_) => match propose_via_supervised(&want, config) {
            Ok(text) => text,
            Err(code) => {
                // No inference available: echo honestly so the
                // caller still gets a reviewable line, marked as
                // unrefined.
                println!("# proposed (review before running; no LLM available):");
                println!("{want}");
                return code;
            }
        },
    };
    let proposed = proposed.trim();
    if proposed.is_empty() {
        println!("# proposed (review before running):");
        println!("{want}");
    } else {
        println!("# proposed (review before running):");
        println!("{proposed}");
    }
    ExitCode::Success
}

/// One supervised LLM question for `cmd` (the `ask` precedent):
/// probe the configured address, boot when nothing answers, ask,
/// stop. Returns the reply text or the exit to fall back with.
fn propose_via_supervised(want: &str, config: &OratorConfig) -> Result<String, ExitCode> {
    use orator_cli::config::resolve_llm_base;
    use orator_core::LlmSession as _;
    use orator_llm::{probe_models, HttpSession};
    let llm = config.llm.clone();
    let explicit = std::env::var("LLM_BASE_URL").unwrap_or_default();
    let (base, model) = resolve_llm_base(&llm);
    common_core::runtime::block_on_shared(async {
        let live = probe_models(&base, common_core::http::shared_http_client())
            .await
            .is_ok()
            || !explicit.is_empty();
        if !live {
            return Err(ExitCode::Error);
        }
        let mut session = HttpSession::open(
            &base,
            &model,
            usize::from(llm.ctx),
            usize::from(llm.n_batch),
        );
        session.push_text(&format!(
            "Propose a shell command for this request. Reply with only the command, no explanation, no fences: {want}"
        ));
        match session.step(&mut orator_core::Sampler::default()) {
            Ok(out) => Ok(out.delta.trim().to_string()),
            Err(_) => Err(ExitCode::Error),
        }
    })
}

/// Answer a pending approval through the daemon. Approvals and
/// denials are ordinary answers on the `Approve` method (never an
/// interrupt): the socket caller is trusted, D-Bus callers gate on
/// `Caller::may_approve` before dialing.
fn approve(id: &str, deny: bool, transport: Transport) -> ExitCode {
    if id.is_empty() {
        eprintln!("orator: approve needs an approval id");
        return ExitCode::Usage;
    }
    daemon_passthrough(
        "Approve",
        serde_json::json!({ "id": id, "approve": !deny }),
        transport,
    )
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
        "run" => daemon_run(),
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

/// Run the daemon in the foreground: replace this process with
/// `oratord` from the same directory (or `$PATH`). Never returns on
/// success; fails loudly when no binary resolves.
fn daemon_run() -> ExitCode {
    use std::os::unix::process::CommandExt as _;
    let candidate = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("oratord")))
        .filter(|path| path.is_file())
        .or_else(|| orator_core::paths::find_on_path("oratord"));
    let Some(binary) = candidate else {
        eprintln!("orator: no oratord binary (same directory or $PATH)");
        return ExitCode::Error;
    };
    eprintln!("orator: starting {} in the foreground", binary.display());
    let error = std::process::Command::new(&binary).exec();
    eprintln!("orator: daemon exec failed: {error}");
    ExitCode::Error
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
    match say::run_audio_turn(&config.s2s, transport, |session| {
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
    let mut cap_drops: usize = 0;
    let mut capped = false;
    while Instant::now() < deadline && !capped {
        if watch.signalled() {
            return ExitCode::Interrupted;
        }
        // Drain the ring in one batch: count cap drops, then break
        // the outer loop (never spin to the deadline once capped).
        let mut batch = Vec::new();
        while let Ok(sample) = capture.consumer.pop() {
            batch.push(sample);
        }
        if !batch.is_empty() {
            let (filled, dropped, hit) =
                orator_cli::dictate::apply_cap(samples.len(), batch.len());
            let keep = filled.saturating_sub(samples.len());
            samples.extend(batch.into_iter().take(keep));
            cap_drops += dropped;
            if hit {
                capped = true;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if capped {
        eprintln!(
            "orator: capture over the {}-sample bound; truncating ({cap_drops} sample(s) dropped at the cap)",
            orator_cli::dictate::MAX_DICTATE_SAMPLES
        );
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
    let clip = ExternalClipboard::new();
    // Single print: the ladder never prints; the caller prints the
    // policy text exactly once (`--stdout` echoes injections, the
    // output-only fallback always prints through the same gate).
    // `--once` joins with single spaces; selection capture stays
    // document-mode while continuation spacing joins in-window repeats
    // via the ladder memory (see ladder tests). The refine chain
    // records clean text per turn.
    let mut memory = LadderMemory::default();
    let mut history = Vec::new();
    match orator_cli::dictate::finish_turn(&text, &mut memory, &mut history, &clip, &NoChord) {
        Ok(outcome) => {
            if let Some(line) = orator_cli::dictate::print_text_for(echo_stdout, &text, &outcome) {
                println!("{line}");
            }
            ExitCode::Success
        }
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
    say::run_audio_turn(&config.s2s, transport, |session| {
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

