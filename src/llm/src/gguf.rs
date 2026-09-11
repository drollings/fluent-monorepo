//! Local GGUF embedding backend behind [`EmbeddingProvider`].
//!
//! There is no in-tree GGUF inference counterpart: the actual model runtime
//! arrives behind the [`GgufRuntimeLoader`] seam (production binds a real
//! llama.cpp runtime; hermetic tests inject a stub playing that role).
//! Artifact resolution reuses the [`crate::artifact`] plane. Text shaping is
//! per catalog `format` (`embeddinggemma` task/title lines vs `qwen3`
//! instruct/query lines), over-context inputs truncate through the model's
//! tokenizer with per-input accounting, and embedding contexts parallelize
//! with the VRAM-derived cap. GPU failures fall back to CPU exactly once per
//! stage; artifact and runtime-import failures never enter GPU fallback.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::artifact::{
    DownloadProgressReporter, EmbeddingProgress, ModelArtifact, ModelArtifactDownloadProgress,
    ModelArtifactSource, ReqwestArtifactFetcher, ResolveArtifactsOptions, SourceKind,
    resolve_model_artifacts,
};
use crate::artifact_lock::LockOptions;
use crate::catalog::{CatalogEntry, EmbeddingBackend};
use crate::embeddings::{
    BatchEmbedding, EmbeddingError, EmbeddingProvider, block_on_provider, check_embed_batch_output,
    validate_embed_batch,
};


/// Compute device selection for local embedding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Device {
    Auto,
    #[default]
    Cpu,
    Metal,
    Vulkan,
    Cuda,
}

impl Device {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "cpu" => Some(Self::Cpu),
            "metal" => Some(Self::Metal),
            "vulkan" => Some(Self::Vulkan),
            "cuda" => Some(Self::Cuda),
            _ => None,
        }
    }
}

/// Runtime GPU mode. `Cpu` disables offload (`gpuLayers: 0`); anything else
/// attempts the named backend with CPU fallback on failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpuSelection {
    Cpu,
    Auto,
    Metal,
    Vulkan,
    Cuda,
}

impl From<Device> for GpuSelection {
    fn from(device: Device) -> Self {
        match device {
            Device::Auto => Self::Auto,
            Device::Cpu => Self::Cpu,
            Device::Metal => Self::Metal,
            Device::Vulkan => Self::Vulkan,
            Device::Cuda => Self::Cuda,
        }
    }
}

/// Document inputs embed raw; query inputs take the catalog format's query
/// shaping (instruct/task prefixes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedPurpose {
    Document,
    Query,
}

/// One embedding context's sizing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GgufContextOptions {
    pub context_size: usize,
    pub threads: u32,
}

/// Catalog `format` values with distinct text shaping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GgufFormat {
    EmbeddingGemma,
    Qwen3,
}

/// Validated GGUF catalog entry view.
#[derive(Debug, Clone)]
pub struct GgufModelSpec {
    pub reference: &'static str,
    pub model: &'static str,
    pub dimension: u32,
    pub context_size: usize,
    pub max_batch_size: usize,
    pub format: GgufFormat,
    pub uri: &'static str,
    pub cache_file: &'static str,
    pub hugging_face: (&'static str, &'static str),
    pub model_scope: (&'static str, &'static str),
    pub artifacts: Vec<ModelArtifact>,
}

impl TryFrom<&CatalogEntry> for GgufModelSpec {
    type Error = EmbeddingError;

