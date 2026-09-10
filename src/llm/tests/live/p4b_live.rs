//! Live artifact-plane + remote-embedding tests (P4b).
//!
//! REAL network calls. Compiled only with `live-ai`, `#[ignore]`d, runnable
//! only via `make test-live` / `make llm-test-live`. The download test needs
//! no credentials (public mirrors); the Qwen test needs `QWEN_API_KEY` and
//! skips cleanly without it (skip-not-fail). Assertions are structural only.

use fluent_llm::artifact::{
    ModelArtifact, ModelArtifactSource, ReqwestArtifactFetcher, ResolveArtifactsOptions,
    SourceKind, resolve_model_artifacts,
};
use fluent_llm::artifact_lock::LockOptions;
use fluent_llm::catalog::get_embedding_model_catalog_entry;
use std::collections::HashMap;

/// Resolve the pinned bge `config.json` (867 bytes) through the real
/// mirrors: full plane (cache check, download, sha256, marker) live.
#[test]
#[ignore = "live: downloads from Hugging Face/ModelScope; run via `make test-live`"]
fn live_artifact_resolution_downloads_pinned_config() {
    let entry = get_embedding_model_catalog_entry("local/bge-small-en-v1.5")
        .expect("bge must be catalogued");
    let config = entry
        .artifacts
        .iter()
        .find(|artifact| artifact.path == "config.json")
        .expect("bge must pin config.json");
    let dir = tempfile::TempDir::new().expect("tempdir");
    let sources = entry
        .sources
        .map(|sources| {
            vec![
                ModelArtifactSource {
                    kind: SourceKind::HuggingFace,
                    repo: sources.hugging_face.0.to_string(),
                    revision: sources.hugging_face.1.to_string(),
                    cache_directory: dir.path().to_path_buf(),
                    local_paths: HashMap::new(),
                },
                ModelArtifactSource {
                    kind: SourceKind::ModelScope,
                    repo: sources.model_scope.0.to_string(),
                    revision: sources.model_scope.1.to_string(),
                    cache_directory: dir.path().join("modelscope"),
                    local_paths: HashMap::new(),
                },
            ]
        })
        .expect("bge must declare mirrors");
    let fetcher = ReqwestArtifactFetcher::new();
    let options = ResolveArtifactsOptions {
        model: entry.reference,
        sources,
        artifacts: vec![ModelArtifact {
            path: config.path.to_string(),
            size: config.size,
            sha256: config.sha256.to_string(),
        }],
        on_progress: None,
        on_download_plan: None,
        on_fallback: None,
        fetcher: &fetcher,
        timeouts: Default::default(),
        lock: LockOptions {
            poll_ms: 50,
            stale_ms: 60_000,
            heartbeat_ms: 5_000,
        },
    };
    let resolved = futures_executor_block_on(resolve_model_artifacts(&options))
        .expect("live artifact resolution should succeed");
    let bytes = std::fs::read(&resolved.paths["config.json"]).expect("artifact file");
    assert_eq!(bytes.len() as u64, config.size);
    assert_eq!(
        common_core::hash::sha256_hex(&bytes),
        config.sha256.to_lowercase()
    );
}

fn futures_executor_block_on<F: std::future::Future>(future: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(move || handle.block_on(future)),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("live test runtime")
            .block_on(future),
    }
}

/// Qwen text embedding against the real endpoint: structural invariants only.
#[test]
#[ignore = "live-AI: requires QWEN_API_KEY; run via `make test-live`"]
fn live_qwen_text_embedding_structural() {
    let Ok(api_key) = std::env::var("QWEN_API_KEY") else {
        eprintln!("QWEN_API_KEY not set; skipping live Qwen test");
        return;
    };
    let endpoint = std::env::var("QWEN_ENDPOINT").ok();
    let model = fluent_llm::QwenTextEmbeddingV4::new(fluent_llm::QwenTextOptions {
        api_key: Some(api_key),
        endpoint,
        extra_headers: HashMap::new(),
        timeout_ms: None,
    })
    .expect("live Qwen model should construct");
    let result =
        futures_executor_block_on(model.embed_texts(&["hello world".to_string(), "second".to_string()]))
            .expect("live Qwen embed should succeed");
    assert_eq!(result.vectors.len(), 2);
    for vector in &result.vectors {
        assert_eq!(vector.len(), 1024);
        assert!(vector.iter().all(|value| value.is_finite()));
    }
    assert!(result.truncated.is_empty());
}
