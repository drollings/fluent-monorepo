//! Headless TUI tests: keystroke mapping, mic gating, transcript
//! rendering goldens, endpoint release. Every test draws into a
//! `TestBackend` buffer — no TTY, no raw mode, no live daemon.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::tui::{
    MicGate, SessionControl, ControlError, SCROLLBACK, TuiApp, TuiLine, TurnEvent, level_bar,
    map_key, parse_command, render, ControlOutcome,
};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

/// Scripted control: records abort/release/calls, answers from a queue.
#[derive(Debug, Default)]
struct StubControl {
    aborted: u32,
    released: u32,
    calls: Vec<(String, serde_json::Value)>,
    answers: std::collections::VecDeque<Result<serde_json::Value, ControlError>>,
}

impl StubControl {
    fn answering(
        answers: Vec<Result<serde_json::Value, ControlError>>,
    ) -> Self {
        Self {
            answers: answers.into(),
            ..Self::default()
        }
    }
}

impl SessionControl for StubControl {
    fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> ControlOutcome {
        self.calls.push((method.to_string(), params));
        self.answers.pop_front().unwrap_or(Ok(serde_json::json!({})))
    }

    fn abort(&mut self) {
        self.aborted += 1;
    }

    fn release(&mut self) {
        self.released += 1;
    }
}

fn hot_frame() -> Vec<f32> {
    (0..320)
        .map(|index| {
            let time = index as f32 / 16_000.0;
            0.35 * (2.0 * std::f32::consts::PI * 150.0 * time).sin()
                + 0.35 * (2.0 * std::f32::consts::PI * 300.0 * time).sin()
        })
        .collect()
}

fn screen_text(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer().clone();
    let area = buffer.area;
    let mut out = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            out.push_str(buffer.cell((x, y)).map_or(" ", |cell| cell.symbol()));
        }
        out.push('\n');
    }
    out
}

#[test]
fn barge_key_fires_abort_quit_releases_endpoint() {
    let mut app = TuiApp::new(StubControl::default());
    assert_eq!(map_key(ctrl(KeyCode::Char('b'))), crate::tui::TuiAction::Barge);
    app.key(ctrl(KeyCode::Char('b')));
    app.key(ctrl(KeyCode::Char('b')));
    assert_eq!(app.control.aborted, 2, "every barge keystroke aborts");
    assert_eq!(app.control.released, 0, "barge never releases");
    app.key(ctrl(KeyCode::Char('q')));
    assert_eq!(app.control.released, 1, "quit releases the endpoint");
    assert!(app.quit, "quit latches the exit flag");
}

#[test]
fn session_keys_drive_start_stop_list_with_exact_params() {
    let mut app = TuiApp::new(StubControl::answering(vec![
        Ok(serde_json::json!({"session": "s1"})),
        Ok(serde_json::json!({})),
        Ok(serde_json::json!({"sessions": ["s1"]})),
    ]));
    app.key(ctrl(KeyCode::Char('s')));
    assert_eq!(app.state.session.as_deref(), Some("s1"));
    app.key(ctrl(KeyCode::Char('e')));
    assert_eq!(app.state.session, None, "stop clears the session");
    app.key(ctrl(KeyCode::Char('l')));
    assert_eq!(
        app.control.calls,
        vec![
            ("StartSession".to_string(), serde_json::json!({"kind": "assistant"})),
            ("StopSession".to_string(), serde_json::json!({})),
            ("ListSessions".to_string(), serde_json::json!({})),
        ]
    );
}