    fn try_from(entry: &CatalogEntry) -> Result<Self, Self::Error> {
        let invalid = |field: &str| {
            EmbeddingError::InvalidCatalogEntry(format!(
                "{} is not a GGUF entry (missing {field})",
                entry.reference
            ))
        };
        if entry.backend != EmbeddingBackend::LlamaCpp {
            return Err(invalid("llama-cpp backend"));
        }
        Ok(Self {
            reference: entry.reference,
            model: entry.model,
            dimension: entry.dimension,
            context_size: entry.context_size.ok_or_else(|| invalid("contextSize"))?,
            max_batch_size: entry.max_batch_size,
            format: match entry.format {
                Some("qwen3") => GgufFormat::Qwen3,
                _ => GgufFormat::EmbeddingGemma,
            },
            uri: entry.uri.ok_or_else(|| invalid("uri"))?,
            cache_file: entry.cache_file.ok_or_else(|| invalid("cacheFile"))?,
            hugging_face: entry
                .sources
                .map(|sources| (sources.hugging_face.0, sources.hugging_face.1))
                .ok_or_else(|| invalid("sources"))?,
            model_scope: entry
                .sources
                .map(|sources| (sources.model_scope.0, sources.model_scope.1))
                .ok_or_else(|| invalid("sources"))?,
            artifacts: entry
                .artifacts
                .iter()
                .map(|artifact| ModelArtifact {
                    path: artifact.path.to_string(),
                    size: artifact.size,
                    sha256: artifact.sha256.to_string(),
                })
                .collect(),
        })
    }
}

// ---------------------------------------------------------------------------
// Runtime seams (the injectable llama.cpp boundary).
// ---------------------------------------------------------------------------

/// One embedding context: sequential embedding lookups over its texts.
#[async_trait]
pub trait GgufEmbeddingContext: Send {
    async fn embedding_for(&self, text: &str) -> Result<Vec<f32>, EmbeddingError>;
    async fn dispose(&self) -> Result<(), EmbeddingError>;
}

/// A loaded GGUF model: tokenizer access plus context creation.
#[async_trait]
pub trait GgufModel: Send + Sync {
    fn train_context_size(&self) -> Option<usize>;
    fn tokenize(&self, text: &str) -> Option<Vec<String>>;
    fn detokenize(&self, tokens: &[String]) -> Option<String>;
    async fn create_embedding_context(
        &self,
        options: &GgufContextOptions,
    ) -> Result<Box<dyn GgufEmbeddingContext>, EmbeddingError>;
    async fn dispose(&self) -> Result<(), EmbeddingError>;
}

/// A llama.cpp runtime instance bound to one GPU mode.
#[async_trait]
pub trait GgufLlamaInstance: Send {
    fn uses_gpu(&self) -> bool;
    fn cpu_math_cores(&self) -> u32;
    /// Free VRAM bytes, or `None` when the runtime cannot report it.
    fn vram_free_bytes(&self) -> Option<u64>;
    async fn load_model(
        &self,
        path: &Path,
        gpu_layers: Option<u32>,
    ) -> Result<Box<dyn GgufModel>, EmbeddingError>;
    async fn dispose(&self) -> Result<(), EmbeddingError>;
}

/// The importable runtime module (`getLlama` entry point).
#[async_trait]
pub trait GgufRuntimeModule: Send + Sync {
    async fn get_llama(
        &self,
        gpu: GpuSelection,
    ) -> Result<Box<dyn GgufLlamaInstance>, EmbeddingError>;
}

/// Loads the runtime module (cached after first use).
#[async_trait]
pub trait GgufRuntimeLoader: Send + Sync {
    async fn load_module(&self) -> Result<Box<dyn GgufRuntimeModule>, EmbeddingError>;
}

/// Artifact-plane seam: resolves the GGUF snapshot, reporting plan,
/// progress and mirror fallback through the caller's hooks.
#[async_trait]
pub trait GgufArtifactResolver: Send + Sync {
    async fn resolve(
        &self,
        request: &GgufArtifactRequest<'_>,
    ) -> Result<GgufResolvedArtifacts, EmbeddingError>;
}

pub struct GgufArtifactRequest<'a> {
    pub model: &'a str,
    pub sources: Vec<ModelArtifactSource>,
    pub artifacts: Vec<ModelArtifact>,
    pub on_download_plan: Option<crate::artifact::DownloadPlanHook<'a>>,
    pub on_progress: Option<crate::artifact::ArtifactProgressHook<'a>>,
    pub on_fallback: Option<crate::artifact::FallbackHook<'a>>,
}

#[derive(Debug, Clone)]
pub struct GgufResolvedArtifacts {
    pub source: ModelArtifactSource,
    pub directory: String,
    pub paths: HashMap<String, String>,
}

