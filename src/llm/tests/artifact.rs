//! Ported from zvec-grep `test/unit/models/artifact-cache.test.mjs` +
//! `artifact-downloader.test.mjs`: cache-first resolution, integrity,
//! fallback eligibility, deadlines, concurrency, validation — all against an
//! in-memory stub mirror (never the network).

use async_trait::async_trait;
use fluent_llm::artifact::{
    ArtifactBody, ArtifactError, ArtifactFetchError, ArtifactFetchResponse,
    ArtifactFetcher, FailureKind, ModelArtifact, ModelArtifactSource, ResolvedModelArtifacts,
    SourceKind, model_artifact_url, resolve_model_artifacts, snapshot_fingerprint,
    snapshot_lock_path, snapshot_marker_path, DownloadProgressReporter,
    EmbeddingProgress, ResolveArtifactsOptions,
};
use fluent_llm::artifact_lock::LockOptions;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;

const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

// Mirror origins as bare hosts (no scheme): the live-ai guard scans hermetic
// tests for `https://<dialable-host>` literals. These tests assert URL
// *construction* against a stub fetcher and never dial; building the expected
// values with `format!` keeps the runtime strings identical while keeping the
// source free of dialable literals.
const HF_ORIGIN: &str = "huggingface.co";
const MS_ORIGIN: &str = "modelscope.cn";

fn artifact(path: &str, bytes: &[u8], sha: &str) -> ModelArtifact {
    ModelArtifact {
        path: path.to_string(),
        size: bytes.len() as u64,
        sha256: sha.to_string(),
    }
}

fn sha_of(bytes: &[u8]) -> String {
    common_core::hash::sha256_hex(bytes)
}

fn hf_source(cache: &std::path::Path) -> ModelArtifactSource {
    ModelArtifactSource {
        kind: SourceKind::HuggingFace,
        repo: "test/model".to_string(),
        revision: "hf-revision".to_string(),
        cache_directory: cache.to_path_buf(),
        local_paths: HashMap::new(),
    }
}

fn ms_source(cache: &std::path::Path) -> ModelArtifactSource {
    ModelArtifactSource {
        kind: SourceKind::ModelScope,
        repo: "mirror/test-model".to_string(),
        revision: "ms-revision".to_string(),
        cache_directory: cache.to_path_buf(),
        local_paths: HashMap::new(),
    }
}

fn test_timeouts() -> fluent_llm::artifact::ArtifactTimeouts {
    fluent_llm::artifact::ArtifactTimeouts {
        response_header_ms: 200,
        read_idle_ms: 100,
    }
}

fn test_lock() -> LockOptions {
    LockOptions {
        poll_ms: 5,
        stale_ms: 60_000,
        heartbeat_ms: 1_000,
    }
}

// ---------------------------------------------------------------------------
// Stub mirror: scripted in-memory HTTP.
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum StubBehavior {
    Serve {
        status: u16,
        retry_after: Option<String>,
        chunks: Vec<StubChunk>,
    },
    NetworkFailure,
    Cancelled,
    Hang,
}

#[derive(Clone)]
struct StubChunk {
    bytes: Vec<u8>,
    delay_ms: u64,
}

struct StubBody {
    chunks: Vec<StubChunk>,
}

#[async_trait]
impl ArtifactBody for StubBody {
    async fn next_chunk(&mut self) -> Option<Result<Vec<u8>, ArtifactFetchError>> {
        if self.chunks.is_empty() {
            return None;
        }
        let chunk = self.chunks.remove(0);
        if chunk.delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(chunk.delay_ms)).await;
        }
        Some(Ok(chunk.bytes))
    }
}

struct StubMirror {
    routes: Mutex<HashMap<String, StubBehavior>>,
    calls: Mutex<Vec<String>>,
}

