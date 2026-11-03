//! Shared audio and transcript vocabulary.
//!
//! `Frame` is the unit of audio motion: a cheap shared handle to pooled
//! samples plus the channel, rate, and media timestamp every downstream
//! stage needs. Frames are shared by reference between the archive tap,
//! echo control, and meters; a stage copies PCM only when it transforms it.

use std::num::NonZeroU32;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Which input a frame (or word) belongs to. Mic and system audio stay
/// separate channels end to end so attribution never requires inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Channel {
    /// The user's microphone, after echo control.
    Mic,
    /// System/playback capture (the remote side).
    Sys,
    /// Synthesized assistant reply, for echo reference and markers.
    Reply,
}

/// Media timestamp: a sample count on the capture clock.
///
/// Mic and system channels carry a measured offset so words from both can be
/// merged by media time. The mapping to wall-clock time is applied by the
/// capture backend; this type only orders and measures audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MediaTime {
    /// Samples since capture start on the owning clock.
    pub samples: u64,
    /// Clock rate in Hz. Always nonzero.
    pub rate: NonZeroU32,
}

impl MediaTime {
    /// A zero timestamp at the given rate.
    #[must_use]
    pub const fn zero(rate: NonZeroU32) -> Self {
        Self { samples: 0, rate }
    }

    /// Timestamp in seconds as a float, for display and export only.
    #[must_use]
    pub fn as_secs_f64(&self) -> f64 {
        self.samples as f64 / f64::from(self.rate.get())
    }

    /// Advance by `samples`, saturating instead of wrapping.
    #[must_use]
    pub fn add_samples(self, samples: u64) -> Self {
        Self {
            samples: self.samples.saturating_add(samples),
            rate: self.rate,
        }
    }
}

/// One analysis frame: 16 kHz mono f32 in 20 ms blocks (320 samples) by
/// convention; the type itself carries the rate so resampled frames stay
/// describable.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// Pooled sample storage, shared by handle. Clone shares; never copies.
    pub samples: Arc<Vec<f32>>,
    /// Which input this frame came from.
    pub channel: Channel,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Capture-clock timestamp of the first sample.
    pub stamp: MediaTime,
}

impl Frame {
    /// Wrap `samples` without copying. A zero `sample_rate` can never
    /// describe audio, so it returns [`FrameError::InvalidSampleRate`]
    /// instead of panicking.
    pub fn new(
        channel: Channel,
        samples: Vec<f32>,
        sample_rate: u32,
    ) -> Result<Self, crate::socket::FrameError> {
        let rate = NonZeroU32::new(sample_rate).ok_or(
            crate::socket::FrameError::InvalidSampleRate(sample_rate),
        )?;
        Ok(Self {
            samples: Arc::new(samples),
            channel,
            sample_rate,
            stamp: MediaTime::zero(rate),
        })
    }

    /// Number of samples in this frame.
    #[must_use]
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Whether the frame carries no samples.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

/// One committed transcript word with timing and confidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    /// Surface text, already normalized for display.
    pub text: String,
    /// Word start on the capture clock.
    pub start: MediaTime,
    /// Word end on the capture clock.
    pub end: MediaTime,
    /// Backend confidence in `[0.0, 1.0]`. A ranking hint, never a
    /// correctness verdict.
    pub confidence: f32,
    /// Which input produced the word.
    pub channel: Channel,
}

/// A streaming STT update: either a newly committed word or a revision of
/// the provisional tail. Committed words are never revised.
#[derive(Debug, Clone, PartialEq)]
pub struct WordUpdate {
    /// The word this update carries.
    pub word: Word,
    /// False once committed. Only the tail stays provisional.
    pub provisional: bool,
}

/// Word-level timing inside a synthesized chunk. The playout task turns
/// these into DAC `SpeechMark`s when samples reach the converter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordMark {
    /// Spoken surface text.
    pub text: String,
    /// Start offset in samples relative to the chunk.
    pub start_samples: u64,
    /// End offset in samples relative to the chunk.
    pub end_samples: u64,
}

/// One synthesized audio chunk with its word timings.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioChunk {
    /// Synthesized samples, shared by handle.
    pub samples: Arc<Vec<f32>>,
    /// Output sample rate in Hz.
    pub sample_rate: u32,
    /// Word timings aligned to `samples`.
    pub marks: Vec<WordMark>,
}

/// One model vocabulary id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Token(pub u32);

/// Output of one LLM generation step over the endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepOut {
    /// Newly generated text since the previous step.
    pub delta: String,
    /// Whether the turn is complete.
    pub finished: bool,
}

