//! Indicator tests: mandatory recording, consent wording.

use crate::indicators::{consent_reminder, indicators_mandatory, session_indicator};
use crate::tray::TrayStatus;

#[test]
fn meeting_and_listen_are_mandatory() {
    assert!(indicators_mandatory("meeting"));
    assert!(indicators_mandatory("listen"));
    assert!(indicators_mandatory("listen-in"));
    assert!(!indicators_mandatory("dictation"));
    assert!(!indicators_mandatory("assistant"));
}

#[test]
fn session_start_forces_recording_display() {
    let (tray, note) = session_indicator("meeting");
    assert_eq!(tray.status, TrayStatus::Recording);
    assert!(!tray.status.hideable());
    assert_eq!(note.title, "Recording");
}

#[test]
fn consent_names_the_capture() {
    assert!(consent_reminder("meeting").contains("system audio"));
    assert!(consent_reminder("dictation").contains("Microphone"));
}