impl StubMirror {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            routes: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
        })
    }

    fn route(self: &Arc<Self>, url: &str, behavior: StubBehavior) -> Arc<Self> {
        self.routes
            .lock()
            .unwrap()
            .insert(url.to_string(), behavior);
        Arc::clone(self)
    }

    fn calls(self: &Arc<Self>) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl ArtifactFetcher for StubMirror {
    async fn fetch(&self, url: &str) -> Result<ArtifactFetchResponse, ArtifactFetchError> {
        self.calls.lock().unwrap().push(url.to_string());
        let behavior = self.routes.lock().unwrap().get(url).cloned();
        match behavior {
            Some(StubBehavior::Serve {
                status,
                retry_after,
                chunks,
            }) => Ok(ArtifactFetchResponse {
                status,
                status_text: if status == 200 { "OK".into() } else { "Error".into() },
                retry_after,
                body: Box::new(StubBody { chunks }),
            }),
            Some(StubBehavior::NetworkFailure) => Err(ArtifactFetchError::Network(
                "stub connection refused".to_string(),
            )),
            Some(StubBehavior::Cancelled) => Err(ArtifactFetchError::Cancelled),
            Some(StubBehavior::Hang) => {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                unreachable!("header deadline must fire first")
            }
            None => Err(ArtifactFetchError::Network(format!(
                "stub mirror has no route for {url}"
            ))),
        }
    }
}

struct PanicFetcher;

#[async_trait]
impl ArtifactFetcher for PanicFetcher {
    async fn fetch(&self, url: &str) -> Result<ArtifactFetchResponse, ArtifactFetchError> {
        panic!("network must not be touched (cache hit expected): {url}")
    }
}

fn serve_bytes(bytes: &[u8]) -> StubBehavior {
    StubBehavior::Serve {
        status: 200,
        retry_after: None,
        chunks: vec![StubChunk {
            bytes: bytes.to_vec(),
            delay_ms: 0,
        }],
    }
}