#[test]
fn typed_line_sends_as_ask_and_slash_commands_dispatch() {
    let mut app = TuiApp::new(StubControl::answering(vec![Ok(
        serde_json::json!({"reply": "hi"}),
    )]));
    for char in "hello".chars() {
        app.key(key(KeyCode::Char(char)));
    }
    assert_eq!(app.state.input, "hello");
    app.key(key(KeyCode::Enter));
    assert_eq!(app.state.input, "", "send clears the line");
    assert_eq!(
        app.control.calls,
        vec![("Ask".to_string(), serde_json::json!({"question": "hello"}))]
    );
    assert_eq!(
        app.state.lines,
        vec![
            TuiLine::Said("hello".to_string()),
            TuiLine::Heard("hi".to_string()),
        ]
    );
    // Slash commands never reach the voice path.
    let calls = app.control.calls.len();
    for char in "/quit".chars() {
        app.key(key(KeyCode::Char(char)));
    }
    app.key(key(KeyCode::Enter));
    assert_eq!(app.control.calls.len(), calls, "slash never voices");
    assert!(app.quit, "/quit latches the exit flag");
    assert_eq!(parse_command("/start"), Some(crate::tui::SlashCommand::Start));
    assert_eq!(parse_command("/stop"), Some(crate::tui::SlashCommand::Stop));
    assert_eq!(parse_command("/list"), Some(crate::tui::SlashCommand::List));
    assert_eq!(parse_command("/quit"), Some(crate::tui::SlashCommand::Quit));
    assert_eq!(parse_command("/nope"), None, "unknown slash is spoken, not dispatched");
    assert_eq!(parse_command("plain"), None);
}

#[test]
fn control_failures_render_into_the_transcript_verbatim() {
    let failure = ControlError {
        code: crate::ExitCode::Error,
        text: "orator: daemon refused Say: {\"error\":\"busy\"}".to_string(),
    };
    let mut app = TuiApp::new(StubControl::answering(vec![Err(failure)]));
    app.key(ctrl(KeyCode::Char('s')));
    assert_eq!(
        app.state.lines,
        vec![TuiLine::Failure(
            "orator: daemon refused Say: {\"error\":\"busy\"}".to_string()
        )],
        "control text renders verbatim — one wording, two renderers"
    );
}

#[test]
fn mic_gate_holds_silence_passes_hot_spans() {
    let mut gate = MicGate::new();
    for _ in 0..10 {
        assert!(!gate.push(&vec![0.0; 320]), "silence never sends");
    }
    assert!(gate.level() < f32::EPSILON, "silence reads zero level");
    let hot = hot_frame();
    assert!(!gate.push(&hot), "first hot frame is attack, not speech");
    assert!(!gate.push(&hot), "second hot frame is attack, not speech");
    assert!(gate.push(&hot), "attack run opens the span");
    assert!(gate.level() > 0.0, "hot span raises the meter");
}

#[test]
fn app_buffers_only_hot_spans_and_clears_on_send() {
    let mut app = TuiApp::new(StubControl::default());
    for _ in 0..10 {
        app.mic(&vec![0.0; 320]);
    }
    assert!(app.pending_utterance.is_empty(), "silence buffers nothing");
    let hot = hot_frame();
    for _ in 0..3 {
        app.mic(&hot);
    }
    assert!(!app.pending_utterance.is_empty(), "hot spans buffer");
    assert!(app.state.mic_level > 0.0, "meter follows the mic");
}

#[test]
fn level_bar_golden() {
    assert_eq!(level_bar(0.0, 8), "░░░░░░░░");
    assert_eq!(level_bar(1.0, 8), "████████");
    assert_eq!(level_bar(0.5, 8), "████░░░░");
    assert_eq!(level_bar(2.0, 4), "████", "clamped, never overflows");
    assert_eq!(level_bar(-1.0, 4), "░░░░", "clamped, never negative");
}

#[test]
fn transcript_golden_for_text_audio_done() {
    let mut app = TuiApp::new(StubControl::default());
    app.event(TurnEvent::Text("hello there".to_string()), true);
    app.event(TurnEvent::Audio(2400), false);
    app.event(TurnEvent::Text("hi back".to_string()), false);
    app.event(TurnEvent::Done, false);
    assert_eq!(
        app.state.lines,
        vec![
            TuiLine::Said("hello there".to_string()),
            TuiLine::Voiced(2400),
            TuiLine::Heard("hi back".to_string()),
            TuiLine::Done,
        ]
    );
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test backend builds");
    render(&mut terminal, &app.state).expect("headless render holds");
    let screen = screen_text(&terminal);
    assert!(screen.contains("hello there"), "user text visible:\n{screen}");
    assert!(screen.contains("hi back"), "reply text visible:\n{screen}");
    assert!(screen.contains("♪ 2400"), "audio marker visible:\n{screen}");
    assert!(screen.contains("done"), "turn-complete visible:\n{screen}");
}

