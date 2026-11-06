//! `chat` helper goldens: slash parsing, voice/text selection,
//! serving labels — all hermetic (no I/O, no models, no daemon).

use crate::chat::{
    asr_serving, display_for_text, llm_serving, parse_chat_line, parse_text_mode, parse_voice_mode,
    spoken_for_voice, tts_serving, ChatLine, ChatSlash, ChatTextMode, ChatVoiceMode,
};
use crate::tui::MicMode;

/// Native-model derivation: preference still fires on the agreed
/// heads, and the assured session model is the agreed head — not
/// the leaf. The authority move derives the session from the
/// heads; this pins the divergence it resolves.
#[test]
fn native_session_derives_from_the_agreed_heads() {
    let mut config = orator_core::OratorConfig::default();
    config.roles.asr.models = vec!["liquidai/lfm3-audio-2b".to_string()];
    config.roles.tts.models = vec!["liquidai/lfm3-audio-2b".to_string()];
    config.s2s.model = "some-other-leaf".to_string();
    assert!(
        orator_core::should_prefer_s2s(&config),
        "heads agree through audio-server: preference fires"
    );
    let agreed = orator_core::agreed_s2s_model(&config);
    assert_eq!(
        agreed, "liquidai/lfm3-audio-2b",
        "session derives from the agreed heads, not the leaf ({})",
        config.s2s.model,
    );
}

/// Turn-path composition (deterministic 2×2): native runs only on
/// preference AND assurance together. Preference is producer
/// confidence (names match); assurance is task-value evidence (a
/// server actually voices). Either alone cascades — confidence
/// never substitutes for a served turn.
#[test]
fn turn_path_needs_preference_and_assurance() {
    use crate::chat::{resolve_turn_path, TurnPath};
    assert_eq!(resolve_turn_path(true, true), TurnPath::Native);
    assert_eq!(resolve_turn_path(true, false), TurnPath::Cascade);
    assert_eq!(resolve_turn_path(false, true), TurnPath::Cascade);
    assert_eq!(resolve_turn_path(false, false), TurnPath::Cascade);
}

/// The LLM label follows the chat head; on default configs the
/// head equals the leaf, so the label is byte-identical.
#[test]
fn llm_serving_prefers_the_chat_head() {
    let mut config = orator_core::OratorConfig::default();
    let (model, _) = llm_serving(&config);
    assert_eq!(model, config.llm.model, "default: head equals leaf");
    config.roles.chat.models = vec!["custom-chat-tag".to_string()];
    config.llm.model = "leaf-model".to_string();
    let (model, backend) = llm_serving(&config);
    assert_eq!(model, "custom-chat-tag");
    assert!(!backend.is_empty(), "backend named");
}

/// Signal stats pin the empty-decode diagnostic: silence reports
/// zeros, a hot buffer reports its peak and RMS.
#[test]
fn decode_signal_stats_name_silence() {
    use crate::chat::decode_signal_stats;
    assert_eq!(decode_signal_stats(&[]), (0.0, 0.0));
    assert_eq!(decode_signal_stats(&[0.0; 16]), (0.0, 0.0));
    let (peak, rms) = decode_signal_stats(&[0.5, -0.5, 0.25, -0.25]);
    assert!((peak - 0.5).abs() < f32::EPSILON);
    let expected = (0.5f32.powi(2) * 2.0 + 0.25f32.powi(2) * 2.0).sqrt() / 2.0;
    assert!((rms - expected).abs() < 1e-6, "rms {rms} vs {expected}");
}

/// Prompt playout timing: a held reply voices at once only into
/// a fully idle session. Any live turn, queued input, hot mic,
/// buffered speech, or live playout holds it for overlap instead —
/// replies never double-talk and order holds.
///
/// Tail hold: capture stays suppressed briefly after a playout so
/// the room tail decays instead of endpointing as a phantom turn.
#[test]
fn voice_now_needs_a_fully_idle_session() {
    use crate::chat::{should_voice_now, VoiceNow};
    let idle = VoiceNow {
        turn_running: false,
        queued: false,
        mic_speaking: false,
        utterance_buffered: false,
        playout_live: false,
    };
    assert!(should_voice_now(idle));
    for blocked in [
        VoiceNow {
            turn_running: true,
            ..idle
        },
        VoiceNow {
            queued: true,
            ..idle
        },
        VoiceNow {
            mic_speaking: true,
            ..idle
        },
        VoiceNow {
            utterance_buffered: true,
            ..idle
        },
        VoiceNow {
            playout_live: true,
            ..idle
        },
    ] {
        assert!(
            !should_voice_now(blocked),
            "any live leg holds the reply: {blocked:?}"
        );
    }
}

