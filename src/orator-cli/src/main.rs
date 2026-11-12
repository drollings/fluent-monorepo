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
use orator::OratorConfig;
use orator_desktop::{ExternalClipboard, LadderMemory, NoChord};
use orator_voice::stt::WhisperBackend;

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
    let (cfg, config_source) = orator::config::load_hierarchical();
    // Removed audio keys refuse loudly: a config that sets them names
    // the removal instead of running with silently ignored fields.
    // `load_hierarchical` falls back to the canonical path when no
    // file exists, so only a real file refusal counts.
    if config_source.is_file() {
        let removed = orator::config::removed_audio_keys(&config_source);
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
        Command::Transcribe {
            file,
            format,
            backend,
        } => transcribe(&file, &format, &backend, &cfg, transport),
        Command::Ask { question, speak } => ask_cmd::ask(&question, speak, &cfg, transport),
        Command::Say { text, backend } => say::say(&text, &backend, &cfg, transport),
        Command::Listen { sys, mic, json } => listen(sys, mic, json, transport),
        Command::Chat {
            log_format,
            verbosity,
            narration,
            ptt,
        } => chat(&cfg, transport, &log_format, verbosity, &narration, &ptt),
        Command::Status { selector } => {
            if !selector.trim().is_empty() {
                eprintln!("orator: status takes no selector (got {selector:?})");
                return ExitCode::Usage;
            }
            daemon_passthrough("GetState", serde_json::json!({}), transport)
        }
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
                eprintln!("orator: hold-to-talk lives in `orator chat --ptt`");
                ExitCode::Error
            }
        }
    }
}

/// Forward one control call to the daemon; absence is exit 3.
fn daemon_passthrough(method: &str, params: serde_json::Value, transport: Transport) -> ExitCode {
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
                unavailable_socket_text(orator::socket_path().as_deref())
            );
        }
        Transport::Ws => {
            let (config, _) = orator::config::load_hierarchical();
            eprintln!("{}", unavailable_ws_text(&config.daemon.ws_listen));
        }
    }
}