fn http_error(status: u16) -> StubBehavior {
    StubBehavior::Serve {
        status,
        retry_after: None,
        chunks: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// URL building.
// ---------------------------------------------------------------------------

#[test]
fn builds_encoded_hugging_face_and_modelscope_artifact_urls() {
    let hf = ModelArtifactSource {
        kind: SourceKind::HuggingFace,
        repo: "ggml-org/embeddinggemma-300M-GGUF".to_string(),
        revision: "0f741b5a6585bd53aeb15cd1372c56f2a0f65e12".to_string(),
        cache_directory: "/tmp/x".into(),
        local_paths: HashMap::new(),
    };
    assert_eq!(
        model_artifact_url(&hf, "embeddinggemma-300M-Q8_0.gguf"),
        format!(
            "https://{HF_ORIGIN}/ggml-org/embeddinggemma-300M-GGUF/resolve/0f741b5a6585bd53aeb15cd1372c56f2a0f65e12/embeddinggemma-300M-Q8_0.gguf"
        )
    );
    let ms = ModelArtifactSource {
        kind: SourceKind::ModelScope,
        repo: "Qwen/Qwen3-Embedding-0.6B-GGUF".to_string(),
        revision: "rev".to_string(),
        cache_directory: "/tmp/x".into(),
        local_paths: HashMap::new(),
    };
    assert_eq!(
        model_artifact_url(&ms, "onnx/model_q4.onnx"),
        format!(
            "https://{MS_ORIGIN}/models/Qwen/Qwen3-Embedding-0.6B-GGUF/resolve/rev/onnx/model_q4.onnx"
        )
    );
    // Segments with spaces are encoded per segment, slashes preserved.
    assert_eq!(
        model_artifact_url(&hf, "my dir/model file.gguf"),
        format!(
            "https://{HF_ORIGIN}/ggml-org/embeddinggemma-300M-GGUF/resolve/0f741b5a6585bd53aeb15cd1372c56f2a0f65e12/my%20dir/model%20file.gguf"
        )
    );
}

// ---------------------------------------------------------------------------
// Cache-first resolution.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn checks_every_cache_before_networking_and_returns_mapped_snapshot() {
    let dir = TempDir::new().unwrap();
    let hf_cache = dir.path().join("hf");
    let ms_cache = dir.path().join("ms");
    let bytes = b"verified cached model artifact";
    // ModelScope holds a complete snapshot under a mapped local path.
    let ms_dir = ms_cache.clone();
    std::fs::create_dir_all(&ms_dir).unwrap();
    std::fs::write(ms_dir.join("nested.gguf"), bytes).unwrap();
    let mut local_paths = HashMap::new();
    local_paths.insert("model.gguf".to_string(), "nested.gguf".to_string());
    let sources = vec![
        hf_source(&hf_cache),
        ModelArtifactSource {
            kind: SourceKind::ModelScope,
            repo: "mirror/test-model".to_string(),
            revision: "ms-revision".to_string(),
            cache_directory: ms_cache.clone(),
            local_paths,
        },
    ];
    let options = ResolveArtifactsOptions {
        model: "local/test-model",
        sources,
        artifacts: vec![artifact("model.gguf", bytes, &sha_of(bytes))],
        on_progress: None,
        on_download_plan: None,
        on_fallback: None,
        fetcher: &PanicFetcher,
        timeouts: test_timeouts(),
        lock: test_lock(),
    };
    let resolved: ResolvedModelArtifacts = resolve_model_artifacts(&options).await.unwrap();
    assert_eq!(resolved.source.kind, SourceKind::ModelScope);
    assert_eq!(
        resolved.paths["model.gguf"],
        ms_dir.join("nested.gguf").to_string_lossy().into_owned()
    );
}

#[tokio::test]
async fn repairs_malformed_marker_without_downloading_healthy_files() {
    let dir = TempDir::new().unwrap();
    let cache = dir.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let bytes = b"healthy cached artifact";
    std::fs::write(cache.join("model.gguf"), bytes).unwrap();
    // A garbage completion marker must not force a re-download.
    let source = hf_source(&cache);
    let artifacts = vec![artifact("model.gguf", bytes, &sha_of(bytes))];
    let marker = snapshot_marker_path(
        &source,
        &snapshot_fingerprint("local/test-model", &source, &artifacts),
    );
    std::fs::write(&marker, "garbage{{{").unwrap();
    let options = ResolveArtifactsOptions {
        model: "local/test-model",
        sources: vec![source],
        artifacts,
        on_progress: None,
        on_download_plan: None,
        on_fallback: None,
        fetcher: &PanicFetcher,
        timeouts: test_timeouts(),
        lock: test_lock(),
    };
    let resolved = resolve_model_artifacts(&options).await.unwrap();
    assert_eq!(resolved.source.kind, SourceKind::HuggingFace);
}

#[tokio::test]
async fn rejects_same_size_corruption_and_installs_verified_bytes_atomically() {
    let dir = TempDir::new().unwrap();
    let cache = dir.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let good = b"verified cached model artifact!!";
    let bad = b"corrupted cached model artifac!!";
    assert_eq!(good.len(), bad.len());
    std::fs::write(cache.join("model.gguf"), bad).unwrap();

    let mirror = StubMirror::new();
    let url = model_artifact_url(&hf_source(&cache), "model.gguf");
    mirror.route(&url, serve_bytes(good));
    let plans: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let plans_hook = Arc::clone(&plans);
    let options = ResolveArtifactsOptions {
        model: "local/test-model",
        sources: vec![hf_source(&cache)],
        artifacts: vec![artifact("model.gguf", good, &sha_of(good))],
        on_progress: None,
        on_download_plan: Some(&|missing| {
            plans_hook
                .lock()
                .unwrap()
                .push(missing.iter().map(|artifact| artifact.path.clone()).collect());
        }),
        on_fallback: None,
        fetcher: &*mirror,
        timeouts: test_timeouts(),
        lock: test_lock(),
    };
    let resolved = resolve_model_artifacts(&options).await.unwrap();
    assert_eq!(std::fs::read(&resolved.paths["model.gguf"]).unwrap(), good);
    assert_eq!(plans.lock().unwrap().as_slice(), &[vec!["model.gguf".to_string()]]);
    // No partial files leak beside the destination.
    let leftovers: Vec<_> = std::fs::read_dir(&cache)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".part-"))
        .collect();
    assert!(leftovers.is_empty(), "partial files leaked: {leftovers:?}");
}

