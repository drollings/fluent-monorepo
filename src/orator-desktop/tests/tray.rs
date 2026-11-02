//! Tray model tests: labels and the non-hideable recording state.

use crate::tray::{TrayState, TrayStatus};

#[test]
fn labels_describe_states() {
    assert_eq!(TrayStatus::Idle.label(), "Orator idle");
    assert!(TrayStatus::Recording.label().contains("recording"));
}

#[test]
fn recording_cannot_hide() {
    assert!(!TrayStatus::Recording.hideable());
    assert!(TrayStatus::Idle.hideable());
    assert!(TrayStatus::MicLive.hideable());
}

#[test]
fn idle_state_is_empty() {
    let state = TrayState::idle();
    assert_eq!(state.status, TrayStatus::Idle);
    assert!(state.reason.is_empty());
}