#[test]
fn render_shows_meter_voicing_and_session_without_tty() {
    let mut app = TuiApp::new(StubControl::default());
    app.state.session = Some("s1".to_string());
    app.state.voicing = true;
    app.state.mic_level = 0.5;
    app.state.input = "he".to_string();
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test backend builds");
    render(&mut terminal, &app.state).expect("headless render holds");
    let screen = screen_text(&terminal);
    assert!(screen.contains("s1"), "session visible:\n{screen}");
    assert!(screen.contains("VOICING"), "server-audio indicator visible:\n{screen}");
    assert!(screen.contains("████░░░░"), "meter bar visible:\n{screen}");
    assert!(screen.contains("he"), "input line visible:\n{screen}");
}

#[test]
fn scrollback_caps_and_quit_without_control_leaks() {
    let mut app = TuiApp::new(StubControl::default());
    for index in 0..SCROLLBACK + 50 {
        app.event(TurnEvent::Text(format!("line {index}")), false);
    }
    assert_eq!(app.state.lines.len(), SCROLLBACK, "scrollback bounded");
    assert_eq!(
        app.state.lines.last(),
        Some(&TuiLine::Heard(format!("line {}", SCROLLBACK + 49)))
    );
    app.key(ctrl(KeyCode::Char('q')));
    assert_eq!(app.control.released, 1, "quit releases exactly once");
}

#[test]
fn signal_flag_quits_and_releases_the_session() {
    // The loop glue, hermetic: a signalled shutdown flag plus an
    // active session must stop the session exactly once on the way
    // out — the same path a real SIGTERM mid-`tui` takes through the
    // armed watch (signal delivery itself is pinned in `signal.rs`
    // against a real SIGTERM to this process).
    use std::sync::atomic::AtomicBool;
    let mut app = TuiApp::new(StubControl::answering(vec![Ok(
        serde_json::json!({"session": "s9"}),
    )]));
    app.key(ctrl(KeyCode::Char('s')));
    assert_eq!(app.state.session.as_deref(), Some("s9"));
    let calm = AtomicBool::new(false);
    assert!(!crate::tui::tui_loop_should_quit(&app, &calm));
    let shutdown = AtomicBool::new(true);
    assert!(crate::tui::tui_loop_should_quit(&app, &shutdown));
    crate::tui::tui_loop_quit_cleanup(&mut app);
    assert_eq!(app.state.session, None, "cleanup stops the session");
    assert_eq!(
        app.control.calls,
        vec![
            ("StartSession".to_string(), serde_json::json!({"kind": "assistant"})),
            ("StopSession".to_string(), serde_json::json!({})),
        ]
    );
}

#[test]
fn mixed_rate_capture_frames_like_same_rate() {
    use super::{drain_capture, FRAME_SAMPLES};
    // Same 0.1 s of DC audio through both domains: 16 kHz in one
    // poll, 48 kHz in two polls. Frame counts must match exactly —
    // the 16 kHz remainder keeps its own domain instead of
    // re-entering as device-rate input.
    let mut same = TuiApp::new(StubControl::default());
    let mut carry = Vec::new();
    let mut remainder = Vec::new();
    let mut input_16k = vec![0.5f32; 1600].into_iter();
    drain_capture(
        &mut || input_16k.next(),
        &mut carry,
        &mut remainder,
        16_000,
        &mut same,
    );
    let mut mixed = TuiApp::new(StubControl::default());
    let mut carry = Vec::new();
    let mut remainder = Vec::new();
    for _ in 0..2 {
        let mut input_48k = vec![0.5f32; 2400].into_iter();
        drain_capture(
            &mut || input_48k.next(),
            &mut carry,
            &mut remainder,
            48_000,
            &mut mixed,
        );
    }
    assert_eq!(
        same.pending_utterance.len(),
        mixed.pending_utterance.len(),
        "mixed-rate frames exactly like same-rate"
    );
    assert_eq!(
        same.pending_utterance.len(),
        3 * FRAME_SAMPLES,
        "five frames in, two attack frames dropped, three buffered"
    );
    assert!(remainder.len() < FRAME_SAMPLES, "remainder never holds a frame");
}

