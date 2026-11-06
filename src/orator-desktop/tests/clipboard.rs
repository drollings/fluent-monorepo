//! Clipboard tests: restore discipline both directions, fake fidelity.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::clipboard::{
    capture_selection, paste_with_restore, ChordSender, Clipboard, InMemoryClipboard, NoChord,
    PasteOutcome,
};
use crate::ladder::InjectError;

/// Chord sender whose paste flips the clipboard like a compositor would.
struct EchoChord {
    clip: Arc<InMemoryClipboard>,
    fail_paste: AtomicBool,
}

impl ChordSender for EchoChord {
    fn send_paste(&self) -> Result<(), InjectError> {
        if self.fail_paste.load(Ordering::SeqCst) {
            return Err(InjectError::Failed("chord lost".to_string()));
        }
        // The "compositor" consumes the clipboard: content moves.
        self.clip.write("consumed")?;
        Ok(())
    }
    fn send_copy(&self) -> Result<(), InjectError> {
        Ok(())
    }
}

fn echo_clip() -> (Arc<InMemoryClipboard>, EchoChord) {
    let clip = Arc::new(InMemoryClipboard::new());
    let chord = EchoChord {
        clip: Arc::clone(&clip),
        fail_paste: AtomicBool::new(false),
    };
    (clip, chord)
}

#[test]
fn moved_clipboard_is_left_alone() {
    let (clip, chord) = echo_clip();
    clip.write("saved").expect("setup");
    let outcome = paste_with_restore(&*clip, &chord, "hello", 180).expect("paste");
    assert_eq!(outcome, PasteOutcome::Pasted);
}

#[test]
fn still_clipboard_restores_previous() {
    struct QuietChord;
    impl ChordSender for QuietChord {
        fn send_paste(&self) -> Result<(), InjectError> {
            Ok(())
        }
        fn send_copy(&self) -> Result<(), InjectError> {
            Ok(())
        }
    }
    let clip = InMemoryClipboard::new();
    clip.write("saved").expect("setup");
    let outcome = paste_with_restore(&clip, &QuietChord, "hello", 10).expect("paste");
    assert_eq!(outcome, PasteOutcome::PastedRestored);
    assert_eq!(clip.read().as_deref(), Some("saved"));
}

#[test]
fn capture_restores_after_reading() {
    struct CopyChord<'a> {
        clip: &'a InMemoryClipboard,
    }
    impl ChordSender for CopyChord<'_> {
        fn send_paste(&self) -> Result<(), InjectError> {
            Ok(())
        }
        fn send_copy(&self) -> Result<(), InjectError> {
            self.clip.write("selected text")?;
            Ok(())
        }
    }
    let clip = InMemoryClipboard::new();
    clip.write("mine").expect("setup");
    let chord = CopyChord { clip: &clip };
    let captured = capture_selection(&clip, &chord, 200).expect("capture");
    assert_eq!(captured.as_deref(), Some("selected text"));
    assert_eq!(clip.read().as_deref(), Some("mine"));
}

#[test]
fn failed_chord_propagates() {
    let (clip, chord) = echo_clip();
    chord.fail_paste.store(true, Ordering::SeqCst);
    paste_with_restore(&*clip, &chord, "hello", 180).expect_err("chord loss must fail");
}

#[test]
fn no_chord_is_always_unavailable() {
    let clip = InMemoryClipboard::new();
    paste_with_restore(&clip, &NoChord, "hello", 10)
        .expect_err("no chord sender must skip the rung");
}

