//! Session indicators: mandatory recording display plus consent.
//!
//! Meeting and listen-in indicators cannot be disabled: starting such
//! a session always flips the tray to recording and posts a
//! notification with a stop action. The consent reminder names what is
//! captured so every participant can be told.

use crate::notify::Notification;
use crate::tray::{TrayState, TrayStatus};

/// Whether `kind` forces visible indicators that ignore configuration.
#[must_use]
pub fn indicators_mandatory(kind: &str) -> bool {
    matches!(kind, "meeting" | "listen" | "listen-in")
}

/// Consent reminder spoken or shown when capture starts.
#[must_use]
pub fn consent_reminder(kind: &str) -> String {
    match kind {
        "meeting" => "Recording this meeting: microphone and system audio are being captured and transcribed.".to_string(),
        "listen" | "listen-in" => {
            "Listening: system audio is being captured and transcribed.".to_string()
        }
        _ => "Microphone is live for this session.".to_string(),
    }
}

/// Indicator state for a session start: tray plus notification. The
/// tray row is recording for mandatory kinds no matter what the
/// configuration requests.
#[must_use]
pub fn session_indicator(kind: &str) -> (TrayState, Notification) {
    let tray = TrayState::for_session(kind, None);
    assert!(
        !indicators_mandatory(kind) || tray.status == TrayStatus::Recording,
        "mandatory indicators cannot downgrade"
    );
    (tray, Notification::recording(kind))
}

#[cfg(test)]
#[path = "../tests/indicators.rs"]
mod tests;