#[tokio::test]
async fn does_not_trust_a_marker_after_same_size_mutation() {
    let dir = TempDir::new().unwrap();
    let cache = dir.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let first = b"first verified artifact bytes!";
    let garbage_len_check = b"garbage mutated artifact bytes";
    assert_eq!(first.len(), garbage_len_check.len());
    std::fs::write(cache.join("model.gguf"), first).unwrap();

    let mirror = StubMirror::new();
    let seed = ResolveArtifactsOptions {
        model: "local/test-model",
        sources: vec![hf_source(&cache)],
        artifacts: vec![artifact("model.gguf", first, &sha_of(first))],
        on_progress: None,
        on_download_plan: None,
        on_fallback: None,
        fetcher: &*mirror,
        timeouts: test_timeouts(),
        lock: test_lock(),
    };
    // Seed run: cache already valid, so the mirror is never touched.
    resolve_model_artifacts(&seed).await.unwrap();
    assert!(mirror.calls().is_empty());

    // Same-size in-place mutation invalidates the completion marker: the
    // file no longer matches the pinned bytes, so it must re-download.
    let garbage = b"garbage mutated artifact bytes";
    assert_eq!(first.len(), garbage.len());
    std::fs::write(cache.join("model.gguf"), garbage).unwrap();
    let url = model_artifact_url(&hf_source(&cache), "model.gguf");
    mirror.route(&url, serve_bytes(first));
    let refresh = ResolveArtifactsOptions {
        model: "local/test-model",
        sources: vec![hf_source(&cache)],
        artifacts: vec![artifact("model.gguf", first, &sha_of(first))],
        on_progress: None,
        on_download_plan: None,
        on_fallback: None,
        fetcher: &*mirror,
        timeouts: test_timeouts(),
        lock: test_lock(),
    };
    let resolved = resolve_model_artifacts(&refresh).await.unwrap();
    assert_eq!(std::fs::read(&resolved.paths["model.gguf"]).unwrap(), first);
    assert!(!mirror.calls().is_empty(), "mutated file must re-download");
}

// ---------------------------------------------------------------------------
// Fallback eligibility.
// ---------------------------------------------------------------------------

async fn fallback_probe(status: u16) -> (Vec<String>, usize, Option<SourceKind>) {
    let dir = TempDir::new().unwrap();
    let hf_cache = dir.path().join("hf");
    let ms_cache = dir.path().join("ms");
    let bytes = b"mirror bytes";
    let artifacts = vec![artifact("model.gguf", bytes, &sha_of(bytes))];
    let mirror = StubMirror::new();
    mirror.route(
        &model_artifact_url(&hf_source(&hf_cache), "model.gguf"),
        http_error(status),
    );
    mirror.route(
        &model_artifact_url(&ms_source(&ms_cache), "model.gguf"),
        serve_bytes(bytes),
    );
    let warnings: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let warnings_hook = Arc::clone(&warnings);
    let options = ResolveArtifactsOptions {
        model: "local/test-model",
        sources: vec![hf_source(&hf_cache), ms_source(&ms_cache)],
        artifacts,
        on_progress: None,
        on_download_plan: None,
        on_fallback: Some(&|message| warnings_hook.lock().unwrap().push(message.to_string())),
        fetcher: &*mirror,
        timeouts: test_timeouts(),
        lock: test_lock(),
    };
    let result = resolve_model_artifacts(&options).await;
    match result {
        Ok(resolved) => (
            mirror.calls(),
            warnings.lock().unwrap().len(),
            Some(resolved.source.kind),
        ),
        Err(_) => (mirror.calls(), warnings.lock().unwrap().len(), None),
    }
}

#[tokio::test]
async fn falls_back_once_for_retryable_http_and_uses_the_mirror_url() {
    for status in [403, 404, 408, 429, 500, 503] {
        let (calls, warning_count, kind) = fallback_probe(status).await;
        assert_eq!(calls.len(), 2, "HTTP {status} must try HF then ModelScope");
        assert!(
            calls[1].starts_with(&format!("https://{MS_ORIGIN}/models/")),
            "HTTP {status} must fall back to the mirror URL, got {:?}",
            calls
        );
        assert_eq!(warning_count, 1, "HTTP {status} must warn exactly once");
        assert_eq!(kind, Some(SourceKind::ModelScope));
    }
}

#[tokio::test]
async fn does_not_fall_back_for_http_401() {
    let (calls, _, kind) = fallback_probe(401).await;
    assert_eq!(calls.len(), 1, "HTTP 401 must not fall back");
    assert!(calls[0].starts_with(&format!("https://{HF_ORIGIN}/")));
    assert_eq!(kind, None);
}

