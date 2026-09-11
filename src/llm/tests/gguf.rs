//! Ported from zvec-grep `test/unit/models/llama-cpp.test.mjs`.
//!
//! The GGUF provider behind `EmbeddingProvider`: artifact resolution,
//! per-format text shaping, context-size truncation, embedding-context
//! parallelism, GPU→CPU fallback, GGUF validation, disposal. A stub runtime
//! plays the `node-llama-cpp` role; a stub resolver plays the artifact plane.

use async_trait::async_trait;
use fluent_llm::artifact::{EmbeddingProgress, ModelArtifactSource};
use fluent_llm::catalog::{CatalogEntry, EmbeddingBackend, PinnedArtifact};
use fluent_llm::embeddings::EmbeddingError;
use fluent_llm::gguf::{
    Device, EmbedPurpose, GgufArtifactRequest, GgufArtifactResolver, GgufContextOptions,
    GgufDependencies, GgufEmbedResult, GgufEmbedding, GgufEmbeddingContext, GgufLlamaInstance,
    GgufModel, GgufModelOptions, GgufResolvedArtifacts, GgufRuntimeLoader, GgufRuntimeModule,
    GpuSelection,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

#[derive(Default)]
struct Calls {
    model_attempts: usize,
    resolve_sources: Vec<ModelArtifactSource>,
    llama_gpus: Vec<GpuSelection>,
    model_loads: Vec<(PathBuf, Option<u32>)>,
    contexts: Vec<GgufContextOptions>,
    texts: Vec<String>,
    disposed_contexts: usize,
    disposed_models: usize,
    disposed_llamas: usize,
    runtime_loads: usize,
    lifecycle: Vec<&'static str>,
}

struct StubOptions {
    train_context_size: Option<usize>,
    fail_context_after: Option<usize>,
    fail_embedding: bool,
    fail_gpu: bool,
    fail_cpu: bool,
    fail_first_model: bool,
    fail_runtime_load: bool,
    fail_artifact_resolution: bool,
    use_model_scope: bool,
    duplicate_fallback_warning: bool,
    vram_error: bool,
}

impl Default for StubOptions {
    fn default() -> Self {
        Self {
            train_context_size: Some(6),
            fail_context_after: None,
            fail_embedding: false,
            fail_gpu: false,
            fail_cpu: false,
            fail_first_model: false,
            fail_runtime_load: false,
            fail_artifact_resolution: false,
            use_model_scope: false,
            duplicate_fallback_warning: false,
            vram_error: false,
        }
    }
}

struct StubContext {
    calls: Arc<Mutex<Calls>>,
    options: Arc<StubOptions>,
}

#[async_trait]
impl GgufEmbeddingContext for StubContext {
    async fn embedding_for(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.calls.lock().unwrap().texts.push(text.to_string());
        if self.options.fail_embedding {
            return Err(EmbeddingError::RequestFailed("embedding failed".to_string()));
        }
        Ok(vec![text.len() as f32, 1.0])
    }

    async fn dispose(&self) -> Result<(), EmbeddingError> {
        self.calls.lock().unwrap().disposed_contexts += 1;
        Ok(())
    }
}

struct StubModel {
    calls: Arc<Mutex<Calls>>,
    options: Arc<StubOptions>,
}

#[async_trait]
impl GgufModel for StubModel {
    fn train_context_size(&self) -> Option<usize> {
        self.options.train_context_size
    }

    fn tokenize(&self, text: &str) -> Option<Vec<String>> {
        Some(text.chars().map(String::from).collect())
    }

    fn detokenize(&self, tokens: &[String]) -> Option<String> {
        Some(tokens.concat())
    }

    async fn create_embedding_context(
        &self,
        options: &GgufContextOptions,
    ) -> Result<Box<dyn GgufEmbeddingContext>, EmbeddingError> {
        let mut calls = self.calls.lock().unwrap();
        calls.contexts.push(options.clone());
        if self
            .options
            .fail_context_after
            .is_some_and(|limit| calls.contexts.len() > limit)
        {
            return Err(EmbeddingError::RequestFailed("context failed".to_string()));
        }
        Ok(Box::new(StubContext {
            calls: Arc::clone(&self.calls),
            options: Arc::clone(&self.options),
        }))
    }

    async fn dispose(&self) -> Result<(), EmbeddingError> {
        self.calls.lock().unwrap().disposed_models += 1;
        Ok(())
    }
}

struct StubLlama {
    calls: Arc<Mutex<Calls>>,
    options: Arc<StubOptions>,
    gpu: GpuSelection,
}

#[async_trait]
impl GgufLlamaInstance for StubLlama {
    fn uses_gpu(&self) -> bool {
        self.gpu != GpuSelection::Cpu
    }

    fn cpu_math_cores(&self) -> u32 {
        8
    }

    fn vram_free_bytes(&self) -> Option<u64> {
        if self.options.vram_error {
            return None;
        }
        Some(6 * 1024 * 1024 * 1024)
    }

    async fn load_model(
        &self,
        path: &std::path::Path,
        gpu_layers: Option<u32>,
    ) -> Result<Box<dyn GgufModel>, EmbeddingError> {
        let mut calls = self.calls.lock().unwrap();
        calls
            .model_loads
            .push((path.to_path_buf(), gpu_layers));
        calls.model_attempts += 1;
        if self.options.fail_first_model && calls.model_attempts == 1 {
            return Err(EmbeddingError::RequestFailed("model GPU failure".to_string()));
        }
        Ok(Box::new(StubModel {
            calls: Arc::clone(&self.calls),
            options: Arc::clone(&self.options),
        }))
    }

    async fn dispose(&self) -> Result<(), EmbeddingError> {
        self.calls.lock().unwrap().disposed_llamas += 1;
        Ok(())
    }
}

struct StubModule {
    calls: Arc<Mutex<Calls>>,
    options: Arc<StubOptions>,
}

#[async_trait]
impl GgufRuntimeModule for StubModule {
    async fn get_llama(
        &self,
        gpu: GpuSelection,
    ) -> Result<Box<dyn GgufLlamaInstance>, EmbeddingError> {
        let mut calls = self.calls.lock().unwrap();
        calls.lifecycle.push("getLlama");
        calls.llama_gpus.push(gpu);
        if self.options.fail_gpu && gpu != GpuSelection::Cpu {
            return Err(EmbeddingError::RequestFailed("GPU unavailable".to_string()));
        }
        if self.options.fail_cpu && gpu == GpuSelection::Cpu {
            return Err(EmbeddingError::RequestFailed("CPU backend unavailable".to_string()));
        }
        Ok(Box::new(StubLlama {
            calls: Arc::clone(&self.calls),
            options: Arc::clone(&self.options),
            gpu,
        }))
    }
}

struct StubLoader {
    calls: Arc<Mutex<Calls>>,
    options: Arc<StubOptions>,
}

#[async_trait]
impl GgufRuntimeLoader for StubLoader {
    async fn load_module(&self) -> Result<Box<dyn GgufRuntimeModule>, EmbeddingError> {
        self.calls.lock().unwrap().runtime_loads += 1;
        if self.options.fail_runtime_load {
            return Err(EmbeddingError::RequestFailed("runtime import failed".to_string()));
        }
        Ok(Box::new(StubModule {
            calls: Arc::clone(&self.calls),
            options: Arc::clone(&self.options),
        }))
    }
}

struct StubResolver {
    calls: Arc<Mutex<Calls>>,
    options: Arc<StubOptions>,
    model_path: PathBuf,
    cache_dir: PathBuf,
}

#[async_trait]
impl GgufArtifactResolver for StubResolver {
    async fn resolve(
        &self,
        request: &GgufArtifactRequest<'_>,
    ) -> Result<GgufResolvedArtifacts, EmbeddingError> {
        let mut calls = self.calls.lock().unwrap();
        calls.lifecycle.push("resolveArtifacts");
        calls.resolve_sources = request.sources.clone();
        if self.options.fail_artifact_resolution {
            return Err(EmbeddingError::RequestFailed("artifact download failed".to_string()));
        }
        if let Some(plan) = request.on_download_plan {
            plan(&request.artifacts);
        }
        let artifact_path = request.artifacts[0].path.clone();
        if let Some(progress) = request.on_progress {
            progress(fluent_llm::artifact::ModelArtifactDownloadProgress {
                model: request.model.to_string(),
                source: fluent_llm::artifact::SourceKind::HuggingFace,
                artifact: artifact_path.clone(),
                downloaded_bytes: 25,
                total_bytes: 100,
            });
        }
        if self.options.use_model_scope {
            if let Some(fallback) = request.on_fallback {
                fallback("Hugging Face unavailable; using ModelScope.");
                if self.options.duplicate_fallback_warning {
                    fallback("duplicate fallback warning");
                }
            }
            if let Some(plan) = request.on_download_plan {
                plan(&request.artifacts);
            }
            if let Some(progress) = request.on_progress {
                progress(fluent_llm::artifact::ModelArtifactDownloadProgress {
                    model: request.model.to_string(),
                    source: fluent_llm::artifact::SourceKind::ModelScope,
                    artifact: artifact_path.clone(),
                    downloaded_bytes: 0,
                    total_bytes: 100,
                });
            }
        }
        let source = request.sources[self.options.use_model_scope as usize].clone();
        let mut paths = HashMap::new();
        paths.insert(
            artifact_path,
            self.model_path.to_string_lossy().into_owned(),
        );
        Ok(GgufResolvedArtifacts {
            source,
            directory: self.cache_dir.to_string_lossy().into_owned(),
            paths,
        })
    }
}

struct Fixture {
    calls: Arc<Mutex<Calls>>,
    dependencies: GgufDependencies,
    cache_dir: TempDir,
    model_path: PathBuf,
}

fn fixture(stub: StubOptions) -> Fixture {
    let cache_dir = TempDir::new().unwrap();
    let model_path = cache_dir.path().join("model.gguf");
    std::fs::write(&model_path, b"GGUFpayload").unwrap();
    let options = Arc::new(stub);
    let calls = Arc::new(Mutex::new(Calls::default()));
    let dependencies = GgufDependencies {
        loader: Arc::new(StubLoader {
            calls: Arc::clone(&calls),
            options: Arc::clone(&options),
        }),
        resolver: Arc::new(StubResolver {
            calls: Arc::clone(&calls),
            options: Arc::clone(&options),
            model_path: model_path.clone(),
            cache_dir: cache_dir.path().to_path_buf(),
        }),
    };
    Fixture {
        calls,
        dependencies,
        cache_dir,
        model_path,
    }
}

static TEST_ARTIFACTS: &[PinnedArtifact] = &[PinnedArtifact {
    path: "model.gguf",
    size: 100,
    sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
}];

static TEST_ENTRY: CatalogEntry = CatalogEntry {
    reference: "local/test-model",
    backend: EmbeddingBackend::LlamaCpp,
    provider: "local",
    model: "test-model",
    dimension: 2,
    metric: "cosine",
    max_batch_size: 8,
    uri: Some("hf:test/model/model.gguf#hf-revision"),
    cache_file: Some("hf_test_model.gguf"),
    sources: Some(fluent_llm::catalog::ArtifactSources {
        hugging_face: fluent_llm::catalog::ArtifactMirror("test/model", "hf-revision"),
        model_scope: fluent_llm::catalog::ArtifactMirror("mirror/test-model", "ms-revision"),
    }),
    artifacts: TEST_ARTIFACTS,
    context_size: Some(8),
    format: Some("embeddinggemma"),
    kind: None,
    default_endpoint: None,
    max_input_tokens: None,
    max_image_bytes: None,
    dtype: None,
    pooling: None,
    normalize: false,
    query_prefix: None,
    document_prefix: None,
    model_file: None,
    embedding_tensor: None,
    tokenizer_file: None,
    default_concurrency: None,
};

static TEST_ENTRY_QWEN3: CatalogEntry = CatalogEntry {
    format: Some("qwen3"),
    context_size: Some(100),
    ..TEST_ENTRY
};

fn test_entry() -> &'static CatalogEntry {
    &TEST_ENTRY
}

