//! Remote Qwen embedding backends behind [`EmbeddingProvider`].
//!
//! Text models speak the OpenAI-compatible embeddings API (model, batched
//! inputs, index-mapped vectors, `dimensions`, `encoding_format: float`)
//! with a 60s default timeout and Bearer auth. The VL model additionally
//! validates images (jpeg/png/webp, ten-image cap, per-model byte cap),
//! base64-encodes them, and accepts the provider's `index` / `text_index` /
//! positional variants. Every failure names the model and never leaks the
//! API key; HTTP errors carry the provider code plus the parsed `Retry-After`
//! context. Hermetic tests drive these against loopback stub HTTP.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;

use crate::catalog::{CatalogEntry, EmbeddingBackend};
use crate::embeddings::{
    BatchEmbedding, EmbeddingError, EmbeddingProvider, block_on_provider, check_embed_batch_output,
    validate_embed_batch,
};


const DEFAULT_REMOTE_EMBEDDING_TIMEOUT_MS: u64 = 60_000;
const QWEN3_VL_MAX_IMAGE_COUNT: usize = 10;

/// Construction options for Qwen providers.
#[derive(Debug, Clone, Default)]
pub struct QwenTextOptions {
    pub api_key: Option<String>,
    pub endpoint: Option<String>,
    pub extra_headers: HashMap<String, String>,
    /// Override for the 60s default (tests use a short fuse).
    pub timeout_ms: Option<u64>,
}

/// Supported VL image formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QwenImageFormat {
    Jpeg,
    Png,
    Webp,
}

impl QwenImageFormat {
    pub fn parse(format: &str) -> Result<Self, EmbeddingError> {
        match format.trim().to_lowercase().as_str() {
            "jpeg" | "jpg" => Ok(Self::Jpeg),
            "png" => Ok(Self::Png),
            "webp" => Ok(Self::Webp),
            _ => Err(EmbeddingError::RequestFailed(format!(
                "Qwen3 VL embedding model does not support image format (format={format})"
            ))),
        }
    }
}

/// One embeddable unit for the VL model. Image formats validate at embed
/// time (like the base input contract), not at construction.
#[derive(Debug, Clone)]
pub enum EmbedContent {
    Text(String),
    Image { data: Vec<u8>, format: String },
}

impl EmbedContent {
    pub fn image(data: Vec<u8>, format: &str) -> Result<Self, EmbeddingError> {
        Ok(Self::Image {
            data,
            format: format.to_string(),
        })
    }
}

/// Batch result (remote models never truncate: the server owns limits).
#[derive(Debug)]
pub struct QwenEmbedResult {
    pub vectors: Vec<Vec<f32>>,
    pub truncated: Vec<usize>,
}

#[derive(Clone, Copy)]
struct QwenTextSpec {
    reference: &'static str,
    model: &'static str,
    display_name: &'static str,
    dimension: u32,
    max_batch_size: usize,
    max_input_tokens: usize,
    default_endpoint: &'static str,
}

