//! Moonshine backend: local streaming-ASR weights through
//! transcribe.cpp (ggml, MIT). English-only, 16 kHz mono in, plain
//! text out (no timestamps, no language detection) — the model that
//! heads the default ASR role.
//!
//! The model loads lazily behind a mutex; per-call sessions run on
//! the caller's thread. CPU serves 13–47x realtime, inside every
//! local-op budget; GPU is explicit opt-in through
//! [`Self::with_gpu`] and fails actionably without a device.

use std::path::PathBuf;
use std::sync::Mutex;

use fluent_concurrency::stream::StreamAbort;
use fluent_wvr::{
    impl_component, Describable, FieldAccess, FieldError, WorkContext, WorkError, WorkOutput,
    WorkUnit,
};
use internment::ArcIntern;
use orator_core::SttError;
use std::sync::Arc;

/// Lazy moonshine backend. First call pays the weight load; later
/// calls reuse the shared model. Model changes drop the handle so
/// the next call reloads.
pub struct MoonshineBackend {
    model_path: PathBuf,
    gpu: bool,
    model: Mutex<Option<Arc<transcribe_cpp::Model>>>,
}

impl MoonshineBackend {
    /// Backend for the moonshine GGUF at `model_path`. CPU
    /// inference; GPU is explicit opt-in through [`Self::with_gpu`].
    /// The model is English-only: there is no language to set.
    pub fn new(model_path: impl Into<PathBuf>) -> Self {
        Self {
            model_path: model_path.into(),
            gpu: false,
            model: Mutex::new(None),
        }
    }

    /// Request GPU inference (explicit opt-in). Without a visible
    /// device the next load fails actionably naming the remedy; the
    /// flag never flips silently back to CPU.
    #[must_use]
    pub fn with_gpu(mut self) -> Self {
        self.gpu = true;
        self
    }

    /// Whether a model is resident.
    #[must_use]
    pub fn is_loaded(&self) -> bool {
        common_core::sync::lock(&self.model).is_some()
    }

    /// Load (once) and return the shared model. GPU is attempted only
    /// when opted in *and* a device is visible; a device-less opt-in
    /// fails here naming the remedy instead of running silently on
    /// CPU. The weight load runs unlocked, then the winner publishes.
    fn load(&self) -> Result<Arc<transcribe_cpp::Model>, SttError> {
        let (model_path, gpu) = {
            let guard = common_core::sync::lock(&self.model);
            if let Some(loaded) = guard.as_ref() {
                return Ok(Arc::clone(loaded));
            }
            (self.model_path.clone(), self.gpu)
        };
        if gpu {
            orator_core::gpu::require_gpu("moonshine").map_err(SttError::Unavailable)?;
        }
        if !model_path.exists() {
            return Err(SttError::Unavailable(format!(
                "moonshine model missing at {} (fetch it with `orator models pull`)",
                model_path.display()
            )));
        }
        let backend = if gpu {
            transcribe_cpp::Backend::Rocm
        } else {
            transcribe_cpp::Backend::Cpu
        };
        let options = transcribe_cpp::ModelOptions {
            backend,
            ..Default::default()
        };
        let model = transcribe_cpp::Model::load_with(&model_path, &options)
            .map_err(|error| SttError::Backend(format!("moonshine load: {error}")))?;
        let loaded = Arc::new(model);
        let mut guard = common_core::sync::lock(&self.model);
        if let Some(winner) = guard.as_ref() {
            return Ok(Arc::clone(winner));
        }
        *guard = Some(Arc::clone(&loaded));
        Ok(loaded)
    }

    /// Run one throwaway inference on half a second of silence so the
    /// first real decode skips graph setup.
    pub fn warm(&self) -> Result<(), SttError> {
        let model = self.load()?;
        let mut session = model
            .session()
            .map_err(|error| SttError::Backend(format!("moonshine warm session: {error}")))?;
        session
            .run(&vec![0.0f32; 8_000], &transcribe_cpp::RunOptions::default())
            .map_err(|error| SttError::Backend(format!("moonshine warm infer: {error}")))?;
        Ok(())
    }