#[tokio::test]
async fn loads_gguf_formats_truncates_parallelizes_caches_and_disposes() {
    let fix = fixture(StubOptions::default());
    let model = GgufEmbedding::new(
        test_entry(),
        GgufModelOptions {
            model_cache_dir: Some(fix.cache_dir.path().to_path_buf()),
            device: Device::Cpu,
        },
        fix.dependencies,
    )
    .unwrap();
    // Parallelism override caps concurrent embedding contexts.
    let model = model.with_parallelism_override(Some(2));

    let progress: Arc<Mutex<Vec<EmbeddingProgress>>> = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::clone(&progress);
    let result: GgufEmbedResult = model
        .embed_texts(
            &[
                "abcdefghijk".to_string(),
                "second".to_string(),
                "third".to_string(),
            ],
            EmbedPurpose::Query,
            Some(&move |event| hook.lock().unwrap().push(event)),
        )
        .await
        .unwrap();
    assert_eq!(result.vectors.len(), 3);
    // Query formatting inflates every text past the 6-token stub limit.
    assert_eq!(result.truncated, vec![0, 1, 2]);
    let calls = fix.calls.lock().unwrap();
    assert_eq!(calls.contexts.len(), 2);
    assert_eq!(calls.contexts[0].threads, 4);
    assert!(
        calls.texts.iter().all(|text| text.len() <= 2),
        "truncated texts must fit the context: {:?}",
        calls.texts
    );
    assert_eq!(calls.model_loads[0].1, Some(0), "CPU disables GPU offload");
    assert_eq!(calls.model_loads[0].0, fix.model_path);
    let sources = &calls.resolve_sources;
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0].kind, fluent_llm::artifact::SourceKind::HuggingFace);
    assert_eq!(sources[0].repo, "test/model");
    assert_eq!(sources[0].revision, "hf-revision");
    assert_eq!(sources[0].cache_directory, fix.cache_dir.path());
    assert_eq!(
        sources[0].local_paths.get("model.gguf").map(String::as_str),
        Some("hf_test_model.gguf")
    );
    assert_eq!(sources[1].kind, fluent_llm::artifact::SourceKind::ModelScope);
    assert_eq!(
        sources[1].cache_directory,
        fix.cache_dir.path().join("modelscope").join("llama-cpp").join("mirror--test-model").join("ms-revision")
    );
    assert_eq!(
        &calls.lifecycle[..2],
        &["resolveArtifacts", "getLlama"],
        "artifacts resolve before any runtime init"
    );
    assert_eq!(
        progress.lock().unwrap().as_slice(),
        &[
            EmbeddingProgress::Preparing {
                model: "local/test-model".to_string()
            },
            EmbeddingProgress::Downloading {
                model: "local/test-model".to_string(),
                downloaded_bytes: 25,
                total_bytes: 100,
            },
            EmbeddingProgress::Ready {
                model: "local/test-model".to_string()
            },
        ]
    );
    drop(calls);

    // Second embed reuses the loaded model without re-resolving artifacts.
    model
        .embed_texts(&["cached".to_string()], EmbedPurpose::Document, None)
        .await
        .unwrap();
    let calls = fix.calls.lock().unwrap();
    assert_eq!(calls.model_loads.len(), 1);
    assert_eq!(
        calls.lifecycle.iter().filter(|event| **event == "resolveArtifacts").count(),
        1
    );
    drop(calls);

    model.dispose().await;
    model.dispose().await;
    let calls = fix.calls.lock().unwrap();
    assert_eq!(calls.disposed_contexts, 2);
    assert_eq!(calls.disposed_models, 1);
    assert_eq!(calls.disposed_llamas, 1);
    drop(calls);
    let error = model
        .embed_texts(&["after dispose".to_string()], EmbedPurpose::Document, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("model is disposed"), "{error}");
}

