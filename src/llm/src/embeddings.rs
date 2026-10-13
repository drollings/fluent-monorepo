use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::Mutex;

use crate::url::validate_https_or_local_http;
use async_trait::async_trait;
use common_core::hash::{content_hash_with_model, hex_encode};
use common_core::http::shared_http_client;
use lru::LruCache;

const DEFAULT_CACHE_LIMIT: usize = 1024;

#[derive(Debug, thiserror::Error)]
pub enum EmbeddingError {
    #[error("unknown provider: {0}")]
    UnknownProvider(String),
    #[error("unknown embedding model reference: {0}")]
    CatalogNotFound(String),
    #[error("invalid GUIDANCE_EMBEDDING: unsupported model {0}")]
    InvalidEmbeddingReference(String),
    #[error("embedding request failed: {0}")]
    RequestFailed(String),
    #[error("invalid API URL")]
    InvalidApiUrl,
    #[error("insecure API URL")]
    InsecureApiUrl,
    #[error("SSRF blocked URL")]
    SsrfBlockedUrl,
    #[error("no API key provided")]
    NoApiKey,
    #[error("parse error: {0}")]
    ParseError(String),
    #[error("embedding batch size {size} exceeds model limit {max}")]
    BatchTooLarge { size: usize, max: usize },
    #[error("invalid embedding batch output: {0}")]
    InvalidBatchOutput(String),
    #[error("embedding catalog entry is not usable here: {0}")]
    InvalidCatalogEntry(String),
    #[error("embedding backend '{backend}' for '{reference}' has no factory constructor")]
    BackendNotWired {
        reference: String,
        backend: &'static str,
    },
    #[error("unknown embedding device: {0}")]
    InvalidDevice(String),
}

#[derive(Debug, Clone)]
pub struct BatchEmbedding {
    pub flat: Vec<f32>,
    pub count: usize,
    pub dims: usize,
}

impl BatchEmbedding {
    pub fn vector(&self, i: usize) -> &[f32] {
        let start = i * self.dims;
        &self.flat[start..start + self.dims]
    }

    /// Checked accessor: returns `None` when `i` is out of bounds instead of
    /// panicking. Prefer over `vector` for input-derived indices.
    pub fn try_vector(&self, i: usize) -> Option<&[f32]> {
        let start = i.checked_mul(self.dims)?;
        let end = start.checked_add(self.dims)?;
        self.flat.get(start..end)
    }
}

#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn dimensions(&self) -> u32;
    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError>;

    fn embed_batch(&self, texts: &[&str]) -> Result<BatchEmbedding, EmbeddingError>;

    async fn embed_async(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.embed(text)
    }

    async fn embed_batch_async(&self, texts: &[&str]) -> Result<BatchEmbedding, EmbeddingError> {
        self.embed_batch(texts)
    }

    /// Batch embedding with per-input truncation indexes. The default impl
    /// reports no truncation; providers that shorten over-limit inputs
    /// override this and return the sorted indexes they truncated.
    fn embed_batch_with_truncation(
        &self,
        texts: &[&str],
    ) -> Result<(BatchEmbedding, Vec<usize>), EmbeddingError> {
        Ok((self.embed_batch(texts)?, Vec::new()))
    }
}

/// Reject batches larger than the model's `maxBatchSize` before any runtime
/// use. Callers pack cross-file batches; providers enforce the per-model cap.
pub fn validate_embed_batch(
    batch_size: usize,
    max_batch_size: usize,
) -> Result<(), EmbeddingError> {
    if batch_size > max_batch_size {
        return Err(EmbeddingError::BatchTooLarge {
            size: batch_size,
            max: max_batch_size,
        });
    }
    Ok(())
}