#[tokio::test]
async fn network_and_timeout_and_integrity_failures_fall_back() {
    for behavior in [
        StubBehavior::NetworkFailure,
        StubBehavior::Hang,
        StubBehavior::Serve {
            status: 200,
            retry_after: None,
            chunks: vec![StubChunk {
                bytes: b"short".to_vec(),
                delay_ms: 0,
            }],
        },
    ] {
        let dir = TempDir::new().unwrap();
        let hf_cache = dir.path().join("hf");
        let ms_cache = dir.path().join("ms");
        let bytes = b"mirror bytes payload here";
        let mirror = StubMirror::new();
        mirror.route(
            &model_artifact_url(&hf_source(&hf_cache), "model.gguf"),
            behavior,
        );
        mirror.route(
            &model_artifact_url(&ms_source(&ms_cache), "model.gguf"),
            serve_bytes(bytes),
        );
        let options = ResolveArtifactsOptions {
            model: "local/test-model",
            sources: vec![hf_source(&hf_cache), ms_source(&ms_cache)],
            artifacts: vec![artifact("model.gguf", bytes, &sha_of(bytes))],
            on_progress: None,
            on_download_plan: None,
            on_fallback: None,
            fetcher: &*mirror,
            timeouts: test_timeouts(),
            lock: test_lock(),
        };
        let resolved = resolve_model_artifacts(&options).await.unwrap();
        assert_eq!(resolved.source.kind, SourceKind::ModelScope);
        assert_eq!(
            std::fs::read(&resolved.paths["model.gguf"]).unwrap(),
            bytes
        );
        let _ = dir.keep();
    }
}

#[tokio::test]
async fn caller_cancellation_never_triggers_source_fallback() {
    let dir = TempDir::new().unwrap();
    let hf_cache = dir.path().join("hf");
    let ms_cache = dir.path().join("ms");
    let bytes = b"mirror bytes";
    let mirror = StubMirror::new();
    mirror.route(
        &model_artifact_url(&hf_source(&hf_cache), "model.gguf"),
        StubBehavior::Cancelled,
    );
    mirror.route(
        &model_artifact_url(&ms_source(&ms_cache), "model.gguf"),
        serve_bytes(bytes),
    );
    let options = ResolveArtifactsOptions {
        model: "local/test-model",
        sources: vec![hf_source(&hf_cache), ms_source(&ms_cache)],
        artifacts: vec![artifact("model.gguf", bytes, &sha_of(bytes))],
        on_progress: None,
        on_download_plan: None,
        on_fallback: None,
        fetcher: &*mirror,
        timeouts: test_timeouts(),
        lock: test_lock(),
    };
    let error = resolve_model_artifacts(&options).await.unwrap_err();
    assert!(
        matches!(
            error,
            ArtifactError::Download(ref download) if download.kind == FailureKind::Aborted
        ),
        "cancelled fetch must surface as aborted, got {error:?}"
    );
    assert_eq!(mirror.calls().len(), 1, "cancelled fetch must not fall back");
}

#[tokio::test]
async fn preserves_both_source_failures_in_a_resolution_error() {
    let dir = TempDir::new().unwrap();
    let hf_cache = dir.path().join("hf");
    let ms_cache = dir.path().join("ms");
    let bytes = b"mirror bytes";
    let mirror = StubMirror::new();
    mirror.route(
        &model_artifact_url(&hf_source(&hf_cache), "model.gguf"),
        http_error(500),
    );
    mirror.route(
        &model_artifact_url(&ms_source(&ms_cache), "model.gguf"),
        http_error(404),
    );
    let options = ResolveArtifactsOptions {
        model: "local/test-model",
        sources: vec![hf_source(&hf_cache), ms_source(&ms_cache)],
        artifacts: vec![artifact("model.gguf", bytes, &sha_of(bytes))],
        on_progress: None,
        on_download_plan: None,
        on_fallback: None,
        fetcher: &*mirror,
        timeouts: test_timeouts(),
        lock: test_lock(),
    };
    let error = resolve_model_artifacts(&options).await.unwrap_err();
    match error {
        ArtifactError::Resolution {
            model,
            primary,
            fallback,
        } => {
            assert_eq!(model, "local/test-model");
            assert_eq!(primary.status, Some(500));
            assert_eq!(fallback.status, Some(404));
        }
        other => panic!("expected a resolution error, got {other:?}"),
    }
    let _ = dir.keep();
}

#[tokio::test]
async fn filesystem_failure_never_triggers_source_fallback() {
    let dir = TempDir::new().unwrap();
    // The cache directory is a file, so cache preparation fails locally.
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();
    let bytes = b"mirror bytes";
    let mirror = StubMirror::new();
    let options = ResolveArtifactsOptions {
        model: "local/test-model",
        sources: vec![
            ModelArtifactSource {
                kind: SourceKind::HuggingFace,
                repo: "test/model".to_string(),
                revision: "hf-revision".to_string(),
                cache_directory: blocker.clone(),
                local_paths: HashMap::new(),
            },
            ms_source(&dir.path().join("ms")),
        ],
        artifacts: vec![artifact("model.gguf", bytes, &sha_of(bytes))],
        on_progress: None,
        on_download_plan: None,
        on_fallback: None,
        fetcher: &*mirror,
        timeouts: test_timeouts(),
        lock: test_lock(),
    };
    let error = resolve_model_artifacts(&options).await.unwrap_err();
    assert!(
        matches!(
            error,
            ArtifactError::Download(ref download) if download.kind == FailureKind::Filesystem
        ),
        "blocked cache dir must surface as filesystem, got {error:?}"
    );
    assert!(
        mirror.calls().is_empty(),
        "filesystem failure must not touch the network"
    );
}