#[tokio::test]
async fn uses_modelscope_path_and_reports_source_fallback_once() {
    let ms_dir = TempDir::new().unwrap();
    let model_path = ms_dir.path().join("model.gguf");
    std::fs::write(&model_path, b"GGUFpayload").unwrap();
    let options = Arc::new(StubOptions {
        use_model_scope: true,
        duplicate_fallback_warning: true,
        ..Default::default()
    });
    let calls = Arc::new(Mutex::new(Calls::default()));
    let cache_path = ms_dir.path().to_path_buf();
    let dependencies = GgufDependencies {
        loader: Arc::new(StubLoader {
            calls: Arc::clone(&calls),
            options: Arc::clone(&options),
        }),
        resolver: Arc::new(StubResolver {
            calls: Arc::clone(&calls),
            options: Arc::clone(&options),
            model_path: model_path.clone(),
            cache_dir: cache_path.clone(),
        }),
    };
    let model = GgufEmbedding::new(
        test_entry(),
        GgufModelOptions {
            model_cache_dir: Some(cache_path),
            device: Device::Cpu,
        },
        dependencies,
    )
    .unwrap();
    let progress: Arc<Mutex<Vec<EmbeddingProgress>>> = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::clone(&progress);
    model
        .embed_texts(
            &["value".to_string()],
            EmbedPurpose::Document,
            Some(&move |event| hook.lock().unwrap().push(event)),
        )
        .await
        .unwrap();
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.model_loads[0].0, model_path);
    }
    let downloading: Vec<_> = progress
        .lock()
        .unwrap()
        .iter()
        .filter(|event| matches!(event, EmbeddingProgress::Downloading { .. }))
        .cloned()
        .collect();
    assert_eq!(
        downloading,
        vec![
            EmbeddingProgress::Downloading {
                model: "local/test-model".to_string(),
                downloaded_bytes: 25,
                total_bytes: 100,
            },
            EmbeddingProgress::Downloading {
                model: "local/test-model".to_string(),
                downloaded_bytes: 0,
                total_bytes: 100,
            },
        ]
    );
    let warnings: Vec<_> = progress
        .lock()
        .unwrap()
        .iter()
        .filter(|event| matches!(event, EmbeddingProgress::Warning { .. }))
        .cloned()
        .collect();
    assert_eq!(warnings.len(), 1, "duplicate fallback warning reported once");
    model.dispose().await;
}