/// Validate a provider-produced batch: vector count, flat length, dimension
/// agreement, finite values, and truncation-index range. Mirrors the base
/// embedding-model validation contract.
pub fn check_embed_batch_output(
    batch: &BatchEmbedding,
    expected_count: usize,
    expected_dims: usize,
    truncated: &[usize],
) -> Result<(), EmbeddingError> {
    let invalid = |message: String| EmbeddingError::InvalidBatchOutput(message);
    if batch.count != expected_count {
        return Err(invalid(format!(
            "wrong number of vectors: expected {expected_count}, got {}",
            batch.count
        )));
    }
    if batch.dims != expected_dims {
        return Err(invalid(format!(
            "wrong dimension: expected {expected_dims}, got {}",
            batch.dims
        )));
    }
    if batch.flat.len() != expected_count * expected_dims {
        return Err(invalid(format!(
            "flat length {} does not match {expected_count}x{expected_dims}",
            batch.flat.len()
        )));
    }
    if !batch.flat.iter().all(|value| value.is_finite()) {
        return Err(invalid("non-finite vector value".to_string()));
    }
    let mut seen = std::collections::HashSet::new();
    for index in truncated {
        if *index >= expected_count || !seen.insert(index) {
            return Err(invalid(format!("invalid truncated input index {index}")));
        }
    }
    Ok(())
}

/// Sync adapter over an async provider core: the workspace's single
/// canonical sync→async bridge (`client::block_on`, itself delegating to
/// `common_core::runtime::block_on`). Shared by providers whose inference
/// core is async-only.
pub(crate) fn block_on_provider<F>(future: F) -> F::Output
where
    F: std::future::Future,
{
    crate::client::block_on(future)
}

pub struct NoopEmbedding {
    dims: u32,
}

impl NoopEmbedding {
    pub fn new(dims: u32) -> Self {
        Self { dims }
    }
}

impl EmbeddingProvider for NoopEmbedding {
    fn name(&self) -> &'static str {
        "none"
    }

    fn dimensions(&self) -> u32 {
        self.dims
    }

    fn embed(&self, _text: &str) -> Result<Vec<f32>, EmbeddingError> {
        Ok(Vec::new())
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<BatchEmbedding, EmbeddingError> {
        let _ = texts;
        Ok(BatchEmbedding {
            flat: vec![],
            count: texts.len(),
            dims: 0,
        })
    }
}

pub struct OllamaEmbedding {
    base_url: String,
    model: String,
    dims: u32,
    /// HTTP client for embedding POSTs. Constructors default to the
    /// workspace shared client (10s connect / 30s total); tests inject
    /// the timeout-free client via [`OllamaEmbedding::with_client`] so
    /// loopback mocks never trip production bounds under load.
    client: reqwest::Client,
}

impl OllamaEmbedding {
    pub fn new(
        model: Option<&str>,
        base_url: Option<&str>,
        dims: u32,
    ) -> Result<Self, EmbeddingError> {
        let base_url = base_url.unwrap_or("http://localhost:11434").to_string();
        validate_url(&base_url)?;
        Ok(Self {
            base_url,
            model: model.unwrap_or("nomic-embed-text").to_string(),
            dims,
            client: shared_http_client().clone(),
        })
    }

    /// Use `client` for embedding POSTs instead of the shared default
    /// (test seam; production constructors keep the bounded client).
    #[must_use]
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    pub fn embed_raw(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let body = serde_json::json!({
            "model": self.model,
            "input": text,
        });
        let url = format!("{}/api/embed", self.base_url.trim_end_matches('/'));
        let resp_bytes = do_http_post(&self.client, &url, &body, None)?;
        parse_ollama_response(&resp_bytes)
    }
}

