//! Driver tests: epoch stickiness at three hops, duck/confirm/cut flow,
//! DAC history, speculation resume, and the degrade table.

use std::num::NonZeroU32;

use orator::{Channel, Epoch, MediaTime, Word};

use crate::turn::driver::{
    degrade_on, BargeInEvidence, BargeInOutcome, History, Hop, HopGate, SessionKind, SessionMode,
    SpecState, TurnDriver,
};

fn word(text: &str) -> Word {
    let stamp = MediaTime::zero(NonZeroU32::new(16_000).expect("rate"));
    Word {
        text: text.to_string(),
        start: stamp,
        end: stamp.add_samples(3200),
        confidence: 0.9,
        channel: Channel::Reply,
    }
}

fn evidence(sustained_ms: u16, audio_ms: u64, words: &[&str]) -> BargeInEvidence {
    BargeInEvidence {
        sustained_ms,
        audio_ms,
        words: words.iter().map(|word| (*word).to_string()).collect(),
        speaker_match: None,
    }
}

#[test]
fn epoch_checks_pass_live_and_drop_stale_at_every_hop() {
    let mut gate = HopGate::new(Epoch(7));
    for hop in [Hop::PostStt, Hop::PreLlm, Hop::PreSpeak] {
        assert!(gate.pass(Epoch(7), hop));
        assert!(!gate.pass(Epoch(6), hop));
        assert!(!gate.pass(Epoch(8), hop));
    }
    assert_eq!(gate.drops(Hop::PostStt), 2);
    assert_eq!(gate.drops(Hop::PreLlm), 2);
    assert_eq!(gate.drops(Hop::PreSpeak), 2);
}

#[test]
fn cut_is_sticky_and_opens_the_next_epoch() {
    let mut gate = HopGate::new(Epoch(3));
    let next = gate.cut();
    assert_eq!(next, Epoch(4));
    assert!(!gate.pass(Epoch(3), Hop::PreLlm));
    assert!(gate.pass(Epoch(4), Hop::PreLlm));
    // The retired epoch never re-passes, however often asked.
    assert!(!gate.pass(Epoch(3), Hop::PostStt));
}

#[test]
fn three_hop_cancel_maps_to_dictation_stages() {
    // Post-transcribe, pre-refine, pre-commit: a cut retires the
    // generation at every hop, so stale audio never plays post-barge-in.
    for cut_at in [Hop::PostStt, Hop::PreLlm, Hop::PreSpeak] {
        let mut gate = HopGate::new(Epoch(1));
        assert!(gate.pass(Epoch(1), Hop::PostStt));
        gate.cut();
        for hop in [Hop::PostStt, Hop::PreLlm, Hop::PreSpeak] {
            assert!(
                !gate.pass(Epoch(1), hop),
                "cut at {cut_at:?} retires the generation at every hop"
            );
        }
        assert!(gate.pass(Epoch(2), Hop::PostStt), "next generation flows");
    }
}

#[test]
fn detect_ducks_past_threshold_ignores_below() {
    let driver = TurnDriver::new(Epoch(1));
    assert_eq!(driver.detect(149), BargeInOutcome::Ignored);
    assert_eq!(driver.detect(150), BargeInOutcome::Ducking);
    assert_eq!(driver.detect(900), BargeInOutcome::Ducking);
}

#[test]
fn with_config_wires_barge_in_threshold() {
    let config = orator::TurnConfig {
        barge_in_ms: 200,
        ..orator::TurnConfig::default()
    };
    let driver = TurnDriver::with_config(Epoch(1), "en", &config);
    assert_eq!(driver.barge_in_ms(), 200);
    assert_eq!(driver.detect(199), BargeInOutcome::Ignored);
    assert_eq!(driver.detect(200), BargeInOutcome::Ducking);
}

/// The backchannel ceiling follows `turn.backchannel_ms`: a tuned
/// 200 ms lets a 300 ms "yeah" through to the turn, where the
/// default 600 ms sheds it as a backchannel.
#[test]
fn with_config_wires_backchannel_ceiling() {
    let tight = orator::TurnConfig {
        backchannel_ms: 200,
        ..orator::TurnConfig::default()
    };
    let mut driver = TurnDriver::with_config(Epoch(1), "en", &tight);
    assert_eq!(
        driver.confirm(&evidence(200, 300, &["yeah"])),
        BargeInOutcome::Cut
    );
    let mut loose = TurnDriver::with_config(Epoch(2), "en", &orator::TurnConfig::default());
    assert_eq!(
        loose.confirm(&evidence(200, 300, &["yeah"])),
        BargeInOutcome::Ignored
    );
}