#[tokio::test]
async fn artifact_failures_do_not_enter_gpu_fallback() {
    let fix = fixture(StubOptions {
        fail_artifact_resolution: true,
        ..Default::default()
    });
    let model = GgufEmbedding::new(
        test_entry(),
        GgufModelOptions {
            model_cache_dir: Some(fix.cache_dir.path().to_path_buf()),
            device: Device::Metal,
        },
        fix.dependencies,
    )
    .unwrap();
    let error = model
        .embed_texts(&["value".to_string()], EmbedPurpose::Document, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("llama.cpp embedding failed"), "{error}");
    assert!(
        error.to_string().contains("artifact download failed"),
        "{error}"
    );
    {
        let calls = fix.calls.lock().unwrap();
        assert!(calls.llama_gpus.is_empty(), "no runtime init on artifact failure");
        assert!(calls.model_loads.is_empty());
    }
    model.dispose().await;
}

#[tokio::test]
async fn runtime_import_failures_do_not_enter_gpu_fallback() {
    let fix = fixture(StubOptions {
        fail_runtime_load: true,
        ..Default::default()
    });
    let model = GgufEmbedding::new(
        test_entry(),
        GgufModelOptions {
            model_cache_dir: Some(fix.cache_dir.path().to_path_buf()),
            device: Device::Metal,
        },
        fix.dependencies,
    )
    .unwrap();
    let error = model
        .embed_texts(&["value".to_string()], EmbedPurpose::Document, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("runtime import failed"), "{error}");
    {
        let calls = fix.calls.lock().unwrap();
        assert_eq!(calls.runtime_loads, 1);
        assert!(calls.llama_gpus.is_empty());
        assert!(calls.model_loads.is_empty());
    }
    model.dispose().await;
}