    /// Decode `samples` (16 kHz mono f32) to segments on this thread.
    /// Abort is honored before and after the blocking run; mid-run
    /// cancellation is not wired in v1 (CPU inference runs 13–47x
    /// realtime, so the window is milliseconds on file turns).
    /// Moonshine emits no timestamps: the single segment spans the
    /// whole input.
    pub fn decode_blocking(
        &self,
        samples: &[f32],
        abort: &StreamAbort,
    ) -> Result<Vec<super::whisper::RawSegment>, SttError> {
        if abort.is_cancelled() {
            return Err(SttError::Aborted);
        }
        let model = self.load()?;
        let mut session = model
            .session()
            .map_err(|error| SttError::Backend(format!("moonshine session: {error}")))?;
        let transcript = session
            .run(samples, &transcribe_cpp::RunOptions::default())
            .map_err(|error| {
                if abort.is_cancelled() {
                    SttError::Aborted
                } else {
                    SttError::Backend(format!("moonshine infer: {error}"))
                }
            })?;
        if abort.is_cancelled() {
            return Err(SttError::Aborted);
        }
        // Centiseconds across the whole input (16 kHz mono).
        let end_cs = samples.len().div_ceil(160) as i64;
        Ok(vec![super::whisper::RawSegment {
            text: transcript.text,
            start_cs: 0,
            end_cs,
            no_speech: 0.0,
        }])
    }

    fn drop_model(&self) {
        *common_core::sync::lock(&self.model) = None;
    }
}

impl crate::streaming::BlockDecoder for MoonshineBackend {
    fn decode(
        &self,
        samples: &[f32],
        offset_ms: u64,
        abort: &StreamAbort,
    ) -> Result<Vec<crate::streaming::DecodedWord>, SttError> {
        let segments = self.decode_blocking(samples, abort)?;
        Ok(segments
            .iter()
            .flat_map(super::whisper::split_segment_words)
            .map(|mut word| {
                word.start_ms = word.start_ms.saturating_add(offset_ms);
                word.end_ms = word.end_ms.saturating_add(offset_ms);
                word
            })
            .collect())
    }
}

impl WorkUnit for MoonshineBackend {
    fn name(&self) -> &str {
        "stt.moonshine"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        if self.is_loaded() {
            Ok(WorkOutput::ok("moonshine loaded"))
        } else {
            Ok(WorkOutput::fail("moonshine unloaded"))
        }
    }
}

impl FieldAccess for MoonshineBackend {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        // M15-S6 keep-decision: same shape as WhisperBackend (drop the
        // loaded model after every successful set; `model_path` stays a
        // lossless `PathBuf`). No post-set hook and no `PathBuf`
        // support in the derive — see the M15-S5 note there. Schema
        // pinned by `moonshine_field_surface_pins_schema_and_table`.
        match name {
            "model_path" => {
                self.model_path = PathBuf::from(value);
                self.drop_model();
                Ok(())
            }
            "gpu" => {
                self.gpu = value
                    .parse()
                    .map_err(|_| FieldError::Parse(format!("invalid bool for 'gpu': {value}")))?;
                self.drop_model();
                Ok(())
            }
            _ => Err(FieldError::NotFound(name.into())),
        }
    }
    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        match name {
            "model_path" => Ok(self.model_path.display().to_string()),
            "gpu" => Ok(self.gpu.to_string()),
            _ => Err(FieldError::NotFound(name.into())),
        }
    }
    fn field_names(&self) -> &'static [&'static str] {
        &["model_path", "gpu"]
    }
}

impl Describable for MoonshineBackend {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "model_path": { "type": "string", "description": "Moonshine model file" },
                "gpu": { "type": "boolean", "description": "GPU offload (explicit opt-in)" }
            },
            "required": ["model_path"]
        })
    }
}

impl_component!(MoonshineBackend);

#[cfg(test)]
#[path = "../tests/moonshine.rs"]
mod tests;
