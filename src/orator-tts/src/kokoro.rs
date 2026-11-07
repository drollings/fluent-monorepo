//! Kokoro backend: lazy model load, serial per-voice synthesis, trimmed
//! chunks with word timings.
//!
//! The ONNX session is the bottleneck, so synthesis runs serially behind
//! a cap-1 limiter (one inference at a time, streaming path only). Word
//! timings come from uniform slicing across the chunk — the backend
//! exposes no alignment — and feed DAC speech marks downstream.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fluent_concurrency::pool::Limiter;
use fluent_wvr::{
    impl_component, Describable, FieldAccess, FieldError, WorkContext, WorkError, WorkOutput,
    WorkUnit,
};
use internment::ArcIntern;
use orator_core::{TtsError, WordMark};

/// Kokoro output rate. Chunks always synthesize at this rate.
pub const KOKORO_SAMPLE_RATE: u32 = 24_000;

/// Default model file name inside the managed directory.
pub const KOKORO_MODEL_FILE: &str = "kokoro-v1.0.onnx";

/// Default voice-pack directory name inside the managed directory.
pub const KOKORO_VOICES_DIR: &str = "kokoro-voices";

/// Weight locations: `KOKORO_MODEL` / `KOKORO_VOICES` win when set,
/// else the managed directory files. `None` when no home base
/// resolves. Existence is checked by the caller (`load` fails
/// actionable when absent).
#[must_use]
pub fn default_paths() -> Option<(PathBuf, PathBuf)> {
    let dir = orator_core::models_dir()?;
    let model =
        std::env::var("KOKORO_MODEL").map_or_else(|_| dir.join(KOKORO_MODEL_FILE), PathBuf::from);
    let voices =
        std::env::var("KOKORO_VOICES").map_or_else(|_| dir.join(KOKORO_VOICES_DIR), PathBuf::from);
    Some((model, voices))
}

/// Near-silence threshold for edge-trim (linear amplitude).
const TRIM_FLOOR: f32 = 0.02;

/// Guard kept each side of the voiced region so soft onsets survive.
const TRIM_GUARD_SAMPLES: usize = KOKORO_SAMPLE_RATE as usize / 100;

/// Lazy Kokoro backend. First synth pays the weight load; later synths
/// reuse the shared session. Voice or path changes drop the session so
/// the next synth reloads.
pub struct KokoroBackend {
    model_path: PathBuf,
    voices_dir: PathBuf,
    voice: String,
    gpu: bool,
    tts: Mutex<Option<Arc<kokoro_en::KokoroTts>>>,
    limiter: Limiter,
}

impl KokoroBackend {
    /// Backend for the model and voice packs at `paths`, speaking `voice`.
    /// CPU inference: GPU is explicit opt-in through [`Self::with_gpu`].
    pub fn new(model_path: PathBuf, voices_dir: PathBuf, voice: impl Into<String>) -> Self {
        Self {
            model_path,
            voices_dir,
            voice: voice.into(),
            gpu: false,
            tts: Mutex::new(None),
            limiter: Limiter::new(1),
        }
    }

    /// Request a GPU session (explicit opt-in, NVIDIA CUDA only).
    /// Without a visible device the next load fails actionably naming
    /// the remedy; the flag never flips silently back to CPU. AMD/ROCm
    /// hardware has no kokoro execution provider (kokoro-en speaks
    /// CUDA/DirectML/CoreML only), so opting in there fails naming
    /// CPU as the remedy instead of probing a CUDA stack that cannot
    /// exist.
    #[must_use]
    pub fn with_gpu(mut self) -> Self {
        self.gpu = true;
        self
    }

    /// Whether a session is resident.
    #[must_use]
    pub fn is_loaded(&self) -> bool {
        common_core::sync::lock(&self.tts).is_some()
    }

