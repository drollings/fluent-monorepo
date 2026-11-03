//! Ladder tests: forced failures land on exact fallback rungs.

use std::sync::Arc;

use crate::clipboard::{ChordSender, InMemoryClipboard};
use crate::ladder::{inject, InjectError, InjectOutcome, LadderMemory, Rung};

struct DeadChord;

impl ChordSender for DeadChord {
    fn send_paste(&self) -> Result<(), InjectError> {
        Ok(())
    }
    fn send_copy(&self) -> Result<(), InjectError> {
        Ok(())
    }
}

#[test]
fn clipboard_rung_wins_when_chord_lands() {
    let clip = InMemoryClipboard::new();
    let outcome = inject(&clip, &DeadChord, "hi", &mut LadderMemory::default())
        .expect("ladder runs");
    // RemoteDesktop is unwired; the clipboard rung pastes into the fake and
    // the still clipboard restores: still a successful injection.
    assert_eq!(outcome, InjectOutcome::Injected(Rung::ClipboardPaste));
}

#[test]
fn remembered_rung_is_tried_first() {
    let clip = InMemoryClipboard::new();
    let mut memory = LadderMemory {
        last_working: Some(Rung::ClipboardPaste),
        last_inject: None,
    };
    let outcome = inject(&clip, &DeadChord, "hi", &mut memory).expect("ladder runs");
    assert_eq!(outcome, InjectOutcome::Injected(Rung::ClipboardPaste));
}

#[test]
fn fatal_aborts_the_ladder() {
    struct FatalChord;
    impl ChordSender for FatalChord {
        fn send_paste(&self) -> Result<(), InjectError> {
            Err(InjectError::Fatal("denied".to_string()))
        }
        fn send_copy(&self) -> Result<(), InjectError> {
            Ok(())
        }
    }
    let clip = InMemoryClipboard::new();
    inject(&clip, &FatalChord, "hi", &mut LadderMemory::default())
        .expect_err("fatal must abort");
}

#[test]
fn headless_reports_output_only_distinctly() {
    use crate::clipboard::NoChord;
    let clip = InMemoryClipboard::new();
    let outcome = inject(&clip, &NoChord, "hello headless", &mut LadderMemory::default())
        .expect("ladder runs");
    assert_eq!(
        outcome,
        InjectOutcome::OutputOnly("hello headless".to_string()),
        "headless never reports false Injected success"
    );
}

#[test]
fn ladder_memory_is_per_handle_not_global() {
    let mut first = LadderMemory::default();
    let second = LadderMemory::default();
    first.last_working = Some(Rung::ClipboardPaste);
    assert_eq!(
        first.last_working,
        Some(Rung::ClipboardPaste),
        "handle keeps its own memory"
    );
    assert_eq!(second.last_working, None, "memory pins per session handle");
    assert_eq!(second.last_inject, None, "fresh handles carry no timestamp");
}

#[test]
fn consecutive_dictation_joins_with_a_space() {
    use crate::clipboard::NoChord;
    let clip = InMemoryClipboard::new();
    let mut memory = LadderMemory::default();
    let first = inject(&clip, &NoChord, "One.", &mut memory).expect("ladder runs");
    assert_eq!(first, InjectOutcome::OutputOnly("One.".to_string()));
    let second = inject(&clip, &NoChord, "Two.", &mut memory).expect("ladder runs");
    assert_eq!(
        second,
        InjectOutcome::OutputOnly(" Two.".to_string()),
        "\"One.\" + \"Two.\" lands as \"One. Two.\""
    );
}

#[test]
fn shared_clipboard_shares_state() {
    let shared: Arc<dyn crate::clipboard::Clipboard> = Arc::new(InMemoryClipboard::new());
    shared.write("x").expect("write");
    assert_eq!(shared.read().as_deref(), Some("x"));
}

#[test]
fn each_failing_rung_falls_back_to_output_only() {
    use crate::clipboard::NoChord;
    // Forced-fail every rung in isolation (headless: no chord can
    // paste): each must land on the exact output-only fallback, never
    // a false `Injected` and never an error.
    for rung in [
        Rung::RemoteDesktop,
        Rung::ClipboardPaste,
        Rung::VirtualKeyboard,
        Rung::Uinput,
    ] {
        let clip = InMemoryClipboard::new();
        let mut memory = LadderMemory {
            last_working: Some(rung.clone()),
            last_inject: None,
        };
        let outcome = inject(&clip, &NoChord, "forced down", &mut memory)
            .expect("ladder always resolves");
        assert_eq!(
            outcome,
            InjectOutcome::OutputOnly("forced down".to_string()),
            "rung {rung:?} must fall back exactly"
        );
    }
}

#[test]
fn failed_paste_skips_to_output_only() {
    use crate::clipboard::NoChord;
    // A lost chord is a skip, not terminal: ClipboardPaste fails and
    // the ladder still resolves output-only (same sink as headless).
    struct LostChord;
    impl ChordSender for LostChord {
        fn send_paste(&self) -> Result<(), InjectError> {
            Err(InjectError::Failed("chord lost".to_string()))
        }
        fn send_copy(&self) -> Result<(), InjectError> {
            Ok(())
        }
    }
    let clip = InMemoryClipboard::new();
    let outcome = inject(&clip, &LostChord, "lost chord", &mut LadderMemory::default())
        .expect("failed paste still resolves");
    assert!(
        matches!(outcome, InjectOutcome::OutputOnly(_)),
        "lost chord must skip to output-only: {outcome:?}"
    );
    // NoChord agrees: the fallback is exact, not chord-dependent.
    let silent = inject(&clip, &NoChord, "lost chord", &mut LadderMemory::default())
        .expect("headless resolves");
    assert_eq!(outcome, silent);
}
