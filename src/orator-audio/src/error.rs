//! Audio error taxonomy. Device absence is ordinary (headless CI); only
//! disk-full on the archive path is fatal and loud.

use thiserror::Error;

/// Failures from capture, playout, and the archive tap.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum AudioError {
    /// No usable input or output device exists.
    #[error("no audio device: {0}")]
    NoDevice(String),
    /// The backend rejected a stream request.
    #[error("audio stream failed: {0}")]
    Stream(String),
    /// The archive tap cannot keep its durability promise.
    #[error("archive failed: {0}")]
    Archive(String),
}

#[cfg(test)]
#[path = "../tests/error.rs"]
mod tests;