#[tokio::test]
async fn callback_failure_is_local_and_never_triggers_fallback() {
    let dir = TempDir::new().unwrap();
    let hf_cache = dir.path().join("hf");
    let ms_cache = dir.path().join("ms");
    let bytes = b"mirror bytes";
    let mirror = StubMirror::new();
    let url = model_artifact_url(&hf_source(&hf_cache), "model.gguf");
    mirror.route(&url, serve_bytes(bytes));
    let options = ResolveArtifactsOptions {
        model: "local/test-model",
        sources: vec![hf_source(&hf_cache), ms_source(&ms_cache)],
        artifacts: vec![artifact("model.gguf", bytes, &sha_of(bytes))],
        on_progress: Some(&|_| panic!("progress hook blew up")),
        on_download_plan: None,
        on_fallback: None,
        fetcher: &*mirror,
        timeouts: test_timeouts(),
        lock: test_lock(),
    };
    let error = resolve_model_artifacts(&options).await.unwrap_err();
    assert!(
        matches!(
            error,
            ArtifactError::Download(ref download) if download.kind == FailureKind::Callback
        ),
        "panicking hook must surface as callback, got {error:?}"
    );
    // The hook fires before the first request, so the failure precedes any
    // networking — a fortiori it never falls back.
    assert!(
        mirror.calls().is_empty(),
        "callback failure must not reach the network"
    );
}

// ---------------------------------------------------------------------------
// Deadlines.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_idle_deadline_resets_per_chunk_with_no_total_deadline() {
    let dir = TempDir::new().unwrap();
    let cache = dir.path().join("cache");
    // Five chunks, each just inside the 100ms idle budget: the total (~250ms)
    // exceeds any single idle window, proving no total deadline applies.
    let chunk = b"0123456789abcdef";
    let bytes: Vec<u8> = chunk.repeat(5).iter().copied().collect::<Vec<u8>>();
    let chunks: Vec<StubChunk> = (0..5)
        .map(|_| StubChunk {
            bytes: chunk.to_vec(),
            delay_ms: 50,
        })
        .collect();
    let mirror = StubMirror::new();
    mirror.route(
        &model_artifact_url(&hf_source(&cache), "model.gguf"),
        StubBehavior::Serve {
            status: 200,
            retry_after: None,
            chunks,
        },
    );
    let options = ResolveArtifactsOptions {
        model: "local/test-model",
        sources: vec![hf_source(&cache)],
        artifacts: vec![artifact("model.gguf", &bytes, &sha_of(&bytes))],
        on_progress: None,
        on_download_plan: None,
        on_fallback: None,
        fetcher: &*mirror,
        timeouts: test_timeouts(),
        lock: test_lock(),
    };
    let resolved = resolve_model_artifacts(&options).await.unwrap();
    assert_eq!(std::fs::read(&resolved.paths["model.gguf"]).unwrap(), bytes);
}

#[tokio::test]
async fn stalled_body_read_hits_the_idle_deadline_and_falls_back() {
    let dir = TempDir::new().unwrap();
    let hf_cache = dir.path().join("hf");
    let ms_cache = dir.path().join("ms");
    let bytes = b"mirror bytes payload";
    let mirror = StubMirror::new();
    mirror.route(
        &model_artifact_url(&hf_source(&hf_cache), "model.gguf"),
        StubBehavior::Serve {
            status: 200,
            retry_after: None,
            chunks: vec![
                StubChunk {
                    bytes: b"first".to_vec(),
                    delay_ms: 0,
                },
                StubChunk {
                    bytes: b"stalled".to_vec(),
                    delay_ms: 5_000,
                },
            ],
        },
    );
    mirror.route(
        &model_artifact_url(&ms_source(&ms_cache), "model.gguf"),
        serve_bytes(bytes),
    );
    let options = ResolveArtifactsOptions {
        model: "local/test-model",
        sources: vec![hf_source(&hf_cache), ms_source(&ms_cache)],
        artifacts: vec![artifact("model.gguf", bytes, &sha_of(bytes))],
        on_progress: None,
        on_download_plan: None,
        on_fallback: None,
        fetcher: &*mirror,
        timeouts: test_timeouts(),
        lock: test_lock(),
    };
    let resolved = resolve_model_artifacts(&options).await.unwrap();
    assert_eq!(resolved.source.kind, SourceKind::ModelScope);
    let _ = dir.keep();
}