/// Production resolver: the real artifact plane over HTTP.
pub struct ProductionGgufResolver {
    fetcher: ReqwestArtifactFetcher,
}
impl ProductionGgufResolver {
    #[must_use]
    pub fn new() -> Self {
        Self {
            fetcher: ReqwestArtifactFetcher::new(),
        }
    }
}

impl Default for ProductionGgufResolver {
    fn default() -> Self {
        Self::new()
    }
}

/// Placeholder runtime loader: construction succeeds so references resolve,
/// but first use fails fast naming the missing binding. The daemon layer
/// replaces this with a real llama.cpp runtime at startup.
pub struct UnboundGgufRuntime;

#[async_trait]
impl GgufRuntimeLoader for UnboundGgufRuntime {
    async fn load_module(&self) -> Result<Box<dyn GgufRuntimeModule>, EmbeddingError> {
        Err(EmbeddingError::RequestFailed(
            "local GGUF runtime is not bound (no llama.cpp runtime registered)".to_string(),
        ))
    }
}

#[async_trait]
impl GgufArtifactResolver for ProductionGgufResolver {
    async fn resolve(
        &self,
        request: &GgufArtifactRequest<'_>,
    ) -> Result<GgufResolvedArtifacts, EmbeddingError> {
        let options = ResolveArtifactsOptions {
            model: request.model,
            sources: request.sources.clone(),
            artifacts: request.artifacts.clone(),
            on_download_plan: request.on_download_plan,
            on_progress: request.on_progress,
            on_fallback: request.on_fallback,
            fetcher: &self.fetcher,
            timeouts: Default::default(),
            lock: LockOptions::default(),
        };
        resolve_model_artifacts(&options)
            .await
            .map(|resolved| GgufResolvedArtifacts {
                source: resolved.source,
                directory: resolved.directory,
                paths: resolved.paths,
            })
            .map_err(|error| EmbeddingError::RequestFailed(error.to_string()))
    }
}

/// Injected dependencies: runtime loader plus artifact resolver.
pub struct GgufDependencies {
    pub loader: Arc<dyn GgufRuntimeLoader>,
    pub resolver: Arc<dyn GgufArtifactResolver>,
}

/// Construction options.
pub struct GgufModelOptions {
    pub model_cache_dir: Option<PathBuf>,
    pub device: Device,
}

/// Batch result with per-input truncation indexes.
#[derive(Debug)]
pub struct GgufEmbedResult {
    pub vectors: Vec<Vec<f32>>,
    pub truncated: Vec<usize>,
}

const DEFAULT_PARALLELISM_CAP: usize = 8;
const MODEL_CACHE_ENVIRONMENT_VARIABLE: &str = "GUIDANCE_MODEL_CACHE";
const GGUF_MAGIC: [u8; 4] = *b"GGUF";

struct LoadedState {
    module: Option<Box<dyn GgufRuntimeModule>>,
    llama: Option<Box<dyn GgufLlamaInstance>>,
    model: Option<Box<dyn GgufModel>>,
    contexts: Vec<Box<dyn GgufEmbeddingContext>>,
    model_path: Option<PathBuf>,
    using_cpu_fallback: bool,
    failed_gpu_modes: HashSet<GpuSelection>,
    cpu_fallback_warned: bool,
    disposed: bool,
}

/// Local GGUF embedding model. One instance owns one loaded model shared by
/// all of its embed calls (model-pool sharing at instance granularity).
pub struct GgufEmbedding {
    spec: GgufModelSpec,
    model_cache_dir: PathBuf,
    gpu: GpuSelection,
    parallelism_override: Option<usize>,
    dependencies: GgufDependencies,
    state: tokio::sync::Mutex<LoadedState>,
}

impl GgufEmbedding {
    pub fn new(
        entry: &CatalogEntry,
        options: GgufModelOptions,
        dependencies: GgufDependencies,
    ) -> Result<Self, EmbeddingError> {
        let spec = GgufModelSpec::try_from(entry)?;
        let cache_dir = options
            .model_cache_dir
            .unwrap_or_else(default_model_cache_dir);
        Ok(Self {
            spec,
            model_cache_dir: cache_dir,
            gpu: GpuSelection::from(options.device),
            parallelism_override: None,
            dependencies,
            state: tokio::sync::Mutex::new(LoadedState {
                module: None,
                llama: None,
                model: None,
                contexts: Vec::new(),
                model_path: None,
                using_cpu_fallback: false,
                failed_gpu_modes: HashSet::new(),
                cpu_fallback_warned: false,
                disposed: false,
            }),
        })
    }