/// Tail hold: a future deadline suppresses capture, an absent or
/// elapsed one releases it.
#[test]
fn tail_hold_covers_the_deadline_only() {
    use crate::chat::{tail_hold_active, ECHO_TAIL_HOLD_MS};
    use std::time::{Duration, Instant};
    let now = Instant::now();
    assert!(tail_hold_active(
        Some(now + Duration::from_millis(ECHO_TAIL_HOLD_MS)),
        now
    ));
    assert!(!tail_hold_active(None, now));
    assert!(
        !tail_hold_active(Some(now), now),
        "an elapsed hold releases"
    );
}

#[test]
fn control_slashes_parse_exactly() {
    assert_eq!(
        parse_chat_line("/quit"),
        Some(ChatLine::Slash(ChatSlash::Quit))
    );
    assert_eq!(
        parse_chat_line("/exit"),
        Some(ChatLine::Slash(ChatSlash::Quit))
    );
    assert_eq!(
        parse_chat_line("/reset"),
        Some(ChatLine::Slash(ChatSlash::Reset))
    );
    assert_eq!(
        parse_chat_line("/talk"),
        Some(ChatLine::Slash(ChatSlash::Talk))
    );
    assert_eq!(parse_chat_line("   "), None, "blank lines skip");
}

#[test]
fn file_slashes_carry_their_paths() {
    assert_eq!(
        parse_chat_line("/wav /tmp/turn.wav"),
        Some(ChatLine::Slash(ChatSlash::Wav("/tmp/turn.wav".to_string())))
    );
    assert_eq!(
        parse_chat_line("/input /tmp/turn.wav"),
        Some(ChatLine::Slash(ChatSlash::Input(
            "/tmp/turn.wav".to_string()
        )))
    );
    assert_eq!(
        parse_chat_line("hello there"),
        Some(ChatLine::Say("hello there".to_string()))
    );
}

#[test]
fn unknown_slashes_are_spoken_never_dispatched() {
    assert_eq!(
        parse_chat_line("/nope"),
        Some(ChatLine::Say("/nope".to_string()))
    );
    assert_eq!(
        parse_chat_line("/voice loud"),
        Some(ChatLine::Say("/voice loud".to_string()))
    );
    assert_eq!(
        parse_chat_line("/mic loud"),
        Some(ChatLine::Say("/mic loud".to_string()))
    );
}

#[test]
fn mic_voice_and_text_modes_parse() {
    assert_eq!(
        parse_chat_line("/mic auto"),
        Some(ChatLine::Slash(ChatSlash::Mic(MicMode::Auto)))
    );
    assert_eq!(
        parse_chat_line("/mic ptt"),
        Some(ChatLine::Slash(ChatSlash::Mic(MicMode::PushToTalk)))
    );
    assert_eq!(
        parse_chat_line("/voice off"),
        Some(ChatLine::Slash(ChatSlash::Voice(ChatVoiceMode::Off)))
    );
    assert_eq!(
        parse_chat_line("/text short"),
        Some(ChatLine::Slash(ChatSlash::Text(ChatTextMode::Short)))
    );
    assert!(parse_voice_mode("loud").is_err(), "bad voice refuses");
    assert!(parse_text_mode("long").is_err(), "bad text refuses");
}

#[test]
fn voice_selection_follows_the_mode() {
    assert_eq!(spoken_for_voice("hi", ChatVoiceMode::Off), None);
    assert_eq!(
        spoken_for_voice("hi", ChatVoiceMode::Short),
        Some("hi".to_string())
    );
    let long = "word ".repeat(200);
    let short = spoken_for_voice(&long, ChatVoiceMode::Short).expect("short voices");
    assert!(short.len() < long.len(), "short compacts long replies");
    assert!(short.contains("Full answer printed"), "{short}");
    assert_eq!(
        spoken_for_voice("hi", ChatVoiceMode::Full),
        Some("hi".to_string())
    );
}

#[test]
fn text_selection_prints_full_once() {
    assert_eq!(
        display_for_text("hi", ChatTextMode::Full),
        vec!["hi".to_string()]
    );
    assert_eq!(
        display_for_text("hi", ChatTextMode::Short),
        vec!["hi".to_string()],
        "short replies print once, never doubled"
    );
    let long = "word ".repeat(200);
    let lines = display_for_text(&long, ChatTextMode::Short);
    assert_eq!(lines.len(), 2, "short prints summary plus full once");
    assert_eq!(lines[1], long, "full answer prints verbatim");
}

#[test]
fn serving_labels_name_the_roles() {
    let config = orator_core::OratorConfig::default();
    let (model, _backend) = llm_serving(&config);
    assert_eq!(model, config.llm.model, "llm labels the chat model");
    let (asr_model, asr_backend) = asr_serving(&config);
    assert!(!asr_model.is_empty(), "asr names a model");
    assert!(!asr_backend.is_empty(), "asr names a backend");
    let (tts_model, tts_backend) = tts_serving(&config);
    assert!(!tts_model.is_empty(), "tts names a model");
    assert!(!tts_backend.is_empty(), "tts names a backend");
}