/// Sampler settings for one generation. Kept structural; tuning lives in
/// configuration and profiles, not in call sites.
#[derive(Debug, Clone, PartialEq)]
pub struct Sampler {
    /// Sampling temperature. Lower for tools, moderate for chat.
    pub temperature: f32,
    /// Nucleus sampling cutoff.
    pub top_p: f32,
}

impl Default for Sampler {
    fn default() -> Self {
        Self {
            temperature: 0.7,
            top_p: 0.9,
        }
    }
}

/// Reply-generation epoch. Every event and audio chunk carries one; late
/// arrivals with an old epoch are discarded after barge-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Epoch(pub u64);

/// One native speech-to-speech step result.
#[derive(Debug, Clone, PartialEq)]
pub struct DuplexOut {
    /// Reply audio produced for this input frame.
    pub audio: AudioChunk,
    /// Whether the model considers its turn complete.
    pub end_of_turn: bool,
}

/// A sequenced daemon event.
///
/// `seq` is monotonic per writer for durable events. Negative `seq` values
/// are live-only ephemeral events: never persisted, never replayed, never
/// advancing a reconnect cursor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Sequence number. Negative means live-only.
    pub seq: i64,
    /// Owning session id.
    pub session: String,
    /// Reply-generation epoch.
    pub epoch: u64,
    /// Wall-clock time in milliseconds since the Unix epoch.
    pub wall_ms: u64,
    /// What happened.
    pub kind: EventKind,
}

impl Event {
    /// Whether this event persists in the store and replays to late
    /// joiners. High-rate provisional events stay in memory.
    #[must_use]
    pub const fn durable(&self) -> bool {
        self.seq >= 0
            && matches!(
                self.kind,
                EventKind::SessionStarted
                    | EventKind::SessionStopped
                    | EventKind::StateChanged(_)
                    | EventKind::SttFinal(_)
                    | EventKind::ReplyFinal(_)
                    | EventKind::BargeIn
                    | EventKind::ToolCall { .. }
                    | EventKind::ToolResult { .. }
                    | EventKind::ApprovalRequested { .. }
                    | EventKind::ApprovalResolved { .. }
                    | EventKind::NoteUpdate(_)
                    | EventKind::DiarizationProgress(_)
                    | EventKind::MeetingExported(_)
                    | EventKind::DeviceLost(_)
                    | EventKind::DeviceRestored(_)
                    | EventKind::StageFailed(_)
                    | EventKind::Degraded(_)
            )
    }
}

/// Event payloads. Durability follows the event-model table: provisional
/// and high-rate kinds stay live-only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EventKind {
    /// A session opened.
    SessionStarted,
    /// A session closed.
    SessionStopped,
    /// Session state label changed.
    StateChanged(String),
    /// Voice activity started on a channel.
    VadStart,
    /// Voice activity ended on a channel.
    VadEnd,
    /// Provisional (revisable) transcript text. Live-only.
    SttPartial(String),
    /// Committed words with timing and confidence.
    SttFinal(Word),
    /// Generated token stream. Live-only.
    LlmDelta(String),
    /// Spoken reply text (DAC-confirmed words only).
    ReplyFinal(String),
    /// A word reached the converter. Live-only.
    SpeechMark(WordMark),
    /// The user interrupted the reply.
    BargeIn,
    /// A tool invocation started.
    ToolCall {
        /// Tool name.
        name: String,
        /// Arguments.
        args: serde_json::Value,
    },
    /// A tool invocation finished.
    ToolResult {
        /// Tool name.
        name: String,
        /// Wrapped (untrusted) output.
        output: String,
    },
    /// Human approval is required.
    ApprovalRequested {
        /// Short approval id.
        id: String,
        /// Human-readable summary.
        summary: String,
    },
    /// Human approval resolved.
    ApprovalResolved {
        /// Short approval id.
        id: String,
        /// Whether the call may proceed.
        approved: bool,
    },
    /// Live notes state changed.
    NoteUpdate(String),
    /// Diarization progress fraction in `[0.0, 1.0]`.
    DiarizationProgress(f32),
    /// A meeting folder was exported.
    MeetingExported(String),
    /// A capture device was lost.
    DeviceLost(String),
    /// A capture device returned.
    DeviceRestored(String),
    /// A pipeline stage failed over to its degrade path.
    StageFailed(String),
    /// The session is running degraded.
    Degraded(String),
    /// Per-turn latency breakdown. Live-only.
    Metrics(String),
}

/// Session kinds for the degrade table: what the session is for
/// decides which degraded shape a stage failure produces. Single
/// home; the turn driver re-exports this for compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    /// Speak → focused window.
    Dictation,
    /// Full duplex with tools.
    Assistant,
    /// Hears others, never speaks.
    ListenIn,
    /// Records and transcribes.
    Meeting,
}

#[cfg(test)]
#[path = "../tests/types.rs"]
mod tests;