    /// Test hook for the parallelism-override role (production reads the
    /// environment in the factory instead of mutating globals in tests).
    #[must_use]
    pub fn with_parallelism_override(mut self, parallelism: Option<usize>) -> Self {
        self.parallelism_override = parallelism;
        self
    }

    /// Resolve artifacts and load the model without embedding (warms the
    /// shared loaded state before batching starts).
    pub async fn prepare(
        &self,
        on_progress: Option<&(dyn Fn(EmbeddingProgress) + Send + Sync)>,
    ) -> Result<(), EmbeddingError> {
        let mut state = self.state.lock().await;
        ensure_model(self, &mut state, on_progress).await
    }

    /// Embed one batch with purpose shaping and truncation accounting.
    pub async fn embed_texts(
        &self,
        texts: &[String],
        purpose: EmbedPurpose,
        on_progress: Option<&(dyn Fn(EmbeddingProgress) + Send + Sync)>,
    ) -> Result<GgufEmbedResult, EmbeddingError> {
        validate_embed_batch(texts.len(), self.spec.max_batch_size)?;
        if texts.is_empty() {
            return Err(EmbeddingError::RequestFailed(
                "Embedding requires at least one content item".to_string(),
            ));
        }
        let mut state = self.state.lock().await;
        if state.disposed {
            return Err(EmbeddingError::RequestFailed(format!(
                "llama.cpp embedding model is disposed (model={})",
                self.spec.reference
            )));
        }
        let result = embed_shaped(self, &mut state, texts, purpose, on_progress)
            .await
            .map_err(|error| EmbeddingError::RequestFailed(format!("llama.cpp embedding failed: {error}")))?;
        check_embed_batch_output(
            &BatchEmbedding {
                flat: result.vectors.iter().flatten().copied().collect(),
                count: result.vectors.len(),
                dims: self.spec.dimension as usize,
            },
            texts.len(),
            self.spec.dimension as usize,
            &result.truncated,
        )?;
        Ok(result)
    }

    /// Idempotent teardown: contexts, then model, then runtime.
    pub async fn dispose(&self) {
        let mut state = self.state.lock().await;
        if state.disposed {
            return;
        }
        state.disposed = true;
        dispose_loaded_runtime(&mut state).await;
    }
}

// ---------------------------------------------------------------------------
// Load pipeline (all take the already-locked state; the mutex is the
// singleflight — concurrent embeds serialize on it).
// ---------------------------------------------------------------------------

async fn embed_shaped(
    provider: &GgufEmbedding,
    state: &mut LoadedState,
    texts: &[String],
    purpose: EmbedPurpose,
    on_progress: Option<&(dyn Fn(EmbeddingProgress) + Send + Sync)>,
) -> Result<GgufEmbedResult, EmbeddingError> {
    ensure_model(provider, state, on_progress).await?;
    let target = effective_parallelism(provider, state, texts.len());
    ensure_contexts(provider, state, target).await?;
    let model = state.model.as_ref().expect("model ensured above");
    let mut truncated = Vec::new();
    let mut safe = Vec::with_capacity(texts.len());
    for (index, text) in texts.iter().enumerate() {
        let shaped = format_text_for_embedding(text, purpose, provider.spec.format);
        let shortened = truncate_to_context_size(model.as_ref(), &shaped, provider.spec.context_size);
        if shortened.truncated {
            truncated.push(index);
        }
        safe.push(shortened.text);
    }
    let context_count = state.contexts.len().min(target).max(1);
    let chunk_size = safe.len().div_ceil(context_count);
    let mut vectors = Vec::with_capacity(safe.len());
    for (chunk_index, chunk) in safe.chunks(chunk_size).enumerate() {
        let context = &state.contexts[chunk_index.min(context_count - 1)];
        for text in chunk {
            vectors.push(context.embedding_for(text).await?);
        }
    }
    Ok(GgufEmbedResult { vectors, truncated })
}