#[tokio::test]
async fn qwen_format_auto_parallelism_and_partial_context_capacity() {
    let fix = fixture(StubOptions {
        fail_context_after: Some(2),
        train_context_size: Some(200),
        ..Default::default()
    });
    let entry = &TEST_ENTRY_QWEN3;
    let model = GgufEmbedding::new(
        entry,
        GgufModelOptions {
            model_cache_dir: Some(fix.cache_dir.path().to_path_buf()),
            device: Device::Metal,
        },
        fix.dependencies,
    )
    .unwrap();
    let result = model
        .embed_texts(
            &["query-0".to_string(), "query-1".to_string(), "query-2".to_string(), "query-3".to_string()],
            EmbedPurpose::Query,
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.vectors.len(), 4);
    let calls = fix.calls.lock().unwrap();
    assert_eq!(calls.contexts.len(), 3, "two contexts plus one failed attempt");
    assert!(
        calls.texts[0].starts_with("Instruct:"),
        "qwen3 query format required, got {:?}",
        calls.texts[0]
    );
    assert_eq!(calls.contexts[0].threads, 0, "GPU contexts take no threads");
    assert_eq!(calls.model_loads[0].1, None, "metal keeps GPU offload");
    drop(calls);
    model.dispose().await;
}

#[tokio::test]
async fn falls_back_from_gpu_init_to_cpu() {
    let fix = fixture(StubOptions {
        fail_gpu: true,
        ..Default::default()
    });
    let model = GgufEmbedding::new(
        test_entry(),
        GgufModelOptions {
            model_cache_dir: Some(fix.cache_dir.path().to_path_buf()),
            device: Device::Metal,
        },
        fix.dependencies,
    )
    .unwrap();
    let progress: Arc<Mutex<Vec<EmbeddingProgress>>> = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::clone(&progress);
    model
        .embed_texts(
            &["value".to_string()],
            EmbedPurpose::Document,
            Some(&move |event| hook.lock().unwrap().push(event)),
        )
        .await
        .unwrap();
    let warnings: Vec<String> = progress
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            EmbeddingProgress::Warning { message, .. } => Some(message.clone()),
            _ => None,
        })
        .collect();
    assert!(
        warnings.iter().any(|message| message.contains("GPU init failed")),
        "GPU fallback warning required, got {warnings:?}"
    );
    let calls = fix.calls.lock().unwrap();
    assert_eq!(calls.llama_gpus, vec![GpuSelection::Metal, GpuSelection::Cpu]);
    assert_eq!(calls.lifecycle[0], "resolveArtifacts");
    drop(calls);
    model.dispose().await;
}