impl TryFrom<(&CatalogEntry, &'static str)> for QwenTextSpec {
    type Error = EmbeddingError;

    fn try_from((entry, display_name): (&CatalogEntry, &'static str)) -> Result<Self, Self::Error> {
        let invalid = |field: &str| {
            EmbeddingError::InvalidCatalogEntry(format!(
                "{} is not a Qwen text entry (missing {field})",
                entry.reference
            ))
        };
        if entry.backend != EmbeddingBackend::Qwen || entry.kind != Some("text") {
            return Err(invalid("qwen/text backend"));
        }
        Ok(Self {
            reference: entry.reference,
            model: entry.model,
            display_name,
            dimension: entry.dimension,
            max_batch_size: entry.max_batch_size,
            max_input_tokens: entry.max_input_tokens.ok_or_else(|| invalid("maxInputTokens"))?,
            default_endpoint: entry
                .default_endpoint
                .ok_or_else(|| invalid("defaultEndpoint"))?,
        })
    }
}

struct ResolvedQwenConfig {
    api_key: String,
    endpoint: String,
    timeout: Duration,
}

fn resolve_qwen_config(
    spec: &QwenTextSpec,
    options: &QwenTextOptions,
) -> Result<ResolvedQwenConfig, EmbeddingError> {
    let api_key = options.api_key.as_deref().unwrap_or("").trim().to_string();
    if api_key.is_empty() {
        return Err(EmbeddingError::RequestFailed(format!(
            "{} model requires an API key",
            spec.display_name
        )));
    }
    let endpoint = match options.endpoint.as_deref() {
        None => spec.default_endpoint.to_string(),
        Some(explicit) => {
            let trimmed = explicit.trim();
            if trimmed.is_empty() {
                return Err(EmbeddingError::RequestFailed(format!(
                    "{} model requires an endpoint",
                    spec.display_name
                )));
            }
            trimmed.to_string()
        }
    };
    validate_endpoint(&endpoint, spec)?;
    Ok(ResolvedQwenConfig {
        api_key,
        endpoint,
        timeout: Duration::from_millis(
            options.timeout_ms.unwrap_or(DEFAULT_REMOTE_EMBEDDING_TIMEOUT_MS),
        ),
    })
}

fn validate_endpoint(endpoint: &str, spec: &QwenTextSpec) -> Result<(), EmbeddingError> {
    crate::url::validate_https_or_local_http(endpoint).map_err(|_| {
        EmbeddingError::RequestFailed(format!(
            "{} model requires an endpoint (model={})",
            spec.display_name, spec.reference
        ))
    })
}

/// Shared HTTP core for Qwen text models.
#[derive(Debug)]
struct QwenTextCore {
    spec_model: &'static str,
    display_name: &'static str,
    reference: &'static str,
    dimension: u32,
    max_batch_size: usize,
    max_input_tokens: usize,
    api_key: String,
    endpoint: String,
    extra_headers: HashMap<String, String>,
    timeout: Duration,
    client: reqwest::Client,
}

impl QwenTextCore {
    fn new(spec: QwenTextSpec, options: &QwenTextOptions) -> Result<Self, EmbeddingError> {
        let config = resolve_qwen_config(&spec, options)?;
        Ok(Self {
            spec_model: spec.model,
            display_name: spec.display_name,
            reference: spec.reference,
            dimension: spec.dimension,
            max_batch_size: spec.max_batch_size,
            max_input_tokens: spec.max_input_tokens,
            api_key: config.api_key,
            endpoint: config.endpoint,
            extra_headers: options.extra_headers.clone(),
            timeout: config.timeout,
            client: reqwest::Client::new(),
        })
    }

    fn error(&self, suffix: &str) -> String {
        format!("{} {suffix}", self.display_name)
    }

    async fn embed_texts(&self, texts: &[String]) -> Result<QwenEmbedResult, EmbeddingError> {
        validate_embed_batch(texts.len(), self.max_batch_size)?;
        if texts.is_empty() {
            return Err(EmbeddingError::RequestFailed(format!(
                "{}: Embedding requires at least one content item",
                self.error("")
            )));
        }
        for (index, text) in texts.iter().enumerate() {
            if text.trim().is_empty() {
                return Err(EmbeddingError::RequestFailed(format!(
                    "{}: Embedding text content must not be empty (model={} index={index})",
                    self.error(""),
                    self.reference
                )));
            }
        }
        let body = serde_json::json!({
            "model": self.spec_model,
            "input": texts,
            "dimensions": self.dimension,
            "encoding_format": "float",
        });
        let response = self.post(body).await?;
        let vectors = parse_text_vectors(
            &response,
            texts.len(),
            self.dimension as usize,
            self.display_name,
            self.reference,
        )?;
        Ok(QwenEmbedResult {
            vectors,
            truncated: Vec::new(),
        })
    }

    async fn post(&self, body: serde_json::Value) -> Result<ProviderResponse, EmbeddingError> {
        let mut request = self.client.post(&self.endpoint).timeout(self.timeout);
        request = request.header("Authorization", format!("Bearer {}", self.api_key));
        request = request.header("Content-Type", "application/json");
        for (name, value) in &self.extra_headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let response = request.json(&body).send().await.map_err(|error| {
            EmbeddingError::RequestFailed(format!(
                "{} request failed (model={} endpoint={} timeoutMs={}): {error}",
                self.display_name,
                self.reference,
                self.endpoint,
                self.timeout.as_millis()
            ))
        })?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body: serde_json::Value = response.json().await.map_err(|error| {
            EmbeddingError::RequestFailed(format!(
                "{} response was not valid JSON (model={} status={status}): {error}",
                self.display_name, self.reference
            ))
        })?;
        Ok(ProviderResponse {
            status,
            retry_after,
            body,
        })
    }
}

struct ProviderResponse {
    status: u16,
    retry_after: Option<String>,
    body: serde_json::Value,
}

fn parse_text_vectors(
    response: &ProviderResponse,
    input_count: usize,
    dimension: usize,
    display_name: &str,
    reference: &str,
) -> Result<Vec<Vec<f32>>, EmbeddingError> {
    if !(200..300).contains(&response.status) {
        let provider = read_provider_error(&response.body);
        return Err(EmbeddingError::RequestFailed(format!(
            "{display_name} request returned an error ({}, status={}, retryAfterMs={}, providerCode={}, providerType={}, providerMessage={})",
            error_code(display_name, reference, "API_ERROR"),
            response.status,
            retry_after_ms(response.retry_after.as_ref())
                .map_or("none".to_string(), |millis| millis.to_string()),
            provider.code,
            provider.kind,
            provider.message
        )));
    }
    let data = response.body.get("data").and_then(serde_json::Value::as_array).ok_or_else(|| {
        EmbeddingError::RequestFailed(format!(
            "{display_name} response did not include data (model={reference})"
        ))
    })?;
    let mut vectors: Vec<Option<Vec<f32>>> = vec![None; input_count];
    for item in data {
        let index = item.get("index").and_then(serde_json::Value::as_u64).ok_or_else(|| {
            EmbeddingError::RequestFailed(format!(
                "{display_name} response included an invalid index (model={reference})"
            ))
        })?;
        if index as usize >= input_count {
            return Err(EmbeddingError::RequestFailed(format!(
                "{display_name} response index was out of range (model={reference} index={index} inputCount={input_count})"
            )));
        }
        let embedding = item
            .get("embedding")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                EmbeddingError::RequestFailed(format!(
                    "{display_name} response included an invalid embedding (model={reference} index={index})"
                ))
            })?;
        vectors[index as usize] = Some(parse_float_vector(embedding, display_name, reference)?);
    }
    vectors
        .into_iter()
        .map(|vector| {
            vector.ok_or_else(|| {
                EmbeddingError::RequestFailed(format!(
                    "{display_name} response included a non-array vector (model={reference})"
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .and_then(|vectors| {
            for vector in &vectors {
                if vector.len() != dimension {
                    return Err(EmbeddingError::RequestFailed(format!(
                        "{display_name} response included a vector with the wrong dimension (model={reference})"
                    )));
                }
                if !vector.iter().all(|value| value.is_finite()) {
                    return Err(EmbeddingError::RequestFailed(format!(
                        "{display_name} response included a non-finite vector value (model={reference})"
                    )));
                }
            }
            Ok(vectors)
        })
}

fn parse_float_vector(
    values: &[serde_json::Value],
    display_name: &str,
    reference: &str,
) -> Result<Vec<f32>, EmbeddingError> {
    values
        .iter()
        .map(|value| {
            value.as_f64().map(|number| number as f32).ok_or_else(|| {
                EmbeddingError::RequestFailed(format!(
                    "{display_name} response included an invalid embedding (model={reference})"
                ))
            })
        })
        .collect()
}

struct ProviderError {
    code: String,
    kind: String,
    message: String,
}

fn read_provider_error(body: &serde_json::Value) -> ProviderError {
    if let Some(error) = body.get("error").and_then(|error| error.as_object()) {
        return ProviderError {
            code: error
                .get("code")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            kind: error
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            message: error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
        };
    }
    if body.is_object() {
        return ProviderError {
            code: body
                .get("code")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            kind: "unknown".to_string(),
            message: body
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
        };
    }
    ProviderError {
        code: "unknown".to_string(),
        kind: "unknown".to_string(),
        message: "unknown".to_string(),
    }
}

fn retry_after_ms(value: Option<&String>) -> Option<u64> {
    let value = value.as_ref()?;
    if let Ok(seconds) = value.parse::<f64>() {
        if seconds.is_finite() && seconds >= 0.0 {
            return Some((seconds * 1000.0).round() as u64);
        }
    }
    // HTTP-date form: milliseconds until the named instant.
    if let Ok(instant) = httpdate::parse_http_date(value) {
        let now = std::time::SystemTime::now();
        return Some(instant.duration_since(now).map_or(0, |duration| {
            duration.as_millis().min(u128::from(u64::MAX)) as u64
        }));
    }
    None
}

fn error_code(display_name: &str, reference: &str, suffix: &str) -> String {
    format!("{display_name} model={reference} code={suffix}")
}

/// Qwen `text-embedding-v4` provider.
#[derive(Debug)]
pub struct QwenTextEmbeddingV4 {
    core: QwenTextCore,
}

impl QwenTextEmbeddingV4 {
    pub fn new(options: &QwenTextOptions) -> Result<Self, EmbeddingError> {
        let entry = crate::catalog::get_embedding_model_catalog_entry("qwen/text-embedding-v4")
            .ok_or_else(|| EmbeddingError::CatalogNotFound("qwen/text-embedding-v4".to_string()))?;
        let spec = QwenTextSpec::try_from((entry, "Qwen text-embedding-v4"))?;
        Ok(Self {
            core: QwenTextCore::new(spec, options)?,
        })
    }

    pub async fn embed_texts(&self, texts: &[String]) -> Result<QwenEmbedResult, EmbeddingError> {
        self.core.embed_texts(texts).await
    }

    #[must_use]
    pub fn max_batch_size(&self) -> usize {
        self.core.max_batch_size
    }

    #[must_use]
    pub fn max_input_tokens(&self) -> usize {
        self.core.max_input_tokens
    }

    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.core.endpoint
    }

    #[must_use]
    pub fn dimension(&self) -> u32 {
        self.core.dimension
    }
}

/// Qwen `qwen3.7-text-embedding` provider.
#[derive(Debug)]
pub struct Qwen37TextEmbedding {
    core: QwenTextCore,
}

impl Qwen37TextEmbedding {
    pub fn new(options: &QwenTextOptions) -> Result<Self, EmbeddingError> {
        let entry =
            crate::catalog::get_embedding_model_catalog_entry("qwen/qwen3.7-text-embedding")
                .ok_or_else(|| {
                    EmbeddingError::CatalogNotFound("qwen/qwen3.7-text-embedding".to_string())
                })?;
        let spec = QwenTextSpec::try_from((entry, "Qwen3.7 text embedding"))?;
        Ok(Self {
            core: QwenTextCore::new(spec, options)?,
        })
    }

    pub async fn embed_texts(&self, texts: &[String]) -> Result<QwenEmbedResult, EmbeddingError> {
        self.core.embed_texts(texts).await
    }

    #[must_use]
    pub fn max_batch_size(&self) -> usize {
        self.core.max_batch_size
    }

    #[must_use]
    pub fn max_input_tokens(&self) -> usize {
        self.core.max_input_tokens
    }
}

// ---------------------------------------------------------------------------
// VL model.
// ---------------------------------------------------------------------------

struct QwenVlSpec {
    reference: &'static str,
    model: &'static str,
    dimension: u32,
    max_batch_size: usize,
    max_image_bytes: u64,
    default_endpoint: &'static str,
}

impl TryFrom<&CatalogEntry> for QwenVlSpec {
    type Error = EmbeddingError;

    fn try_from(entry: &CatalogEntry) -> Result<Self, Self::Error> {
        let invalid = |field: &str| {
            EmbeddingError::InvalidCatalogEntry(format!(
                "{} is not a Qwen VL entry (missing {field})",
                entry.reference
            ))
        };
        if entry.backend != EmbeddingBackend::Qwen || entry.kind != Some("multimodal") {
            return Err(invalid("qwen/multimodal backend"));
        }
        Ok(Self {
            reference: entry.reference,
            model: entry.model,
            dimension: entry.dimension,
            max_batch_size: entry.max_batch_size,
            max_image_bytes: entry.max_image_bytes.ok_or_else(|| invalid("maxImageBytes"))?,
            default_endpoint: entry
                .default_endpoint
                .ok_or_else(|| invalid("defaultEndpoint"))?,
        })
    }
}

/// Qwen `qwen3-vl-embedding` provider (text + image inputs).
#[derive(Debug)]
pub struct Qwen3VlEmbedding {
    reference: &'static str,
    model: &'static str,
    dimension: u32,
    max_batch_size: usize,
    max_image_bytes: u64,
    api_key: String,
    endpoint: String,
    extra_headers: HashMap<String, String>,
    timeout: Duration,
    client: reqwest::Client,
}

impl Qwen3VlEmbedding {
    pub fn new(options: &QwenTextOptions) -> Result<Self, EmbeddingError> {
        let entry = crate::catalog::get_embedding_model_catalog_entry("qwen/qwen3-vl-embedding")
            .ok_or_else(|| {
                EmbeddingError::CatalogNotFound("qwen/qwen3-vl-embedding".to_string())
            })?;
        let spec = QwenVlSpec::try_from(entry)?;
        let api_key = options.api_key.as_deref().unwrap_or("").trim().to_string();
        if api_key.is_empty() {
            return Err(EmbeddingError::RequestFailed(
                "Qwen3 VL embedding model requires an API key".to_string(),
            ));
        }
        let endpoint = match options.endpoint.as_deref() {
            None => spec.default_endpoint.to_string(),
            Some(explicit) => {
                let trimmed = explicit.trim();
                if trimmed.is_empty() {
                    return Err(EmbeddingError::RequestFailed(
                        "Qwen3 VL embedding model requires an endpoint".to_string(),
                    ));
                }
                trimmed.to_string()
            }
        };
        validate_endpoint_vl(&endpoint, spec.reference)?;
        Ok(Self {
            reference: spec.reference,
            model: spec.model,
            dimension: spec.dimension,
            max_batch_size: spec.max_batch_size,
            max_image_bytes: spec.max_image_bytes,
            api_key,
            endpoint,
            extra_headers: options.extra_headers.clone(),
            timeout: Duration::from_millis(
                options.timeout_ms.unwrap_or(DEFAULT_REMOTE_EMBEDDING_TIMEOUT_MS),
            ),
            client: reqwest::Client::new(),
        })
    }

    pub async fn embed_contents(
        &self,
        contents: &[EmbedContent],
    ) -> Result<QwenEmbedResult, EmbeddingError> {
        validate_embed_batch(contents.len(), self.max_batch_size)?;
        if contents.is_empty() {
            return Err(EmbeddingError::RequestFailed(
                "Embedding requires at least one content item".to_string(),
            ));
        }
        validate_vl_contents(self.reference, self.max_image_bytes, contents)?;
        let request_contents: Vec<serde_json::Value> = contents
            .iter()
            .map(|content| match content {
                EmbedContent::Text(text) => serde_json::json!({ "text": text }),
                EmbedContent::Image { data, .. } => serde_json::json!({
                    "image": base64_encode(data),
                }),
            })
            .collect();
        let body = serde_json::json!({
            "model": self.model,
            "input": { "contents": request_contents },
            "parameters": { "dimension": self.dimension },
        });
        let mut request = self.client.post(&self.endpoint).timeout(self.timeout);
        request = request.header("Authorization", format!("Bearer {}", self.api_key));
        request = request.header("Content-Type", "application/json");
        for (name, value) in &self.extra_headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let response = request.json(&body).send().await.map_err(|error| {
            EmbeddingError::RequestFailed(format!(
                "Qwen3 VL embedding request failed (model={} endpoint={} timeoutMs={}): {error}",
                self.reference,
                self.endpoint,
                self.timeout.as_millis()
            ))
        })?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body: serde_json::Value = response.json().await.map_err(|error| {
            EmbeddingError::RequestFailed(format!(
                "Qwen3 VL embedding response was not valid JSON (model={} status={status}): {error}",
                self.reference
            ))
        })?;
        if !(200..300).contains(&status) {
            let provider = read_provider_error(&body);
            return Err(EmbeddingError::RequestFailed(format!(
                "Qwen3 VL embedding request returned an error (model={} status={status} retryAfterMs={} providerCode={} providerType={} providerMessage={})",
                self.reference,
                retry_after_ms(retry_after.as_ref()).map_or("none".to_string(), |millis| millis.to_string()),
                provider.code,
                provider.kind,
                provider.message
            )));
        }
        let embeddings = body
            .get("output")
            .and_then(|output| output.get("embeddings"))
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                EmbeddingError::RequestFailed(format!(
                    "Qwen3 VL embedding response did not include embeddings (model={})",
                    self.reference
                ))
            })?;
        let mut vectors: Vec<Option<Vec<f32>>> = vec![None; contents.len()];
        for (fallback, item) in embeddings.iter().enumerate() {
            let item = item.as_object().ok_or_else(|| {
                EmbeddingError::RequestFailed(format!(
                    "Qwen3 VL embedding response included an invalid embedding item (model={} index={fallback})",
                    self.reference
                ))
            })?;
            let index = read_embedding_index(item, fallback);
            if index >= contents.len() {
                return Err(EmbeddingError::RequestFailed(format!(
                    "Qwen3 VL embedding response index was out of range (model={} index={index} inputCount={})",
                    self.reference,
                    contents.len()
                )));
            }
            let embedding = item
                .get("embedding")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    EmbeddingError::RequestFailed(format!(
                        "Qwen3 VL embedding response included an invalid embedding (model={} index={index})",
                        self.reference
                    ))
                })?;
            vectors[index] = Some(parse_float_vector(embedding, "Qwen3 VL embedding", self.reference)?);
        }
        let vectors = vectors
            .into_iter()
            .map(|vector| {
                vector.ok_or_else(|| {
                    EmbeddingError::RequestFailed(format!(
                        "Qwen3 VL embedding response included a non-array vector (model={})",
                        self.reference
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        for vector in &vectors {
            if vector.len() != self.dimension as usize {
                return Err(EmbeddingError::RequestFailed(format!(
                    "Qwen3 VL embedding response included a vector with the wrong dimension (model={})",
                    self.reference
                )));
            }
        }
        Ok(QwenEmbedResult {
            vectors,
            truncated: Vec::new(),
        })
    }
}

fn read_embedding_index(item: &serde_json::Map<String, serde_json::Value>, fallback: usize) -> usize {
    item.get("index")
        .and_then(serde_json::Value::as_u64)
        .map(|index| index as usize)
        .or_else(|| {
            item.get("text_index")
                .and_then(serde_json::Value::as_u64)
                .map(|index| index as usize)
        })
        .unwrap_or(fallback)
}

fn validate_vl_contents(
    reference: &str,
    max_image_bytes: u64,
    contents: &[EmbedContent],
) -> Result<(), EmbeddingError> {
    let mut images = 0;
    for (index, content) in contents.iter().enumerate() {
        match content {
            EmbedContent::Text(text) => {
                if text.trim().is_empty() {
                    return Err(EmbeddingError::RequestFailed(format!(
                        "Embedding text content must not be empty (model={reference} index={index})"
                    )));
                }
            }
            EmbedContent::Image { data, format } => {
                images += 1;
                if QwenImageFormat::parse(format).is_err() {
                    return Err(EmbeddingError::RequestFailed(format!(
                        "Qwen3 VL embedding model does not support image format (model={reference} index={index} format={format})"
                    )));
                }
                if data.is_empty() {
                    return Err(EmbeddingError::RequestFailed(format!(
                        "Embedding image content must not be empty (model={reference} index={index})"
                    )));
                }
                if data.len() as u64 > max_image_bytes {
                    return Err(EmbeddingError::RequestFailed(format!(
                        "Embedding image content exceeds model limit (model={reference} index={index})"
                    )));
                }
            }
        }
    }
    if images > QWEN3_VL_MAX_IMAGE_COUNT {
        return Err(EmbeddingError::RequestFailed(format!(
            "Qwen3 VL embedding image count exceeds model limit (model={reference} imageCount={images} maxImageCount={QWEN3_VL_MAX_IMAGE_COUNT})"
        )));
    }
    Ok(())
}

fn validate_endpoint_vl(endpoint: &str, reference: &str) -> Result<(), EmbeddingError> {
    crate::url::validate_https_or_local_http(endpoint).map_err(|_| {
        EmbeddingError::RequestFailed(format!(
            "Qwen3 VL embedding model requires an endpoint (model={reference})"
        ))
    })
}

fn base64_encode(bytes: &[u8]) -> String {
    const CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = *chunk.get(1).unwrap_or(&0);
        let third = *chunk.get(2).unwrap_or(&0);
        output.push(CHARS[(first >> 2) as usize] as char);
        output.push(CHARS[(((first & 3) << 4) | (second >> 4)) as usize] as char);
        output.push(if chunk.len() > 1 {
            CHARS[(((second & 15) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            CHARS[(third & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}

// ---------------------------------------------------------------------------
// EmbeddingProvider impls (document purpose; remote models never truncate).
// ---------------------------------------------------------------------------

fn qwen_batch(
    result: QwenEmbedResult,
    dimension: u32,
) -> (BatchEmbedding, Vec<usize>) {
    (
        BatchEmbedding {
            flat: result.vectors.iter().flatten().copied().collect(),
            count: result.vectors.len(),
            dims: dimension as usize,
        },
        result.truncated,
    )
}

#[async_trait]
impl EmbeddingProvider for QwenTextEmbeddingV4 {
    fn name(&self) -> &'static str {
        "qwen"
    }

    fn dimensions(&self) -> u32 {
        self.core.dimension
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        Ok(block_on_provider(self.core.embed_texts(&[text.to_string()]))?
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
        let result = block_on_provider(self.core.embed_texts(&owned))?;
        check_embed_batch_output(
            &BatchEmbedding {
                flat: result.vectors.iter().flatten().copied().collect(),
                count: result.vectors.len(),
                dims: self.core.dimension as usize,
            },
            texts.len(),
            self.core.dimension as usize,
            &result.truncated,
        )?;
        Ok(qwen_batch(result, self.core.dimension))
    }
}

#[async_trait]
impl EmbeddingProvider for Qwen37TextEmbedding {    fn name(&self) -> &'static str {
        "qwen"
    }

    fn dimensions(&self) -> u32 {
        self.core.dimension
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        Ok(block_on_provider(self.core.embed_texts(&[text.to_string()]))?
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
        let result = block_on_provider(self.core.embed_texts(&owned))?;
        check_embed_batch_output(
            &BatchEmbedding {
                flat: result.vectors.iter().flatten().copied().collect(),
                count: result.vectors.len(),
                dims: self.core.dimension as usize,
            },
            texts.len(),
            self.core.dimension as usize,
            &result.truncated,
        )?;
        Ok(qwen_batch(result, self.core.dimension))
    }
}

#[async_trait]
impl EmbeddingProvider for Qwen3VlEmbedding {
    fn name(&self) -> &'static str {
        "qwen"
    }

    fn dimensions(&self) -> u32 {
        self.dimension
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        Ok(
            block_on_provider(self.embed_contents(&[EmbedContent::Text(text.to_string())]))?
                .vectors
                .into_iter()
                .next()
                .unwrap_or_default(),
        )
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<BatchEmbedding, EmbeddingError> {
        Ok(self.embed_batch_with_truncation(texts)?.0)
    }

    fn embed_batch_with_truncation(
        &self,
        texts: &[&str],
    ) -> Result<(BatchEmbedding, Vec<usize>), EmbeddingError> {
        let contents: Vec<EmbedContent> = texts
            .iter()
            .map(|text| EmbedContent::Text((*text).to_string()))
            .collect();
        let result = block_on_provider(self.embed_contents(&contents))?;
        check_embed_batch_output(
            &BatchEmbedding {
                flat: result.vectors.iter().flatten().copied().collect(),
                count: result.vectors.len(),
                dims: self.dimension as usize,
            },
            texts.len(),
            self.dimension as usize,
            &result.truncated,
        )?;
        Ok(qwen_batch(result, self.dimension))
    }
}