async fn ensure_model(
    provider: &GgufEmbedding,
    state: &mut LoadedState,
    on_progress: Option<&(dyn Fn(EmbeddingProgress) + Send + Sync)>,
) -> Result<(), EmbeddingError> {
    if state.model.is_some() {
        return Ok(());
    }
    let reporter = DownloadProgressReporter::new(provider.spec.reference, move |event| {
        if let Some(hook) = on_progress {
            hook(event);
        }
    });
    reporter.start();
    // Artifact failures must never enter a GPU-to-CPU retry path, so the
    // model path resolves (from cache when already known) before any runtime
    // initializes.
    let model_path = if let Some(path) = state.model_path.clone() {
        path
    } else {
        let resolved = resolve_model_path(provider, &reporter).await?;
        validate_gguf_file(&resolved, provider.spec.uri)?;
        state.model_path = Some(resolved.clone());
        resolved
    };
    // Runtime initialization owns its own GPU fallback; it stays outside the
    // model-load retry so import errors are never mislabeled as GPU model
    // failures.
    ensure_llama(provider, state, &reporter).await?;
    match load_model_into_state(provider, state, &model_path).await {
        Ok(()) => {
            reporter.finish();
            Ok(())
        }
        Err(error) => {
            if !can_retry_on_cpu(provider, state) {
                return Err(error);
            }
            report_warning(
                &reporter,
                &format!("llama.cpp GPU model load failed ({error}), falling back to CPU."),
            );
            state.using_cpu_fallback = true;
            dispose_loaded_runtime(state).await;
            let model_path = state.model_path.clone().expect("path resolved above");
            ensure_llama(provider, state, &reporter).await?;
            load_model_into_state(provider, state, &model_path).await?;
            reporter.finish();
            Ok(())
        }
    }
}

fn load_model_into_state<'a>(
    provider: &'a GgufEmbedding,
    state: &'a mut LoadedState,
    model_path: &'a Path,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), EmbeddingError>> + Send + 'a>> {
    Box::pin(async move {
        let offloaded = offload_disabled(provider, state);
        let llama = state.llama.as_mut().expect("llama ensured above");
        let gpu_layers = if offloaded { Some(0) } else { None };
        let model = llama.load_model(model_path, gpu_layers).await?;
        state.model = Some(model);
        Ok(())
    })
}

async fn ensure_llama(
    provider: &GgufEmbedding,
    state: &mut LoadedState,
    reporter: &DownloadProgressReporter<'_>,
) -> Result<(), EmbeddingError> {
    if state.llama.is_some() {
        return Ok(());
    }
    ensure_module(provider, state).await?;
    // The module lives in state; take it out briefly so the fallback loader
    // can borrow module and state disjointly.
    let module = state.module.take().expect("module ensured above");
    let result = load_llama_with_fallback(provider, module.as_ref(), state, reporter).await;
    match result {
        Ok(llama) => {
            state.llama = Some(llama);
            state.module = Some(module);
            Ok(())
        }
        Err(error) => {
            state.module = Some(module);
            Err(error)
        }
    }
}

async fn ensure_module(
    provider: &GgufEmbedding,
    state: &mut LoadedState,
) -> Result<(), EmbeddingError> {
    if state.module.is_some() {
        return Ok(());
    }
    // Runtime import failures are not GPU failures: they propagate before
    // any llama.cpp fallback labeling applies.
    let module = provider.dependencies.loader.load_module().await?;
    state.module = Some(module);
    Ok(())
}

