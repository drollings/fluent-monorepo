//! Engine traits: one bounded step at a time.
//!
//! Every engine call does one bounded unit of work — one audio chunk, one
//! endpoint request step, one TTS chunk — so cancel, priority, and timeout
//! stay meaningful between steps. Engines are `Component`s: their tunables
//! are runtime-editable through `FieldAccess`, and their lifecycle reports
//! through `WorkUnit::execute` as a health verdict (loaded, degraded,
//! unavailable). Bulk data flows through the step methods, never through
//! `execute`.

use fluent_concurrency::stream::StreamAbort;
use fluent_wvr::Component;

use crate::error::{DuplexError, LlmError, SttError, TtsError};
use crate::types::{AudioChunk, DuplexOut, Epoch, Frame, Sampler, StepOut, Token, WordUpdate};

/// Streaming speech-to-text: one 20 ms frame in, committed words out.
pub trait SttStream: Component {
    /// Push one frame. Returns newly committed words first, then the
    /// provisional tail. Committed words are never revised later.
    fn push(&mut self, frame: &Frame) -> Result<Vec<WordUpdate>, SttError>;

    /// Commit everything decodable at the endpoint of an utterance.
    fn finalize(&mut self) -> Result<Vec<WordUpdate>, SttError>;

    /// Drop utterance state. Weights and configuration survive.
    fn reset(&mut self);
}

/// Language-model session over the supervised endpoint.
pub trait LlmSession: Component {
    /// Send the new-turn prefix, chunked by the endpoint. Abort via
    /// `abort`; on abort the caller re-requests the confirmed prefix.
    fn prefill(&mut self, toks: &[Token], abort: &StreamAbort) -> Result<(), LlmError>;

    /// Advance the open request by one step.
    fn step(&mut self, sampler: &mut Sampler) -> Result<StepOut, LlmError>;
}

/// Streaming text-to-speech: text in, timestamped audio chunks out.
pub trait TtsStream: Component {
    /// Queue text for `epoch`. Text for a stale epoch is dropped, never
    /// spoken.
    fn push_text(&mut self, text: &str, epoch: Epoch);

    /// Synthesize the next chunk, or `None` when the queue is drained.
    /// Each chunk carries word timings for DAC speech marks.
    fn step(&mut self) -> Result<Option<AudioChunk>, TtsError>;
}

/// Native speech-to-speech backend (Moshi-class). Collapses the cascade
/// into one graph node when assembled.
pub trait DuplexEngine: Component {
    /// Step the full-duplex model on one microphone frame.
    fn step(&mut self, mic: &Frame) -> Result<DuplexOut, DuplexError>;
}

#[cfg(test)]
#[path = "../tests/traits.rs"]
mod tests;
