//! Notifications with approve/deny/stop actions.
//!
//! Summaries stay content-free by default: the notification names the
//! tool and its effect, never the transcript or reply text that
//! prompted it. Delivery rides an injected sink so tests and the
//! desktop backend share the shape; the backend wires
//! `org.freedesktop.Notifications` later.

/// Notification actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotifyAction {
    /// Approve the pending round.
    Approve(String),
    /// Deny the pending round.
    Deny(String),
    /// Stop the recording session.
    StopRecording,
}

impl NotifyAction {
    /// Button label.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Approve(_) => "Approve",
            Self::Deny(_) => "Deny",
            Self::StopRecording => "Stop recording",
        }
    }
}

/// One notification: title, content-free summary, and actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// Short title (tool name, session state).
    pub title: String,
    /// Summary without transcript or reply content.
    pub summary: String,
    /// Action buttons.
    pub actions: Vec<NotifyAction>,
}

impl Notification {
    /// Approval prompt for `tool`: names the effect, never the content.
    #[must_use]
    pub fn approval(id: &str, tool: &str) -> Self {
        Self {
            title: "Approval requested".to_string(),
            summary: format!("{tool} needs approval"),
            actions: vec![
                NotifyAction::Approve(id.to_string()),
                NotifyAction::Deny(id.to_string()),
            ],
        }
    }

    /// Mandatory recording indicator for Meeting/Listen-in.
    #[must_use]
    pub fn recording(kind: &str) -> Self {
        Self {
            title: "Recording".to_string(),
            summary: format!("{kind} session is recording"),
            actions: vec![NotifyAction::StopRecording],
        }
    }
}

/// Delivery sink: the desktop backend implements the real one.
pub trait NotifySink: Send + Sync {
    /// Show `notification`.
    fn show(&self, notification: &Notification);
}

#[cfg(test)]
#[path = "../tests/notify.rs"]
mod tests;