    /// Load (once) and return the shared session. Missing files fail as
    /// unavailable with the path named, never as a backend panic. A GPU
    /// opt-in without a visible device fails here naming the remedy
    /// instead of running silently on CPU; on AMD-only hardware the
    /// opt-in fails too (kokoro-en has no ROCm/MIGraphX provider, so a
    /// CUDA request there could never succeed). Without the opt-in the
    /// CPU session loads directly — on NVIDIA hardware the dependency's
    /// own `auto` probing still applies.
    async fn load(&self) -> Result<Arc<kokoro_en::KokoroTts>, TtsError> {
        {
            let guard = common_core::sync::lock(&self.tts);
            if let Some(loaded) = guard.as_ref() {
                return Ok(Arc::clone(loaded));
            }
        }
        if self.gpu {
            orator_core::gpu::require_gpu("kokoro").map_err(TtsError::Unavailable)?;
            if !orator_core::gpu::nvidia_present() {
                return Err(TtsError::Unavailable(
                    "kokoro has no AMD/ROCm execution provider (kokoro-en 0.1.5 speaks CUDA/DirectML/CoreML only); set the tts `gpu` leaf to false for CPU"
                        .to_string(),
                ));
            }
        }
        if let Some(provider) = ort_provider_override(
            self.gpu,
            orator_core::gpu::nvidia_present(),
            std::env::var_os("KOKORO_ORT_PROVIDER").is_some(),
        ) {
            std::env::set_var("KOKORO_ORT_PROVIDER", provider);
        }
        if !self.model_path.exists() {
            return Err(TtsError::Unavailable(format!(
                "kokoro model missing at {}",
                self.model_path.display()
            )));
        }
        if !self.voices_dir.exists() {
            return Err(TtsError::Unavailable(format!(
                "kokoro voices missing at {}",
                self.voices_dir.display()
            )));
        }
        let session = kokoro_en::KokoroTts::new(&self.model_path, &self.voices_dir)
            .await
            .map_err(|error| TtsError::Backend(format!("kokoro load: {error}")))?;
        let loaded = Arc::new(session);
        *common_core::sync::lock(&self.tts) = Some(Arc::clone(&loaded));
        Ok(loaded)
    }

    /// Synthesize `text` to voiced 24 kHz samples (edge silence trimmed,
    /// no tail gap — the pipeline owns spacing). Serial per voice; runs on
    /// the caller's thread and bridges the limiter without a per-call
    /// runtime.
    pub fn synth_blocking(&self, text: &str) -> Result<Vec<f32>, TtsError> {
        common_core::runtime::block_on(self.synth_async(text))
    }

    /// Synthesize each sentence chunk separately, in order (chunked
    /// synthesis: first audio after the first chunk, not the whole
    /// turn). Chunking uses the shared splitter; single-chunk input
    /// collapses to one identical synth call (transparency). Playback
    /// of chunk N overlaps synthesis of chunk N+1 through the
    /// streaming player; concatenation order is chunk order.
    pub fn synth_chunks_blocking(&self, text: &str) -> Result<Vec<Vec<f32>>, TtsError> {
        crate::splitter::split_all(text)
            .iter()
            .map(|chunk| self.synth_blocking(&chunk.text))
            .collect()
    }

    async fn synth_async(&self, text: &str) -> Result<Vec<f32>, TtsError> {
        let voice = self.voice.clone();
        let text = text.to_string();
        // The limiter is the thread-safety boundary: the session is
        // never proven safe for concurrent synth, so one permit covers
        // load and inference alike. Overlapping calls serialize —
        // never interleave, never corrupt, never deadlock.
        self.limiter
            .run(|| async {
                let session = self.load().await?;
                session
                    .synth(text.as_str(), voice.as_str())
                    .await
                    .map(|(samples, _)| trim_silence(&samples).to_vec())
                    .map_err(|error| TtsError::Backend(format!("kokoro synth: {error}")))
            })
            .await
    }

    fn drop_session(&self) {
        *common_core::sync::lock(&self.tts) = None;
    }
}

/// ORT provider override for one kokoro load, if the backend pins one.
///
/// Pure so the mapping is unit-pinned without weights. An explicit GPU
/// opt-in pins `cuda`, but only where NVIDIA hardware can serve it
/// (the caller errors first on AMD-only hardware, so `gpu && !nvidia`
/// never reaches a pin). Without the opt-in the dependency default
/// (`auto`) is always left alone — the CPU path never touches provider
/// selection. A user-set `KOKORO_ORT_PROVIDER` always wins.
fn ort_provider_override(
    gpu: bool,
    nvidia_present: bool,
    env_already_set: bool,
) -> Option<&'static str> {
    if env_already_set || !gpu {
        return None;
    }
    nvidia_present.then_some("cuda")
}