/// Metrics: render a stat file locally, or dial the daemon.
fn stats(file: &str, transport: Transport) -> ExitCode {
    if file.is_empty() {
        daemon_passthrough("GetState", serde_json::json!({}), transport)
    } else {
        match orator_cli::stats::render_stat_file(std::path::Path::new(file)) {
            Ok(rendered) => {
                print!("{rendered}");
                ExitCode::Success
            }
            Err(line) => {
                eprintln!("{line}");
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

/// Scrolling-log speech assistant: the CLI twin of the TUI turn
/// semantics over the fallback dials (daemon first, local specialized
/// second — the same paths the TUI workers dial).
///
/// Behavior change from the previous `chat` (which required the
/// interleaved audio endpoint): turns now run `ask_dial` then
/// `say_dial`, so a missing voice server falls back to the direct
/// endpoint plus local kokoro instead of refusing the session. File
/// turns (`/wav`, `/input`) transcribe through `transcribe_dial`.
/// Output moved to the scrolling log (human text on stderr, JSON
/// lines on stdout) so stdout stays machine-clean; the transcript
/// still shows every reply before any voice error attaches (the
/// `VoiceTurn` rule: words are never lost to a playback problem).
/// Pipelined turns keep FIFO voice order: every turn appends its
/// voiced audio to one per-user queue, and the loop drains a single
/// player while later turns run — order holds structurally, so one
/// user never hears two narrations at once. A barge clears whatever
/// never played instead of talking over it. The turn outcome joins
/// before anything prints so text order holds too. Abort mirrors
/// the TUI: each turn carries a fresh abort (rearmed per spawn) that
/// barge and the shutdown watch cancel; a cancelled turn stays
/// silent and never delivers stale speech. Keys reuse the TUI chords
/// (`tui::map_key`, no second table): line discipline means a chord
/// arrives as its control character followed by Enter; Ctrl+C aborts
/// through the signal watch and Ctrl+D ends input gracefully.
/// Device-draining audio finishes its buffer on barge — the cut sheds
/// queued turns and pending (not-yet-playing) audio.
fn chat(
    config: &OratorConfig,
    transport: Transport,
    log_format_raw: &str,
    verbosity: u8,
    narration_raw: &str,
    ptt_raw: &str,
) -> ExitCode {
    use orator_cli::chat::{asr_serving, parse_chat_line, tts_serving, ChatLine, ChatSlash};
    use orator_cli::chatlog::{emit, parse_log_format, ChatEvent};
    let format = match parse_log_format(log_format_raw) {
        Ok(format) => format,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::Usage;
        }
    };
    let log = |event: ChatEvent| emit(&event, format, verbosity);
    let (asr_model, asr_backend) = asr_serving(config);
    let (tts_model, tts_backend) = tts_serving(config);
    // Native interleaved preference: one model heading both roles
    // through the audio server (the LFM3-Audio shape) is assured once
    // here; success runs turns natively, failure names the remedy and
    // the cascade serves every turn instead. Startup-only cost.
    let prefer_s2s = orator::should_prefer_s2s(config);
    let s2s_shared: Option<S2sShared> = if prefer_s2s {
        // The session serves the agreed role model (heads), falling
        // back to the `s2s.model` leaf only when the heads cannot
        // agree. Bundle backbone follows the same tag through the
        // models table, so assurance, weights, and session agree.
        let session_model = orator::agreed_s2s_model(config);
        let s2s_effective = orator::S2sConfig {
            model: session_model.clone(),
            ..config.s2s.clone()
        };
        match common_core::runtime::block_on(orator_voice::s2s::ensure_audio_server(
            &orator_cli::config::s2s_base_url(&config.s2s),
            &session_model,
            &orator::bundle_from_s2s(&s2s_effective, &config.models, &config.gguf_dir),
            config.s2s.auto_boot,
            config.s2s.gpu,
        )) {
            Ok(endpoint) => {
                let url = endpoint.base_url().to_string();
                let origin = match &endpoint {
                    orator_voice::s2s::AudioEndpoint::Shared(_) => "shared existing server",
                    orator_voice::s2s::AudioEndpoint::Owned { .. } => {
                        "supervised child (s2s.auto_boot=on)"
                    }
                };
                let session = orator_voice::s2s::AudioSession::new(orator_voice::s2s::S2sConfig {
                    endpoint: url.clone(),
                    ..s2s_effective
                });
                log(ChatEvent::System {
                    at: std::time::SystemTime::now(),
                    message: format!(
                        "Mode: Line+S2S | Pipeline: {session_model} (native interleaved) | endpoint: {url} ({origin})"
                    ),
                });
                Some(S2sShared {
                    session: std::sync::Arc::new(std::sync::Mutex::new(session)),
                    endpoint: std::sync::Arc::new(endpoint),
                })
            }
            Err(error) => {
                log(ChatEvent::System {
                    at: std::time::SystemTime::now(),
                    message: format!("native s2s unavailable ({error}); cascade serves instead"),
                });
                None
            }
        }
    } else {
        None
    };
    if s2s_shared.is_none() {
        log(ChatEvent::System {
            at: std::time::SystemTime::now(),
            message: format!(
                "Mode: Line | Pipeline: {asr_model} -> {} -> {tts_model}",
                orator_cli::ask::resolve_ask_model(config),
            ),
        });
    }
    // Current abort holder: barge and the watch cancel whatever turn
    // is live when they land; each spawn rearms a fresh abort so a
    // cancelled turn never poisons the next one (TUI rearm shape).
    let current = std::sync::Arc::new(std::sync::Mutex::new(StreamAbort::new()));
    let watch = {
        let current = std::sync::Arc::clone(&current);
        let s2s_endpoint = s2s_shared
            .as_ref()
            .map(|shared| std::sync::Arc::clone(&shared.endpoint));
        orator_cli::SignalWatch::arm(move || {
            common_core::sync::lock(&current).cancel();
            if let Some(endpoint) = s2s_endpoint {
                endpoint.release();
            }
        })
    };
    // Line intake runs on its own thread (bounded: pasting floods
    // back-pressures instead of queuing without bound); the loop
    // polls lines, keys-as-lines, and the turn job on one cadence.
    let (line_tx, line_rx) = std::sync::mpsc::sync_channel::<String>(16);
    std::thread::spawn(move || {
        use std::io::BufRead as _;
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(line) => {
                    if line_tx.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    let mut voice_mode = orator_cli::chat::initial_voice_mode(config);
    if orator_cli::chat::parse_voice_mode(config.chat.voice.trim()).is_err() {
        log(ChatEvent::System {
            at: std::time::SystemTime::now(),
            message: format!(
                "chat.voice={:?} unknown (expected off|short|full); using short",
                config.chat.voice
            ),
        });
    }
    let mut text_mode = orator_cli::chat::initial_text_mode(config);
    if orator_cli::chat::parse_text_mode(config.chat.text.trim()).is_err() {
        log(ChatEvent::System {
            at: std::time::SystemTime::now(),
            message: format!(
                "chat.text={:?} unknown (expected short|full); using full",
                config.chat.text
            ),
        });
    }
    let mut narration_mode = if narration_raw.trim().is_empty() {
        let mode = orator_cli::narration::initial_narration_mode(config);
        if orator_cli::narration::parse_narration_level(config.chat.narration.trim()).is_err() {
            log(ChatEvent::System {
                at: std::time::SystemTime::now(),
                message: format!(
                    "chat.narration={:?} unknown (expected none|intro|summary|full); using none",
                    config.chat.narration
                ),
            });
        }
        mode
    } else {
        match orator_cli::narration::parse_narration_level(narration_raw.trim()) {
            Ok(mode) => mode,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::Usage;
            }
        }
    };
    let mut ptt_mode = if ptt_raw.trim().is_empty() {
        let mode = orator_cli::ptt::initial_ptt_mode(config);
        if orator_cli::ptt::parse_ptt_mode(config.chat.ptt.trim()).is_err() {
            log(ChatEvent::System {
                at: std::time::SystemTime::now(),
                message: format!(
                    "chat.ptt={:?} unknown (expected off|on); using off",
                    config.chat.ptt
                ),
            });
        }
        mode
    } else {
        match orator_cli::ptt::parse_ptt_mode(ptt_raw.trim()) {
            Ok(mode) => mode,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::Usage;
            }
        }
    };
    if !matches!(config.chat.mic.trim(), "auto" | "ptt") {
        log(ChatEvent::System {
            at: std::time::SystemTime::now(),
            message: format!(
                "chat.mic={:?} unknown (expected auto|ptt); using auto",
                config.chat.mic
            ),
        });
    }
    // Legacy alias carry: a file that sets `chat.mic=ptt` without the
    // newer `chat.ptt` still starts latched-off, exactly as before.
    if ptt_mode == orator_cli::ptt::PttMode::Off && config.chat.mic.trim() == "ptt" {
        ptt_mode = orator_cli::ptt::PttMode::On;
        log(ChatEvent::System {
            at: std::time::SystemTime::now(),
            message: "chat.mic=ptt honored (legacy alias of chat.ptt=on)".to_string(),
        });
    }
    let mut mic_mode = orator_cli::chat::mic_for_ptt(ptt_mode);
    let mut talking = false;
    // Ordered overlap: at most one voiced reply waits while the next
    // turn runs (moved samples plus rate, never copied twice).
    // Voice queue: every turn appends its voiced audio here instead
    // of playing it, and the loop drains one item at a time through
    // the single playout below. Turns may run while earlier replies
    // still drain — order holds structurally (FIFO per user), so a
    // single user never hears two narrations at once, and a barge
    // clears what never played instead of talking over it. Shared
    // with the turn workers so Full narration streams chunk by chunk
    // while the loop drains (first audio lands after the first
    // chunk, not after the whole reply).
    let voice_queue: VoiceQueue =
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    // Prompt playout: a completed short reply voices at once when
    // the session is idle instead of waiting for a next turn that
    // may never come (a single exchange still talks back). Quiet
    // while a turn runs or the mic is hot — those hold for overlap.
    // At most one playout runs; a new turn cuts a live one so
    // replies never double-talk and order holds, and barge stops
    // the device within one poll instead of draining the buffer.
    let mut direct_play: Option<DirectPlay> = None;
    // At most one turn runs; its outcome returns over this job.
    let mut turn_job: Option<std::sync::mpsc::Receiver<TurnEnd>> = None;
    // The live turn's question-word slot (companion to `turn_job`):
    // the worker publishes its transcribed or typed text as soon as
    // known, and a barge harvests it into `pending_partial` below.
    // Cleared wherever `turn_job` resolves.
    let mut turn_partial: Option<std::sync::Arc<std::sync::Mutex<Option<String>>>> = None;
    // Words preserved from a barged turn: prepended verbatim to the
    // next turn's transcription, then consumed. Set only by a barge
    // that harvested a published question.
    let mut pending_partial: Option<String> = None;
    let mut input_closed = false;
    // Stashed lines that arrived while a turn ran (bounded: past
    // 16 the oldest sheds with a counter — typed intent is never
    // silently dropped, and FIFO order holds because stashed lines
    // predate fresh channel lines).
    let mut stash: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let mut stash_shed: u64 = 0;
    // Live mic (when a capture device answers): VAD-gated utterance
    // turns in auto mode, push-to-talk in PTT mode — the TUI capture
    // shape over the same gate, remainder domain, and cap. Absence
    // refuses once and stays line-only; it never fails the session.
    let mut mic = match orator_audio::CpalBackend::open_default() {
        Ok(backend) => {
            let rate = backend.sample_rate;
            log(ChatEvent::System {
                at: std::time::SystemTime::now(),
                message: if ptt_mode == orator_cli::ptt::PttMode::On {
                    format!("mic: live on default input ({rate} Hz, push-to-talk) — /talk to latch")
                } else {
                    format!("mic: live on default input ({rate} Hz, auto VAD) — speak or type")
                },
            });
            Some(MicCapture::with_config(backend, &config.vad))
        }
        Err(error) => {
            log(ChatEvent::Error {
                at: std::time::SystemTime::now(),
                message: format!("orator: mic unavailable ({error}); line input stays"),
            });
            None
        }
    };
    // Voice barge-in starts off the `chat.barge` leaf (default on):
    // talking over a reply cuts the live turn and stops playout at
    // once, and the utterance becomes the next turn. Toggle off sheds
    // mic input while a turn runs instead. Key barge always works.
    let mut barge_on = orator_cli::chat::initial_barge_on(config);
    // Saturated-daemon cooldown: a refused mic turn sheds instead of
    // hot-looping the bound (the TUI rule). Talk-off endpoints the
    // buffered turn even when the VAD still holds speech open.
    let mut cooldown_until: Option<std::time::Instant> = None;
    // Room-tail hold: capture stays suppressed briefly after any
    // playout ends so the room tail decays instead of endpointing
    // as a phantom turn.
    let mut quiet_until: Option<std::time::Instant> = None;
    let mut last_shed_line: Option<std::time::Instant> = None;
    // Over-cap capture drops (oldest-first, bounded buffer): counted
    // per session, reported on shed lines.
    let mut mic_dropped: u64 = 0;
    let mut ptt_end = false;
    // Sustained-speech continuity: the only temporal home is the
    // shared turn driver (bound from the `turn.barge_in_ms` leaf);
    // the loop keeps no cumulative counter of its own.
    let mut barge_sustain = orator_cli::chat::BargeSustain::new(config);
    // Loop cadence shared by line intake, capture drain, and the
    // barge-in continuity quantum: one tick, never three clocks.
    let poll_ms = u64::from(config.turn.poll_ms);
    loop {
        if watch.signalled() {
            return ExitCode::Interrupted;
        }
        // Ready turn outcome first: order holds because only one turn
        // runs and nothing prints until its outcome joins.
        // Reap a finished prompt playout first, every poll: capture
        // holds while one is live, so only polling keeps the mic
        // alive between turns. A reaped playout holds capture past
        // the room tail.
        if reap_direct_play(&mut direct_play, &log, mic.as_mut()) {
            quiet_until = Some(
                std::time::Instant::now()
                    + std::time::Duration::from_millis(u64::from(config.turn.tail_hold_ms)),
            );
        }
        if let Some(job) = turn_job.as_ref() {
            if let Ok(end) = job.try_recv() {
                turn_job = None;
                turn_partial = None;
                if let Some(until) = end.cool_until {
                    cooldown_until = Some(until);
                }
            }
        }
        // Drain the voice queue FIFO through the single player: one
        // narration at a time per user, in turn order. The drain runs
        // while turns compute (pipelined), but never over a hot mic
        // or a live playout — a barge clears the queue instead of
        // talking over it. Evaluated every poll so a reply held
        // during a hot-mic moment voices as soon as the room goes
        // quiet instead of muting the mic with no turn to consume it.
        // Spawning sheds the tail like a finished playout: the
        // speakers' first syllables never commit as a phantom turn.
        {
            let (speaking, buffered) = mic.as_ref().map_or((false, false), |mic| {
                (mic.gate.speaking(), !mic.utterance.is_empty())
            });
            if orator_cli::chat::should_voice_now(orator_cli::chat::VoiceNow {
                mic_speaking: speaking,
                utterance_buffered: buffered,
                playout_live: direct_play.is_some(),
            }) {
                if let Some(item) = common_core::sync::lock(&voice_queue).pop_front() {
                    if let Some(mic) = mic.as_mut() {
                        if !mic.gate.speaking() {
                            mic.shed();
                        }
                    }
                    // Reference feeds from the playout itself as the
                    // device drains it — never the whole buffer upfront.
                    direct_play = Some(DirectPlay::spawn(item.samples, item.rate));
                }
            }
        }
        // Capture drain on every poll: hot spans buffer unless the
        // PTT mic is cold. While a turn voices, capture suppresses
        // unless voice barge-in is on, in which case the mic stays
        // live and an endpoint during voicing cuts the turn instead.
        // The tail hold below sheds the room tail after every playout.
        if let Some(mic) = mic.as_mut() {
            let voicing_lock = narration_mode != orator_cli::narration::NarrationLevel::None
                && (turn_job.is_some()
                    || !common_core::sync::lock(&voice_queue).is_empty()
                    || direct_play.is_some()
                    || orator_cli::chat::tail_hold_active(quiet_until, std::time::Instant::now()));
            let ptt_cold = mic_mode == orator_cli::tui::MicMode::PushToTalk && !talking;
            orator_cli::tui::drain_capture(
                &mut || mic.backend.consumer.pop().ok(),
                &mut mic.carry,
                &mut mic.remainder_16k,
                mic.backend.sample_rate,
                &mut mic.gate,
                &mut mic.utterance,
                &mut mic_dropped,
                (!voicing_lock || barge_on) && !ptt_cold,
            );
            // Voice barge: VAD-hot AND sustained past the
            // `turn.barge_in_ms` bound cuts the turn it interrupts
            // (the utterance stays buffered and becomes the next
            // turn). Key barge sheds the queue; voice barge keeps
            // the words. Sustained means continuous speech: the
            // tracker advances only on hot polls that grew the
            // buffer, so pauses never accumulate.
            let sustained_past_bound =
                barge_sustain.poll(mic.gate.speaking(), mic.utterance.len(), ptt_cold, poll_ms);
            if orator_cli::chat::should_cut_for_barge(&orator_cli::chat::BargeCutInput {
                turn_live: turn_job.is_some(),
                barge_on,
                gate_quiet: !mic.utterance.is_empty() && !mic.gate.speaking(),
                sustained_past_bound,
            }) {
                barge_turn(
                    &current,
                    &mut turn_job,
                    &mut turn_partial,
                    &mut pending_partial,
                    &voice_queue,
                    &mut direct_play,
                    &log,
                );
            }
            // PTT toggle-off endpoints the buffered turn even when the
            // VAD still holds speech open (consumed once, never stale).
            let endpoint = ptt_end;
            ptt_end = false;
            let cooling = cooldown_until.is_some_and(|until| std::time::Instant::now() < until);
            if cooling && !mic.utterance.is_empty() && !mic.gate.speaking() {
                let shed = mic.take().len();
                let due = last_shed_line.is_none_or(|at| {
                    at.elapsed() > std::time::Duration::from_secs(config.turn.shed_notice_secs)
                });
                if due {
                    last_shed_line = Some(std::time::Instant::now());
                    log(ChatEvent::System {
                        at: std::time::SystemTime::now(),
                        message: format!(
                            "daemon saturated; shedding mic input until it drains ({shed} samples shed, {mic_dropped} over-cap total)"
                        ),
                    });
                }
            } else if turn_job.is_none()
                && !cooling
                && !mic.utterance.is_empty()
                && (!mic.gate.speaking() || endpoint)
            {
                let samples = mic.take();
                cooldown_until = None;
                if cut_direct_play(&mut direct_play, &log, Some(mic)) {
                    // The cut playout stopped at once: hold capture
                    // past the room tail so it never endpoints as a
                    // phantom.
                    quiet_until = Some(
                        std::time::Instant::now()
                            + std::time::Duration::from_millis(u64::from(config.turn.tail_hold_ms)),
                    );
                }
                let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
                turn_partial = Some(std::sync::Arc::clone(&slot));
                turn_job = Some(spawn_turn(TurnSpec {
                    input: TurnInput::File(samples),
                    voice_queue: std::sync::Arc::clone(&voice_queue),
                    transport,
                    abort: fresh_abort(&current),
                    config: config.clone(),
                    format,
                    verbosity,
                    asr: (asr_model.clone(), asr_backend.clone()),
                    tts: (tts_model.clone(), tts_backend.clone()),
                    narration: narration_mode,
                    mic: (mic_mode, talking),
                    s2s: s2s_shared.clone(),
                    partial_slot: slot,
                    prepend_question: pending_partial.take(),
                }));
            }
        }
        // Stashed lines (older) drain before fresh channel lines.
        // Control chords handle even mid-turn — barge cuts the live
        // worker at its next stage boundary — while content lines
        // stash until the turn frees.
        let line = if turn_job.is_none() {
            stash.pop_front().map_or_else(
                || match line_rx.recv_timeout(std::time::Duration::from_millis(poll_ms)) {
                    Ok(line) => Some(line),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        input_closed = true;
                        None
                    }
                },
                Some,
            )
        } else {
            match line_rx.recv_timeout(std::time::Duration::from_millis(poll_ms)) {
                Ok(line) => Some(line),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    input_closed = true;
                    None
                }
            }
        };
        let Some(line) = line else {
            if input_closed && turn_job.is_none() {
                break;
            }
            continue;
        };
        if let Some(action) = orator_cli::chat::parse_control_line(&line) {
            match action {
                orator_cli::tui::TuiAction::Barge => {
                    barge_turn(
                        &current,
                        &mut turn_job,
                        &mut turn_partial,
                        &mut pending_partial,
                        &voice_queue,
                        &mut direct_play,
                        &log,
                    );
                }
                orator_cli::tui::TuiAction::Quit => break,
                orator_cli::tui::TuiAction::Mode => {
                    cycle_chat_mic(&mut mic_mode, &mut ptt_mode, &mut talking, &log);
                }
                orator_cli::tui::TuiAction::Talk => {
                    toggle_chat_talk(mic_mode, &mut talking, &mut ptt_end, &log);
                }
                orator_cli::tui::TuiAction::Start
                | orator_cli::tui::TuiAction::Stop
                | orator_cli::tui::TuiAction::List => {
                    log(ChatEvent::System {
                        at: std::time::SystemTime::now(),
                        message: "sessions live in tui; chat is sessionless".to_string(),
                    });
                }
                orator_cli::tui::TuiAction::Type(_)
                | orator_cli::tui::TuiAction::Backspace
                | orator_cli::tui::TuiAction::Send
                | orator_cli::tui::TuiAction::Ignore => {}
            }
            continue;
        }
        let Some(parsed) = parse_chat_line(&line) else {
            continue;
        };
        // A turn is live: content lines stash (bounded,
        // FIFO); mode lines act immediately.
        if turn_job.is_some() {
            match &parsed {
                ChatLine::Slash(
                    ChatSlash::Voice(_)
                    | ChatSlash::Text(_)
                    | ChatSlash::Mic(_)
                    | ChatSlash::Talk
                    | ChatSlash::Ptt(_)
                    | ChatSlash::Barge(_)
                    | ChatSlash::Narration(_)
                    | ChatSlash::Set { .. }
                    | ChatSlash::Reset,
                ) => {}
                ChatLine::Slash(ChatSlash::Quit) => {
                    common_core::sync::lock(&current).cancel();
                    turn_job = None;
                    // A quit with a live turn cuts its queued voice
                    // too: the goodbye below must not narrate a turn
                    // that never completed.
                    common_core::sync::lock(&voice_queue).clear();
                    break;
                }
                ChatLine::Slash(ChatSlash::Wav(_) | ChatSlash::Input(_)) | ChatLine::Say(_) => {
                    if stash.len() >= 16 {
                        stash.pop_front();
                        stash_shed += 1;
                        log(ChatEvent::System {
                            at: std::time::SystemTime::now(),
                            message: format!(
                                "input queue full; shedding oldest ({stash_shed} shed)"
                            ),
                        });
                    }
                    stash.push_back(line.clone());
                    continue;
                }
            }
        }
        match parsed {
            ChatLine::Slash(ChatSlash::Quit) => break,
            ChatLine::Slash(ChatSlash::Reset) => {
                common_core::sync::lock(&voice_queue).clear();
                if let Some(shared) = s2s_shared.as_ref() {
                    common_core::sync::lock(&shared.session).reset();
                }
                log(ChatEvent::System {
                    at: std::time::SystemTime::now(),
                    message: "context reset".to_string(),
                });
            }
            ChatLine::Slash(ChatSlash::Voice(mode)) => {
                voice_mode = mode;
                narration_mode = voice_to_narration(mode);
                log(ChatEvent::System {
                    at: std::time::SystemTime::now(),
                    message: format!(
                        "voice: {} (legacy, narration: {})",
                        voice_label(voice_mode),
                        narration_label(narration_mode)
                    ),
                });
            }
            ChatLine::Slash(ChatSlash::Text(mode)) => {
                text_mode = mode;
                log(ChatEvent::System {
                    at: std::time::SystemTime::now(),
                    message: format!(
                        "text: {} (legacy, replies always show full)",
                        text_label(text_mode)
                    ),
                });
            }
            ChatLine::Slash(ChatSlash::Narration(mode)) => {
                narration_mode = mode;
                voice_mode = narration_to_voice(mode);
                log(ChatEvent::System {
                    at: std::time::SystemTime::now(),
                    message: format!("narration: {}", narration_label(narration_mode)),
                });
            }
            ChatLine::Slash(ChatSlash::Set { key, value }) => {
                apply_set_command(
                    &key,
                    &value,
                    &mut narration_mode,
                    &mut voice_mode,
                    &mut text_mode,
                    &mut mic_mode,
                    &mut ptt_mode,
                    &mut talking,
                    &mut barge_on,
                    &log,
                );
            }
            ChatLine::Slash(ChatSlash::Mic(mode)) => {
                mic_mode = mode;
                ptt_mode = orator_cli::chat::ptt_for_mic(mode);
                talking = false;
                log(ChatEvent::System {
                    at: std::time::SystemTime::now(),
                    message: format!(
                        "mic: {} (legacy alias of ptt: {})",
                        mic_label(mic_mode),
                        ptt_label(ptt_mode)
                    ),
                });
            }
            ChatLine::Slash(ChatSlash::Talk) => {
                toggle_chat_talk(mic_mode, &mut talking, &mut ptt_end, &log);
            }
            ChatLine::Slash(ChatSlash::Ptt(mode)) => {
                ptt_mode = mode;
                mic_mode = orator_cli::chat::mic_for_ptt(mode);
                talking = false;
                log(ChatEvent::System {
                    at: std::time::SystemTime::now(),
                    message: format!("ptt: {}", ptt_label(ptt_mode)),
                });
            }
            ChatLine::Slash(ChatSlash::Barge(on)) => {
                barge_on = on;
                log(ChatEvent::System {
                    at: std::time::SystemTime::now(),
                    message: format!(
                        "voice barge: {}",
                        if on {
                            "on (talk over a reply to cut it)"
                        } else {
                            "off (mic sheds while a turn runs)"
                        }
                    ),
                });
            }
            ChatLine::Slash(ChatSlash::Wav(path) | ChatSlash::Input(path)) => {
                if path.trim().is_empty() {
                    log(ChatEvent::Error {
                        at: std::time::SystemTime::now(),
                        message: "orator: /input needs a WAV file path".to_string(),
                    });
                    continue;
                }
                match read_input_samples(std::path::Path::new(path.trim())) {
                    Ok(samples) => {
                        if cut_direct_play(&mut direct_play, &log, mic.as_mut()) {
                            // The cut playout stopped at once: hold
                            // capture past the room tail so it never
                            // endpoints as a phantom.
                            quiet_until = Some(
                                std::time::Instant::now()
                                    + std::time::Duration::from_millis(u64::from(
                                        config.turn.tail_hold_ms,
                                    )),
                            );
                        }
                        let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
                        turn_partial = Some(std::sync::Arc::clone(&slot));
                        turn_job = Some(spawn_turn(TurnSpec {
                            input: TurnInput::File(samples),
                            voice_queue: std::sync::Arc::clone(&voice_queue),
                            transport,
                            abort: fresh_abort(&current),
                            config: config.clone(),
                            format,
                            verbosity,
                            asr: (asr_model.clone(), asr_backend.clone()),
                            tts: (tts_model.clone(), tts_backend.clone()),
                            narration: narration_mode,
                            mic: (mic_mode, talking),
                            s2s: s2s_shared.clone(),
                            partial_slot: slot,
                            prepend_question: pending_partial.take(),
                        }));
                    }
                    Err(error) => {
                        log(ChatEvent::Error {
                            at: std::time::SystemTime::now(),
                            message: error,
                        });
                    }
                }
            }
            ChatLine::Say(text) => {
                if text.trim().is_empty() {
                    continue;
                }
                if cut_direct_play(&mut direct_play, &log, mic.as_mut()) {
                    // The cut playout stopped at once: hold capture
                    // past the room tail so it never endpoints as a
                    // phantom.
                    quiet_until = Some(
                        std::time::Instant::now()
                            + std::time::Duration::from_millis(u64::from(config.turn.tail_hold_ms)),
                    );
                }
                let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
                turn_partial = Some(std::sync::Arc::clone(&slot));
                turn_job = Some(spawn_turn(TurnSpec {
                    input: TurnInput::Text(text),
                    voice_queue: std::sync::Arc::clone(&voice_queue),
                    transport,
                    abort: fresh_abort(&current),
                    config: config.clone(),
                    format,
                    verbosity,
                    asr: (asr_model.clone(), asr_backend.clone()),
                    tts: (tts_model.clone(), tts_backend.clone()),
                    narration: narration_mode,
                    mic: (mic_mode, talking),
                    s2s: s2s_shared.clone(),
                    partial_slot: slot,
                    prepend_question: pending_partial.take(),
                }));
            }
        }
    }
    // A quit with a live turn cuts it first: nothing delivers stale
    // speech after the goodbye. The assured endpoint releases here.
    if turn_job.is_some() {
        common_core::sync::lock(&current).cancel();
    }
    if let Some(shared) = s2s_shared.as_ref() {
        shared.endpoint.release();
    }
    // Finish an in-flight prompt playout first, then drain the
    // queued remainder in order: goodbye order matches live order,
    // and nothing delivers stale speech after it.
    join_direct_play(&mut direct_play, &log, mic.as_mut());
    loop {
        let item = common_core::sync::lock(&voice_queue).pop_front();
        let Some(item) = item else { break };
        if let Err(error) = orator_audio::play_blocking(&item.samples, item.rate) {
            eprintln!("orator: {error}");
        }
    }
    log(ChatEvent::System {
        at: std::time::SystemTime::now(),
        message: "goodbye".to_string(),
    });
    ExitCode::Success
}

/// One finished turn: loop signals only — voice output rides the
/// shared queue, never the outcome. All logging happens on the
/// worker before sending, so the loop only stores. A barged job
/// goes unread, and its queued audio was already cleared — never
/// stale speech.
struct TurnEnd {
    /// Saturated daemon refused a mic turn: shed instead of
    /// hot-looping the bound until this instant passes.
    cool_until: Option<std::time::Instant>,
}

/// One turn's input: typed text, or file samples for the ASR path.
enum TurnInput {
    /// Typed line (already committed text, no audio involved).
    Text(String),
    /// File samples at 16 kHz (gated, then transcribed).
    File(Vec<f32>),
}

/// Everything one turn worker needs (all owned: the worker outlives
/// the loop poll that spawned it when barged).
struct TurnSpec {
    /// Typed text or file samples.
    input: TurnInput,
    /// Per-user FIFO voice queue: narration appends here (Full
    /// streams chunk by chunk) and the loop drains one item at a
    /// time through the single player, so turns overlap compute
    /// with playout but never double-talk.
    voice_queue: VoiceQueue,
    /// Daemon transport for the fallback dials.
    transport: Transport,
    /// Turn abort (barge/watch cancel; rearmed per spawn).
    abort: StreamAbort,
    /// Loaded config (roles drive selection).
    config: OratorConfig,
    /// Log wire shape.
    format: orator_cli::chatlog::LogFormat,
    /// Agent detail gate.
    verbosity: u8,
    /// ASR server label (model, backend).
    asr: (String, String),
    /// TTS server label (model, backend).
    tts: (String, String),
    /// Narration level (authoritative for the spoken portion).
    narration: orator_cli::narration::NarrationLevel,
    /// Mic gate for file turns (mode, talk latched on).
    mic: (orator_cli::tui::MicMode, bool),
    /// Best-effort slot for this turn's question words: the worker
    /// publishes its transcribed (or typed) text as soon as known,
    /// so a barge can harvest the cut turn's words instead of losing
    /// them with the dropped job.
    partial_slot: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// Words preserved from a barged turn, prepended verbatim to this
    /// turn's transcription (or typed text).
    prepend_question: Option<String>,
    /// Assured native interleaved session (`None` on the cascade):
    /// shared across turns (history accrues), released at quit.
    s2s: Option<S2sShared>,
}

/// Assured native session shared by every turn of one chat: the
/// interleaved history lives here, so turns stay in context without
/// re-sending it. One turn runs at a time; the loop holds the other
/// handle for `/reset` and quit release.
#[derive(Clone)]
/// Assured native session shared by every turn of one chat: the
/// interleaved history lives here, so turns stay in context without
/// re-sending it. One turn runs at a time; the loop holds the other
/// handle for `/reset` and quit release.
struct S2sShared {
    /// Interleaved session (locked per turn).
    session: std::sync::Arc<std::sync::Mutex<orator_voice::s2s::AudioSession>>,
    /// Assured endpoint (released at quit).
    endpoint: std::sync::Arc<orator_voice::s2s::AudioEndpoint>,
}

/// Rearm: a fresh abort becomes the live one; the watch cancels it
/// mid-turn through the shared holder.
fn fresh_abort(current: &std::sync::Arc<std::sync::Mutex<StreamAbort>>) -> StreamAbort {
    *common_core::sync::lock(current) = StreamAbort::new();
    common_core::sync::lock(current).clone()
}

/// One prompt playout: the worker handle plus its cut flag. Barge
/// fires the flag and detaches — the worker stops the device within
/// one poll instead of draining the buffer. Order still holds: a new
/// turn cuts (never joins) a live playout, and quit still joins for
/// an orderly goodbye.
struct DirectPlay {
    /// Playout worker (abortable drain).
    worker: std::thread::JoinHandle<Result<(), orator_audio::AudioError>>,
    /// Cut flag the worker polls: fired by barge and turn starts.
    cut: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl DirectPlay {
    /// Spawn a playout worker for `samples` at `rate`.
    fn spawn(samples: Vec<f32>, rate: u32) -> Self {
        use std::sync::atomic::AtomicBool;
        let cut = std::sync::Arc::new(AtomicBool::new(false));
        let worker_cut = std::sync::Arc::clone(&cut);
        let worker = std::thread::spawn(move || {
            orator_audio::play_blocking_abortable(&samples, rate, &worker_cut).map(|_| ())
        });
        Self { worker, cut }
    }

    /// Whether the worker finished (reapable without blocking).
    fn finished(&self) -> bool {
        self.worker.is_finished()
    }
}

/// Cut the live turn and stop playout at once: cancel, harvest the
/// turn's published question words (never drops them unread), drop
/// the job (a detached worker runs silent to completion — at most
/// one stray per barge), clear queued voice that never played, and
/// cut the prompt playout so the device stops within one poll.
/// Harvested words wait in `pending_partial` for the next turn's
/// transcription, which prepends them verbatim.
fn barge_turn(
    current: &std::sync::Arc<std::sync::Mutex<StreamAbort>>,
    turn_job: &mut Option<std::sync::mpsc::Receiver<TurnEnd>>,
    turn_partial: &mut Option<std::sync::Arc<std::sync::Mutex<Option<String>>>>,
    pending_partial: &mut Option<String>,
    voice_queue: &VoiceQueue,
    direct_play: &mut Option<DirectPlay>,
    log: &impl Fn(orator_cli::chatlog::ChatEvent),
) {
    use orator_cli::chatlog::ChatEvent;
    common_core::sync::lock(current).cancel();
    if let Some(slot) = turn_partial.take() {
        if let Some(words) = common_core::sync::lock(&slot).take() {
            let chars = words.chars().count();
            *pending_partial = Some(words);
            log(ChatEvent::System {
                at: std::time::SystemTime::now(),
                message: format!(
                    "barge: kept {chars} chars of the cut turn for the next transcription"
                ),
            });
        }
    }
    *turn_job = None;
    common_core::sync::lock(voice_queue).clear();
    cut_direct_play(direct_play, &|_| {}, None);
    *common_core::sync::lock(current) = StreamAbort::new();
    log(ChatEvent::System {
        at: std::time::SystemTime::now(),
        message: "barge: cut the in-flight turn".to_string(),
    });
}

/// Cycle the file-turn mic gate (same order as the TUI). Leaving PTT
/// unlatches talk; PTT state tracks the mic both ways.
fn cycle_chat_mic(
    mic_mode: &mut orator_cli::tui::MicMode,
    ptt_mode: &mut orator_cli::ptt::PttMode,
    talking: &mut bool,
    log: &impl Fn(orator_cli::chatlog::ChatEvent),
) {
    use orator_cli::chatlog::ChatEvent;
    use orator_cli::tui::MicMode;
    *mic_mode = match mic_mode {
        MicMode::Auto => MicMode::PushToTalk,
        MicMode::PushToTalk => MicMode::Auto,
    };
    *ptt_mode = orator_cli::chat::ptt_for_mic(*mic_mode);
    *talking = false;
    log(ChatEvent::System {
        at: std::time::SystemTime::now(),
        message: format!(
            "mic: {} (ptt: {})",
            mic_label(*mic_mode),
            ptt_label(*ptt_mode)
        ),
    });
}

/// Toggle the PTT talk latch. Outside PTT mode this names the remedy
/// instead of acting (the TUI rule).
fn toggle_chat_talk(
    mic_mode: orator_cli::tui::MicMode,
    talking: &mut bool,
    ptt_end: &mut bool,
    log: &impl Fn(orator_cli::chatlog::ChatEvent),
) {
    use orator_cli::chatlog::ChatEvent;
    use orator_cli::tui::MicMode;
    if mic_mode != MicMode::PushToTalk {
        log(ChatEvent::System {
            at: std::time::SystemTime::now(),
            message: "talk is for push-to-talk mode (/ptt on to switch)".to_string(),
        });
        return;
    }
    *talking = !*talking;
    if !*talking {
        // Toggle-off endpoints the buffered turn even when the VAD
        // still holds speech open (consumed once by the loop).
        *ptt_end = true;
    }
    log(ChatEvent::System {
        at: std::time::SystemTime::now(),
        message: if *talking {
            "talk: mic hot — file turns send".to_string()
        } else {
            "talk: mic off — file turns shed".to_string()
        },
    });
}

/// Live capture state for one chat session: the device backend plus
/// the gate, resample carry, 16 kHz remainder, and buffered hot-span
/// utterance. Mirrors the TUI capture shape.
struct MicCapture {
    /// Open input device (dropping stops the stream).
    backend: orator_audio::CpalBackend,
    /// VAD gate scoring capture frames.
    gate: orator_cli::tui::MicGate,
    /// Device-rate tail awaiting resample.
    carry: Vec<f32>,
    /// 16 kHz remainder keeping its own domain across polls.
    remainder_16k: Vec<f32>,
    /// Buffered hot-span samples awaiting endpointing.
    utterance: Vec<f32>,
}

impl MicCapture {
    /// Capture state over an open `backend` with the gate tuned from
    /// `VadConfig`: the single tunable surface for live capture.
    fn with_config(backend: orator_audio::CpalBackend, vad: &orator::VadConfig) -> Self {
        Self {
            backend,
            gate: orator_cli::tui::MicGate::with_config(vad),
            carry: Vec::new(),
            remainder_16k: Vec::new(),
            utterance: Vec::new(),
        }
    }

    /// Take the buffered utterance, leaving an empty buffer.
    fn take(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.utterance)
    }

    /// Shed the buffered utterance (tail, barge cut): never
    /// transcribed. The gate resets so a shed tail cannot commit as a
    /// phantom turn.
    fn shed(&mut self) {
        self.utterance.clear();
        self.gate.reset();
    }
}

/// Spawn one turn worker; its outcome returns over the job receiver.
/// The worker logs every event itself (thread-safe sinks), so a
/// barged job simply goes unread — never stale speech.
fn spawn_turn(spec: TurnSpec) -> std::sync::mpsc::Receiver<TurnEnd> {
    let (tx, rx) = std::sync::mpsc::channel::<TurnEnd>();
    std::thread::spawn(move || {
        let _ = tx.send(run_turn_job(&spec));
    });
    rx
}

/// Run one turn: overlap-play the previous reply while inferring,
/// then ask, then voice. Every stage checks the abort first; a
/// cancelled turn stays silent (no stale delivery after a barge).
/// The outcome names whether audio reached the device (tail shed)
/// and whether a saturated refusal starts a cooldown.
/// One queued voice reply: PCM samples at their rate. Turns append;
/// the loop drains FIFO through the single [`DirectPlay`] below, so
/// at most one narration ever reaches the device at a time.
struct VoiceItem {
    /// Voiced samples (24 kHz for kokoro, native rate otherwise).
    samples: Vec<f32>,
    /// Sample rate of `samples`.
    rate: u32,
}

/// Per-user FIFO voice queue, shared by the loop and the live turn
/// worker: Full narration appends chunk by chunk while the loop
/// drains ahead of it. A barge clears what never played.
type VoiceQueue = std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<VoiceItem>>>;

/// A cancelled or empty turn: nothing queued, no cooldown. Stays
/// silent by construction (the caller already checked the abort
/// before logging anything stale).
fn silent_end() -> TurnEnd {
    TurnEnd { cool_until: None }
}

fn run_turn_job(spec: &TurnSpec) -> TurnEnd {
    use orator_cli::chatlog::{emit, ChatEvent};
    let log = |event: ChatEvent| emit(&event, spec.format, spec.verbosity);
    if spec.abort.is_cancelled() {
        return silent_end();
    }
    // Typed turns publish their words at once: a barge before the
    // first dial still preserves exactly what the user typed.
    if let TurnInput::Text(text) = &spec.input {
        *common_core::sync::lock(&spec.partial_slot) = Some(text.clone());
    }
    // Resolve the turn input: typed lines are committed text; file
    // turns gate on the mic (PTT latch, then speech presence). Native
    // turns feed audio straight to the interleaved session (the model
    // hears it — no separate transcription); cascade turns transcribe
    // first, then serve through the ask dial. Voice from earlier turns
    // drains from the shared queue on the loop (single player, FIFO),
    // so this worker never plays audio itself and overlapping turns
    // can never double-talk.
    let (text, audio): (Option<String>, Option<Vec<f32>>) = match &spec.input {
        TurnInput::Text(text) => (
            Some(orator_cli::chat::merge_preserved_question(
                spec.prepend_question.as_deref(),
                text,
            )),
            None,
        ),
        TurnInput::File(samples) => {
            if let Err(gate) =
                orator_cli::chat::check_file_gate(spec.mic.0, spec.mic.1, samples, &spec.config.vad)
            {
                match gate {
                    orator_cli::chat::FileGate::PttOff { samples } => {
                        log(ChatEvent::System {
                            at: std::time::SystemTime::now(),
                            message: format!(
                                "mic off; shedding file turn ({samples} samples) — /talk to send"
                            ),
                        });
                    }
                    orator_cli::chat::FileGate::NoSpeech => {
                        log(ChatEvent::Error {
                            at: std::time::SystemTime::now(),
                            message: "orator: no speech detected; empty transcript".to_string(),
                        });
                    }
                }
                return TurnEnd { cool_until: None };
            }
            if spec.abort.is_cancelled() {
                return silent_end();
            }
            (None, Some(samples.clone()))
        }
    };
    if text.as_deref().is_some_and(|text| text.trim().is_empty()) {
        return TurnEnd { cool_until: None };
    }
    if spec.abort.is_cancelled() {
        return silent_end();
    }
    // Native interleaved: the assured session hears audio directly.
    if let Some(shared) = spec.s2s.as_ref() {
        return native_turn(spec, shared, text.as_deref(), audio.as_deref(), &log);
    }
    // Cascade: audio decodes to text first, then the ask dial serves.
    let question = match (text, audio) {
        (Some(text), _) => text,
        (None, Some(samples)) => match transcribe_leg(spec, &samples, &log) {
            Ok((text, _)) => text,
            Err(end) => return end,
        },
        (None, None) => {
            return TurnEnd { cool_until: None };
        }
    };
    cascade_turn(spec, &question, &log)
}

/// Decode one file leg through the fallback dial (cascade audio and
/// native-fallback turns share it): ASR event on success, Error
/// event plus an optional saturated cooldown on refusal. `Err`
/// carries the finished outcome when the loop should move to the
/// next line; `Ok` hands the text and its latency back. Voice from
/// earlier turns drains from the shared queue — never through here.
fn transcribe_leg(
    spec: &TurnSpec,
    samples: &[f32],
    log: &impl Fn(orator_cli::chatlog::ChatEvent),
) -> Result<(String, u64), TurnEnd> {
    use orator_cli::chatlog::ChatEvent;
    match timed_transcribe(samples, spec.transport, &spec.abort, &spec.config) {
        Ok((text, latency_ms, leg)) => {
            if spec.abort.is_cancelled() {
                return Err(silent_end());
            }
            // Publish the transcribed words at once, then merge any
            // words preserved from a barged turn: the cut turn's
            // question survives verbatim ahead of the fresh text.
            *common_core::sync::lock(&spec.partial_slot) = Some(text.clone());
            let merged =
                orator_cli::chat::merge_preserved_question(spec.prepend_question.as_deref(), &text);
            // Name what actually decoded: the daemon leg serves the
            // configured voice model; the local leg keeps the
            // precomputed role-walk label.
            let (model, backend) = match leg {
                orator_cli::fallback::DialLeg::Daemon => {
                    orator_cli::chat::daemon_asr_serving(&spec.config)
                }
                orator_cli::fallback::DialLeg::Local => spec.asr.clone(),
                orator_cli::fallback::DialLeg::Shared => {
                    let model = orator::resolve_role_model(&spec.config.roles, "asr")
                        .unwrap_or_else(|| spec.config.s2s.model.clone());
                    (model, "audio-server".to_string())
                }
            };
            log(ChatEvent::Asr {
                at: std::time::SystemTime::now(),
                text: merged.clone(),
                latency_ms,
                model,
                backend,
            });
            Ok((merged, latency_ms))
        }
        Err(error) => {
            if spec.abort.is_cancelled() {
                return Err(silent_end());
            }
            let saturated = error.to_ascii_lowercase().contains("saturated");
            log(ChatEvent::Error {
                at: std::time::SystemTime::now(),
                message: error,
            });
            Err(TurnEnd {
                cool_until: saturated.then(|| {
                    std::time::Instant::now()
                        + std::time::Duration::from_secs(spec.config.turn.saturated_cooldown_secs)
                }),
            })
        }
    }
}

/// One cascade turn: ask dial for the reply, then voice it. Used by
/// cascade lines and by native turns whose server advanced down the
/// chain (voiceless or failed voice turns fall through honestly).
/// Voice appends to the shared queue — the loop drains it, so this
/// worker never plays audio itself.
fn cascade_turn(
    spec: &TurnSpec,
    question: &str,
    log: &impl Fn(orator_cli::chatlog::ChatEvent),
) -> TurnEnd {
    use orator_cli::chat::llm_serving;
    use orator_cli::chatlog::ChatEvent;
    let turn_started = std::time::Instant::now();
    let reply =
        orator_cli::fallback::ask_dial(question, None, spec.transport, &spec.abort, &spec.config);
    if spec.abort.is_cancelled() {
        return silent_end();
    }
    let reply = match reply {
        Ok(reply) => reply,
        Err(error) => {
            log(ChatEvent::Error {
                at: std::time::SystemTime::now(),
                message: error,
            });
            return TurnEnd { cool_until: None };
        }
    };
    let (llm_model, _) = llm_serving(&spec.config);
    // Narration displays the full reply verbatim; the spoken portion
    // below selects how much of it voices.
    log(ChatEvent::Agent {
        at: std::time::SystemTime::now(),
        message: reply.clone(),
        latency_ms: turn_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        model: llm_model,
        detail: false,
    });
    let Some(spoken) = spoken_for_narration_turn(&reply, spec, &log) else {
        return TurnEnd { cool_until: None };
    };
    if spoken.trim().is_empty() || spec.abort.is_cancelled() {
        return TurnEnd { cool_until: None };
    }
    voice_reply(&spoken, spec, &log)
}

/// Spoken text for one cascade reply under the turn's narration
/// level: none voices nothing, intro voices the first sentence, full
/// voices everything, summary voices first plus an LLM-compressed
/// middle plus last. Summary failures fall back to first-plus-last
/// so the turn still voices instead of failing the reply.
fn spoken_for_narration_turn(
    reply: &str,
    spec: &TurnSpec,
    log: &impl Fn(orator_cli::chatlog::ChatEvent),
) -> Option<String> {
    use orator_cli::narration::{
        compose_summary_spoken, first_sentence_of, last_sentence_of, middle_text_of,
        spoken_for_narration, NarrationLevel,
    };
    match spec.narration {
        NarrationLevel::None => None,
        NarrationLevel::Intro | NarrationLevel::Full => spoken_for_narration(reply, spec.narration),
        NarrationLevel::Summary => {
            if reply.trim().is_empty() || spec.abort.is_cancelled() {
                return None;
            }
            let first = first_sentence_of(reply);
            let last = last_sentence_of(reply);
            let middle = middle_text_of(reply);
            if middle.trim().is_empty() {
                let composed = compose_summary_spoken(&first, "", &last);
                return if composed.trim().is_empty() {
                    None
                } else {
                    Some(composed)
                };
            }
            let prompt = orator_cli::narration::build_summary_prompt(&middle);
            match orator_cli::fallback::ask_dial(
                &prompt,
                None,
                spec.transport,
                &spec.abort,
                &spec.config,
            ) {
                Ok(summary) => {
                    if spec.abort.is_cancelled() {
                        return None;
                    }
                    let composed = compose_summary_spoken(&first, &summary, &last);
                    if composed.trim().is_empty() {
                        None
                    } else {
                        Some(composed)
                    }
                }
                Err(error) => {
                    log(orator_cli::chatlog::ChatEvent::Error {
                        at: std::time::SystemTime::now(),
                        message: format!("orator: summary voice fell back to ends ({error})"),
                    });
                    let composed = compose_summary_spoken(&first, "", &last);
                    if composed.trim().is_empty() {
                        None
                    } else {
                        Some(composed)
                    }
                }
            }
        }
    }
}

/// One native interleaved turn against the assured session: text
/// and/or audio in, reply text plus 24 kHz audio out, history
/// accruing on the session. Voice appends to the shared queue — the
/// loop drains it, so native turns never play audio themselves. A
/// voiceless or failed voice turn advances down the cascade honestly
/// (the shared fallback classifier); anything else reports with the
/// transcript intact.
#[allow(clippy::too_many_lines)]
/// One native interleaved turn against the assured session: text
/// and/or audio in, reply text plus 24 kHz audio out, history
/// accruing on the session. Voice appends to the shared queue — the
/// loop drains it, so native turns never play audio themselves. A
/// voiceless or failed voice turn advances down the cascade honestly
/// (the shared fallback classifier); anything else reports with the
/// transcript intact.
fn native_turn(
    spec: &TurnSpec,
    shared: &S2sShared,
    text: Option<&str>,
    audio: Option<&[f32]>,
    log: &impl Fn(orator_cli::chatlog::ChatEvent),
) -> TurnEnd {
    use orator_cli::chatlog::ChatEvent;
    if text.is_none() && audio.is_none_or(<[f32]>::is_empty) {
        return TurnEnd { cool_until: None };
    }
    let turn_started = std::time::Instant::now();
    let outcome = {
        let mut session = common_core::sync::lock(&shared.session);
        session.chat_turn(text, audio, &spec.abort)
    };
    if spec.abort.is_cancelled() {
        return silent_end();
    }
    match outcome {
        Ok(turn) => {
            log(ChatEvent::Agent {
                at: std::time::SystemTime::now(),
                message: turn.text.clone(),
                latency_ms: turn_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                model: orator::agreed_s2s_model(&spec.config),
                detail: false,
            });
            if matches!(spec.narration, orator_cli::narration::NarrationLevel::None) {
                return TurnEnd { cool_until: None };
            }
            if matches!(spec.narration, orator_cli::narration::NarrationLevel::Full)
                && !turn.audio.is_empty()
            {
                log(ChatEvent::Tts {
                    at: std::time::SystemTime::now(),
                    message: format!("Voicing {} samples... [Native]", turn.audio.len()),
                    latency_ms: turn_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                    model: orator::agreed_s2s_model(&spec.config),
                    backend: "audio-server".to_string(),
                });
                common_core::sync::lock(&spec.voice_queue).push_back(VoiceItem {
                    samples: turn.audio,
                    rate: orator_voice::s2s::AUDIO_OUT_RATE,
                });
                return TurnEnd { cool_until: None };
            }
            if matches!(
                spec.narration,
                orator_cli::narration::NarrationLevel::Intro
                    | orator_cli::narration::NarrationLevel::Summary
            ) {
                // Intro/summary voice only a slice: ignore native audio
                // and synthesize the narrated portion instead.
                let Some(spoken) = spoken_for_narration_turn(&turn.text, spec, &log) else {
                    return TurnEnd { cool_until: None };
                };
                if spoken.trim().is_empty() || spec.abort.is_cancelled() {
                    return TurnEnd { cool_until: None };
                }
                return voice_reply(&spoken, spec, &log);
            }
            if !turn.audio.is_empty() {
                log(ChatEvent::Tts {
                    at: std::time::SystemTime::now(),
                    message: format!("Voicing {} samples... [Native]", turn.audio.len()),
                    latency_ms: turn_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                    model: orator::agreed_s2s_model(&spec.config),
                    backend: "audio-server".to_string(),
                });
                common_core::sync::lock(&spec.voice_queue).push_back(VoiceItem {
                    samples: turn.audio,
                    rate: orator_voice::s2s::AUDIO_OUT_RATE,
                });
                return TurnEnd { cool_until: None };
            }
            // The chat stream carried no audio deltas (stock server):
            // voice the reply text through POST /tts — the same
            // fallback one-shot `say` uses — instead of printing
            // silently. A server without /tts advances down the voice
            // chain like any voiceless turn.
            let tts_audio = {
                let session = common_core::sync::lock(&shared.session);
                session.speak_text_via_tts(&turn.text, &spec.abort)
            };
            match tts_audio {
                Ok(clips) if !clips.is_empty() => {
                    log(ChatEvent::Tts {
                        at: std::time::SystemTime::now(),
                        message: format!("Voicing {} samples... [Native]", clips.len()),
                        latency_ms: turn_started.elapsed().as_millis().min(u128::from(u64::MAX))
                            as u64,
                        model: orator::agreed_s2s_model(&spec.config),
                        backend: "audio-server".to_string(),
                    });
                    common_core::sync::lock(&spec.voice_queue).push_back(VoiceItem {
                        samples: clips,
                        rate: orator_voice::s2s::AUDIO_OUT_RATE,
                    });
                    return TurnEnd { cool_until: None };
                }
                Err(error) if !orator_voice::s2s::should_advance_chain(&error) => {
                    if spec.abort.is_cancelled() {
                        return silent_end();
                    }
                    log(ChatEvent::Error {
                        at: std::time::SystemTime::now(),
                        message: format!("orator: {error}"),
                    });
                    return TurnEnd { cool_until: None };
                }
                Ok(_) | Err(_) => {}
            }
            if spec.abort.is_cancelled() {
                return silent_end();
            }
            // Advance: twin of the Err-branch fall-through below
            // (audio legs decode first, exactly like cascade audio
            // turns).
            let question = match (text, audio) {
                (Some(text), _) => text.to_string(),
                (None, Some(samples)) => match transcribe_leg(spec, samples, &log) {
                    Ok((text, _)) => text,
                    Err(end) => return end,
                },
                (None, None) => {
                    return TurnEnd { cool_until: None };
                }
            };
            cascade_turn(spec, &question, &log)
        }
        Err(error) => {
            if spec.abort.is_cancelled() {
                return silent_end();
            }
            if !orator_voice::s2s::should_advance_chain(&error) {
                log(ChatEvent::Error {
                    at: std::time::SystemTime::now(),
                    message: format!("orator: {error}"),
                });
                return TurnEnd { cool_until: None };
            }
            // Advance: this turn falls through to the cascade (audio
            // legs decode first, exactly like cascade audio turns).
            let question = match (text, audio) {
                (Some(text), _) => text.to_string(),
                (None, Some(samples)) => match transcribe_leg(spec, samples, &log) {
                    Ok((text, _)) => text,
                    Err(end) => return end,
                },
                (None, None) => {
                    return TurnEnd { cool_until: None };
                }
            };
            cascade_turn(spec, &question, &log)
        }
    }
}

/// Reap a finished prompt playout without stalling the loop:
/// errors log, and the tail sheds exactly like an overlap
/// delivery — but only when the gate is quiet, so a live user
/// utterance is never shed as a tail. A still-running playout
/// stays; the next turn cuts it before starting so order holds.
/// Runs every poll (not just at turn completion): capture holds
/// while a playout is live, so a finished-but-unreaped worker would
/// mute the mic with no turn to reap it.
fn reap_direct_play(
    slot: &mut Option<DirectPlay>,
    log: &impl Fn(orator_cli::chatlog::ChatEvent),
    mic: Option<&mut MicCapture>,
) -> bool {
    if !slot.as_ref().is_some_and(DirectPlay::finished) {
        return false;
    }
    let Some(play) = slot.take() else {
        return false;
    };
    if !join_playback(Some(play.worker), log) {
        return false;
    }
    if let Some(mic) = mic {
        if !mic.gate.speaking() {
            mic.shed();
        }
    }
    true
}

/// Cut a live prompt playout before a new turn starts: the device
/// stops within one poll instead of draining the buffer, so replies
/// never double-talk and order holds. A finished worker joins to
/// surface its outcome; a live one detaches after the flag fires.
/// Reports whether audio reached the device (the tail-shed signal)
/// — a cut counts, an error does not.
fn cut_direct_play(
    slot: &mut Option<DirectPlay>,
    log: &impl Fn(orator_cli::chatlog::ChatEvent),
    mic: Option<&mut MicCapture>,
) -> bool {
    let Some(play) = slot.take() else {
        return false;
    };
    if play.finished() {
        if !join_playback(Some(play.worker), log) {
            return false;
        }
    } else {
        play.cut.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    if let Some(mic) = mic {
        if !mic.gate.speaking() {
            mic.shed();
        }
    }
    true
}

/// Join a live prompt playout at quit: the goodbye keeps live order
/// (cutting here would truncate the farewell). Bounded by the reply
/// length; the common case finds nothing live.
fn join_direct_play(
    slot: &mut Option<DirectPlay>,
    log: &impl Fn(orator_cli::chatlog::ChatEvent),
    mic: Option<&mut MicCapture>,
) -> bool {
    let Some(play) = slot.take() else {
        return false;
    };
    if !join_playback(Some(play.worker), log) {
        return false;
    }
    if let Some(mic) = mic {
        if !mic.gate.speaking() {
            mic.shed();
        }
    }
    true
}

/// Join the overlap playback worker. Failures log and continue: a
/// device blip on a stale reply never fails the live turn. Reports
/// whether audio reached the device (the tail-shed signal).
fn join_playback(
    playback: Option<std::thread::JoinHandle<Result<(), orator_audio::AudioError>>>,
    log: &impl Fn(orator_cli::chatlog::ChatEvent),
) -> bool {
    use orator_cli::chatlog::ChatEvent;
    let Some(worker) = playback else {
        return false;
    };
    match worker.join() {
        Ok(Ok(())) => true,
        Ok(Err(error)) => {
            log(ChatEvent::Error {
                at: std::time::SystemTime::now(),
                message: format!("orator: {error}"),
            });
            false
        }
        Err(_) => {
            log(ChatEvent::Error {
                at: std::time::SystemTime::now(),
                message: "orator: playback worker failed".to_string(),
            });
            false
        }
    }
}

/// Voice one reply under narration: none voices nothing; intro
/// and summary synthesize whole and queue for the loop player; full
/// voices the complete reply chunk-streamed into the queue (each
/// chunk synthesizes then queues while the loop drains ahead of it,
/// so first audio lands early and barge clears the remainder).
/// Failures attach to the transcript, never erase it. Queued — not
/// played — here: the loop owns the single player, so turns never
/// double-talk no matter how fast they complete.
fn voice_reply(
    spoken: &str,
    spec: &TurnSpec,
    log: &impl Fn(orator_cli::chatlog::ChatEvent),
) -> TurnEnd {
    use orator_cli::chatlog::ChatEvent;
    use orator_cli::narration::NarrationLevel;
    match spec.narration {
        NarrationLevel::None => TurnEnd { cool_until: None },
        NarrationLevel::Intro | NarrationLevel::Summary => {
            let tts_started = std::time::Instant::now();
            match orator_cli::fallback::say_dial(spoken, spec.transport, &spec.abort, &spec.config)
            {
                Ok((samples, rate)) => {
                    if spec.abort.is_cancelled() {
                        return silent_end();
                    }
                    let words = spoken.split_whitespace().count();
                    log(ChatEvent::Tts {
                        at: std::time::SystemTime::now(),
                        message: format!("Synthesizing {words} words... [Ready]"),
                        latency_ms: tts_started.elapsed().as_millis().min(u128::from(u64::MAX))
                            as u64,
                        model: spec.tts.0.clone(),
                        backend: spec.tts.1.clone(),
                    });
                    common_core::sync::lock(&spec.voice_queue)
                        .push_back(VoiceItem { samples, rate });
                    TurnEnd { cool_until: None }
                }
                Err(error) => {
                    log(ChatEvent::Error {
                        at: std::time::SystemTime::now(),
                        message: error,
                    });
                    TurnEnd { cool_until: None }
                }
            }
        }
        NarrationLevel::Full => {
            let words = spoken.split_whitespace().count();
            log(ChatEvent::Tts {
                at: std::time::SystemTime::now(),
                message: format!("Synthesizing {words} words... [Streaming]"),
                latency_ms: 0,
                model: spec.tts.0.clone(),
                backend: spec.tts.1.clone(),
            });
            stream_full_voice(spoken, spec, log);
            TurnEnd { cool_until: None }
        }
    }
}

/// Chunk-stream one full reply into the shared voice queue:
/// per-chunk synth through the fallback dial, queued as ready. A
/// failed chunk keeps what queued and attaches the error; a barge
/// between chunks clears the queue (the loop cleared it on cancel)
/// so the remainder never plays. Single-chunk input collapses to
/// one identical synth.
fn stream_full_voice(spoken: &str, spec: &TurnSpec, log: &impl Fn(orator_cli::chatlog::ChatEvent)) {
    use orator_cli::chatlog::ChatEvent;
    let chunks = orator_cli::say_chain::normalized_chunks(spoken);
    if chunks.iter().all(|chunk| chunk.trim().is_empty()) {
        return;
    }
    let rate = orator_voice::tts::KOKORO_SAMPLE_RATE;
    let mut queued_chunks = 0usize;
    for chunk in &chunks {
        if spec.abort.is_cancelled() {
            break;
        }
        match orator_cli::fallback::say_dial(chunk, spec.transport, &spec.abort, &spec.config) {
            Ok((mut samples, chunk_rate)) => {
                if chunk_rate != rate && !samples.is_empty() {
                    samples = orator_audio::resample_linear(&samples, chunk_rate, rate);
                }
                // Order holds structurally: chunks append in order
                // while the loop drains ahead, so playback overlaps
                // synthesis without a second player.
                common_core::sync::lock(&spec.voice_queue).push_back(VoiceItem { samples, rate });
                queued_chunks += 1;
            }
            Err(error) => {
                log(ChatEvent::Error {
                    at: std::time::SystemTime::now(),
                    message: format!(
                        "orator: local voice failed after {queued_chunks}/{} chunks: {error}",
                        chunks.len()
                    ),
                });
                return;
            }
        }
    }
}

/// Short label for a voice mode in the System log.
fn voice_label(mode: orator_cli::chat::ChatVoiceMode) -> &'static str {
    match mode {
        orator_cli::chat::ChatVoiceMode::Off => "off",
        orator_cli::chat::ChatVoiceMode::Short => "short",
        orator_cli::chat::ChatVoiceMode::Full => "full",
    }
}

/// Short label for a narration level in the System log.
fn narration_label(mode: orator_cli::narration::NarrationLevel) -> &'static str {
    mode.as_str()
}

/// Legacy voice mode mapped onto narration (single source stays in
/// `narration`; this only keeps `/voice` working after the move).
fn voice_to_narration(
    mode: orator_cli::chat::ChatVoiceMode,
) -> orator_cli::narration::NarrationLevel {
    use orator_cli::narration::NarrationLevel;
    match mode {
        orator_cli::chat::ChatVoiceMode::Off => NarrationLevel::None,
        orator_cli::chat::ChatVoiceMode::Short => NarrationLevel::Intro,
        orator_cli::chat::ChatVoiceMode::Full => NarrationLevel::Full,
    }
}

/// Narration mapped back onto the closest legacy voice mode, so the
/// voicing lock and legacy labels stay consistent after `/narration`.
fn narration_to_voice(
    mode: orator_cli::narration::NarrationLevel,
) -> orator_cli::chat::ChatVoiceMode {
    use orator_cli::narration::NarrationLevel;
    match mode {
        NarrationLevel::None => orator_cli::chat::ChatVoiceMode::Off,
        NarrationLevel::Intro | NarrationLevel::Summary => orator_cli::chat::ChatVoiceMode::Short,
        NarrationLevel::Full => orator_cli::chat::ChatVoiceMode::Full,
    }
}

/// Apply one `/set <key> <value>` line to the live session modes.
/// Unknown keys and bad spellings stay spoken (never dispatched),
/// so the caller logs nothing here — the parse already decided.
#[allow(clippy::too_many_arguments)]
/// Apply one `/set <key> <value>` line to the live session modes.
/// Unknown keys and bad spellings stay spoken (never dispatched),
/// so the caller logs nothing here — the parse already decided.
fn apply_set_command(
    key: &str,
    value: &str,
    narration_mode: &mut orator_cli::narration::NarrationLevel,
    voice_mode: &mut orator_cli::chat::ChatVoiceMode,
    text_mode: &mut orator_cli::chat::ChatTextMode,
    mic_mode: &mut orator_cli::tui::MicMode,
    ptt_mode: &mut orator_cli::ptt::PttMode,
    talking: &mut bool,
    barge_on: &mut bool,
    log: &impl Fn(orator_cli::chatlog::ChatEvent),
) {
    use orator_cli::chatlog::ChatEvent;
    match key {
        "narration" => {
            if let Ok(mode) = orator_cli::narration::parse_narration_level(value) {
                *narration_mode = mode;
                *voice_mode = narration_to_voice(mode);
                log(ChatEvent::System {
                    at: std::time::SystemTime::now(),
                    message: format!("narration: {}", narration_label(*narration_mode)),
                });
            }
        }
        "voice" => {
            if let Ok(mode) = orator_cli::chat::parse_voice_mode(value) {
                *voice_mode = mode;
                *narration_mode = voice_to_narration(mode);
                log(ChatEvent::System {
                    at: std::time::SystemTime::now(),
                    message: format!(
                        "voice: {} (legacy, narration: {})",
                        voice_label(*voice_mode),
                        narration_label(*narration_mode)
                    ),
                });
            }
        }
        "text" => {
            if let Ok(mode) = orator_cli::chat::parse_text_mode(value) {
                *text_mode = mode;
                log(ChatEvent::System {
                    at: std::time::SystemTime::now(),
                    message: format!(
                        "text: {} (legacy, replies always show full)",
                        text_label(*text_mode)
                    ),
                });
            }
        }
        "mic" => match value {
            "auto" => {
                *mic_mode = orator_cli::tui::MicMode::Auto;
                *ptt_mode = orator_cli::chat::ptt_for_mic(*mic_mode);
                *talking = false;
                log(ChatEvent::System {
                    at: std::time::SystemTime::now(),
                    message: format!(
                        "mic: {} (legacy alias of ptt: {})",
                        mic_label(*mic_mode),
                        ptt_label(*ptt_mode)
                    ),
                });
            }
            "ptt" => {
                *mic_mode = orator_cli::tui::MicMode::PushToTalk;
                *ptt_mode = orator_cli::chat::ptt_for_mic(*mic_mode);
                *talking = false;
                log(ChatEvent::System {
                    at: std::time::SystemTime::now(),
                    message: format!(
                        "mic: {} (legacy alias of ptt: {})",
                        mic_label(*mic_mode),
                        ptt_label(*ptt_mode)
                    ),
                });
            }
            _ => {}
        },
        "ptt" => {
            if let Ok(mode) = orator_cli::ptt::parse_ptt_mode(value) {
                *ptt_mode = mode;
                *mic_mode = orator_cli::chat::mic_for_ptt(mode);
                *talking = false;
                log(ChatEvent::System {
                    at: std::time::SystemTime::now(),
                    message: format!("ptt: {}", ptt_label(*ptt_mode)),
                });
            }
        }
        "barge" => match value {
            "on" => {
                *barge_on = true;
                log(ChatEvent::System {
                    at: std::time::SystemTime::now(),
                    message: "voice barge: on (talk over a reply to cut it)".to_string(),
                });
            }
            "off" => {
                *barge_on = false;
                log(ChatEvent::System {
                    at: std::time::SystemTime::now(),
                    message: "voice barge: off (mic sheds while a turn runs)".to_string(),
                });
            }
            _ => {}
        },
        _ => {}
    }
}

/// Short label for a push-to-talk state in the System log.
fn ptt_label(mode: orator_cli::ptt::PttMode) -> &'static str {
    mode.as_str()
}