#[test]
fn slow_transcribe_bounds_the_utterance_queue() {
    use super::MAX_PENDING_UTTERANCE;
    let mut app = TuiApp::new(StubControl::default());
    let hot = vec![0.5f32; 320];
    app.mic(&hot);
    app.mic(&hot);
    for _ in 0..(MAX_PENDING_UTTERANCE / 320 + 10) {
        app.mic(&hot);
    }
    assert!(
        app.pending_utterance.len() <= MAX_PENDING_UTTERANCE,
        "utterance queue bounded while transcribe lags"
    );
    assert!(
        app.pending_dropped > 0,
        "over-cap input drops with a counter, never silently"
    );
}

#[test]
fn transcribe_is_a_daemon_call_never_local_inference() {
    use super::{transcribe_samples, SttWorker, Transport};
    use fluent_concurrency::stream::StreamAbort;
    // Cancelled aborts fast without dialing (no backend, no model, no
    // endpoint involved — the thin path needs none of them).
    let abort = StreamAbort::new();
    abort.cancel();
    let error = transcribe_samples(&vec![0.0; 16_000], Transport::Socket, &abort)
        .expect_err("cancelled aborts fast");
    assert!(error.contains("aborted"), "{error}");
    // The worker forwards to the dial function and delivers its text.
    let worker = SttWorker::spawn_with(|samples, _transport, _abort| {
        assert_eq!(samples.len(), 1600);
        Ok("hello there".to_string())
    });
    let job = worker.transcribe(vec![0.0; 1600], Transport::Socket, StreamAbort::new());
    let result = job
        .receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("worker answers promptly");
    assert_eq!(result.expect("scripted text delivers"), "hello there");
}

#[test]
fn voice_turn_voices_through_the_daemon_not_a_local_synth() {
    use super::ask_then_voice_with;
    // A refused Say surfaces with its text: the voice path dials the
    // daemon with the reply text and reports its answer — local
    // synthesis is never consulted, and words are never lost to a
    // playback problem.
    let turn = ask_then_voice_with(
        "hi there",
        None,
        &|text, _session| {
            assert_eq!(text, "hi there");
            Ok("hi there".to_string())
        },
        &|spoken| {
            assert_eq!(spoken, "hi there", "short replies voice verbatim");
            Err("orator: daemon refused Say".to_string())
        },
    )
    .expect("say failure still returns its text");
    assert_eq!(turn.reply, "hi there");
    assert_eq!(
        turn.voice,
        Err("orator: daemon refused Say".to_string()),
        "refusals attach verbatim"
    );
}

#[test]
fn voice_turn_summarizes_long_replies_and_skips_empty_ones() {
    use super::ask_then_voice_with;
    // Long replies voice the concise form while the transcript keeps
    // the full text.
    let long = format!("First. Second. {}", "More. ".repeat(200));
    let turn = ask_then_voice_with(
        "tell me everything",
        None,
        &|_, _| Ok(long.clone()),
        &|spoken| {
            assert!(spoken.len() < long.len(), "voice shortens: {spoken}");
            assert!(spoken.contains("Full answer printed"), "{spoken}");
            Ok((vec![0.1; 24], 24_000))
        },
    )
    .expect("voiced turn runs");
    assert_eq!(turn.reply, long, "transcript keeps every word");
    assert!(matches!(turn.voice, Ok(Some(_))), "audio rides along");
    // Empty replies never reach synthesis.
    let turn = ask_then_voice_with(
        "silence",
        None,
        &|_, _| Ok("   ".to_string()),
        &|_| panic!("empty replies must never dial Say"),
    )
    .expect("empty turn runs");
    assert!(matches!(turn.voice, Ok(None)), "nothing to voice");
}