#[test]
fn own_write_never_counts_as_movement() {
    // The fingerprint is taken AFTER our own write: if it were taken
    // before, the write itself would look like external movement and
    // the paste would wrongly report `Pasted`.
    struct QuietChord;
    impl ChordSender for QuietChord {
        fn send_paste(&self) -> Result<(), InjectError> {
            Ok(())
        }
        fn send_copy(&self) -> Result<(), InjectError> {
            Ok(())
        }
    }
    let clip = InMemoryClipboard::new();
    let outcome = paste_with_restore(&clip, &QuietChord, "hello", 180).expect("paste");
    assert_eq!(
        outcome,
        PasteOutcome::PastedRestored,
        "a still clipboard restores instead of claiming movement"
    );
    assert_eq!(clip.read().as_deref(), None, "nothing saved, clipboard cleared");
}

#[test]
fn polling_observes_changes_promptly() {
    use std::time::{Duration, Instant};
    // A change landing mid-deadline returns promptly: a poll loop
    // observes it within milliseconds, while an endpoint-only check
    // would burn the whole deadline. Bands are generous (10 ms polls
    // against a 1 s budget); only a wedged scheduler could flake this.
    let clip = Arc::new(InMemoryClipboard::new());
    let worker = Arc::clone(&clip);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        worker.write("transient").expect("transient change");
    });
    let started = Instant::now();
    let outcome = paste_with_restore(&*clip, &QuietPaste, "hello", 2000).expect("paste");
    assert_eq!(outcome, PasteOutcome::Pasted, "mid-deadline change observed");
    assert!(
        started.elapsed() < Duration::from_millis(1000),
        "polling returns promptly, never burns the deadline"
    );
}

struct QuietPaste;

impl ChordSender for QuietPaste {
    fn send_paste(&self) -> Result<(), InjectError> {
        Ok(())
    }
    fn send_copy(&self) -> Result<(), InjectError> {
        Ok(())
    }
}

/// The clipboard child bound from the parent module: tests pin the
/// shipped values so the calibration row stays honest.
#[test]
fn clipboard_child_bound_stays_pinned() {
    use crate::clipboard::{CLIPBOARD_CHILD_DEADLINE_MS, CLIPBOARD_POLL_MS};
    assert_eq!(CLIPBOARD_POLL_MS, 10);
    assert_eq!(CLIPBOARD_CHILD_DEADLINE_MS, 2000);
}

/// An instantly-exiting child reports its status, never a timeout.
#[test]
fn bounded_wait_returns_instant_child_status() {
    use crate::clipboard::wait_child_bounded;
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn true");
    let status = wait_child_bounded(&mut child, 2000).expect("instant child reports");
    assert!(status.success());
}

/// An exited-but-failed child reports failure, never a timeout: exit
/// status and deadline are different signals.
#[test]
fn bounded_wait_reports_failure_status_not_timeout() {
    use crate::clipboard::wait_child_bounded;
    let mut child = std::process::Command::new("false")
        .spawn()
        .expect("spawn false");
    let status = wait_child_bounded(&mut child, 2000).expect("failed child still reports");
    assert!(!status.success());
}

/// A parked child is killed, reaped, and named — well under any
/// interactive hang. The post-call `try_wait` proves no zombie.
#[test]
fn bounded_wait_kills_and_reaps_parked_child() {
    use std::time::{Duration, Instant};
    use crate::clipboard::{wait_child_bounded, ChildWaitError};
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");
    let started = Instant::now();
    let error = wait_child_bounded(&mut child, 200).expect_err("parked child must time out");
    assert_eq!(error, ChildWaitError::Timeout);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "deadline fires, never the old hang: {:?}",
        started.elapsed()
    );
    assert!(
        child.try_wait().expect("waitable").is_some(),
        "killed child is reaped, never a zombie"
    );
}

/// A slow-but-under-deadline child succeeds: the bound never fires
/// early on a merely slow compositor handshake.
#[test]
fn bounded_wait_tolerates_slow_under_deadline_child() {
    use crate::clipboard::wait_child_bounded;
    let mut child = std::process::Command::new("sleep")
        .arg("0.2")
        .spawn()
        .expect("spawn sleep");
    let status = wait_child_bounded(&mut child, 5000).expect("slow child succeeds");
    assert!(status.success());
}