/// Trim the near-silent padding around the voiced middle, keeping a small
/// guard each side. All-silence returns unchanged (a gap, not an error).
fn trim_silence(samples: &[f32]) -> &[f32] {
    let voiced = |sample: &f32| sample.abs() > TRIM_FLOOR;
    let start = samples.iter().position(voiced).unwrap_or(0);
    let end = samples.iter().rposition(voiced).map_or(0, |pos| pos + 1);
    if end <= start {
        return samples;
    }
    let from = start.saturating_sub(TRIM_GUARD_SAMPLES);
    let to = (end + TRIM_GUARD_SAMPLES).min(samples.len());
    &samples[from..to]
}

/// Slice `samples` uniformly across `words`: each word owns an equal span.
/// The honest v1 approximation without backend alignment; exact for short
/// chunks, and timings stay ordered and monotonic by construction. Marks
/// are sample offsets in the `sample_rate` domain passed alongside the
/// count: callers pass the synth rate and the gapped length together, so
/// DAC consumers read one consistent clock. The rate carries no scaling
/// of its own — a zero rate names no clock and yields no marks.
#[must_use]
pub fn uniform_marks(text: &str, sample_count: usize, sample_rate: u32) -> Vec<WordMark> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() || sample_count == 0 || sample_rate == 0 {
        return Vec::new();
    }
    let count = words.len() as u64;
    let total = sample_count as u64;
    words
        .into_iter()
        .enumerate()
        .map(|(index, word)| {
            let index = index as u64;
            WordMark {
                text: word.to_string(),
                start_samples: index * total / count,
                end_samples: (index + 1) * total / count,
            }
        })
        .collect()
}

impl WorkUnit for KokoroBackend {
    fn name(&self) -> &str {
        "tts.kokoro"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        if self.is_loaded() {
            Ok(WorkOutput::ok(format!("voice={}", self.voice)))
        } else {
            Ok(WorkOutput::fail("kokoro unloaded"))
        }
    }
}

impl FieldAccess for KokoroBackend {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        // M15-S7 keep-decision: every successful set drops the session
        // (a retune must reload voices, never synthesize stale), and
        // `model_path`/`voices_dir` stay lossless `PathBuf`s. No
        // post-set hook and no `PathBuf` support in the derive — see
        // the M15-S5 note on WhisperBackend. Schema pinned by
        // `kokoro_field_surface_pins_schema_and_table`.
        match name {
            "voice" => {
                self.voice = value.to_string();
                self.drop_session();
                Ok(())
            }
            "model_path" => {
                self.model_path = PathBuf::from(value);
                self.drop_session();
                Ok(())
            }
            "voices_dir" => {
                self.voices_dir = PathBuf::from(value);
                self.drop_session();
                Ok(())
            }
            "gpu" => {
                self.gpu = value
                    .parse()
                    .map_err(|_| FieldError::Parse(format!("invalid bool for 'gpu': {value}")))?;
                self.drop_session();
                Ok(())
            }
            _ => Err(FieldError::NotFound(name.into())),
        }
    }
    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        match name {
            "voice" => Ok(self.voice.clone()),
            "model_path" => Ok(self.model_path.display().to_string()),
            "voices_dir" => Ok(self.voices_dir.display().to_string()),
            "gpu" => Ok(self.gpu.to_string()),
            _ => Err(FieldError::NotFound(name.into())),
        }
    }
    fn field_names(&self) -> &'static [&'static str] {
        &["voice", "model_path", "voices_dir", "gpu"]
    }
}

impl Describable for KokoroBackend {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "voice": { "type": "string", "description": "Voice tag" },
                "model_path": { "type": "string", "description": "Kokoro ONNX model" },
                "voices_dir": { "type": "string", "description": "Voice pack directory" },
                "gpu": { "type": "boolean", "description": "GPU offload (explicit opt-in)" }
            },
            "required": ["voice", "model_path", "voices_dir"]
        })
    }
}

impl_component!(KokoroBackend);

#[cfg(test)]
#[path = "../tests/kokoro.rs"]
mod tests;
