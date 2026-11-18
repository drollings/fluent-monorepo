//! Capture sources: cpal bring-up, synthetic fallback, and the
//! supervised per-device driver shape PipeWire promotes later.
//!
//! Each device gets a supervised driver (plain task plus
//! `retry_async`/`capped_backoff_ms`): an unplugged headset pauses the
//! session and resumes when a device returns, without tearing down
//! the scope. The native PipeWire binding (per-app capture, monitor
//! source, node hot-plug) is probed, not linked: `pipewire_source`
//! reports the actionable absence until the binding lands.

use std::time::Duration;

use thiserror::Error;

/// Capture source kinds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceKind {
    /// Microphone through the bring-up backend.
    Mic,
    /// System-audio monitor through the bring-up backend.
    SystemMonitor,
    /// Per-application PipeWire node (binding lands later).
    AppNode(String),
    /// Synthetic sine source (tests, headless machines).
    Synthetic,
}

/// Hot-plug events from the backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotPlug {
    /// A node appeared.
    NodeAdded(String),
    /// A node disappeared.
    NodeRemoved(String),
}

/// Driver failures.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum DriverError {
    /// No device for this source right now (hot-plug will heal it).
    #[error("no device for {0}")]
    NoDevice(String),
    /// The backend is not linked in this build.
    #[error("backend unavailable: {0}")]
    Unavailable(String),
}

/// Name the device for `source`, if one is visible now.
#[must_use]
pub fn probe_device(source: &SourceKind) -> Option<String> {
    match source {
        SourceKind::Mic | SourceKind::SystemMonitor => crate::backend::list_input_devices()
            .first()
            .map(|info| info.name.clone()),
        SourceKind::AppNode(_) => pipewire_source(),
        SourceKind::Synthetic => Some("synthetic".to_string()),
    }
}

/// PipeWire per-app source probe. Returns the monitor name when the
/// native binding is linked; today it reports the actionable absence
/// (the binding is a named later step, never a silent fallback).
#[must_use]
pub fn pipewire_source() -> Option<String> {
    None
}

/// Why PipeWire capture is not live yet (for `doctor` output).
#[must_use]
pub const fn pipewire_note() -> &'static str {
    "PipeWire per-app capture: native binding not linked; cpal bring-up serves capture"
}

/// Run `work` under the supervised-driver policy: up to 5 attempts
/// with jittered backoff, then the device-level error surfaces and the
/// session pauses (never a silent restart). Unavailable backends fail
/// fast on the first attempt.
pub async fn supervise_driver<T, F, Fut>(work: F) -> Result<T, DriverError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, DriverError>>,
{
    common_core::retry::retry_async(
        5,
        200,
        25,
        |error: &DriverError| !matches!(error, DriverError::Unavailable(_)),
        work,
    )
    .await
}

/// Backoff ceiling for driver retries (matches the capped helper).
#[must_use]
pub const fn driver_backoff_cap_ms() -> u32 {
    5_000
}

/// Idle longer than this with no device pauses the session.
pub const DEVICE_LOST_AFTER: Duration = Duration::from_secs(5);

#[cfg(test)]
#[path = "../tests/source.rs"]
mod tests;