#[async_trait]
impl EmbeddingProvider for OllamaEmbedding {
    fn name(&self) -> &'static str {
        "ollama"
    }

    fn dimensions(&self) -> u32 {
        self.dims
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.embed_raw(text)
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<BatchEmbedding, EmbeddingError> {
        let inputs: Vec<serde_json::Value> = texts
            .iter()
            .map(|t| serde_json::Value::String(t.to_string()))
            .collect();
        let body = serde_json::json!({
            "model": self.model,
            "input": inputs,
        });
        let url = format!("{}/api/embed", self.base_url.trim_end_matches('/'));
        let resp_bytes = do_http_post(&self.client, &url, &body, None)?;
        parse_ollama_batch_response(&resp_bytes)
    }

    async fn embed_async(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let body = serde_json::json!({
            "model": self.model,
            "input": text,
        });
        let url = format!("{}/api/embed", self.base_url.trim_end_matches('/'));
        let resp_bytes = do_http_post_async(&self.client, &url, &body, None).await?;
        parse_ollama_response(&resp_bytes)
    }

    async fn embed_batch_async(&self, texts: &[&str]) -> Result<BatchEmbedding, EmbeddingError> {
        let inputs: Vec<serde_json::Value> = texts
            .iter()
            .map(|t| serde_json::Value::String(t.to_string()))
            .collect();
        let body = serde_json::json!({
            "model": self.model,
            "input": inputs,
        });
        let url = format!("{}/api/embed", self.base_url.trim_end_matches('/'));
        let resp_bytes = do_http_post_async(&self.client, &url, &body, None).await?;
        parse_ollama_batch_response(&resp_bytes)
    }
}

/// One lock-striping shard of the embedding cache: exact key-equality map
/// from content-hash hex to the shared vector allocation.
type CacheShard = Arc<Mutex<LruCache<String, Arc<Vec<f32>>>>>;

/// Generic caching wrapper around any EmbeddingProvider.
/// Uses content_hash_with_model for cache keys.
/// Bounded near `limit` entries total (see `CACHE_SHARDS`).
///
/// Values are `Arc`-shared: repeat hits clone the pointer, not the vector,
/// and one batch fans the same allocation out to every duplicate input.
/// A poisoned shard is treated as a miss (best-effort cache, matching the
/// batch path) — inference is always re-runnable, so availability beats
/// erroring on cache state.
pub struct CachedEmbeddingProvider<T: EmbeddingProvider> {
    inner: T,
    shards: [CacheShard; CACHE_SHARDS],
    /// LoRA adapter namespace. `None` (the default) is today's behavior:
    /// the key is `(model, text)`. `Some` extends the key to
    /// `(model, adapter, text)`, so a hit in one namespace never serves
    /// another while key-equality reuse is preserved inside a namespace.
    adapter: Option<String>,
}

/// Lock-striping shard count. Pure concurrency tuning — NOT a quality,
/// recall, or correctness threshold: every key routes to exactly one shard
/// and lookup is exact key-equality, so any count preserves identical
/// hit/miss semantics (M11.5). Sixteen keeps per-shard contention negligible
/// while keeping the total bound tight (`shards × per-shard cap`).
const CACHE_SHARDS: usize = 16;

fn shard_of(key: &str) -> usize {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    (hasher.finish() % CACHE_SHARDS as u64) as usize
}

impl<T: EmbeddingProvider> CachedEmbeddingProvider<T> {
    pub fn new(inner: T) -> Self {
        Self::new_with_limit(inner, DEFAULT_CACHE_LIMIT)
    }

    pub fn new_with_limit(inner: T, limit: usize) -> Self {
        let per_shard =
            NonZeroUsize::new(limit.div_ceil(CACHE_SHARDS).max(1)).unwrap_or(NonZeroUsize::MIN);
        Self {
            inner,
            shards: [(); CACHE_SHARDS].map(|()| Arc::new(Mutex::new(LruCache::new(per_shard)))),
            adapter: None,
        }
    }

    /// Serve the `adapter` namespace: base and base+LoRA embeddings never
    /// share a key. The default (no adapter) keys exactly as before.
    #[must_use]
    pub fn with_adapter(mut self, adapter: impl Into<String>) -> Self {
        self.adapter = Some(adapter.into());
        self
    }

    /// The adapter namespace, if any.
    #[must_use]
    pub fn adapter(&self) -> Option<&str> {
        self.adapter.as_deref()
    }

    fn cache_key(&self, text: &str) -> String {
        // The model side of the hash already namespaces providers by model
        // name; the adapter extends it with a `\x00` separator that can never
        // collide with a bare model name, so `None` keys are byte-identical
        // to today.
        let model = match &self.adapter {
            Some(adapter) => format!("{}\x00{adapter}", self.inner.name()),
            None => self.inner.name().to_string(),
        };
        let bytes = content_hash_with_model(text, &model);
        hex_encode(&bytes)
    }

