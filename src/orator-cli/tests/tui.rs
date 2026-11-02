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
fn whisper_backend_reuses_across_utterances() {
    use super::WhisperCache;
    let mut cache = WhisperCache::default();
    let path = std::path::PathBuf::from("/nonexistent/ggml-test.bin");
    let first = std::ptr::from_ref(cache.get_or_build("m", "en", path.clone()));
    let second = std::ptr::from_ref(cache.get_or_build("m", "en", path.clone()));
    assert_eq!(first, second, "same key reuses the backend");
    assert_eq!(cache.builds, 1);
    cache.get_or_build("other", "en", path);
    assert_eq!(cache.builds, 2, "key change rebuilds exactly once");
}

#[test]
fn kokoro_backend_reuses_across_replies() {
    use super::KokoroCache;
    let mut cache = KokoroCache::default();
    let dir = std::path::PathBuf::from("/nonexistent-models");
    let first = std::ptr::from_ref(cache.get_or_build(dir.clone(), dir.clone(), "bf_emma", false));
    let second = std::ptr::from_ref(cache.get_or_build(dir.clone(), dir.clone(), "bf_emma", false));
    assert_eq!(first, second, "same key reuses the session");
    assert_eq!(cache.builds, 1);
    cache.get_or_build(dir.clone(), dir, "other_voice", false);
    assert_eq!(cache.builds, 2, "voice change rebuilds exactly once");
}

#[test]
fn cancelled_abort_never_decodes() {
    use super::{transcribe_samples, WhisperCache};
    use fluent_concurrency::stream::StreamAbort;
    let abort = StreamAbort::new();
    abort.cancel();
    let mut whisper = WhisperCache::default();
    let error = transcribe_samples(
        &vec![0.0; 16_000],
        "whisper",
        "en",
        "large-v3-turbo-q5_0",
        "http://127.0.0.1:9",
        &abort,
        &mut whisper,
    )
    .expect_err("cancelled aborts fast");
    assert!(error.contains("aborted"), "{error}");
    assert_eq!(whisper.builds, 0, "aborted turns build nothing");
}

#[test]
fn barged_worker_turns_never_deliver() {
    use super::SttWorker;
    use fluent_concurrency::stream::StreamAbort;
    let worker = SttWorker::spawn();
    let abort = StreamAbort::new();
    abort.cancel();
    let config = orator_core::OratorConfig::default();
    let job = worker.transcribe(vec![0.0; 1600], &config, abort);
    let result = job
        .receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("worker answers promptly");
    assert!(
        result.expect_err("barged turn reports abort").contains("aborted"),
        "stale results never deliver as text"
    );
}
