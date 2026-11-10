//! The single wire-protocol registry.
//!
//! Message ids and method names live here and nowhere else. Both the daemon
//! and every client validate against these tables, so a renamed variant
//! fails loudly instead of corrupting the stream at runtime.

use crate::error::ProtocolError;

/// One-byte wire message families. Discriminants are stable on the wire:
/// never reorder, never reuse a retired value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MessageType {
    /// Protocol version and capability handshake.
    Hello = 0,
    /// Opt-in PCM or Opus audio, up or down.
    Audio = 1,
    /// Correlated control request.
    Request = 2,
    /// Correlated control response.
    Response = 3,
    /// Sequenced event (durable or live).
    Event = 4,
    /// Transport or application error.
    Error = 5,
    /// Keepalive.
    Ping = 6,
    /// Event subscription and replay cursor.
    Subscribe = 7,
}

impl MessageType {
    /// The wire byte for this family.
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        self as u8
    }

    /// Decode a wire byte. Unknown values are rejected, never defaulted.
    #[must_use]
    pub const fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Hello),
            1 => Some(Self::Audio),
            2 => Some(Self::Request),
            3 => Some(Self::Response),
            4 => Some(Self::Event),
            5 => Some(Self::Error),
            6 => Some(Self::Ping),
            7 => Some(Self::Subscribe),
            _ => None,
        }
    }

    /// Every family on the wire, in discriminant order.
    pub const ALL: [Self; 8] = [
        Self::Hello,
        Self::Audio,
        Self::Request,
        Self::Response,
        Self::Event,
        Self::Error,
        Self::Ping,
        Self::Subscribe,
    ];
}

/// Method names accepted in `Request` bodies and over D-Bus: the wire
/// contract both sides speak. The CLI builds requests only from this
/// set; the daemon answers each with work or a named refusal.
/// `Transcribe` lets thin clients (the TUI) turn captured audio into
/// text through the daemon instead of owning inference.
/// `ExportMeeting` renders a session's recorded turns to a managed
/// markdown artifact (additive: existing bytes untouched).
pub const METHOD_NAMES: [&str; 14] = [
    "StartSession",
    "StopSession",
    "Interrupt",
    "Say",
    "Ask",
    "ToggleDictation",
    "ListSessions",
    "GetState",
    "VoiceEndpoint",
    "StopDaemon",
    "Reload",
    "Approve",
    "Transcribe",
    "ExportMeeting",
];

/// Accept `method` iff it belongs to the wire contract.
pub fn validate_method(method: &str) -> Result<(), ProtocolError> {
    if METHOD_NAMES.contains(&method) {
        Ok(())
    } else {
        Err(ProtocolError::UnknownMethod(method.to_string()))
    }
}

#[cfg(test)]
#[path = "../tests/protocol.rs"]
mod tests;
