//! Control API model for `org.fluent.Orator1` (session bus).
//!
//! The method table equals the wire contract exactly — a D-Bus method
//! that is not a contract method cannot exist, and the parity test
//! enforces it. Content signals (`Transcript`, `ReplyText`) are off
//! by default; streams of content go over the credentialed socket.
//! Approvals arrive only from the socket or a notification action:
//! an arbitrary D-Bus caller id never approves.

use orator::protocol::METHOD_NAMES;

/// Service name on the session bus.
pub const SERVICE_NAME: &str = "org.fluent.Orator1";

/// Properties exposed on the service.
pub const PROPERTIES: [&str; 5] = ["State", "ActiveSession", "MicLive", "Profile", "Recording"];

/// Signals emitted by the service.
pub const SIGNALS: [&str; 5] = [
    "StateChanged",
    "ApprovalRequested",
    "Error",
    "Transcript",
    "ReplyText",
];

/// Content signals, off unless the user opts in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SignalPolicy {
    /// Whether transcript/reply signals emit.
    pub content_signals: bool,
}

impl SignalPolicy {
    /// Default policy: content signals off.
    #[must_use]
    pub const fn locked_down() -> Self {
        Self {
            content_signals: false,
        }
    }

    /// Whether `signal` may emit under this policy.
    #[must_use]
    pub fn may_emit(&self, signal: &str) -> bool {
        match signal {
            "Transcript" | "ReplyText" => self.content_signals,
            "StateChanged" | "ApprovalRequested" | "Error" => true,
            _ => false,
        }
    }
}

/// Where an approval answer arrived from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// The credentialed socket.
    Socket,
    /// A notification action button.
    Notification,
    /// An arbitrary D-Bus caller id (never trusted for approvals).
    Dbus(String),
}

impl Caller {
    /// Whether an approval answer from this caller is accepted.
    #[must_use]
    pub const fn may_approve(&self) -> bool {
        match self {
            Self::Socket | Self::Notification => true,
            Self::Dbus(_) => false,
        }
    }
}

/// Whether `method` is served on the control API. The table is the
/// wire contract — one set, never two.
#[must_use]
pub fn serves(method: &str) -> bool {
    METHOD_NAMES.contains(&method)
}

#[cfg(test)]
#[path = "../tests/dbus.rs"]
mod tests;