#[test]
fn barged_worker_turns_never_dial() {
    use super::{SttWorker, Transport};
    use fluent_concurrency::stream::StreamAbort;
    // A cancelled turn never reaches the dial function: the worker
    // answers abort before any network, so stale audio never becomes
    // text after a barge.
    let worker = SttWorker::spawn_with(|_, _, _| {
        panic!("barged turns must never dial");
    });
    let abort = StreamAbort::new();
    abort.cancel();
    let job = worker.transcribe(vec![0.0; 1600], Transport::Socket, abort);
    let result = job
        .receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("worker answers promptly");
    assert!(
        result.expect_err("barged turn reports abort").contains("aborted"),
        "stale results never deliver as text"
    );
}

#[test]
fn saturated_send_keeps_the_typed_message_for_retry() {
    use crate::ExitCode;
    let saturated = ControlError {
        code: ExitCode::Error,
        text: "orator: daemon refused Ask: saturated (bound 2)".to_string(),
    };
    let mut app = TuiApp::new(StubControl::answering(vec![Err(saturated)]));
    for ch in "hello".chars() {
        app.key(key(KeyCode::Char(ch)));
    }
    app.key(key(KeyCode::Enter));
    assert_eq!(app.state.input, "hello", "saturated send restores the input");
    assert!(
        app.state.lines.iter().any(|line| matches!(line, TuiLine::Status(note) if note.contains("retry"))),
        "the remedy names retry: {:?}",
        app.state.lines
    );
    // Retry delivers once the daemon drains: the restored input sends.
    app.control.answers.push_back(Ok(serde_json::json!({"reply": "hi"})));
    app.key(key(KeyCode::Enter));
    assert_eq!(app.state.input, "", "delivered input clears");
    assert!(
        app.state.lines.iter().any(|line| matches!(line, TuiLine::Heard(reply) if reply == "hi")),
        "retry answer lands: {:?}",
        app.state.lines
    );
}

#[test]
fn other_send_failures_still_drop_with_the_line() {
    use crate::ExitCode;
    let refused = ControlError {
        code: ExitCode::Error,
        text: "orator: daemon refused Ask: backend gone".to_string(),
    };
    let mut app = TuiApp::new(StubControl::answering(vec![Err(refused)]));
    for ch in "hi".chars() {
        app.key(key(KeyCode::Char(ch)));
    }
    app.key(key(KeyCode::Enter));
    assert_eq!(app.state.input, "", "non-saturated failures drop like before");
    assert!(
        app.state.lines.iter().any(|line| matches!(line, TuiLine::Said(text) if text == "hi")),
        "the attempt stays on record: {:?}",
        app.state.lines
    );
}

#[test]
fn voice_worker_delivers_turns_and_drops_cuts() {
    use crate::tui::{Transport, VoiceJob, VoiceTurn, VoiceWorker};
    use fluent_concurrency::stream::StreamAbort;
    let worker = VoiceWorker::spawn_with(|text, _session, _transport, _abort| {
        Ok(VoiceTurn {
            reply: format!("echo:{text}"),
            voice: Ok(Some((vec![0.5; 48], 24_000))),
        })
    });
    let job: VoiceJob = worker.turn(
        "hi".to_string(),
        None,
        Transport::Socket,
        StreamAbort::new(),
    );
    let turn = job.receiver.recv_timeout(std::time::Duration::from_secs(10)).expect("turn arrives").expect("turn ok");
    assert_eq!(turn.reply, "echo:hi");
    assert!(matches!(turn.voice, Ok(Some(_))), "audio rides along");
    // A cut before delivery never arrives: the worker skips the send
    // and the loop has already dropped the job.
    let abort = StreamAbort::new();
    abort.cancel();
    let cut: VoiceJob = worker.turn(
        "late".to_string(),
        None,
        Transport::Socket,
        abort,
    );
    let outcome = cut.receiver.recv_timeout(std::time::Duration::from_secs(10));
    assert!(
        matches!(outcome, Err(_) | Ok(Err(_))),
        "cut turns never deliver speech: {outcome:?}"
    );
}

