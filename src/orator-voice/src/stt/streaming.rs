//! Rolling-window streaming wrapper: commit by local agreement.
//!
//! whisper.cpp decodes whole blocks, not streams. The wrapper keeps the
//! utterance window, re-decodes on a cadence, and commits only words that
//! agree across consecutive passes. Committed words are never revised; the
//! tail stays provisional. Hallucination guards (no-speech ceiling,
//! repeat suppression, nonspeech stripping) apply before anything commits.
//!
//! The decode seam is a trait so every policy here is provable with scripted
//! lattices and no model. Real decodes run on the caller's thread: node
//! tasks invoke `push` from the inference pool under `Limiter::run`.

use std::sync::Arc;

use fluent_concurrency::stream::StreamAbort;
use fluent_wvr::{
    impl_component, Describable, FieldAccess, FieldError, WorkContext, WorkError, WorkOutput,
    WorkUnit,
};
use internment::ArcIntern;
use orator::{Channel, Frame, MediaTime, SttError, SttStream, Word, WordUpdate};

/// One word from a block decode, timed in milliseconds since utterance start.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedWord {
    /// Surface text.
    pub text: String,
    /// Start offset in ms.
    pub start_ms: u64,
    /// End offset in ms.
    pub end_ms: u64,
    /// Segment no-speech probability in `[0.0, 1.0]`.
    pub no_speech: f32,
}

/// Synchronous block decoder. Runs on the caller's thread; `abort` is
/// polled between passes so cancellation surfaces instead of truncating.
///
/// Timing contract: returned words are timed in milliseconds since the
/// UTTERANCE start, not the buffer start. `offset_ms` is the absolute
/// time of `samples[0]` (zero for a window starting at the utterance
/// origin); buffer-relative decoders add it, utterance-absolute
/// decoders ignore it. The stream evicts committed audio and passes
/// the eviction offset here, so decoders never track it themselves.
pub trait BlockDecoder: Send + Sync {
    /// Decode 16 kHz mono samples to timed words.
    fn decode(
        &self,
        samples: &[f32],
        offset_ms: u64,
        abort: &StreamAbort,
    ) -> Result<Vec<DecodedWord>, SttError>;
}

/// Streaming wrapper over any [`BlockDecoder`].
pub struct StreamingStt {
    decoder: Arc<dyn BlockDecoder>,
    channel: Channel,
    base: MediaTime,
    window: Vec<f32>,
    /// Previous decode in utterance-absolute ms (eviction-proof: the
    /// decoder may re-emit committed words or omit them, and both
    /// align after the committed-region filter).
    previous: Vec<DecodedWord>,
    committed: usize,
    /// Absolute end (ms) of the last committed word; everything at or
    /// below it is committed and eventually evicted from the window.
    committed_through_ms: u64,
    /// Samples dropped from the window front, total.
    evicted_samples: u64,
    frames_since_decode: usize,
    /// Re-decode cadence in 20 ms frames.
    pub decode_every_frames: usize,
    /// Window cap in samples (~25 s at 16 kHz). Eviction keeps live
    /// windows far below it; the cap stays a fail-closed backstop.
    pub window_cap_samples: usize,
    /// Tail newer than this (ms) stays provisional.
    pub provisional_ms: u64,
    /// Start-time agreement tolerance (ms) across passes.
    pub agree_ms: u64,
    /// Words at or above this no-speech level never commit.
    pub no_speech_max: f32,
}

impl StreamingStt {
    /// Wrapper with the documented policy defaults.
    pub fn new(decoder: Arc<dyn BlockDecoder>, channel: Channel, base: MediaTime) -> Self {
        Self {
            decoder,
            channel,
            base,
            window: Vec::new(),
            previous: Vec::new(),
            committed: 0,
            committed_through_ms: 0,
            evicted_samples: 0,
            frames_since_decode: 0,
            decode_every_frames: 15,
            window_cap_samples: 400_000,
            provisional_ms: 1500,
            agree_ms: 120,
            no_speech_max: 0.6,
        }
    }

    /// Move the utterance origin (new turn). Drops window and agreement.
    pub fn retarget(&mut self, base: MediaTime) {
        self.base = base;
        self.window.clear();
        self.previous.clear();
        self.committed = 0;
        self.committed_through_ms = 0;
        self.evicted_samples = 0;
        self.frames_since_decode = 0;
    }

    /// Words committed so far.
    #[must_use]
    pub fn committed(&self) -> usize {
        self.committed
    }

    /// Samples currently buffered (the uncommitted tail after eviction).
    #[must_use]
    pub fn window_samples(&self) -> usize {
        self.window.len()
    }

    /// Samples dropped from the window front by commit eviction, total.
    #[must_use]
    pub fn evicted_samples(&self) -> u64 {
        self.evicted_samples
    }

    /// Push one frame, abort-aware. A fired `abort` returns `Aborted`
    /// before any decode runs; otherwise behaves exactly like `push`.
    pub fn push_checked(
        &mut self,
        frame: &Frame,
        abort: &StreamAbort,
    ) -> Result<Vec<WordUpdate>, SttError> {
        if abort.is_cancelled() {
            return Err(SttError::Aborted);
        }
        self.push_inner(frame, abort)
    }