async fn load_llama_with_fallback(
    provider: &GgufEmbedding,
    module: &dyn GgufRuntimeModule,
    state: &mut LoadedState,
    reporter: &DownloadProgressReporter<'_>,
) -> Result<Box<dyn GgufLlamaInstance>, EmbeddingError> {
    let requested = if state.using_cpu_fallback {
        GpuSelection::Cpu
    } else {
        provider.gpu
    };
    if requested == GpuSelection::Cpu {
        return load_cpu_compatible_llama(module, state, reporter).await;
    }
    if state.failed_gpu_modes.contains(&requested) {
        let detail = match requested {
            GpuSelection::Auto => String::new(),
            mode => format!(" for device={mode:?}"),
        };
        report_warning(
            reporter,
            &format!("skipping previously failed llama.cpp GPU init{detail}, using CPU."),
        );
        state.using_cpu_fallback = true;
        return load_cpu_compatible_llama(module, state, reporter).await;
    }
    match module.get_llama(requested).await {
        Ok(llama) => Ok(llama),
        Err(error) => {
            state.failed_gpu_modes.insert(requested);
            report_warning(
                reporter,
                &format!("llama.cpp GPU init failed ({error}), falling back to CPU."),
            );
            state.using_cpu_fallback = true;
            load_cpu_compatible_llama(module, state, reporter).await
        }
    }
}

async fn load_cpu_compatible_llama(
    module: &dyn GgufRuntimeModule,
    state: &mut LoadedState,
    reporter: &DownloadProgressReporter<'_>,
) -> Result<Box<dyn GgufLlamaInstance>, EmbeddingError> {
    match module.get_llama(GpuSelection::Cpu).await {
        Ok(llama) => Ok(llama),
        Err(error) => {
            if !state.cpu_fallback_warned {
                state.cpu_fallback_warned = true;
                report_warning(
                    reporter,
                    &format!(
                        "CPU-only llama.cpp backend unavailable ({error}); using packaged backend with GPU model offloading disabled."
                    ),
                );
            }
            module.get_llama(GpuSelection::Auto).await
        }
    }
}

fn effective_parallelism(
    provider: &GgufEmbedding,
    state: &mut LoadedState,
    text_count: usize,
) -> usize {
    let requested = match provider.parallelism_override {
        Some(parallelism) => parallelism.max(1),
        None => resolve_parallelism(provider, state),
    };
    requested.min(text_count.max(1)).max(1)
}

fn resolve_parallelism(provider: &GgufEmbedding, state: &LoadedState) -> usize {
    if offload_disabled(provider, state) {
        return 1;
    }
    let llama = state.llama.as_ref().expect("llama ensured above");
    if !llama.uses_gpu() {
        return 1;
    }
    match llama.vram_free_bytes() {
        Some(free_bytes) => {
            let free_mb = free_bytes as f64 / (1024.0 * 1024.0);
            (free_mb * 0.25 / 150.0).floor().clamp(1.0, DEFAULT_PARALLELISM_CAP as f64) as usize
        }
        None => 2,
    }
}

async fn ensure_contexts(
    provider: &GgufEmbedding,
    state: &mut LoadedState,
    target: usize,
) -> Result<(), EmbeddingError> {
    if state.contexts.len() >= target {
        return Ok(());
    }
    match create_contexts(provider, state, target).await {
        Ok(()) => Ok(()),
        Err(error) => {
            if !can_retry_on_cpu(provider, state) {
                return Err(error);
            }
            eprintln!(
                "guidance warning: llama.cpp GPU embedding context failed ({error}), falling back to CPU."
            );
            state.using_cpu_fallback = true;
            dispose_loaded_runtime(state).await;
            ensure_model(provider, state, None).await?;
            create_contexts(provider, state, target).await
        }
    }
}

async fn create_contexts(
    provider: &GgufEmbedding,
    state: &mut LoadedState,
    target: usize,
) -> Result<(), EmbeddingError> {
    let threads = threads_per_context(provider, state, target);
    let initial = state.contexts.len();
    while state.contexts.len() < target {
        // Disjoint field borrows: the model for creation, the vec for push.
        let context = {
            let model = state.model.as_ref().expect("model ensured above");
            model
                .create_embedding_context(&GgufContextOptions {
                    context_size: provider.spec.context_size,
                    threads,
                })
                .await
        };
        match context {
            Ok(context) => state.contexts.push(context),
            Err(error) => {
                if state.contexts.len() == initial {
                    return Err(error);
                }
                break;
            }
        }
    }
    Ok(())
}

