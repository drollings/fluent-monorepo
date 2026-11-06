//! Dictate bounds: seconds validation, deadline math, capture choice.

use crate::dictate::{
    capture_choice, checked_deadline, finish_turn, print_text_for, reserve_for, validate_seconds,
    CaptureChoice, MAX_DICTATE_SECS,
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

#[test]
fn injected_plus_stdout_prints_once() {
    use orator_desktop::{InjectOutcome, Rung};
    let outcome = InjectOutcome::Injected(Rung::ClipboardPaste);
    assert_eq!(
        print_text_for(true, "hello", &outcome),
        Some("hello".to_string()),
        "injected + --stdout prints the text once"
    );
}

#[test]
fn injected_without_stdout_prints_nothing() {
    use orator_desktop::{InjectOutcome, Rung};
    let outcome = InjectOutcome::Injected(Rung::ClipboardPaste);
    assert_eq!(
        print_text_for(false, "hello", &outcome),
        None,
        "injected without --stdout stays silent (already in the window)"
    );
}

#[test]
fn output_only_prints_through_the_same_gate() {
    use orator_desktop::InjectOutcome;
    let outcome = InjectOutcome::OutputOnly("hello".to_string());
    // Stdout is the sink: prints even without --stdout, exactly once.
    assert_eq!(
        print_text_for(false, "hello", &outcome),
        Some("hello".to_string())
    );
    assert_eq!(
        print_text_for(true, "hello", &outcome),
        Some("hello".to_string()),
        "--stdout with OutputOnly still prints once, never twice"
    );
}

#[test]
fn cap_counts_drops_and_breaks_early() {
    use crate::dictate::{apply_cap, MAX_DICTATE_SAMPLES};
    // Room for 10: keeps all, no drops, not capped.
    assert_eq!(apply_cap(0, 10), (10, 0, false));
    // Exactly at cap: kept fills, capped signals outer break.
    assert_eq!(
        apply_cap(MAX_DICTATE_SAMPLES - 5, 5),
        (MAX_DICTATE_SAMPLES, 0, true)
    );
    // Over cap: keeps to the bound, counts the rest, capped.
    assert_eq!(
        apply_cap(MAX_DICTATE_SAMPLES - 5, 10),
        (MAX_DICTATE_SAMPLES, 5, true)
    );
    // Already full: everything drops.
    assert_eq!(
        apply_cap(MAX_DICTATE_SAMPLES, 3),
        (MAX_DICTATE_SAMPLES, 3, true)
    );
}

/// An expired clipboard bound still delivers the words: the ladder
/// skips the dead rung to the output-only sink instead of hanging,
/// and the transcript is recorded either way.
#[test]
fn expired_clipboard_bound_still_delivers_words() {
    use orator_desktop::{Clipboard, InjectOutcome, LadderMemory, NoChord};
    struct ExpiredBound;
    impl Clipboard for ExpiredBound {
        fn read(&self) -> Option<String> {
            None
        }
        fn write(&self, _text: &str) -> Result<(), orator_desktop::InjectError> {
            Err(orator_desktop::InjectError::Failed(
                "wl-copy exceeded its bound (compositor busy or absent)".to_string(),
            ))
        }
        fn fingerprint(&self) -> u64 {
            0
        }
    }
    let clip = ExpiredBound;
    let mut memory = LadderMemory::default();
    let mut history = Vec::new();
    let started = std::time::Instant::now();
    let outcome = finish_turn("hello there", &mut memory, &mut history, &clip, &NoChord)
        .expect("words survive an expired bound");
    assert_eq!(
        outcome,
        InjectOutcome::OutputOnly("hello there".to_string())
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "bounded composition, never the old hang: {:?}",
        started.elapsed()
    );
    assert_eq!(history, vec!["hello there".to_string()]);
}