#[test]
fn control_lines_reuse_the_tui_chord_table() {
    use crate::chat::parse_control_line;
    use crate::tui::TuiAction;
    assert_eq!(parse_control_line("\u{2}"), Some(TuiAction::Barge));
    assert_eq!(parse_control_line("\u{11}"), Some(TuiAction::Quit));
    assert_eq!(parse_control_line("\u{7}"), Some(TuiAction::Mode));
    assert_eq!(parse_control_line("\u{14}"), Some(TuiAction::Talk));
    assert_eq!(
        parse_control_line("hello"),
        None,
        "ordinary lines pass through"
    );
    assert_eq!(parse_control_line(""), None, "empty lines pass through");
    assert_eq!(
        parse_control_line("\u{2}\u{2}"),
        None,
        "multi-char lines are text, never chords"
    );
    assert_eq!(parse_control_line("\u{7f}"), None, "DEL is not a chord");
}

#[test]
fn file_gate_sheds_ptt_off_then_silence() {
    use crate::chat::{check_file_gate, FileGate};
    use crate::tui::MicMode;
    let hot = vec![0.5; 16_000];
    let silent = vec![0.0; 16_000];
    assert_eq!(
        check_file_gate(MicMode::PushToTalk, false, &hot),
        Err(FileGate::PttOff { samples: 16_000 }),
        "latched-off PTT sheds before any decode"
    );
    assert_eq!(
        check_file_gate(MicMode::PushToTalk, true, &silent),
        Err(FileGate::NoSpeech),
        "silence sheds even with talk on"
    );
    assert_eq!(
        check_file_gate(MicMode::Auto, false, &silent),
        Err(FileGate::NoSpeech),
        "auto VAD-gates file turns"
    );
    assert!(
        check_file_gate(MicMode::PushToTalk, true, &hot).is_ok(),
        "hot span with talk on commits"
    );
}

#[test]
fn barge_mode_parses_with_a_spoken_fallback() {
    use crate::chat::{parse_chat_line, ChatLine, ChatSlash};
    assert_eq!(
        parse_chat_line("/barge on"),
        Some(ChatLine::Slash(ChatSlash::Barge(true)))
    );
    assert_eq!(
        parse_chat_line("/barge off"),
        Some(ChatLine::Slash(ChatSlash::Barge(false)))
    );
    assert_eq!(
        parse_chat_line("/barge loudly"),
        Some(ChatLine::Say("/barge loudly".to_string())),
        "bad barge modes are spoken, never dispatched"
    );
}

#[test]
fn turn_path_needs_both_preference_and_assurance() {
    use crate::chat::{resolve_turn_path, TurnPath};
    assert_eq!(resolve_turn_path(true, true), TurnPath::Native);
    assert_eq!(resolve_turn_path(true, false), TurnPath::Cascade);
    assert_eq!(resolve_turn_path(false, true), TurnPath::Cascade);
    assert_eq!(resolve_turn_path(false, false), TurnPath::Cascade);
}

/// M7-S1 characterization: serving-selector outputs for a fixture
/// config with role heads and a models table. The ASR fixture keeps
/// every ordered tag on a non-local backend (`audio-server`) with the
/// leaf listed among the heads, so the local-ASR walk finds nothing
/// hermetically and the role walk decides.
#[test]
fn serving_selectors_pin_fixture_outputs() {
    use orator_core::ModelSpec;
    let mut config = orator_core::OratorConfig::default();
    config.roles.chat.models = vec!["chat-fixture".to_string()];
    config.llm.model = "leaf-model".to_string();
    config.roles.asr.models = vec!["asr-fixture".to_string()];
    config.stt.model = "asr-fixture".to_string();
    config.roles.tts.models = vec!["tts-fixture".to_string()];
    for (tag, backend) in [
        ("asr-fixture", "audio-server"),
        ("tts-fixture", "kokoro"),
        ("chat-fixture", "audio-server"),
    ] {
        config.models.models.insert(
            tag.to_string(),
            ModelSpec {
                weights: None,
                backends: vec![backend.to_string()],
                params: None,
                role_params: None,
            },
        );
    }
    assert_eq!(
        llm_serving(&config),
        ("chat-fixture".to_string(), "daemon-or-direct".to_string())
    );
    assert_eq!(
        asr_serving(&config),
        ("asr-fixture".to_string(), "audio-server".to_string())
    );
    assert_eq!(
        tts_serving(&config),
        ("tts-fixture".to_string(), "kokoro".to_string())
    );
}