async fn dispose_loaded_runtime(state: &mut LoadedState) {
    for context in state.contexts.drain(..) {
        let _ = context.dispose().await;
    }
    if let Some(model) = state.model.take() {
        let _ = model.dispose().await;
    }
    if let Some(llama) = state.llama.take() {
        // Never hang teardown on a wedged runtime.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), llama.dispose()).await;
    }
    state.module = None;
    // The validated model path survives disposal: a GPU→CPU retry reloads
    // the same snapshot without re-resolving artifacts.
}

async fn resolve_model_path(
    provider: &GgufEmbedding,
    reporter: &DownloadProgressReporter<'_>,
) -> Result<PathBuf, EmbeddingError> {
    let spec = &provider.spec;
    let primary = spec.artifacts.first().ok_or_else(|| {
        EmbeddingError::RequestFailed(format!(
            "llama.cpp catalog entry has no artifacts (model={})",
            spec.reference
        ))
    })?;
    let sources = vec![
        ModelArtifactSource {
            kind: SourceKind::HuggingFace,
            repo: spec.hugging_face.0.to_string(),
            revision: spec.hugging_face.1.to_string(),
            cache_directory: provider.model_cache_dir.clone(),
            local_paths: HashMap::from([(primary.path.clone(), spec.cache_file.to_string())]),
        },
        ModelArtifactSource {
            kind: SourceKind::ModelScope,
            repo: spec.model_scope.0.to_string(),
            revision: spec.model_scope.1.to_string(),
            cache_directory: provider
                .model_cache_dir
                .join("modelscope")
                .join("llama-cpp")
                .join(spec.model_scope.0.replace('/', "--"))
                .join(spec.model_scope.1),
            local_paths: HashMap::new(),
        },
    ];
    let fallback_warned = Mutex::new(false);
    let request = GgufArtifactRequest {
        model: spec.reference,
        sources,
        artifacts: spec.artifacts.clone(),
        on_download_plan: Some(&|artifacts: &[ModelArtifact]| {
            reporter.set_download_plan(artifacts);
        }),
        on_progress: Some(&|progress: ModelArtifactDownloadProgress| {
            reporter.report(&progress.artifact, progress.downloaded_bytes);
        }),
        on_fallback: Some(&|message: &str| {
            if let Ok(mut warned) = fallback_warned.lock() {
                if !*warned {
                    *warned = true;
                    reporter.warning(message);
                }
            }
        }),
    };
    let resolved = provider.dependencies.resolver.resolve(&request).await?;
    resolved.paths.get(&primary.path).map(PathBuf::from).ok_or_else(|| {
        EmbeddingError::RequestFailed(format!(
            "Resolved GGUF artifact path is missing (model={} artifact={})",
            spec.reference, primary.path
        ))
    })
}

// ---------------------------------------------------------------------------
// Pure helpers.
// ---------------------------------------------------------------------------

fn default_model_cache_dir() -> PathBuf {
    if let Ok(dir) = std::env::var(MODEL_CACHE_ENVIRONMENT_VARIABLE) {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    default_home().join("models")
}

fn default_home() -> PathBuf {
    if let Ok(home) = std::env::var("GUIDANCE_HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home);
        }
    }
    #[cfg(unix)]
    if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home).join(".guidance");
        }
    }
    #[cfg(windows)]
    if let Ok(profile) = std::env::var("USERPROFILE") {
        if !profile.trim().is_empty() {
            return PathBuf::from(profile).join(".guidance");
        }
    }
    PathBuf::from(".guidance")
}

fn format_text_for_embedding(text: &str, purpose: EmbedPurpose, format: GgufFormat) -> String {
    match format {
        GgufFormat::Qwen3 => match purpose {
            EmbedPurpose::Query => {
                format!("Instruct: Retrieve relevant documents for the given query\nQuery: {text}")
            }
            EmbedPurpose::Document => text.to_string(),
        },
        GgufFormat::EmbeddingGemma => match purpose {
            EmbedPurpose::Query => format!("task: search result | query: {text}"),
            EmbedPurpose::Document => format!("title: none | text: {text}"),
        },
    }
}

struct TruncatedText {
    text: String,
    truncated: bool,
}