#[test]
fn confirm_cuts_speech_restores_backchannels_and_empties() {
    let mut driver = TurnDriver::new(Epoch(1));
    assert_eq!(
        driver.confirm(&evidence(200, 900, &["what", "time", "is", "it"])),
        BargeInOutcome::Cut
    );
    assert_eq!(driver.live(), Epoch(2));
    assert_eq!(
        driver.confirm(&evidence(200, 300, &["yeah"])),
        BargeInOutcome::Ignored
    );
    assert_eq!(
        driver.confirm(&evidence(200, 800, &[])),
        BargeInOutcome::Ignored
    );
    assert_eq!(
        driver.confirm(&evidence(100, 900, &["what", "time"])),
        BargeInOutcome::Ignored
    );
}

#[test]
fn verification_mismatch_never_cuts() {
    let mut driver = TurnDriver::new(Epoch(1));
    let mut tv = evidence(900, 4000, &["breaking", "news", "tonight"]);
    tv.speaker_match = Some(false);
    assert_eq!(driver.confirm(&tv), BargeInOutcome::Ignored);
    assert_eq!(driver.live(), Epoch(1));
}

#[test]
fn history_commits_spoken_words_plus_marker() {
    let mut history = History::new();
    history.push_reply(Epoch(1), vec![word("hello"), word("there")], false);
    history.push_reply(Epoch(2), vec![word("actually")], true);
    assert_eq!(
        history.reply_final(),
        "hello there actually [interrupted by user]"
    );
    assert_eq!(history.turns().len(), 2);
}

#[test]
fn speculation_resume_hands_back_the_prefix_and_cuts() {
    let mut driver = TurnDriver::new(Epoch(5));
    assert_eq!(driver.on_resume(), None);
    driver.speculate("the stable text".to_string());
    let prefix = driver.on_resume().expect("armed prefix returns");
    assert_eq!(prefix, "the stable text");
    assert_eq!(driver.live(), Epoch(6));
    assert!(!driver.check(Epoch(5), Hop::PreLlm));
    assert_eq!(driver.on_resume(), None);
}

#[test]
fn spec_state_arms_and_drops() {
    let mut spec = SpecState::Idle;
    assert!(!spec.is_speculating());
    spec.arm("prefix".to_string());
    assert!(spec.is_speculating());
    assert_eq!(spec.on_resume().as_deref(), Some("prefix"));
    assert!(!spec.is_speculating());
}

#[test]
fn degrade_table_pins_every_row() {
    use SessionKind::{Assistant, Dictation, ListenIn, Meeting};
    assert_eq!(degrade_on("archive", Meeting), SessionMode::Paused);
    assert_eq!(degrade_on("capture.mic", Assistant), SessionMode::Paused);
    assert_eq!(degrade_on("capture.sys", Meeting), SessionMode::RecordOnly);
    assert_eq!(degrade_on("aec", Assistant), SessionMode::Full);
    assert_eq!(degrade_on("vad", Assistant), SessionMode::Full);
    assert_eq!(degrade_on("stt", Dictation), SessionMode::Paused);
    assert_eq!(degrade_on("stt", Meeting), SessionMode::RecordOnly);
    assert_eq!(degrade_on("llm", Assistant), SessionMode::DictationOnly);
    assert_eq!(degrade_on("llm", Dictation), SessionMode::Full);
    assert_eq!(degrade_on("tts", Assistant), SessionMode::TextOnly);
    assert_eq!(degrade_on("tts", ListenIn), SessionMode::Full);
    assert_eq!(degrade_on("notes", Meeting), SessionMode::Full);
}