    fn cached(&self, key: &str) -> Option<Arc<Vec<f32>>> {
        self.shards[shard_of(key)].lock().ok()?.get(key).cloned()
    }

    fn store(&self, key: String, vec: Arc<Vec<f32>>) {
        if let Ok(mut map) = self.shards[shard_of(&key)].lock() {
            map.put(key, vec);
        }
    }
}

#[async_trait]
impl<T: EmbeddingProvider + Send + Sync + 'static> EmbeddingProvider
    for CachedEmbeddingProvider<T>
{
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn dimensions(&self) -> u32 {
        self.inner.dimensions()
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let key = self.cache_key(text);
        if let Some(cached) = self.cached(&key) {
            return Ok(cached.as_slice().to_vec());
        }
        let result = self.inner.embed(text)?;
        // The no-op path is passthrough: empty vectors occupy no LRU slots.
        if !result.is_empty() {
            self.store(key, Arc::new(result.clone()));
        }
        Ok(result)
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<BatchEmbedding, EmbeddingError> {
        // The sync variant is a thin runtime shim over the single async
        // core — the cache loop lives in exactly one place, driven through
        // the canonical bridge (`block_on_provider`).
        crate::embeddings::block_on_provider(self.embed_batch_async(texts))
    }

    async fn embed_async(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let key = self.cache_key(text);
        if let Some(cached) = self.cached(&key) {
            return Ok(cached.as_slice().to_vec());
        }
        let result = self.inner.embed_async(text).await?;
        // The no-op path is passthrough: empty vectors occupy no LRU slots.
        if !result.is_empty() {
            self.store(key, Arc::new(result.clone()));
        }
        Ok(result)
    }

    async fn embed_batch_async(&self, texts: &[&str]) -> Result<BatchEmbedding, EmbeddingError> {
        let dims = self.dimensions() as usize;
        let mut flat = Vec::with_capacity(texts.len().saturating_mul(dims));
        let mut results: Vec<Option<Arc<Vec<f32>>>> = vec![None; texts.len()];

        // Exact in-batch dedup: group positions by distinct text so one
        // inner vector serves every duplicate input (single inner call,
        // single shard put per key).
        let mut group_of: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        let mut groups: Vec<(&str, Vec<usize>)> = Vec::new();
        for (i, text) in texts.iter().enumerate() {
            if text.is_empty() {
                results[i] = Some(Arc::new(Vec::new()));
                continue;
            }
            if let Some(&g) = group_of.get(text) {
                groups[g].1.push(i);
            } else {
                group_of.insert(text, groups.len());
                groups.push((text, vec![i]));
            }
        }

        let mut uncached_texts: Vec<&str> = Vec::new();
        let mut uncached_groups: Vec<usize> = Vec::new();
        for (g, (text, positions)) in groups.iter().enumerate() {
            let key = self.cache_key(text);
            if let Some(cached) = self.cached(&key) {
                for &pos in positions {
                    results[pos] = Some(Arc::clone(&cached));
                }
            } else {
                uncached_texts.push(text);
                uncached_groups.push(g);
            }
        }

        if !uncached_texts.is_empty() {
            let batch = self.inner.embed_batch_async(&uncached_texts).await?;
            for (j, &g) in uncached_groups.iter().enumerate() {
                let vec = batch.try_vector(j).ok_or_else(|| {
                    EmbeddingError::ParseError(format!(
                        "batch returned fewer vectors than requested (requested {j})"
                    ))
                })?;
                let vec = Arc::new(vec.to_vec());
                // Empty vectors stay uncached (no-op passthrough).
                if !vec.is_empty() {
                    let key = self.cache_key(groups[g].0);
                    self.store(key, Arc::clone(&vec));
                }
                for &pos in &groups[g].1 {
                    results[pos] = Some(Arc::clone(&vec));
                }
            }
        }

        for v in results.iter().flatten() {
            flat.extend_from_slice(v.as_slice());
        }

        let count = results.len();
        let actual_dims = if count > 0 {
            results[0].as_ref().map_or(0, |v| v.len())
        } else {
            0
        };
        Ok(BatchEmbedding {
            flat,
            count,
            dims: actual_dims,
        })
    }
}

pub struct OpenAiEmbedding {
    base_url: String,
    api_key: String,
    model: String,
    dims: u32,
    /// Model-level inference params merged into every embeddings request body
    /// (e.g. `num_ctx` for llama.cpp slot sizing). Kept on the embedding so the
    /// chart HNSW embedder reuses the same slot configuration as the chat
    /// classifier instead of opening a second default-context instance.
    params: Option<serde_json::Value>,
    /// HTTP client for embedding POSTs. Constructors default to the
    /// workspace shared client (10s connect / 30s total); tests inject
    /// the timeout-free client via [`OpenAiEmbedding::with_client`] so
    /// loopback mocks never trip production bounds under load.
    client: reqwest::Client,
}

impl OpenAiEmbedding {
    pub fn new(
        model: Option<&str>,
        base_url: Option<&str>,
        api_key: Option<&str>,
        dims: u32,
    ) -> Result<Self, EmbeddingError> {
        Self::new_with_params(model, base_url, api_key, dims, None)
    }

    pub fn new_with_params(
        model: Option<&str>,
        base_url: Option<&str>,
        api_key: Option<&str>,
        dims: u32,
        params: Option<serde_json::Value>,
    ) -> Result<Self, EmbeddingError> {
        let base_url = base_url.unwrap_or("https://api.openai.com/v1").to_string();
        let api_key = api_key.ok_or(EmbeddingError::NoApiKey)?;
        validate_url(&base_url)?;
        Ok(Self {
            base_url,
            api_key: api_key.to_string(),
            model: model.unwrap_or("text-embedding-3-small").to_string(),
            dims,
            params,
            client: shared_http_client().clone(),
        })
    }

    /// Use `client` for embedding POSTs instead of the shared default
    /// (test seam; production constructors keep the bounded client).
    #[must_use]
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// Build the request body merging model-level `params` into the standard
    /// `{"model", "input"}` shape. Core fields cannot be overwritten. The
    /// merge is the shared wire-format helper (`openai::apply_params`, M12) —
    /// same overlay-wins/protected-keys discipline as the chat body.
    fn request_body(&self, input: &serde_json::Value) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": self.model,
            "input": input,
        });
        crate::openai::apply_params(&mut body, self.params.as_ref(), &["model", "input"]);
        body
    }

    fn embeddings_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        if base.ends_with("/embeddings") {
            return base.to_string();
        }
        let has_path = base[8..].contains('/'); // after https:// or http://
        if has_path {
            format!("{base}/embeddings")
        } else {
            format!("{base}/v1/embeddings")
        }
    }
}