/// Short label for a mic gate in the System log.
fn mic_label(mode: orator_cli::tui::MicMode) -> &'static str {
    match mode {
        orator_cli::tui::MicMode::Auto => "auto VAD (file turns gate on speech)",
        orator_cli::tui::MicMode::PushToTalk => "push-to-talk (/talk toggles file turns)",
    }
}

/// Short label for a text mode in the System log.
fn text_label(mode: orator_cli::chat::ChatTextMode) -> &'static str {
    match mode {
        orator_cli::chat::ChatTextMode::Short => "short",
        orator_cli::chat::ChatTextMode::Full => "full",
    }
}

/// Read a WAV file for an `/input` (or `/wav`) turn, resampled to the
/// 16 kHz the transcribe path expects. Failures name the file.
fn read_input_samples(path: &std::path::Path) -> Result<Vec<f32>, String> {
    match orator_audio::read_wav_mono(path) {
        Ok(wav) => {
            if wav.sample_rate == 16_000 {
                Ok(wav.samples)
            } else {
                Ok(resample_linear(&wav.samples, wav.sample_rate, 16_000))
            }
        }
        Err(error) => Err(format!("orator: /input {}: {error}", path.display())),
    }
}

/// One transcribed file turn: decode cost and server travel with the
/// text; failures travel as the exact line. Returns `Err(message)`
/// when the loop should move to the next line. Logging stays with
/// the caller so a barged decode stays silent.
fn timed_transcribe(
    samples: &[f32],
    transport: Transport,
    abort: &StreamAbort,
    config: &OratorConfig,
) -> Result<(String, u64, orator_cli::fallback::DialLeg), String> {
    let started = std::time::Instant::now();
    match orator_cli::fallback::transcribe_dial_leg(samples, transport, abort, config) {
        (Ok(text), leg) => {
            if text.trim().is_empty() {
                let (peak, rms) = orator_cli::chat::decode_signal_stats(samples);
                return Err(format!(
                    "orator: no speech detected; empty transcript ({} samples, peak {:.3}, rms {:.4})",
                    samples.len(),
                    peak,
                    rms
                ));
            }
            Ok((
                text,
                started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                leg,
            ))
        }
        (Err(error), _) => Err(error),
    }
}

