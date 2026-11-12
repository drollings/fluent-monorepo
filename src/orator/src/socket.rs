//! Credentialed socket framing: length, type, body.
//!
//! Every frame on the daemon socket is a 4-byte little-endian length,
//! a 1-byte message-type id from the single registry, and an opaque
//! body (MessagePack on the wire; tests use short JSON bodies through
//! the same framing). The socket file itself is mode 0600 in a 0700
//! runtime directory, and the daemon serves a peer only when its uid
//! matches its own — the equality gate below, fed by the platform
//! credential read at accept time.

use std::io::{Read, Write};

use crate::protocol::MessageType;

/// Socket file name inside the runtime directory.
pub const SOCKET_NAME: &str = "orator.sock";

/// Socket file mode: owner only.
pub const SOCKET_MODE: u32 = 0o600;

/// Largest accepted frame body in bytes (8 MiB; audio chunks stay far
/// below, and a week-old replay pages instead of batching).
pub const MAX_BODY: usize = 8 * 1024 * 1024;

/// Failures decoding one frame.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
/// Failures decoding one frame.
pub enum FrameError {
    /// Fewer than 5 header bytes available.
    #[error("frame too short for header")]
    Short,
    /// Declared length exceeds the cap.
    #[error("frame body of {0} bytes exceeds the cap")]
    TooLarge(usize),
    /// Trailing bytes past the declared body.
    #[error("frame has {0} trailing bytes")]
    Trailing(usize),
    /// Unknown message-type id.
    #[error("unknown message type: {0}")]
    UnknownType(u8),
    /// Transport failure while streaming a frame.
    #[error("io: {0}")]
    Io(String),
    /// Audio frame with a zero sample rate, which can never describe
    /// audio. Carries the offending rate.
    #[error("invalid sample rate: {0}")]
    InvalidSampleRate(u32),
}

/// One framed message: its family plus the opaque body.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One framed message: its family plus the opaque body.
pub struct SocketFrame {
    /// Message family from the single registry.
    pub kind: MessageType,
    /// Opaque body bytes.
    pub body: Vec<u8>,
}

impl SocketFrame {
    /// Encode to wire bytes: length + type + body.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(5 + self.body.len());
        let len = (self.body.len() + 1) as u32;
        out.extend_from_slice(&len.to_le_bytes());
        out.push(self.kind.to_u8());
        out.extend_from_slice(&self.body);
        out
    }

    /// Decode one frame from the front of `bytes`. Returns the frame;
    /// trailing bytes are an error, never silently accepted.
    pub fn decode(bytes: &[u8]) -> Result<Self, FrameError> {
        if bytes.len() < 5 {
            return Err(FrameError::Short);
        }
        let len = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        if len < 1 {
            return Err(FrameError::Short);
        }
        let body_len = len - 1;
        if body_len > MAX_BODY {
            return Err(FrameError::TooLarge(body_len));
        }
        if bytes.len() < 5 + body_len {
            return Err(FrameError::Short);
        }
        if bytes.len() > 5 + body_len {
            return Err(FrameError::Trailing(bytes.len() - 5 - body_len));
        }
        let kind = MessageType::from_u8(bytes[4]).ok_or(FrameError::UnknownType(bytes[4]))?;
        Ok(Self {
            kind,
            body: bytes[5..].to_vec(),
        })
    }

    /// Read exactly one frame from `reader`: 4-byte little-endian length,
    /// 1-byte type, body. The wire bytes match [`SocketFrame::encode`]
    /// byte-for-byte; length, cap and type violations report the same
    /// [`FrameError`] variants as [`SocketFrame::decode`].
    pub fn read_from(reader: &mut impl Read) -> Result<Self, FrameError> {
        let mut len_buf = [0u8; 4];
        reader
            .read_exact(&mut len_buf)
            .map_err(|error| FrameError::Io(error.to_string()))?;
        let len = u32::from_le_bytes(len_buf) as usize;
        if len < 1 {
            return Err(FrameError::Short);
        }
        let body_len = len - 1;
        if body_len > MAX_BODY {
            return Err(FrameError::TooLarge(body_len));
        }
        let mut type_buf = [0u8; 1];
        reader
            .read_exact(&mut type_buf)
            .map_err(|error| FrameError::Io(error.to_string()))?;
        let kind = MessageType::from_u8(type_buf[0]).ok_or(FrameError::UnknownType(type_buf[0]))?;
        let mut body = vec![0u8; body_len];
        if body_len > 0 {
            reader
                .read_exact(&mut body)
                .map_err(|error| FrameError::Io(error.to_string()))?;
        }
        Ok(Self { kind, body })
    }

    /// Write this frame to `writer`: length + type + body.
    pub fn write_to(&self, writer: &mut impl Write) -> Result<(), FrameError> {
        writer
            .write_all(&self.encode())
            .map_err(|error| FrameError::Io(error.to_string()))
    }
}

/// Runtime socket path: `$XDG_RUNTIME_DIR/orator/<SOCKET_NAME>`,
/// falling back to the home share when the variable is absent. Returns
/// `None` when neither `XDG_RUNTIME_DIR` nor `HOME` is set — the caller
/// refuses to start rather than falling back to a shared directory.
#[must_use]
/// Runtime socket path: `$XDG_RUNTIME_DIR/orator/<SOCKET_NAME>`,
/// falling back to the home share when the variable is absent. Returns
/// `None` when neither `XDG_RUNTIME_DIR` nor `HOME` is set — the caller
/// refuses to start rather than falling back to a shared directory.
pub fn socket_path() -> Option<std::path::PathBuf> {
    let base = match std::env::var("XDG_RUNTIME_DIR") {
        Ok(dir) if !dir.is_empty() => Some(std::path::PathBuf::from(dir)),
        _ => match std::env::var("HOME") {
            Ok(home) if !home.is_empty() => {
                Some(std::path::PathBuf::from(home).join(".local/share"))
            }
            _ => None,
        },
    };
    base.map(|base| base.join("orator").join(SOCKET_NAME))
}

/// Peer-credential gate: a peer is served only when its uid equals the
/// daemon's own. The accept path feeds both uids from the platform
/// credential read; this pure gate keeps the policy unit-pinned.
#[must_use]
/// Peer-credential gate: a peer is served only when its uid equals the
/// daemon's own. The accept path feeds both uids from the platform
/// credential read; this pure gate keeps the policy unit-pinned.
pub const fn check_peer_uid(own_uid: u32, peer_uid: u32) -> bool {
    own_uid == peer_uid
}

#[cfg(test)]
#[path = "../tests/socket.rs"]
mod tests;