#[async_trait]
impl EmbeddingProvider for OpenAiEmbedding {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn dimensions(&self) -> u32 {
        self.dims
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let body = self.request_body(&serde_json::json!(text));
        let resp_bytes = do_http_post(&self.client, &self.embeddings_url(), &body, Some(&self.api_key))?;
        parse_openai_response(&resp_bytes)
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<BatchEmbedding, EmbeddingError> {
        let inputs: Vec<serde_json::Value> = texts
            .iter()
            .map(|t| serde_json::Value::String(t.to_string()))
            .collect();
        let body = self.request_body(&serde_json::Value::Array(inputs));
        let resp_bytes = do_http_post(&self.client, &self.embeddings_url(), &body, Some(&self.api_key))?;
        parse_openai_batch_response(&resp_bytes)
    }

    async fn embed_async(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let body = self.request_body(&serde_json::json!(text));
        let resp_bytes =
            do_http_post_async(&self.client, &self.embeddings_url(), &body, Some(&self.api_key)).await?;
        parse_openai_response(&resp_bytes)
    }

    async fn embed_batch_async(&self, texts: &[&str]) -> Result<BatchEmbedding, EmbeddingError> {
        let inputs: Vec<serde_json::Value> = texts
            .iter()
            .map(|t| serde_json::Value::String(t.to_string()))
            .collect();
        let body = self.request_body(&serde_json::Value::Array(inputs));
        let resp_bytes =
            do_http_post_async(&self.client, &self.embeddings_url(), &body, Some(&self.api_key)).await?;
        parse_openai_batch_response(&resp_bytes)
    }
}

/// Embedding POST over a caller-supplied HTTP client. Production callers
/// pass the workspace shared client (connection pooling, 10s connect /
/// 30s total timeouts — the same envelope as every other workspace HTTP
/// call, bounding a stalled embedding server that the previous unbounded
/// private client would hang on forever); tests pass the timeout-free
/// client so loopback mocks never trip production bounds under load.
/// Status→error and body-byte mapping are unchanged (pinned by the HTTP
/// parity tests in `tests/embeddings.rs`).
async fn do_http_post_async(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    auth_header: Option<&str>,
) -> Result<Vec<u8>, EmbeddingError> {
    let mut req = client.post(url);
    req = req.header("Content-Type", "application/json");
    if let Some(token) = auth_header {
        req = req.header("Authorization", &format!("Bearer {token}"));
    }
    let resp = req
        .json(&body)
        .send()
        .await
        .map_err(|e| EmbeddingError::RequestFailed(e.to_string()))?;
    let status = resp.status();
    if status.as_u16() >= 400 {
        let text = resp.text().await.unwrap_or_default();
        return Err(EmbeddingError::RequestFailed(format!(
            "HTTP {status}: {text}",
        )));
    }
    resp.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| EmbeddingError::RequestFailed(e.to_string()))
}