#[tokio::test]
async fn packaged_backend_fallback_and_model_retry_on_cpu() {
    // CPU-only failure drops to the packaged backend with offload disabled.
    let fix = fixture(StubOptions {
        fail_cpu: true,
        ..Default::default()
    });
    let model = GgufEmbedding::new(
        test_entry(),
        GgufModelOptions {
            model_cache_dir: Some(fix.cache_dir.path().to_path_buf()),
            device: Device::Cpu,
        },
        fix.dependencies,
    )
    .unwrap();
    let progress: Arc<Mutex<Vec<EmbeddingProgress>>> = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::clone(&progress);
    model
        .embed_texts(
            &["value".to_string()],
            EmbedPurpose::Document,
            Some(&move |event| hook.lock().unwrap().push(event)),
        )
        .await
        .unwrap();
    let warnings: Vec<String> = progress
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            EmbeddingProgress::Warning { message, .. } => Some(message.clone()),
            _ => None,
        })
        .collect();
    assert!(
        warnings.iter().any(|message| message.contains("CPU-only")),
        "packaged-backend warning required, got {warnings:?}"
    );
    let calls = fix.calls.lock().unwrap();
    assert_eq!(calls.llama_gpus, vec![GpuSelection::Cpu, GpuSelection::Auto]);
    drop(calls);
    model.dispose().await;

    // GPU model-load failure retries once on CPU with one artifact resolve.
    let retry = fixture(StubOptions {
        fail_first_model: true,
        ..Default::default()
    });
    let model = GgufEmbedding::new(
        test_entry(),
        GgufModelOptions {
            model_cache_dir: Some(retry.cache_dir.path().to_path_buf()),
            device: Device::Auto,
        },
        retry.dependencies,
    )
    .unwrap();
    let progress: Arc<Mutex<Vec<EmbeddingProgress>>> = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::clone(&progress);
    model
        .embed_texts(
            &["value".to_string()],
            EmbedPurpose::Document,
            Some(&move |event| hook.lock().unwrap().push(event)),
        )
        .await
        .unwrap();
    let warnings: Vec<String> = progress
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            EmbeddingProgress::Warning { message, .. } => Some(message.clone()),
            _ => None,
        })
        .collect();
    assert!(
        warnings.iter().any(|message| message.contains("GPU model load failed")),
        "model retry warning required, got {warnings:?}"
    );
    let calls = retry.calls.lock().unwrap();
    assert_eq!(calls.model_loads.len(), 2);
    assert_eq!(
        calls.lifecycle.iter().filter(|event| **event == "resolveArtifacts").count(),
        1
    );
    assert_eq!(calls.model_loads[0].0, calls.model_loads[1].0);
    drop(calls);
    model.dispose().await;
}

