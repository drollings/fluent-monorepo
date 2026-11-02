//! Tray status model: what the indicator shows. The live StatusNotifier
//! connection lands with the desktop surface; the model (states, labels,
//! transitions) is pinned now so behavior never depends on the backend.

/// Indicator states. Recording states cannot be hidden by configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrayStatus {
    /// Daemon idle, no capture.
    Idle,
    /// Microphone live (dictation/assistant).
    MicLive,
    /// Meeting or listen-in recording (mandatory indicator).
    Recording,
    /// Something needs attention (model missing, device lost).
    Attention,
}

impl TrayStatus {
    /// Short label for tooltips and menus.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Idle => "Orator idle",
            Self::MicLive => "Orator: microphone live",
            Self::Recording => "Orator: recording",
            Self::Attention => "Orator needs attention",
        }
    }

    /// Whether this state may ever be hidden. Recording indicators cannot
    /// be disabled.
    #[must_use]
    pub const fn hideable(self) -> bool {
        !matches!(self, Self::Recording)
    }
}

/// Current tray state plus the reason shown on hover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayState {
    /// Indicator state.
    pub status: TrayStatus,
    /// Human-readable reason (device name, session kind, error).
    pub reason: String,
}

impl TrayState {
    /// Idle with no reason.
    #[must_use]
    pub fn idle() -> Self {
        Self {
            status: TrayStatus::Idle,
            reason: String::new(),
        }
    }

    /// Indicator for a session kind: meetings and listen-ins always
    /// show recording (mandatory, never hideable); dictation and the
    /// assistant show the live microphone.
    #[must_use]
    pub fn for_session(kind: &str, alert: Option<&str>) -> Self {
        if let Some(alert) = alert {
            return Self {
                status: TrayStatus::Attention,
                reason: alert.to_string(),
            };
        }
        match kind {
            "meeting" | "listen" | "listen-in" => Self {
                status: TrayStatus::Recording,
                reason: format!("{kind} session recording"),
            },
            _ => Self {
                status: TrayStatus::MicLive,
                reason: format!("{kind} session live"),
            },
        }
    }
}

#[cfg(test)]
#[path = "../tests/tray.rs"]
mod tests;
