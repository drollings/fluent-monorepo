#![forbid(unsafe_code)]

//! orator-daemon: the `oratord` composition root.
//!
//! Owns the process lifetime as a tree of supervision scopes (infrastructure,
//! audio, inference, sessions), serves control traffic, and shuts everything
//! down within a bound on exit signals.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fluent_concurrency::scope::Scope;
use orator_audio::{spawn_archive_feed, ArchiveItem, ArchiveWriter, Manifest};
use orator::protocol::METHOD_NAMES;
use orator::Channel;
use thiserror::Error;

/// Session presets over pipeline stages.
pub mod preset;

/// Admission policy: bounded concurrency, fast refusal, ejection.
pub mod admission;

/// Live session registry.
pub mod session;

/// Credentialed socket serving.
pub mod socket;

/// WebSocket listener over the same dispatch core.
pub mod ws;

/// Daemon-owned voice server.
pub mod voice;

/// Methods this daemon serves. Mirrors the wire contract in `orator`;
/// the dispatch test enforces set equality so the two can never drift.
pub const DISPATCH_METHODS: [&str; 14] = METHOD_NAMES;

/// Daemon shutdown signal: set by the `StopDaemon` call, observed by the
/// accept loop. Test seam resets through `clear_shutdown_for_tests`.
static SHUTDOWN_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Signal the serving loop to stop accepting after the current call.
pub fn request_shutdown() {
    SHUTDOWN_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Whether a `StopDaemon` call has been served.
#[must_use]
/// Whether a `StopDaemon` call has been served.
pub fn shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Reset the shutdown signal. Test-only seam.
#[cfg(test)]
/// Reset the shutdown signal. Test-only seam.
pub fn clear_shutdown_for_tests() {
    SHUTDOWN_REQUESTED.store(false, std::sync::atomic::Ordering::SeqCst);
}

/// Dispatch failures. Unknown methods are rejected, never defaulted.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
/// Dispatch failures. Unknown methods are rejected, never defaulted.
pub enum DispatchError {
    /// The method is not part of the wire contract.
    #[error("unknown method: {0}")]
    Unknown(String),
    /// The method is in the contract but has no serving implementation yet.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

/// Route one control call. `Reload` stays unimplemented by design
/// (live reload is never silent); every other contract method has a
/// serving implementation behind the socket layer. `VoiceEndpoint` is
/// stateful (it may boot the voice server) and is answered by the
/// serving loop, never here — but it belongs to the contract, so the
/// match accepts it.
pub fn dispatch(method: &str) -> Result<(), DispatchError> {
    match method {
        "VoiceEndpoint" | "StopDaemon" | "StartSession" | "StopSession" | "Interrupt" | "Say"
        | "Ask" | "Transcribe" | "ToggleDictation" | "ListSessions" | "GetState" | "Approve"
        | "ExportMeeting" => Ok(()),
        "Reload" => Err(DispatchError::NotImplemented("Reload")),
        _ => Err(DispatchError::Unknown(method.to_string())),
    }
}

/// Assert the served set equals the wire contract. Called by tests; kept
/// as a function so both bins and tests share one check.
pub fn contract_matches() -> bool {
    let served: HashSet<&str> = DISPATCH_METHODS.into_iter().collect();
    let wired: HashSet<&str> = METHOD_NAMES.into_iter().collect();
    served == wired
}

/// The daemon's owned task tree. Children close before parents; dropping
/// without closing panics by construction (the inner `Scope`s enforce it).
pub struct RootScope {
    infra: Scope,
    audio: Scope,
    inference: Scope,
    sessions: Scope,
}

impl RootScope {
    /// Build the four owned scopes. No tasks spawn here; session and
    /// driver code registers into the handles it is given.
    #[must_use]
    pub fn build() -> Self {
        Self {
            infra: Scope::new(),
            audio: Scope::new(),
            inference: Scope::new(),
            sessions: Scope::new(),
        }
    }

    /// Bounded graceful shutdown: sessions drain first, then inference,
    /// audio, and infrastructure (which flushes the archive and event log).
    pub async fn shutdown(&mut self, timeout: Duration) {
        self.sessions.close_graceful(timeout).await;
        self.inference.close_graceful(timeout).await;
        self.audio.close_graceful(timeout).await;
        self.infra.close_graceful(timeout).await;
    }

    /// Whether every scope is still empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.infra.is_empty()
            && self.audio.is_empty()
            && self.inference.is_empty()
            && self.sessions.is_empty()
    }
}

impl Default for RootScope {
    fn default() -> Self {
        Self::build()
    }
}

/// Loopback failures.
#[derive(Error, Debug)]
/// Loopback failures.
pub enum LoopbackError {
    /// The archive tap failed.
    #[error("loopback archive failed: {0}")]
    Archive(#[from] orator_audio::AudioError),
    /// The writer stayed shared after the feed drained (a leaked handle).
    #[error("loopback writer still shared after drain")]
    WriterShared,
    /// The feed worker did not finish draining.
    #[error("loopback worker failed: {0}")]
    Worker(String),
}

/// Capture→archive loopback: a synthetic 440 Hz source feeds the durable
/// archive worker for `seconds`, then everything drains and the manifest is
/// returned. Real devices replace the source without touching the path.
pub async fn run_loopback(dir: &Path, seconds: u64) -> Result<Manifest, LoopbackError> {
    let mut root = RootScope::build();
    let writer = Arc::new(Mutex::new(ArchiveWriter::open(dir)?));
    let worker = spawn_archive_feed(fluent_concurrency::tokio_runtime(), Arc::clone(&writer));
    let feed = worker.sender();
    let driver = worker.start();
    // The source task owns the feed sender; its completion signal tells us
    // every frame was accepted, so draining afterwards cannot lose audio.
    let (done_tx, mut done_rx) = tokio::sync::mpsc::channel::<()>(1);
    root.audio.spawn(async move {
        synthetic_source(feed, seconds).await;
        let _ = done_tx.send(()).await;
    });
    let _ = done_rx.recv().await;
    worker.drain();
    driver
        .await
        .map_err(|error| LoopbackError::Worker(error.to_string()))?;
    root.shutdown(Duration::from_secs(5)).await;
    drop(worker);
    let writer = Arc::try_unwrap(writer).map_err(|_| LoopbackError::WriterShared)?;
    let writer = writer
        .into_inner()
        .map_err(|_| LoopbackError::WriterShared)?;
    Ok(writer.close()?)
}

/// Synthetic sine source: 20 ms frames at the archive rate, in real time.
/// Stands in for the cpal drain task where no hardware exists.
async fn synthetic_source(feed: tokio::sync::mpsc::Sender<ArchiveItem>, seconds: u64) {
    let frames = seconds.saturating_mul(50);
    for index in 0..frames {
        let samples: Vec<f32> = (0..orator_audio::archive::FRAME_SAMPLES)
            .map(|sample| {
                let angle =
                    (index as usize * orator_audio::archive::FRAME_SAMPLES + sample) as f32 * 0.05;
                angle.sin() * 0.3
            })
            .collect();
        let item = ArchiveItem::Pcm {
            channel: Channel::Mic,
            samples,
        };
        if feed.send(item).await.is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(test)]
#[path = "../tests/dispatch.rs"]
mod dispatch_tests;

#[cfg(test)]
#[path = "../tests/loopback.rs"]
mod loopback_tests;

#[cfg(test)]
#[path = "../tests/scope.rs"]
mod scope_tests;

#[cfg(test)]
#[path = "../tests/session.rs"]
mod session_tests;