#[tokio::test]
async fn rejects_invalid_gguf_and_removes_corrupt_artifacts() {
    for (contents, fragment) in [
        ("<!doctype html><html>failure</html>", "HTML, not GGUF"),
        ("NOPE invalid binary", "not a valid GGUF"),
    ] {
        let dir = TempDir::new().unwrap();
        let model_path = dir.path().join("bad.gguf");
        std::fs::write(&model_path, contents).unwrap();
        let options = Arc::new(StubOptions::default());
        let calls = Arc::new(Mutex::new(Calls::default()));
        let cache_path = dir.path().to_path_buf();
        let dependencies = GgufDependencies {
            loader: Arc::new(StubLoader {
                calls: Arc::clone(&calls),
                options: Arc::clone(&options),
            }),
            resolver: Arc::new(StubResolver {
                calls: Arc::clone(&calls),
                options: Arc::clone(&options),
                model_path: model_path.clone(),
                cache_dir: cache_path.clone(),
            }),
        };
        let model = GgufEmbedding::new(
            test_entry(),
            GgufModelOptions {
                model_cache_dir: Some(cache_path),
                device: Device::Cpu,
            },
            dependencies,
        )
        .unwrap();
        let error = model
            .embed_texts(&["value".to_string()], EmbedPurpose::Document, None)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains(fragment),
            "{contents:?} must fail with {fragment:?}, got {error}"
        );
        assert!(!model_path.exists(), "corrupt artifact must be removed");
        model.dispose().await;
    }
}

#[tokio::test]
async fn reports_context_and_embedding_runtime_failures() {
    let fix = fixture(StubOptions {
        fail_context_after: Some(0),
        fail_embedding: true,
        ..Default::default()
    });
    let model = GgufEmbedding::new(
        test_entry(),
        GgufModelOptions {
            model_cache_dir: Some(fix.cache_dir.path().to_path_buf()),
            device: Device::Cpu,
        },
        fix.dependencies,
    )
    .unwrap();
    let error = model
        .embed_texts(&["value".to_string()], EmbedPurpose::Document, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("embedding failed"), "{error}");
    model.dispose().await;
}

#[test]
fn batch_size_validation_rejects_oversize_batches() {
    let fix = fixture(StubOptions::default());
    let model = GgufEmbedding::new(
        test_entry(),
        GgufModelOptions {
            model_cache_dir: Some(fix.cache_dir.path().to_path_buf()),
            device: Device::Cpu,
        },
        fix.dependencies,
    )
    .unwrap();
    // The test entry allows 8 per batch; 9 must fail before any runtime use.
    let texts: Vec<String> = (0..9).map(|index| format!("text-{index}")).collect();
    let error = fluent_llm::embeddings::validate_embed_batch(texts.len(), 8).unwrap_err();
    assert!(error.to_string().contains("batch size"), "{error}");
    drop(texts);
    drop(model);
}