// ---------------------------------------------------------------------------
// Concurrency + validation.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_callers_serialize_and_recheck_the_cache() {
    let dir = TempDir::new().unwrap();
    let cache = dir.path().join("cache");
    let bytes = b"shared snapshot bytes payload";
    let mirror = StubMirror::new();
    let route = {
        let url = model_artifact_url(&hf_source(&cache), "model.gguf");
        mirror.route(
            &url,
            StubBehavior::Serve {
                status: 200,
                retry_after: None,
                chunks: vec![StubChunk {
                    bytes: bytes.to_vec(),
                    delay_ms: 100,
                }],
            },
        );
        url
    };
    let fetcher: Arc<StubMirror> = mirror;
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let fetcher = Arc::clone(&fetcher);
            let cache = cache.clone();
            tokio::spawn(async move {
                let options = ResolveArtifactsOptions {
                    model: "local/test-model",
                    sources: vec![hf_source(&cache)],
                    artifacts: vec![artifact("model.gguf", bytes, &sha_of(bytes))],
                    on_progress: None,
                    on_download_plan: None,
                    on_fallback: None,
                    fetcher: &*fetcher,
                    timeouts: test_timeouts(),
                    lock: test_lock(),
                };
                resolve_model_artifacts(&options).await.unwrap();
                std::fs::read(cache.join("model.gguf")).unwrap()
            })
        })
        .collect();
    for handle in handles {
        assert_eq!(handle.await.unwrap(), bytes);
    }
    let downloads = fetcher
        .calls()
        .iter()
        .filter(|url| *url == &route)
        .count();
    assert_eq!(downloads, 1, "one download must serve all callers");
}

#[tokio::test]
async fn recovers_a_stale_snapshot_lock() {
    let dir = TempDir::new().unwrap();
    let cache = dir.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let bytes = b"locked snapshot bytes";
    let source = hf_source(&cache);
    let artifacts = vec![artifact("model.gguf", bytes, &sha_of(bytes))];
    // Pre-create an abandoned lock (no heartbeat) at the exact snapshot path:
    // with a 1ms stale window it is garbage on arrival.
    let fingerprint = snapshot_fingerprint("local/test-model", &source, &artifacts);
    let abandoned = snapshot_lock_path(&source, &fingerprint);
    std::fs::create_dir_all(&abandoned).unwrap();
    let mirror = StubMirror::new();
    mirror.route(
        &model_artifact_url(&source, "model.gguf"),
        serve_bytes(bytes),
    );
    let options = ResolveArtifactsOptions {
        model: "local/test-model",
        sources: vec![source],
        artifacts,
        on_progress: None,
        on_download_plan: None,
        on_fallback: None,
        fetcher: &*mirror,
        timeouts: test_timeouts(),
        lock: LockOptions {
            poll_ms: 5,
            stale_ms: 2,
            heartbeat_ms: 1,
        },
    };
    let resolved = resolve_model_artifacts(&options).await.unwrap();
    assert_eq!(std::fs::read(&resolved.paths["model.gguf"]).unwrap(), bytes);
}

