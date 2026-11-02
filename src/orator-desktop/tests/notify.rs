//! Notify tests: content-free summaries, action labels.

use crate::notify::{Notification, NotifyAction};

#[test]
fn approval_names_the_tool_not_the_content() {
    let note = Notification::approval("appr-1", "shell");
    assert_eq!(note.actions.len(), 2);
    assert_eq!(note.actions[0].label(), "Approve");
    assert_eq!(note.actions[1].label(), "Deny");
    assert!(note.summary.contains("shell"));
    assert!(!note.summary.contains("password"));
}

#[test]
fn recording_indicator_carries_a_stop_action() {
    let note = Notification::recording("Meeting");
    assert_eq!(note.actions, vec![NotifyAction::StopRecording]);
    assert_eq!(NotifyAction::StopRecording.label(), "Stop recording");
}