/// Meeting lifecycle: local validation, daemon execution.
fn meeting(action: &str, id: &str, transport: Transport) -> ExitCode {
    match action {
        "start" => daemon_passthrough(
            "StartSession",
            serde_json::json!({ "kind": "meeting" }),
            transport,
        ),
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
        // transcript store (orator-memory over the event log) — not
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
            if id.trim().is_empty() {
                eprintln!("orator: meeting export needs an id (run `meeting list` first)");
                return ExitCode::Usage;
            }
            daemon_passthrough(
                "ExportMeeting",
                serde_json::json!({ "session": id }),
                transport,
            )
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
/// probe the configured address, dial it when live, refuse naming
/// the remedy otherwise. Never boots: `cmd` has no per-command
/// server ownership (unlike `ask` with `llm.auto_boot` set).
/// Returns the reply text or the exit to fall back with.
fn propose_via_supervised(want: &str, config: &OratorConfig) -> Result<String, ExitCode> {
    use orator_cli::config::resolve_llm_base;
    use orator::LlmSession as _;
    use orator_agent::llm::{probe_models, HttpSession};
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
        match session.step(&mut orator::Sampler::default()) {
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
            candidates.extend(orator::config_path());
            let Some(path) = common_core::config::find_hierarchical(&candidates)
                .or_else(orator::config_path)
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
            let (loaded, _) = orator::config::load_hierarchical();
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
            candidates.extend(orator::config_path());
            let Some(path) = common_core::config::find_hierarchical(&candidates)
                .or_else(orator::config_path)
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
        .or_else(|| orator::paths::find_on_path("oratord"));
    let Some(binary) = candidate else {
        eprintln!("orator: no oratord binary (same directory or $PATH)");
        return ExitCode::Error;
    };
    eprintln!("orator: starting {} in the foreground", binary.display());
    let error = std::process::Command::new(&binary).exec();
    eprintln!("orator: daemon exec failed: {error}");
    ExitCode::Error
}

fn stt_backend(config: &OratorConfig) -> Result<WhisperBackend, ExitCode> {
    let (path, tag) = orator_cli::fallback::resolve_whisper_model(config).map_err(|error| {
        eprintln!("orator: {error}");
        ExitCode::Error
    })?;
    // The composed language for the serving tag wins; the leaf fills
    // in when the tables say nothing (defaults: the leaf).
    let backend = WhisperBackend::new(
        path,
        orator_cli::fallback::resolve_asr_language(config, &tag),
    );
    Ok(if config.stt.gpu {
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
            eprintln!("orator: transcribe {}: {error}", file.display());
            return ExitCode::Error;
        }
    };
    let samples = if wav.sample_rate == 16_000 {
        wav.samples
    } else {
        resample_linear(&wav.samples, wav.sample_rate, 16_000)
    };
    // Commit gate before inference: gated input (silence, hiss, noise,
    // pure tones) emits the empty output with a stderr note, never a
    // hallucination. The gate is transparent when it fires — speech
    // takes the exact path below, byte-identical to the ungated flow.
    if !orator_voice::vad::speech_present_16k_with_config(&samples, &config.vad) {
        eprintln!("{}", orator_cli::transcribe::NO_SPEECH_NOTE);
        let empty: Vec<Cue> = Vec::new();
        return match render(&empty, format) {
            Ok(text) => {
                print!("{text}");
                ExitCode::Success
            }
            Err(error) => {
                eprintln!("orator: {error}");
                ExitCode::Usage
            }
        };
    }
    let selected = resolve_backend(backend, &config.stt.backend);
    if selected == "audio-server" {
        return transcribe_audio_server(&samples, format, config, transport);
    }
    if selected == "moonshine" {
        return transcribe_moonshine(&samples, format, config);
    }
    if selected != "whisper" {
        eprintln!("orator: unknown STT backend: {selected}");
        return ExitCode::Usage;
    }
    let backend = match stt_backend(config) {
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

/// Transcribe 16 kHz samples through the local moonshine backend
/// (role-side moonshine authority). Segment timing is uniform
/// across the input — moonshine emits no timestamps — so cues
/// cover the clip span evenly.
fn transcribe_moonshine(
    samples: &[f32],
    format: TranscriptFormat,
    config: &OratorConfig,
) -> ExitCode {
    let (path, tag) = match orator_cli::fallback::resolve_moonshine_model(config) {
        Ok(found) => found,
        Err(error) => {
            eprintln!("orator: {error}");
            return ExitCode::Error;
        }
    };
    eprintln!("orator: transcribing locally (moonshine {tag})");
    let mut backend = orator_voice::stt::MoonshineBackend::new(path);
    if config.stt.gpu {
        backend = backend.with_gpu();
    }
    let segments = match backend.decode_blocking(samples, &StreamAbort::new()) {
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
    match say::run_audio_turn(config, transport, |session| {
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
        Err(orator_voice::s2s::S2sError::Aborted) => ExitCode::Interrupted,
        Err(error) => {
            eprintln!("orator: {error}");
            ExitCode::Error
        }
    }
}

fn dictate_once(
    seconds: u64,
    echo_stdout: bool,
    backend: &str,
    config: &OratorConfig,
    transport: Transport,
) -> ExitCode {
    let duration = match orator_cli::dictate::validate_seconds(seconds) {
        Ok(duration) => duration,
        Err(error) => {
            eprintln!("orator: {error}");
            return ExitCode::Usage;
        }
    };
    let choice = match orator_cli::dictate::capture_choice(&config.audio.backend, &config.audio.mic)
    {
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
            let (filled, dropped, hit) = orator_cli::dictate::apply_cap(samples.len(), batch.len());
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
    orator_cli::dictate::note_progress(&format!("capture done ({} samples)", samples.len()));
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
        match dictate_whisper(&samples, config) {
            Ok(text) => text,
            Err(code) => return code,
        }
    } else if selected == "moonshine" {
        match dictate_moonshine(&samples, config) {
            Ok(text) => text,
            Err(code) => return code,
        }
    } else {
        eprintln!("orator: unknown STT backend: {selected}");
        return ExitCode::Usage;
    };
    orator_cli::dictate::note_progress(&format!("transcript: {} chars", text.len()));
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
    orator_cli::dictate::note_progress("injecting");
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
/// Shares the single whisper file-decode home with the local ASR fallback.
fn dictate_whisper(samples: &[f32], config: &OratorConfig) -> Result<String, ExitCode> {
    let engine = stt_backend(config)?;
    let segments = engine
        .decode_blocking(samples, &StreamAbort::new())
        .map_err(|error| {
            eprintln!("orator: {error}");
            ExitCode::Error
        })?;
    Ok(segments
        .iter()
        .map(|segment| segment.text.clone())
        .collect::<Vec<_>>()
        .join(" "))
}

/// Dictate through the local moonshine backend. Moonshine is
/// English-only: the configured language is not consulted (the
/// whisper path keeps using it).
fn dictate_moonshine(samples: &[f32], config: &OratorConfig) -> Result<String, ExitCode> {
    let (path, _tag) = orator_cli::fallback::resolve_moonshine_model(config).map_err(|error| {
        eprintln!("orator: {error}");
        ExitCode::Error
    })?;
    let mut backend = orator_voice::stt::MoonshineBackend::new(path);
    if config.stt.gpu {
        backend = backend.with_gpu();
    }
    backend
        .decode_blocking(samples, &StreamAbort::new())
        .map(|segments| {
            segments
                .iter()
                .map(|segment| segment.text.clone())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .map_err(|error| {
            eprintln!("orator: {error}");
            ExitCode::Error
        })
}

/// Dictate through the audio endpoint. Chat-model thinking blocks are
/// stripped before injection: dictation carries what was said, never
/// how the model thought about it.
fn dictate_audio_server(
    samples: &[f32],
    config: &OratorConfig,
    transport: Transport,
) -> Result<String, ExitCode> {
    say::run_audio_turn(config, transport, |session| {
        session.transcribe_blocking(samples)
    })
    .map_err(|error| {
        if error == orator_voice::s2s::S2sError::Aborted {
            return ExitCode::Interrupted;
        }
        eprintln!("orator: {error}");
        ExitCode::Error
    })
    .map(|text| fluent_llm::thinking::strip_thinking_blocks(&text))
}