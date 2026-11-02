//! Dictate bounds: seconds validation, deadline math, capture choice.

use crate::dictate::{
    capture_choice, checked_deadline, finish_turn, reserve_for, validate_seconds, CaptureChoice,
    MAX_DICTATE_SECS,
};

#[test]
fn zero_seconds_refuses_as_usage() {
    assert!(validate_seconds(0).is_err());
}

#[test]
fn huge_seconds_refuses_with_named_bound() {
    let error = validate_seconds(u64::MAX).expect_err("huge refuses");
    assert!(error.contains(&MAX_DICTATE_SECS.to_string()), "{error}");
    let error = validate_seconds(MAX_DICTATE_SECS + 1).expect_err("over-ceiling refuses");
    assert!(error.contains("ceiling"), "{error}");
}

#[test]
fn valid_seconds_yield_duration() {
    assert_eq!(
        validate_seconds(5).expect("valid"),
        std::time::Duration::from_secs(5)
    );
}

#[test]
fn checked_deadline_never_panics() {
    let now = std::time::Instant::now();
    assert!(checked_deadline(now, std::time::Duration::from_secs(5)).is_ok());
    assert!(checked_deadline(now, std::time::Duration::from_secs(u64::MAX)).is_err());
}

#[test]
fn reserve_caps_at_max_samples() {
    assert!(reserve_for(30, 16_000) <= crate::dictate::MAX_DICTATE_SAMPLES);
    assert_eq!(
        reserve_for(u64::MAX, 48_000),
        crate::dictate::MAX_DICTATE_SAMPLES
    );
}

#[test]
fn capture_choice_wires_mic_and_backend() {
    assert_eq!(
        capture_choice("cpal", "default").expect("default"),
        CaptureChoice::Default
    );
    assert_eq!(
        capture_choice("cpal", "front-mic").expect("named"),
        CaptureChoice::Named("front-mic".to_string())
    );
    assert!(capture_choice("pipewire", "default").is_err());
}

#[test]
fn finish_turn_refines_records_and_joins_continuation() {
    use orator_desktop::{InMemoryClipboard, InjectOutcome, LadderMemory, NoChord};
    let clip = InMemoryClipboard::new();
    let mut memory = LadderMemory::default();
    let mut history = Vec::new();
    let first = finish_turn("One.", &mut memory, &mut history, &clip, &NoChord)
        .expect("headless injects to stdout");
    assert_eq!(first, InjectOutcome::OutputOnly("One.".to_string()));
    let second = finish_turn("Two.", &mut memory, &mut history, &clip, &NoChord)
        .expect("headless injects to stdout");
    assert_eq!(
        second,
        InjectOutcome::OutputOnly(" Two.".to_string()),
        "continuation spacing through the real dictate path"
    );
    assert_eq!(
        history,
        vec!["One.".to_string(), "Two.".to_string()],
        "clean text recorded for every turn"
    );
}
