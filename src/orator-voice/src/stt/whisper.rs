//! whisper.cpp backend: lazy model load, warm inference, blocking decode.
//!
//! The context is shared behind `Arc`; per-call states are created inside
//! `decode_blocking`, which runs on the caller's thread. Node tasks invoke
//! it from the inference pool under `Limiter::run` — never on a tokio
//! worker, never on the real-time path.

use std::path::PathBuf;
use std::sync::Mutex;

use fluent_concurrency::stream::StreamAbort;
use fluent_wvr::{
    impl_component, Describable, FieldAccess, FieldError, WorkContext, WorkError, WorkOutput,
    WorkUnit,
};
use internment::ArcIntern;
use orator::SttError;
use std::sync::Arc;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// One decoded segment before word splitting.
#[derive(Debug, Clone, PartialEq)]
/// One decoded segment before word splitting.
pub struct RawSegment {
    /// Segment text.
    pub text: String,
    /// Start in centiseconds.
    pub start_cs: i64,
    /// End in centiseconds.
    pub end_cs: i64,
    /// No-speech probability in `[0.0, 1.0]`.
    pub no_speech: f32,
}

/// Lazy whisper.cpp backend. First call pays the weight load; later calls
/// reuse the shared context. Language or model changes drop the context so
/// the next call reloads.
pub struct WhisperBackend {
    model_path: PathBuf,
    language: String,
    gpu: bool,
    ctx: Mutex<Option<Arc<WhisperContext>>>,
}

impl WhisperBackend {
    /// Backend for the model file at `model_path`, recognizing `language`.
    /// CPU inference: GPU is explicit opt-in through [`Self::with_gpu`].
    pub fn new(model_path: PathBuf, language: impl Into<String>) -> Self {
        Self {
            model_path,
            language: language.into(),
            gpu: false,
            ctx: Mutex::new(None),
        }
    }

    /// Request GPU inference (explicit opt-in). Without a visible device
    /// the next load fails actionably naming the remedy; the flag never
    /// flips silently back to CPU.
    #[must_use]
    pub fn with_gpu(mut self) -> Self {
        self.gpu = true;
        self
    }

    /// Whether a context is resident.
    #[must_use]
    pub fn is_loaded(&self) -> bool {
        common_core::sync::lock(&self.ctx).is_some()
    }

    /// Load (once) and return the shared context. GPU is attempted only
    /// when opted in *and* a device is visible; a device-less opt-in
    /// fails here naming the remedy instead of running silently on CPU.
    /// Double-checked: path and flags clone under the lock, the weight
    /// load runs unlocked, then the winner publishes — a concurrent
    /// second decode never blocks on the full load.
    fn load(&self) -> Result<Arc<WhisperContext>, SttError> {
        let (model_path, gpu) = {
            let guard = common_core::sync::lock(&self.ctx);
            if let Some(loaded) = guard.as_ref() {
                return Ok(Arc::clone(loaded));
            }
            (self.model_path.clone(), self.gpu)
        };
        if gpu {
            orator::gpu::require_gpu("whisper").map_err(SttError::Unavailable)?;
        }
        if !model_path.exists() {
            return Err(SttError::Unavailable(format!(
                "whisper model missing at {}",
                model_path.display()
            )));
        }
        let params = WhisperContextParameters {
            use_gpu: gpu,
            ..WhisperContextParameters::default()
        };
        let ctx = WhisperContext::new_with_params(&model_path, params)
            .map_err(|error| SttError::Backend(format!("whisper load: {error}")))?;
        let loaded = Arc::new(ctx);
        let mut guard = common_core::sync::lock(&self.ctx);
        if let Some(winner) = guard.as_ref() {
            return Ok(Arc::clone(winner));
        }
        *guard = Some(Arc::clone(&loaded));
        Ok(loaded)
    }

    /// Run one throwaway inference on half a second of silence so the first
    /// real decode skips graph setup. Same greedy params as `decode`.
    pub fn warm(&self) -> Result<(), SttError> {
        let ctx = self.load()?;
        let state = ctx
            .create_state()
            .map_err(|error| SttError::Backend(format!("whisper warm state: {error}")))?;
        let params = Self::decode_params(&self.language, None, transcribe_threads());
        let mut state = state;
        state
            .full(params, &vec![0.0f32; 8_000])
            .map_err(|error| SttError::Backend(format!("whisper warm infer: {error}")))?;
        Ok(())
    }

    /// Decode `samples` (16 kHz mono f32) to segments on this thread.
    /// `abort` is polled by whisper.cpp's abort callback between passes: a
    /// fired abort surfaces as `Aborted`, never as a truncated transcript.
    pub fn decode_blocking(
        &self,
        samples: &[f32],
        abort: &StreamAbort,
    ) -> Result<Vec<RawSegment>, SttError> {
        let ctx = self.load()?;
        let state = ctx
            .create_state()
            .map_err(|error| SttError::Backend(format!("whisper state: {error}")))?;
        let params = Self::decode_params(&self.language, Some(abort), transcribe_threads());
        let mut state = state;
        match state.full(params, samples) {
            Ok(()) => {}
            Err(error) => {
                if abort.is_cancelled() {
                    return Err(SttError::Aborted);
                }
                return Err(SttError::Backend(format!("whisper infer: {error}")));
            }
        }
        let mut segments = Vec::new();
        for segment in state.as_iter() {
            let text = segment
                .to_str_lossy()
                .map_err(|error| SttError::Backend(format!("segment text: {error}")))?
                .into_owned();
            segments.push(RawSegment {
                text,
                start_cs: segment.start_timestamp(),
                end_cs: segment.end_timestamp(),
                no_speech: segment.no_speech_probability(),
            });
        }
        Ok(segments)
    }