    fn push_inner(
        &mut self,
        frame: &Frame,
        abort: &StreamAbort,
    ) -> Result<Vec<WordUpdate>, SttError> {
        self.window.extend_from_slice(&frame.samples);
        if self.window.len() > self.window_cap_samples {
            let overflow = self.window.len() - self.window_cap_samples;
            self.window.drain(..overflow);
            self.base = self.base.add_samples(overflow as u64);
            self.evicted_samples += overflow as u64;
            // Agreement restarts on the tail, but the commit boundary
            // holds: already-committed words can never recommit.
            self.previous.clear();
        }
        self.frames_since_decode += 1;
        if self.frames_since_decode < self.decode_every_frames {
            return Ok(Vec::new());
        }
        self.frames_since_decode = 0;
        let offset_ms = self.evicted_samples * 1000 / 16_000;
        let decoded = self.decoder.decode(&self.window, offset_ms, abort)?;
        Ok(self.reconcile(decoded))
    }

    /// Commit the agreed prefix, snapshot the provisional tail, then
    /// evict committed audio so the window holds only the tail.
    fn reconcile(&mut self, decoded: Vec<DecodedWord>) -> Vec<WordUpdate> {
        let guarded = apply_guards(decoded, self.no_speech_max);
        // Utterance-absolute coordinates (the decoder contract): stable
        // across evictions. The committed region filters out, whether
        // the decoder re-emits it (full-lattice scripts) or omits it
        // (tail-only decodes after eviction).
        let absolute: Vec<DecodedWord> = guarded
            .into_iter()
            .filter(|word| word.end_ms > self.committed_through_ms)
            .collect();
        let prev_suffix: Vec<DecodedWord> = self
            .previous
            .iter()
            .filter(|word| word.end_ms > self.committed_through_ms)
            .cloned()
            .collect();
        let agreed = agreed_prefix(&prev_suffix, &absolute, self.agree_ms);
        let evicted_ms = self.evicted_samples * 1000 / 16_000;
        let window_end_ms = evicted_ms + self.window.len() as u64 * 1000 / 16_000;
        let mut updates = Vec::new();
        let mut commit_through = self.committed;
        let mut boundary = self.committed_through_ms;
        for (suffix_index, word) in absolute.iter().enumerate() {
            if suffix_index < agreed && word.end_ms + self.provisional_ms <= window_end_ms {
                updates.push(WordUpdate {
                    word: self.to_window_word(
                        word.start_ms.saturating_sub(evicted_ms),
                        word.end_ms.saturating_sub(evicted_ms),
                        &word.text,
                        word.no_speech,
                    ),
                    provisional: false,
                });
                commit_through += 1;
                boundary = boundary.max(word.end_ms);
            }
        }
        self.committed = commit_through;
        self.committed_through_ms = boundary;
        for word in absolute.iter().skip(agreed) {
            updates.push(WordUpdate {
                word: self.to_window_word(
                    word.start_ms.saturating_sub(evicted_ms),
                    word.end_ms.saturating_sub(evicted_ms),
                    &word.text,
                    word.no_speech,
                ),
                provisional: true,
            });
        }
        self.previous = absolute;
        self.evict_committed();
        updates
    }

    /// Drop window audio through the commit boundary. Committed words
    /// are already emitted; the decoder re-finds the tail on its own.
    fn evict_committed(&mut self) {
        let drop = self
            .committed_through_ms
            .saturating_mul(16)
            .saturating_sub(self.evicted_samples);
        let drop = drop.min(self.window.len() as u64) as usize;
        if drop > 0 {
            self.window.drain(..drop);
            self.base = self.base.add_samples(drop as u64);
            self.evicted_samples += drop as u64;
        }
    }

    fn to_window_word(&self, start_ms: u64, end_ms: u64, text: &str, no_speech: f32) -> Word {
        let start = self.base.add_samples(start_ms.saturating_mul(16));
        let end = self.base.add_samples(end_ms.saturating_mul(16));
        Word {
            text: text.to_string(),
            start,
            end,
            confidence: (1.0 - no_speech).clamp(0.0, 1.0),
            channel: self.channel,
        }
    }
}

/// Longest prefix of `next` matching `prev` by text with start times inside
/// `agree_ms`. Stability across passes is what makes a word committable.
fn agreed_prefix(prev: &[DecodedWord], next: &[DecodedWord], agree_ms: u64) -> usize {
    let mut agreed = 0;
    for pair in prev.iter().zip(next.iter()) {
        let (old, new) = pair;
        if old.text != new.text {
            break;
        }
        let drift = old.start_ms.abs_diff(new.start_ms);
        if drift > agree_ms {
            break;
        }
        agreed += 1;
    }
    agreed
}