#[test]
fn mic_modes_cycle_with_keys_and_slashes() {
    use crate::tui::{MicMode, SlashCommand};
    let mut app = TuiApp::new(StubControl::default());
    assert_eq!(app.state.mic_mode, MicMode::Auto);
    assert_eq!(crate::tui::parse_command("/ptt"), Some(SlashCommand::Ptt));
    assert_eq!(crate::tui::parse_command("/auto"), Some(SlashCommand::Auto));
    assert_eq!(crate::tui::parse_command("/talk"), Some(SlashCommand::Talk));
    app.key(ctrl(KeyCode::Char('g')));
    assert_eq!(app.state.mic_mode, MicMode::PushToTalk);
    assert!(!app.state.talking, "mode switch never latches talk");
    app.key(ctrl(KeyCode::Char('g')));
    assert_eq!(app.state.mic_mode, MicMode::Auto);
    for line in ["/ptt", "/talk", "/auto"] {
        for ch in line.chars() {
            app.key(key(KeyCode::Char(ch)));
        }
        app.key(key(KeyCode::Enter));
    }
    assert_eq!(app.state.mic_mode, MicMode::Auto, "slash round-trip ends home");
}


fn hot_span(app: &mut TuiApp<StubControl>, frames: usize) {
    // The detector needs an attack run before a span opens.
    let hot = hot_frame();
    for _ in 0..frames {
        app.mic(&hot);
    }
}

#[test]
fn ptt_buffers_only_while_talking_and_commits_on_toggle_off() {
    let mut app = TuiApp::new(StubControl::default());
    app.key(ctrl(KeyCode::Char('g')));
    app.mic(&hot_frame());
    assert!(app.pending_utterance.is_empty(), "cold PTT mic buffers nothing");
    app.key(ctrl(KeyCode::Char('t')));
    assert!(app.state.talking, "talk latches hot");
    hot_span(&mut app, 3);
    assert!(!app.pending_utterance.is_empty(), "hot PTT mic buffers");
    app.key(ctrl(KeyCode::Char('t')));
    assert!(!app.state.talking, "toggle-off unlatches");
    assert!(app.ptt_commit, "toggle-off endpoints the turn");
}

#[test]
fn talk_key_names_the_remedy_outside_ptt() {
    let mut app = TuiApp::new(StubControl::default());
    app.key(ctrl(KeyCode::Char('t')));
    assert!(!app.state.talking, "auto mode has no talk state");
    assert!(
        app.state.lines.iter().any(|line| matches!(line, TuiLine::Status(note) if note.contains("Ctrl+G"))),
        "the hint names the mode key: {:?}",
        app.state.lines
    );
}

#[test]
fn voicing_suppression_moves_meter_without_buffering() {
    let mut app = TuiApp::new(StubControl::default());
    app.suppress_utterance = true;
    hot_span(&mut app, 3);
    assert!(app.state.mic_level > 0.0, "meter stays live under suppression");
    assert!(app.pending_utterance.is_empty(), "echo never buffers");
    app.suppress_utterance = false;
    app.gate.reset();
    hot_span(&mut app, 3);
    assert!(!app.pending_utterance.is_empty(), "release resumes buffering");
}

#[test]
fn shed_drops_the_buffer_with_a_count_and_a_reset_gate() {
    let mut app = TuiApp::new(StubControl::default());
    hot_span(&mut app, 3);
    assert!(!app.pending_utterance.is_empty());
    let shed = app.shed_utterance();
    assert!(shed > 0, "echo tail has samples");
    assert!(app.pending_utterance.is_empty());
    assert_eq!(app.pending_dropped, shed as u64, "shed samples count");
    assert!(!app.gate_speaking(), "reset closes the phantom span");
}

#[test]
fn barge_clears_the_buffer_and_unlatches_talk() {
    let mut app = TuiApp::new(StubControl::default());
    app.key(ctrl(KeyCode::Char('g')));
    app.key(ctrl(KeyCode::Char('t')));
    hot_span(&mut app, 3);
    assert!(!app.pending_utterance.is_empty());
    app.key(ctrl(KeyCode::Char('b')));
    assert!(app.pending_utterance.is_empty(), "barge sheds the cut");
    assert!(!app.state.talking, "barge unlatches talk");
    assert!(!app.ptt_commit, "no endpoint after a cut");
    assert_eq!(app.control.aborted, 1, "barge still fires abort");
}