    fn decode_params<'a>(
        language: &'a str,
        abort: Option<&StreamAbort>,
        threads: std::os::raw::c_int,
    ) -> FullParams<'a, 'a> {
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(language));
        params.set_translate(false);
        params.set_no_context(true);
        params.set_temperature(0.0);
        params.set_temperature_inc(0.0);
        params.set_n_threads(threads);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        if let Some(signal) = abort {
            let polled = signal.clone();
            let callback: Box<dyn FnMut() -> bool> = Box::new(move || polled.is_cancelled());
            params.set_abort_callback_safe::<
                Option<Box<dyn FnMut() -> bool>>,
                Box<dyn FnMut() -> bool>,
            >(Some(callback));
        }
        params
    }

    fn drop_context(&self) {
        *common_core::sync::lock(&self.ctx) = None;
    }
}

/// Split one decoded segment into words with uniform timing across its
/// span. Token-level alignment lands later; uniform slicing is the honest
/// v1 approximation and is exact for short segments.
#[must_use]
/// Split one decoded segment into words with uniform timing across its
/// span. Token-level alignment lands later; uniform slicing is the honest
/// v1 approximation and is exact for short segments.
pub fn split_segment_words(segment: &RawSegment) -> Vec<crate::stt::streaming::DecodedWord> {
    let tokens: Vec<&str> = segment.text.split_whitespace().collect();
    if tokens.is_empty() {
        return Vec::new();
    }
    let span_ms =
        (segment.end_cs.saturating_sub(segment.start_cs).max(0) as u64).saturating_mul(10);
    let base_ms = (segment.start_cs.max(0) as u64).saturating_mul(10);
    let count = tokens.len() as u64;
    tokens
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            let index = index as u64;
            crate::stt::streaming::DecodedWord {
                text: text.to_string(),
                start_ms: base_ms.saturating_add(index.saturating_mul(span_ms) / count),
                end_ms: base_ms.saturating_add((index + 1).saturating_mul(span_ms) / count),
                no_speech: segment.no_speech,
            }
        })
        .collect()
}

impl crate::stt::streaming::BlockDecoder for WhisperBackend {
    fn decode(
        &self,
        samples: &[f32],
        offset_ms: u64,
        abort: &StreamAbort,
    ) -> Result<Vec<crate::stt::streaming::DecodedWord>, SttError> {
        let segments = self.decode_blocking(samples, abort)?;
        Ok(segments
            .iter()
            .flat_map(split_segment_words)
            .map(|mut word| {
                word.start_ms = word.start_ms.saturating_add(offset_ms);
                word.end_ms = word.end_ms.saturating_add(offset_ms);
                word
            })
            .collect())
    }
}

/// whisper.cpp thread budget: host parallelism capped so decodes never
/// oversubscribe the machine.
fn transcribe_threads() -> std::os::raw::c_int {
    std::thread::available_parallelism()
        .map_or(4, |parallel| parallel.get().min(8) as std::os::raw::c_int)
}

impl WorkUnit for WhisperBackend {
    fn name(&self) -> &str {
        "stt.whisper"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        if self.is_loaded() {
            Ok(WorkOutput::ok("whisper loaded"))
        } else {
            Ok(WorkOutput::fail("whisper unloaded"))
        }
    }
}

impl FieldAccess for WhisperBackend {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        // M15-S5 keep-decision: every successful set drops the loaded
        // context (a retune must reload, never serve stale weights) and
        // `model_path` stays a lossless `PathBuf`. The derive has no
        // post-set hook and no `PathBuf` support (its getter needs
        // `Display`), so migration would silently drop invalidation or
        // lossy-convert non-UTF8 model paths. The schema this mirrors
        // is pinned by `whisper_field_surface_pins_schema_and_table`.
        match name {
            "language" => {
                self.language = value.to_string();
                self.drop_context();
                Ok(())
            }
            "model_path" => {
                self.model_path = PathBuf::from(value);
                self.drop_context();
                Ok(())
            }
            "gpu" => {
                self.gpu = value
                    .parse()
                    .map_err(|_| FieldError::Parse(format!("invalid bool for 'gpu': {value}")))?;
                self.drop_context();
                Ok(())
            }
            _ => Err(FieldError::NotFound(name.into())),
        }
    }
    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        match name {
            "language" => Ok(self.language.clone()),
            "model_path" => Ok(self.model_path.display().to_string()),
            "gpu" => Ok(self.gpu.to_string()),
            _ => Err(FieldError::NotFound(name.into())),
        }
    }
    fn field_names(&self) -> &'static [&'static str] {
        &["language", "model_path", "gpu"]
    }
}

impl Describable for WhisperBackend {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "language": { "type": "string", "description": "Recognition language" },
                "model_path": { "type": "string", "description": "Whisper model file" },
                "gpu": { "type": "boolean", "description": "GPU offload (explicit opt-in)" }
            },
            "required": ["language", "model_path"]
        })
    }
}

impl_component!(WhisperBackend);

#[cfg(test)]
#[path = "../../tests/stt_whisper.rs"]
mod tests;