/// Sync HTTP POST adapter for embedding calls: the canonical bridge
/// (`client::block_on` — `block_in_place` inside a multi-thread runtime,
/// the process-wide fallback runtime outside one).
fn do_http_post(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    auth_header: Option<&str>,
) -> Result<Vec<u8>, EmbeddingError> {
    crate::client::block_on(do_http_post_async(client, url, body, auth_header))
}

fn parse_float_array(arr: &[serde_json::Value]) -> Result<Vec<f32>, EmbeddingError> {
    arr.iter()
        .map(|x| {
            x.as_f64()
                .map(|v| v as f32)
                .ok_or_else(|| EmbeddingError::ParseError("non-float in embedding".into()))
        })
        .collect()
}

fn parse_batch_embeddings(arrays: &[Vec<f32>]) -> BatchEmbedding {
    let count = arrays.len();
    let dims = arrays.first().map_or(0, Vec::len);
    let flat = arrays.iter().flatten().copied().collect();
    BatchEmbedding { flat, count, dims }
}

pub fn parse_ollama_response(json: &[u8]) -> Result<Vec<f32>, EmbeddingError> {
    let v: serde_json::Value =
        serde_json::from_slice(json).map_err(|e| EmbeddingError::ParseError(e.to_string()))?;
    let embeddings = v["embeddings"]
        .as_array()
        .ok_or_else(|| EmbeddingError::ParseError("missing embeddings array".into()))?;
    let first = embeddings
        .first()
        .ok_or_else(|| EmbeddingError::ParseError("empty embeddings array".into()))?;
    let arr = first
        .as_array()
        .ok_or_else(|| EmbeddingError::ParseError("embedding is not an array".into()))?;
    parse_float_array(arr)
}

pub fn parse_ollama_batch_response(json: &[u8]) -> Result<BatchEmbedding, EmbeddingError> {
    let v: serde_json::Value =
        serde_json::from_slice(json).map_err(|e| EmbeddingError::ParseError(e.to_string()))?;
    let embeddings = v["embeddings"]
        .as_array()
        .ok_or_else(|| EmbeddingError::ParseError("missing embeddings array".into()))?;
    let arrays: Result<Vec<Vec<f32>>, EmbeddingError> = embeddings
        .iter()
        .map(|emb| {
            let arr = emb
                .as_array()
                .ok_or_else(|| EmbeddingError::ParseError("embedding is not an array".into()))?;
            parse_float_array(arr)
        })
        .collect();
    Ok(parse_batch_embeddings(&arrays?))
}