fn truncate_to_context_size(
    model: &dyn GgufModel,
    text: &str,
    context_size: usize,
) -> TruncatedText {
    let untruncated = || TruncatedText {
        text: text.to_string(),
        truncated: false,
    };
    let Some(tokens) = model.tokenize(text) else {
        return untruncated();
    };
    let limit = context_size
        .min(model.train_context_size().unwrap_or(context_size))
        .max(1);
    if tokens.len() <= limit {
        return untruncated();
    }
    let keep = limit.saturating_sub(4).max(1);
    match model.detokenize(&tokens[..keep]) {
        Some(text) => TruncatedText {
            text,
            truncated: true,
        },
        None => untruncated(),
    }
}

fn can_retry_on_cpu(provider: &GgufEmbedding, state: &LoadedState) -> bool {
    provider.gpu != GpuSelection::Cpu && !state.using_cpu_fallback
}

fn offload_disabled(provider: &GgufEmbedding, state: &LoadedState) -> bool {
    provider.gpu == GpuSelection::Cpu || state.using_cpu_fallback
}

fn threads_per_context(
    provider: &GgufEmbedding,
    state: &LoadedState,
    parallelism: usize,
) -> u32 {
    let Some(llama) = state.llama.as_ref() else {
        return 0;
    };
    if !offload_disabled(provider, state) && llama.uses_gpu() {
        return 0;
    }
    if parallelism <= 1 {
        return 0;
    }
    (llama.cpu_math_cores() / parallelism as u32).max(1)
}

fn validate_gguf_file(path: &Path, model_uri: &str) -> Result<(), EmbeddingError> {
    if !path.exists() {
        return Ok(());
    }
    let failed = |message: String| EmbeddingError::RequestFailed(format!("llama.cpp embedding failed: {message}"));
    let bytes = std::fs::read(path).map_err(|error| failed(error.to_string()))?;
    let sniff = &bytes[..bytes.len().min(512)];
    if sniff.len() >= 4 && sniff[..4] == GGUF_MAGIC {
        return Ok(());
    }
    let text = String::from_utf8_lossy(sniff).to_lowercase();
    let is_html = text.contains("<!doctype") || text.contains("<html");
    let size_kb = path.metadata().map_or(0, |metadata| metadata.len() / 1024);
    let _ = std::fs::remove_file(path);
    if is_html {
        return Err(failed(format!(
            "Downloaded local embedding model is HTML, not GGUF (model={model_uri} sizeKB={size_kb})"
        )));
    }
    let got = String::from_utf8_lossy(&sniff[..sniff.len().min(4)]);
    Err(failed(format!(
        "Local embedding model is not a valid GGUF file (model={model_uri} actual={got} sizeKB={size_kb})"
    )))
}

fn report_warning(reporter: &DownloadProgressReporter<'_>, message: &str) {
    if !reporter.warning(message) {
        eprintln!("guidance warning: {message}");
    }
}

// ---------------------------------------------------------------------------
// EmbeddingProvider impl (document purpose; single shared loaded model).
// ---------------------------------------------------------------------------

#[async_trait]
impl EmbeddingProvider for GgufEmbedding {
    fn name(&self) -> &'static str {
        "llama-cpp"
    }

    fn dimensions(&self) -> u32 {
        self.spec.dimension
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let owned = text.to_string();
        Ok(block_on_provider(self.embed_texts(
            std::slice::from_ref(&owned),
            EmbedPurpose::Document,
            None,
        ))?
        .vectors
        .into_iter()
        .next()
        .unwrap_or_default())
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<BatchEmbedding, EmbeddingError> {
        Ok(self.embed_batch_with_truncation(texts)?.0)
    }

    fn embed_batch_with_truncation(
        &self,
        texts: &[&str],
    ) -> Result<(BatchEmbedding, Vec<usize>), EmbeddingError> {
        let owned: Vec<String> = texts.iter().map(ToString::to_string).collect();
        let result = block_on_provider(self.embed_texts(&owned, EmbedPurpose::Document, None))?;
        Ok((
            BatchEmbedding {
                flat: result.vectors.iter().flatten().copied().collect(),
                count: result.vectors.len(),
                dims: self.spec.dimension as usize,
            },
            result.truncated,
        ))
    }
}