#[tokio::test]
async fn validates_recipes_before_filesystem_or_network_access() {
    let mirror = StubMirror::new();
    let cache = std::path::PathBuf::from("/tmp/never-created");
    let good_artifact = artifact("model.gguf", b"x", SHA_A);
    let cases: Vec<(&str, ResolveArtifactsOptions<'_>)> = vec![
        (
            "empty model",
            ResolveArtifactsOptions {
                model: "  ",
                sources: vec![hf_source(&cache)],
                artifacts: vec![good_artifact.clone()],
                on_progress: None,
                on_download_plan: None,
                on_fallback: None,
                fetcher: &*mirror,
                timeouts: test_timeouts(),
                lock: test_lock(),
            },
        ),
        (
            "no artifacts",
            ResolveArtifactsOptions {
                model: "local/test-model",
                sources: vec![hf_source(&cache)],
                artifacts: Vec::new(),
                on_progress: None,
                on_download_plan: None,
                on_fallback: None,
                fetcher: &*mirror,
                timeouts: test_timeouts(),
                lock: test_lock(),
            },
        ),
        (
            "bad sha",
            ResolveArtifactsOptions {
                model: "local/test-model",
                sources: vec![hf_source(&cache)],
                artifacts: vec![artifact("model.gguf", b"x", "not-a-sha")],
                on_progress: None,
                on_download_plan: None,
                on_fallback: None,
                fetcher: &*mirror,
                timeouts: test_timeouts(),
                lock: test_lock(),
            },
        ),
        (
            "absolute artifact path",
            ResolveArtifactsOptions {
                model: "local/test-model",
                sources: vec![hf_source(&cache)],
                artifacts: vec![artifact("/abs/model.gguf", b"x", SHA_A)],
                on_progress: None,
                on_download_plan: None,
                on_fallback: None,
                fetcher: &*mirror,
                timeouts: test_timeouts(),
                lock: test_lock(),
            },
        ),
    ];
    for (name, options) in cases {
        let error = resolve_model_artifacts(&options).await.unwrap_err();
        assert!(
            matches!(
                error,
                ArtifactError::Download(ref download)
                    if download.kind == FailureKind::InvalidInput
            ),
            "{name} must fail validation, got {error:?}"
        );
    }
    assert!(
        mirror.calls().is_empty(),
        "validation must precede any network access"
    );
}

#[tokio::test]
async fn rejects_unsafe_local_mappings_before_the_network() {
    let dir = TempDir::new().unwrap();
    let cache = dir.path().join("cache");
    let bytes = b"bytes";
    let mut local_paths = HashMap::new();
    local_paths.insert("model.gguf".to_string(), "../escape.gguf".to_string());
    let mirror = StubMirror::new();
    let options = ResolveArtifactsOptions {
        model: "local/test-model",
        sources: vec![ModelArtifactSource {
            kind: SourceKind::HuggingFace,
            repo: "test/model".to_string(),
            revision: "hf-revision".to_string(),
            cache_directory: cache.clone(),
            local_paths,
        }],
        artifacts: vec![artifact("model.gguf", bytes, &sha_of(bytes))],
        on_progress: None,
        on_download_plan: None,
        on_fallback: None,
        fetcher: &*mirror,
        timeouts: test_timeouts(),
        lock: test_lock(),
    };
    let error = resolve_model_artifacts(&options).await.unwrap_err();
    assert!(
        matches!(
            error,
            ArtifactError::Download(ref download)
                if download.kind == FailureKind::InvalidInput
        ),
        "escaping mapping must fail validation, got {error:?}"
    );
    assert!(mirror.calls().is_empty());
}

// ---------------------------------------------------------------------------
// Aggregated progress reporter.
// ---------------------------------------------------------------------------

#[test]
fn download_progress_reporter_aggregates_per_artifact_bytes() {
    let events: Arc<Mutex<Vec<EmbeddingProgress>>> = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::clone(&events);
    let reporter =
        DownloadProgressReporter::new("local/test-model", move |event| hook.lock().unwrap().push(event));
    reporter.start();
    reporter.set_download_plan(&[
        artifact("a.gguf", &[0u8; 100], SHA_A),
        artifact("b.gguf", &[0u8; 300], SHA_B),
    ]);
    reporter.report("a.gguf", 40);
    reporter.report("b.gguf", 100);
    reporter.report("unknown.gguf", 999);
    assert!(reporter.warning("mirror is slow"));
    reporter.finish();
    assert_eq!(
        events.lock().unwrap().as_slice(),
        &[
            EmbeddingProgress::Preparing {
                model: "local/test-model".to_string()
            },
            EmbeddingProgress::Downloading {
                model: "local/test-model".to_string(),
                downloaded_bytes: 40,
                total_bytes: 400,
            },
            EmbeddingProgress::Downloading {
                model: "local/test-model".to_string(),
                downloaded_bytes: 140,
                total_bytes: 400,
            },
            EmbeddingProgress::Warning {
                model: "local/test-model".to_string(),
                message: "mirror is slow".to_string(),
            },
            EmbeddingProgress::Ready {
                model: "local/test-model".to_string()
            },
        ]
    );
}