#[test]
fn speculation_resume_is_exact_at_every_position() {
    // Abort at every token position: arming on each prefix of the
    // stable text and resuming must hand back exactly that prefix —
    // the re-request re-sends it byte-identical for cache reuse. The
    // epoch cut holds at every position too: no prefix ever leaks
    // past the retired epoch.
    let stable = "the stable text";
    for end in 0..=stable.len() {
        let prefix = stable[..end].to_string();
        let mut driver = TurnDriver::new(Epoch(5));
        driver.speculate(prefix.clone());
        let back = driver.on_resume().expect("armed prefix returns");
        assert_eq!(back, prefix, "position {end} must round-trip exactly");
        assert_eq!(driver.live(), Epoch(6), "position {end} cuts the epoch");
        assert!(!driver.check(Epoch(5), Hop::PreLlm));
    }
    // Empty arm resumes to empty (armed, not idle): the re-request
    // sends nothing rather than reusing a stale cache.
    let mut driver = TurnDriver::new(Epoch(5));
    driver.speculate(String::new());
    assert_eq!(driver.on_resume().as_deref(), Some(""));
}

/// M3-S1 characterization: the driver's playout constants duplicate
/// the `orator-audio` envelope home (`DUCK_DEPTH_DB`, `DUCK_RAMP_MS`,
/// `CUT_FADE_MS`). The values are calibrated playout targets — the
/// migration replaces these declarations with the shared home, and
/// this test must pass unchanged.
#[test]
fn orator_turn_duck_constants_match_the_envelope_home() {
    use crate::turn::driver::{CUT_FADE_MS, DUCK_DB, DUCK_RAMP_MS};
    assert!(
        (DUCK_DB - -12.0).abs() < f32::EPSILON,
        "duck depth matches the envelope home"
    );
    assert_eq!(DUCK_RAMP_MS, 100, "duck ramp matches the envelope home");
    assert_eq!(CUT_FADE_MS, 20, "cut fade matches the envelope home");
}

/// Per-frame continuity (M5): the run counts only unbroken hot
/// frames and trips at the configured bound; any cold or PTT-cold
/// frame resets it. Long hot runs saturate instead of wrapping.
#[test]
fn push_frame_counts_only_unbroken_hot_runs() {
    use crate::turn::driver::{BargeInOutcome, TurnDriver};
    use orator::TurnConfig;
    let mut driver = TurnDriver::with_config(Epoch(1), "en", &TurnConfig::default());
    // Below the 150 ms bound: 7 hot frames (140 ms) stay ignored.
    for _ in 0..7 {
        assert_eq!(
            driver.push_frame(true, false, 20),
            BargeInOutcome::Ignored,
            "below-bound run holds"
        );
    }
    // At the bound: the 8th hot frame ducks, and heat keeps ducking.
    assert_eq!(
        driver.push_frame(true, false, 20),
        BargeInOutcome::Ducking,
        "at-bound frame trips"
    );
    assert_eq!(
        driver.push_frame(true, false, 20),
        BargeInOutcome::Ducking,
        "continued heat stays tripped"
    );
    // Any cold frame resets: 7 more hot frames hold again.
    assert_eq!(
        driver.push_frame(false, false, 20),
        BargeInOutcome::Ignored,
        "cold frame resets the run"
    );
    for _ in 0..7 {
        assert_eq!(
            driver.push_frame(true, false, 20),
            BargeInOutcome::Ignored,
            "post-pause run restarts from zero"
        );
    }
    // PTT-cold frames reset even when hot.
    let mut ptt = TurnDriver::with_config(Epoch(2), "en", &TurnConfig::default());
    for _ in 0..10 {
        assert_eq!(
            ptt.push_frame(true, true, 20),
            BargeInOutcome::Ignored,
            "latched-off mic never advances the run"
        );
    }
    // Saturation: a very long hot run stays tripped, never wraps.
    let mut long = TurnDriver::with_config(Epoch(3), "en", &TurnConfig::default());
    for _ in 0..100_000 {
        long.push_frame(true, false, 20);
    }
    assert_eq!(
        long.push_frame(true, false, 20),
        BargeInOutcome::Ducking,
        "saturated run stays tripped"
    );
    // A raised leaf moves the trip point with it.
    let mut raised = TurnDriver::with_config(
        Epoch(4),
        "en",
        &orator::TurnConfig {
            barge_in_ms: 300,
            ..TurnConfig::default()
        },
    );
    for _ in 0..14 {
        raised.push_frame(true, false, 20);
    }
    assert_eq!(
        raised.push_frame(true, false, 20),
        BargeInOutcome::Ducking,
        "raised bound trips at 300 ms"
    );
}