/// Hallucination guards, in order: no-speech ceiling, bracketed-nonspeech
/// strip, word-run suppression. Everything rejected here never commits.
fn apply_guards(words: Vec<DecodedWord>, no_speech_max: f32) -> Vec<DecodedWord> {
    const NONSPEECH: &[&str] = &[
        "blank_audio",
        "blank audio",
        "silence",
        "silent",
        "no speech",
        "inaudible",
        "music",
    ];
    let mut kept = Vec::with_capacity(words.len());
    let mut run_text = String::new();
    let mut run_len = 0usize;
    for word in words {
        if word.no_speech >= no_speech_max {
            continue;
        }
        let lowered = word.text.to_lowercase();
        let bracketed = (lowered.contains('[') || lowered.contains('('))
            && NONSPEECH.iter().any(|key| lowered.contains(key));
        if bracketed {
            continue;
        }
        if word.text == run_text {
            run_len += 1;
            if run_len > 3 {
                continue;
            }
        } else {
            run_text.clone_from(&word.text);
            run_len = 1;
        }
        kept.push(word);
    }
    kept
}

impl SttStream for StreamingStt {
    fn push(&mut self, frame: &Frame) -> Result<Vec<WordUpdate>, SttError> {
        let live = StreamAbort::new();
        self.push_inner(frame, &live)
    }

    fn finalize(&mut self) -> Result<Vec<WordUpdate>, SttError> {
        let live = StreamAbort::new();
        let offset_ms = self.evicted_samples * 1000 / 16_000;
        let decoded = self.decoder.decode(&self.window, offset_ms, &live)?;
        let guarded = apply_guards(decoded, self.no_speech_max);
        // The tail decode may re-emit the committed region
        // (full-lattice scripts) or start past it (tail-only
        // decodes): either way the boundary filters exactly once.
        // Decoder output is utterance-absolute; emission subtracts
        // the eviction offset back to window-relative.
        let mut updates = Vec::new();
        let mut boundary = self.committed_through_ms;
        for word in &guarded {
            if word.end_ms <= self.committed_through_ms {
                continue;
            }
            updates.push(WordUpdate {
                word: self.to_window_word(
                    word.start_ms.saturating_sub(offset_ms),
                    word.end_ms.saturating_sub(offset_ms),
                    &word.text,
                    word.no_speech,
                ),
                provisional: false,
            });
            boundary = boundary.max(word.end_ms);
        }
        self.committed += updates.len();
        self.committed_through_ms = boundary;
        self.previous = guarded;
        Ok(updates)
    }

    fn reset(&mut self) {
        self.window.clear();
        self.previous.clear();
        self.committed = 0;
        self.committed_through_ms = 0;
        self.evicted_samples = 0;
        self.frames_since_decode = 0;
    }
}

impl WorkUnit for StreamingStt {
    fn name(&self) -> &str {
        "stt.stream"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        Ok(WorkOutput::ok(format!("committed={}", self.committed)))
    }
}

impl FieldAccess for StreamingStt {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        // M15-S4 keep-decision: the `no_speech_max` [0.0, 1.0] band is
        // enforced here but deliberately absent from the schema below.
        // A derive needs `min`/`max` attrs to enforce it (which would
        // change `describe()`) or drops the enforcement (fail-open).
        // Neither is foldable into a refactor, so the hand impl stays —
        // the band and schema are pinned by
        // `streaming_field_surface_pins_schema_and_table`.
        match name {
            "provisional_ms" => {
                self.provisional_ms = value.parse().map_err(|_| {
                    FieldError::Parse("provisional_ms must be a number".to_string())
                })?;
                Ok(())
            }
            "agree_ms" => {
                self.agree_ms = value
                    .parse()
                    .map_err(|_| FieldError::Parse("agree_ms must be a number".to_string()))?;
                Ok(())
            }
            "no_speech_max" => {
                let parsed: f32 = value
                    .parse()
                    .map_err(|_| FieldError::Parse("no_speech_max must be a number".to_string()))?;
                if !(0.0..=1.0).contains(&parsed) {
                    return Err(FieldError::Constraint(
                        "no_speech_max must be within [0.0, 1.0]".to_string(),
                    ));
                }
                self.no_speech_max = parsed;
                Ok(())
            }
            _ => Err(FieldError::NotFound(name.into())),
        }
    }
    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        match name {
            "provisional_ms" => Ok(self.provisional_ms.to_string()),
            "agree_ms" => Ok(self.agree_ms.to_string()),
            "no_speech_max" => Ok(self.no_speech_max.to_string()),
            _ => Err(FieldError::NotFound(name.into())),
        }
    }
    fn field_names(&self) -> &'static [&'static str] {
        &["provisional_ms", "agree_ms", "no_speech_max"]
    }
}

impl Describable for StreamingStt {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "provisional_ms": { "type": "integer", "description": "Tail newer than this stays provisional" },
                "agree_ms": { "type": "integer", "description": "Cross-pass start-time tolerance" },
                "no_speech_max": { "type": "number", "description": "No-speech ceiling for commits" }
            },
            "required": ["provisional_ms", "agree_ms", "no_speech_max"]
        })
    }
}

impl_component!(StreamingStt);

#[cfg(test)]
#[path = "../../tests/stt_streaming.rs"]
mod tests;