pub fn parse_openai_response(json: &[u8]) -> Result<Vec<f32>, EmbeddingError> {
    let v: serde_json::Value =
        serde_json::from_slice(json).map_err(|e| EmbeddingError::ParseError(e.to_string()))?;
    let data = v["data"]
        .as_array()
        .ok_or_else(|| EmbeddingError::ParseError("missing data array".into()))?;
    let first = data
        .first()
        .ok_or_else(|| EmbeddingError::ParseError("empty data array".into()))?;
    let embedding = first["embedding"]
        .as_array()
        .ok_or_else(|| EmbeddingError::ParseError("missing embedding field".into()))?;
    parse_float_array(embedding)
}

pub fn parse_openai_batch_response(json: &[u8]) -> Result<BatchEmbedding, EmbeddingError> {
    let v: serde_json::Value =
        serde_json::from_slice(json).map_err(|e| EmbeddingError::ParseError(e.to_string()))?;
    let data = v["data"]
        .as_array()
        .ok_or_else(|| EmbeddingError::ParseError("missing data array".into()))?;
    let arrays: Result<Vec<Vec<f32>>, EmbeddingError> = data
        .iter()
        .map(|entry| {
            let embedding = entry["embedding"]
                .as_array()
                .ok_or_else(|| EmbeddingError::ParseError("missing embedding field".into()))?;
            parse_float_array(embedding)
        })
        .collect();
    Ok(parse_batch_embeddings(&arrays?))
}

fn validate_url(url: &str) -> Result<(), EmbeddingError> {
    use crate::url::UrlError;
    validate_https_or_local_http(url).map_err(|e| match e {
        UrlError::InvalidApiUrl => EmbeddingError::InvalidApiUrl,
        UrlError::InsecureApiUrl => EmbeddingError::InsecureApiUrl,
        UrlError::SsrfBlockedUrl => EmbeddingError::SsrfBlockedUrl,
    })
}

pub fn create_embedding_provider(
    name: &str,
    model: Option<&str>,
    base_url: Option<&str>,
    api_key: Option<&str>,
    dims: u32,
    cache_limit: Option<usize>,
    params: Option<&serde_json::Value>,
) -> Result<Box<dyn EmbeddingProvider>, EmbeddingError> {
    let limit = cache_limit.unwrap_or(DEFAULT_CACHE_LIMIT);
    let provider: Box<dyn EmbeddingProvider> = match name {
        "none" => Box::new(NoopEmbedding::new(dims)),
        "ollama" => Box::new(CachedEmbeddingProvider::new_with_limit(
            OllamaEmbedding::new(model, base_url, dims)?,
            limit,
        )),
        "openai" => Box::new(CachedEmbeddingProvider::new_with_limit(
            OpenAiEmbedding::new_with_params(model, base_url, api_key, dims, params.cloned())?,
            limit,
        )),
        _ => {
            if let Some(ollama_model) = name.strip_prefix("ollama:") {
                Box::new(CachedEmbeddingProvider::new_with_limit(
                    OllamaEmbedding::new(Some(ollama_model), base_url, dims)?,
                    limit,
                ))
            } else if let Some(llama_model) = name.strip_prefix("llama:") {
                // llama-server speaks OpenAI-compatible `/v1/embeddings`;
                // local servers ignore auth, so a missing key defaults
                // instead of declining (remote OpenAI use goes through
                // the `openai:` scheme, which still requires a key).
                Box::new(CachedEmbeddingProvider::new_with_limit(
                    OpenAiEmbedding::new_with_params(
                        Some(llama_model),
                        base_url,
                        api_key.or(Some("local")),
                        dims,
                        params.cloned(),
                    )?,
                    limit,
                ))
            } else if let Some(custom_url) = name.strip_prefix("custom:") {
                Box::new(CachedEmbeddingProvider::new_with_limit(
                    OpenAiEmbedding::new_with_params(
                        model,
                        Some(custom_url),
                        api_key,
                        dims,
                        params.cloned(),
                    )?,
                    limit,
                ))
            } else {
                return Err(EmbeddingError::UnknownProvider(name.to_string()));
            }
        }
    };
    Ok(provider)
}

#[cfg(test)]
#[path = "../tests/embeddings.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/embed_cache.rs"]
mod embed_cache_tests;
