//! Speaking pipeline: normalized text in, timestamped audio chunks out.
//!
//! Each `push_text` normalizes, splits, and stages chunks tagged with the
//! epoch's generation. `step` synthesizes ahead until the prebuffer lead
//! holds, then returns the head chunk with its word marks. A newer epoch
//! bumps the generation and clears the queue — stale chunks can never
//! play after barge-in, and text for an old epoch is dropped, never
//! spoken.

use std::collections::VecDeque;
use std::sync::Arc;

use fluent_wvr::{
    Describable, FieldAccess, FieldError, WorkContext, WorkError, WorkOutput, WorkUnit,
    impl_component,
};
use internment::ArcIntern;
use orator_core::{AudioChunk, Epoch, TtsError, TtsStream};

use crate::kokoro::{uniform_marks, KOKORO_SAMPLE_RATE};
use crate::normalize::normalize_for_speech;
use crate::splitter::{split_all, tail_gap_ms, Chunk};

/// Seconds of audio buffered before the first chunk releases.
pub const PREBUFFER_SECS: f32 = 0.6;

/// Blocking synth seam. Runs on the caller's thread; the pipeline
/// serializes calls, so fakes stay deterministic.
pub trait SynthSeam: Send + Sync {
    /// Synthesize normalized `text` to samples at `rate` Hz.
    fn synth(&self, text: &str) -> Result<SynthOut, TtsError>;
}

/// One synth result.
#[derive(Debug, Clone, PartialEq)]
pub struct SynthOut {
    /// Mono f32 samples.
    pub samples: Vec<f32>,
    /// Sample rate in Hz.
    pub rate: u32,
}

struct StagedChunk {
    text: String,
    hard: bool,
    generation: u64,
    audio: Option<Vec<f32>>,
}

/// The speaking pipeline over any [`SynthSeam`].
pub struct TtsPipeline {
    backend: Arc<dyn SynthSeam>,
    epoch: Epoch,
    generation: u64,
    queue: VecDeque<StagedChunk>,
    synth_calls: u64,
}

impl TtsPipeline {
    /// Pipeline over `backend`, starting at `epoch`.
    pub fn new(backend: Arc<dyn SynthSeam>, epoch: Epoch) -> Self {
        Self {
            backend,
            epoch,
            generation: 0,
            queue: VecDeque::new(),
            synth_calls: 0,
        }
    }

    /// Synth calls so far. First-audio accounting, not a confidence.
    #[must_use]
    pub fn synth_calls(&self) -> u64 {
        self.synth_calls
    }

    /// Chunks waiting (staged or synthesized).
    #[must_use]
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Current epoch.
    #[must_use]
    pub const fn epoch(&self) -> Epoch {
        self.epoch
    }

    /// Synthesize one staged chunk through the seam.
    fn synth_one(&mut self, index: usize) -> Result<(), TtsError> {
        let text = self.queue[index].text.clone();
        let out = self.backend.synth(&text)?;
        self.synth_calls += 1;
        self.queue[index].audio = Some(out.samples);
        Ok(())
    }

    /// Buffered lead in seconds across synthesized head chunks.
    fn lead_secs(&self) -> f32 {
        let mut lead = 0f32;
        for staged in &self.queue {
            match &staged.audio {
                Some(audio) => lead += audio.len() as f32 / KOKORO_SAMPLE_RATE as f32,
                None => break,
            }
        }
        lead
    }
}

impl TtsStream for TtsPipeline {
    fn push_text(&mut self, text: &str, epoch: Epoch) {
        if epoch.0 < self.epoch.0 {
            return;
        }
        if epoch.0 > self.epoch.0 {
            self.epoch = epoch;
            self.generation += 1;
            self.queue.clear();
        }
        let generation = self.generation;
        let normalized = normalize_for_speech(text);
        if normalized.is_empty() {
            return;
        }
        // Chunk count drives prebuffer math downstream; keep every piece.
        for chunk in split_all(&normalized) {
            let Chunk { text, hard } = chunk;
            self.queue.push_back(StagedChunk {
                text,
                hard,
                generation,
                audio: None,
            });
        }
    }

    fn step(&mut self) -> Result<Option<AudioChunk>, TtsError> {
        // Drop anything a newer epoch orphaned (defensive: `push_text`
        // already clears on bump, but a bump-then-partial-state must never
        // leak old audio).
        while self.queue.front().is_some_and(|head| head.generation != self.generation) {
            self.queue.pop_front();
        }
        // Synth ahead until the prebuffer lead holds or the queue drains.
        loop {
            let next = self
                .queue
                .iter()
                .position(|staged| staged.audio.is_none());
            let Some(index) = next else { break };
            if self.lead_secs() >= PREBUFFER_SECS {
                break;
            }
            self.synth_one(index)?;
        }
        let Some(head) = self.queue.pop_front() else {
            return Ok(None);
        };
        let Some(mut samples) = head.audio else {
            // Prebuffer already held with nothing synthesized: the queue
            // holds only over-long silences — requeue and report drained.
            return Ok(None);
        };
        // Spacing rides with the audio: a breath after sentences, a flow
        // after soft breaks — but never after the last known chunk, which
        // may yet grow more text.
        if !self.queue.is_empty() {
            let gap = tail_gap_ms(&Chunk { text: head.text.clone(), hard: head.hard }, false);
            samples.resize(
                samples.len() + gap as usize * KOKORO_SAMPLE_RATE as usize / 1000,
                0.0,
            );
        }
        let marks = uniform_marks(&head.text, samples.len(), KOKORO_SAMPLE_RATE);
        Ok(Some(AudioChunk {
            samples: Arc::new(samples),
            sample_rate: KOKORO_SAMPLE_RATE,
            marks,
        }))
    }
}

impl WorkUnit for TtsPipeline {
    fn name(&self) -> &str {
        "tts.pipeline"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        Ok(WorkOutput::ok(format!(
            "epoch={} queued={}",
            self.epoch.0,
            self.queue.len()
        )))
    }
}

impl FieldAccess for TtsPipeline {
    fn set_field(&mut self, _name: &str, _value: &str) -> Result<(), FieldError> {
        Err(FieldError::NotFound("pipeline tunables live on the backend".into()))
    }
    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        match name {
            "epoch" => Ok(self.epoch.0.to_string()),
            "queued" => Ok(self.queue.len().to_string()),
            _ => Err(FieldError::NotFound(name.into())),
        }
    }
    fn field_names(&self) -> &'static [&'static str] {
        &["epoch", "queued"]
    }
}

impl Describable for TtsPipeline {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "epoch": { "type": "integer", "description": "Reply-generation epoch" },
                "queued": { "type": "integer", "description": "Chunks waiting" }
            },
            "required": ["epoch", "queued"]
        })
    }
}

impl_component!(TtsPipeline);

#[cfg(test)]
#[path = "../tests/pipeline.rs"]
mod tests;

/// Gap helper re-export for playout drivers.
pub use crate::splitter::tail_gap_ms as chunk_tail_gap;
