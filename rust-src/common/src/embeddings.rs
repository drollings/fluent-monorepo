use async_trait::async_trait;
use crate::url::validate_https_or_local_http;

#[derive(Debug, thiserror::Error)]
pub enum EmbeddingError {
    #[error("unknown provider: {0}")]
    UnknownProvider(String),
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
}

impl OllamaEmbedding {
    pub fn new(model: Option<&str>, base_url: Option<&str>, dims: u32) -> Result<Self, EmbeddingError> {
        let base_url = base_url
            .unwrap_or("http://localhost:11434")
            .to_string();
        validate_url(&base_url)?;
        Ok(Self {
            base_url,
            model: model.unwrap_or("nomic-embed-text").to_string(),
            dims,
        })
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
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let body = serde_json::json!({
            "model": self.model,
            "input": text,
        });
        let resp_bytes = do_embed_request(&self.base_url, "api/embed", &body.to_string())?;
        parse_ollama_response(&resp_bytes)
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<BatchEmbedding, EmbeddingError> {
        let inputs: Vec<serde_json::Value> = texts.iter().map(|t| serde_json::Value::String(t.to_string())).collect();
        let body = serde_json::json!({
            "model": self.model,
            "input": inputs,
        });
        let resp_bytes = do_embed_request(&self.base_url, "api/embed", &body.to_string())?;
        parse_ollama_batch_response(&resp_bytes)
    }

    async fn embed_async(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let body = serde_json::json!({
            "model": self.model.clone(),
            "input": text,
        });
        let resp_bytes = do_embed_request_async(self.base_url.clone(), "api/embed".into(), body.to_string()).await?;
        parse_ollama_response(&resp_bytes)
    }

    async fn embed_batch_async(&self, texts: &[&str]) -> Result<BatchEmbedding, EmbeddingError> {
        let inputs: Vec<serde_json::Value> = texts.iter().map(|t| serde_json::Value::String(t.to_string())).collect();
        let body = serde_json::json!({
            "model": self.model.clone(),
            "input": inputs,
        });
        let resp_bytes = do_embed_request_async(self.base_url.clone(), "api/embed".into(), body.to_string()).await?;
        parse_ollama_batch_response(&resp_bytes)
    }
}

pub struct OpenAiEmbedding {
    base_url: String,
    api_key: String,
    model: String,
    dims: u32,
}

impl OpenAiEmbedding {
    pub fn new(
        model: Option<&str>,
        base_url: Option<&str>,
        api_key: Option<&str>,
        dims: u32,
    ) -> Result<Self, EmbeddingError> {
        let base_url = base_url
            .unwrap_or("https://api.openai.com/v1")
            .to_string();
        let api_key = api_key.ok_or(EmbeddingError::NoApiKey)?;
        validate_url(&base_url)?;
        Ok(Self {
            base_url,
            api_key: api_key.to_string(),
            model: model.unwrap_or("text-embedding-3-small").to_string(),
            dims,
        })
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
        let body = serde_json::json!({
            "model": self.model,
            "input": text,
        });
        let resp_bytes = do_openai_request(&self.embeddings_url(), &self.api_key, &body.to_string())?;
        parse_openai_response(&resp_bytes)
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<BatchEmbedding, EmbeddingError> {
        let inputs: Vec<serde_json::Value> = texts.iter().map(|t| serde_json::Value::String(t.to_string())).collect();
        let body = serde_json::json!({
            "model": self.model,
            "input": inputs,
        });
        let resp_bytes = do_openai_request(&self.embeddings_url(), &self.api_key, &body.to_string())?;
        parse_openai_batch_response(&resp_bytes)
    }

    async fn embed_async(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let body = serde_json::json!({
            "model": self.model.clone(),
            "input": text,
        });
        let resp_bytes = do_openai_request_async(self.embeddings_url(), self.api_key.clone(), body.to_string()).await?;
        parse_openai_response(&resp_bytes)
    }

    async fn embed_batch_async(&self, texts: &[&str]) -> Result<BatchEmbedding, EmbeddingError> {
        let inputs: Vec<serde_json::Value> = texts.iter().map(|t| serde_json::Value::String(t.to_string())).collect();
        let body = serde_json::json!({
            "model": self.model.clone(),
            "input": inputs,
        });
        let resp_bytes = do_openai_request_async(self.embeddings_url(), self.api_key.clone(), body.to_string()).await?;
        parse_openai_batch_response(&resp_bytes)
    }
}

async fn do_embed_request_async(base_url: String, path: String, body_str: String) -> Result<Vec<u8>, EmbeddingError> {
    tokio::task::spawn_blocking(move || do_embed_request(&base_url, &path, &body_str))
        .await
        .map_err(|e| EmbeddingError::RequestFailed(e.to_string()))?
}

async fn do_openai_request_async(url: String, api_key: String, body_str: String) -> Result<Vec<u8>, EmbeddingError> {
    tokio::task::spawn_blocking(move || do_openai_request(&url, &api_key, &body_str))
        .await
        .map_err(|e| EmbeddingError::RequestFailed(e.to_string()))?
}

fn do_embed_request(base_url: &str, path: &str, body_str: &str) -> Result<Vec<u8>, EmbeddingError> {
    let url = format!("{}/{}", base_url.trim_end_matches('/'), path);
    let body: serde_json::Value = serde_json::from_str(body_str)
        .map_err(|e| EmbeddingError::RequestFailed(e.to_string()))?;
    let mut resp = ureq::post(&url)
        .header("Content-Type", "application/json")
        .send_json(&body)
        .map_err(|e| EmbeddingError::RequestFailed(e.to_string()))?;
    if resp.status().as_u16() >= 400 {
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        return Err(EmbeddingError::RequestFailed(format!("HTTP {}: {}", resp.status(), text)));
    }
    let bytes = resp.body_mut().read_to_vec()
        .map_err(|e| EmbeddingError::RequestFailed(e.to_string()))?;
    Ok(bytes)
}

fn do_openai_request(url: &str, api_key: &str, body_str: &str) -> Result<Vec<u8>, EmbeddingError> {
    let body: serde_json::Value = serde_json::from_str(body_str)
        .map_err(|e| EmbeddingError::RequestFailed(e.to_string()))?;
    let mut resp = ureq::post(url)
        .header("Content-Type", "application/json")
        .header("Authorization", &format!("Bearer {api_key}"))
        .send_json(&body)
        .map_err(|e| EmbeddingError::RequestFailed(e.to_string()))?;
    if resp.status().as_u16() >= 400 {
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        return Err(EmbeddingError::RequestFailed(format!("HTTP {}: {}", resp.status(), text)));
    }
    let bytes = resp.body_mut().read_to_vec()
        .map_err(|e| EmbeddingError::RequestFailed(e.to_string()))?;
    Ok(bytes)
}

pub fn parse_ollama_response(json: &[u8]) -> Result<Vec<f32>, EmbeddingError> {
    let v: serde_json::Value = serde_json::from_slice(json)
        .map_err(|e| EmbeddingError::ParseError(e.to_string()))?;
    let embeddings = v.get("embeddings")
        .and_then(|e| e.as_array())
        .ok_or_else(|| EmbeddingError::ParseError("missing embeddings array".into()))?;
    let first = embeddings.first()
        .ok_or_else(|| EmbeddingError::ParseError("empty embeddings array".into()))?;
    let arr = first.as_array()
        .ok_or_else(|| EmbeddingError::ParseError("embedding is not an array".into()))?;
    let vec: Vec<f32> = arr.iter()
        .map(|x| x.as_f64().ok_or_else(|| EmbeddingError::ParseError("non-float in embedding".into())))
        .collect::<Result<Vec<_>, _>>()?
        .iter().map(|&x| x as f32)
        .collect();
    Ok(vec)
}

pub fn parse_ollama_batch_response(json: &[u8]) -> Result<BatchEmbedding, EmbeddingError> {
    let v: serde_json::Value = serde_json::from_slice(json)
        .map_err(|e| EmbeddingError::ParseError(e.to_string()))?;
    let embeddings = v.get("embeddings")
        .and_then(|e| e.as_array())
        .ok_or_else(|| EmbeddingError::ParseError("missing embeddings array".into()))?;
    let count = embeddings.len();
    if count == 0 {
        return Ok(BatchEmbedding { flat: vec![], count: 0, dims: 0 });
    }
    let dims = embeddings[0].as_array()
        .ok_or_else(|| EmbeddingError::ParseError("embedding is not an array".into()))?
        .len();
    let mut flat = Vec::with_capacity(count * dims);
    for emb in embeddings {
        let arr = emb.as_array()
            .ok_or_else(|| EmbeddingError::ParseError("embedding is not an array".into()))?;
        for val in arr {
            let f = val.as_f64()
                .ok_or_else(|| EmbeddingError::ParseError("non-float in embedding".into()))? as f32;
            flat.push(f);
        }
    }
    Ok(BatchEmbedding { flat, count, dims })
}

pub fn parse_openai_response(json: &[u8]) -> Result<Vec<f32>, EmbeddingError> {
    let v: serde_json::Value = serde_json::from_slice(json)
        .map_err(|e| EmbeddingError::ParseError(e.to_string()))?;
    let data = v.get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| EmbeddingError::ParseError("missing data array".into()))?;
    let first = data.first()
        .ok_or_else(|| EmbeddingError::ParseError("empty data array".into()))?;
    let embedding = first.get("embedding")
        .and_then(|e| e.as_array())
        .ok_or_else(|| EmbeddingError::ParseError("missing embedding field".into()))?;
    let vec: Vec<f32> = embedding.iter()
        .map(|x| x.as_f64().map(|v| v as f32).ok_or_else(|| EmbeddingError::ParseError("non-float in embedding".into())))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(vec)
}

pub fn parse_openai_batch_response(json: &[u8]) -> Result<BatchEmbedding, EmbeddingError> {
    let v: serde_json::Value = serde_json::from_slice(json)
        .map_err(|e| EmbeddingError::ParseError(e.to_string()))?;
    let data = v.get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| EmbeddingError::ParseError("missing data array".into()))?;
    let count = data.len();
    if count == 0 {
        return Ok(BatchEmbedding { flat: vec![], count: 0, dims: 0 });
    }
    let dims = data[0].get("embedding")
        .and_then(|e| e.as_array())
        .ok_or_else(|| EmbeddingError::ParseError("missing embedding field".into()))?
        .len();
    let mut flat = Vec::with_capacity(count * dims);
    for entry in data {
        let embedding = entry.get("embedding")
            .and_then(|e| e.as_array())
            .ok_or_else(|| EmbeddingError::ParseError("missing embedding field".into()))?;
        for val in embedding {
            let f = val.as_f64()
                .ok_or_else(|| EmbeddingError::ParseError("non-float in embedding".into()))? as f32;
            flat.push(f);
        }
    }
    Ok(BatchEmbedding { flat, count, dims })
}

fn validate_url(url: &str) -> Result<(), EmbeddingError> {
    validate_https_or_local_http(url).map_err(|_| EmbeddingError::InvalidApiUrl)
}

pub fn create_embedding_provider(
    name: &str,
    model: Option<&str>,
    base_url: Option<&str>,
    api_key: Option<&str>,
    dims: u32,
) -> Result<Box<dyn EmbeddingProvider>, EmbeddingError> {
    match name {
        "none" => Ok(Box::new(NoopEmbedding::new(dims))),
        "ollama" => Ok(Box::new(OllamaEmbedding::new(model, base_url, dims)?)),
        "openai" => Ok(Box::new(OpenAiEmbedding::new(model, base_url, api_key, dims)?)),
        _ => {
            if let Some(ollama_model) = name.strip_prefix("ollama:") {
                return Ok(Box::new(OllamaEmbedding::new(Some(ollama_model), base_url, dims)?));
            }
            if let Some(custom_url) = name.strip_prefix("custom:") {
                return Ok(Box::new(OpenAiEmbedding::new(model, Some(custom_url), api_key, dims)?));
            }
            Err(EmbeddingError::UnknownProvider(name.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_hash_with_model;

    #[test]
    fn create_noop_provider() {
        let p = create_embedding_provider("none", None, None, None, 768).unwrap();
        assert_eq!(p.name(), "none");
        assert_eq!(p.dimensions(), 768);
    }

    #[test]
    fn create_unknown_provider() {
        let result = create_embedding_provider("bogus", None, None, None, 0);
        assert!(result.is_err());
    }

    #[test]
    fn noop_embedding_returns_empty() {
        let p = NoopEmbedding::new(768);
        let vec = p.embed("hello").unwrap();
        assert!(vec.is_empty());
    }

    #[test]
    fn noop_embed_batch() {
        let p = NoopEmbedding::new(768);
        let batch = p.embed_batch(&[]).unwrap();
        assert_eq!(batch.count, 0);
        assert_eq!(batch.dims, 0);
        assert!(batch.flat.is_empty());

        let batch = p.embed_batch(&["a"]).unwrap();
        assert_eq!(batch.count, 1);
        assert_eq!(batch.dims, 0);

        let batch = p.embed_batch(&["a", "b", "c"]).unwrap();
        assert_eq!(batch.count, 3);
    }

    #[test]
    fn batch_embedding_vector_access() {
        let batch = BatchEmbedding {
            flat: vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
            count: 2,
            dims: 3,
        };
        assert_eq!(batch.vector(0), &[0.1, 0.2, 0.3]);
        assert_eq!(batch.vector(1), &[0.4, 0.5, 0.6]);
    }

    #[test]
    fn ollama_embedding_init() {
        let p = OllamaEmbedding::new(Some("llama3"), Some("http://localhost:11434"), 4096).unwrap();
        assert_eq!(p.name(), "ollama");
    }

    #[test]
    fn ollama_rejects_insecure_remote() {
        let result = OllamaEmbedding::new(None, Some("http://evil.com"), 0);
        assert!(result.is_err());
    }

    #[test]
    fn parse_ollama_response_valid() {
        let json = br#"{"embeddings": [[0.1, 0.2, 0.3]]}"#;
        let vec = parse_ollama_response(json).unwrap();
        assert_eq!(vec.len(), 3);
        assert!((vec[0] - 0.1).abs() < 1e-6);
    }

    #[test]
    fn parse_ollama_response_truncated_json() {
        let json = br#"{"embeddings": ["#;
        let result = parse_ollama_response(json);
        assert!(result.is_err());
    }

    #[test]
    fn parse_ollama_response_wrong_structure() {
        let json = br#"{"foo": "bar"}"#;
        let result = parse_ollama_response(json);
        assert!(result.is_err());
    }

    #[test]
    fn parse_ollama_batch_response_valid() {
        let json = br#"{"embeddings": [[0.1, 0.2], [0.3, 0.4]]}"#;
        let batch = parse_ollama_batch_response(json).unwrap();
        assert_eq!(batch.count, 2);
        assert_eq!(batch.dims, 2);
        assert_eq!(batch.flat.len(), 4);
        assert!((batch.vector(0)[0] - 0.1).abs() < 1e-6);
        assert!((batch.vector(1)[1] - 0.4).abs() < 1e-6);
    }

    #[test]
    fn parse_openai_response_valid() {
        let json = br#"{"data": [{"embedding": [0.1, 0.2, 0.3], "index": 0}]}"#;
        let vec = parse_openai_response(json).unwrap();
        assert_eq!(vec.len(), 3);
    }

    #[test]
    fn parse_openai_response_truncated_json() {
        let json = br#"{"data": ["#;
        let result = parse_openai_response(json);
        assert!(result.is_err());
    }

    #[test]
    fn parse_openai_batch_response_valid() {
        let json = br#"{"data": [{"embedding": [0.1, 0.2], "index": 0}, {"embedding": [0.3, 0.4], "index": 1}]}"#;
        let batch = parse_openai_batch_response(json).unwrap();
        assert_eq!(batch.count, 2);
        assert_eq!(batch.dims, 2);
        assert_eq!(batch.flat.len(), 4);
    }

    #[test]
    fn factory_ollama_prefix() {
        let p = create_embedding_provider("ollama:llama3", None, Some("http://localhost:11434"), None, 4096).unwrap();
        assert_eq!(p.name(), "ollama");
    }

    #[test]
    fn factory_custom_prefix() {
        let p = create_embedding_provider(
            "custom:http://localhost:8080",
            None,
            None,
            Some("sk-test"),
            768,
        ).unwrap();
        assert_eq!(p.name(), "openai");
    }

    #[test]
    fn openai_embeddings_url_standard() {
        let e = OpenAiEmbedding::new(None, Some("https://api.openai.com/v1"), Some("sk-test"), 768).unwrap();
        assert_eq!(e.embeddings_url(), "https://api.openai.com/v1/embeddings");
    }

    #[test]
    fn openai_embeddings_url_already_embeddings() {
        let e = OpenAiEmbedding::new(None, Some("https://api.openai.com/v1/embeddings"), Some("sk-test"), 768).unwrap();
        assert_eq!(e.embeddings_url(), "https://api.openai.com/v1/embeddings");
    }

    #[test]
    fn openai_embeddings_url_custom_path() {
        let e = OpenAiEmbedding::new(None, Some("https://my-server.com/custom/path"), Some("sk-test"), 768).unwrap();
        assert_eq!(e.embeddings_url(), "https://my-server.com/custom/path/embeddings");
    }

    #[test]
    fn content_hash_deterministic_and_model_sensitive() {
        let h1 = content_hash_with_model("text", "model-a");
        let h2 = content_hash_with_model("text", "model-a");
        assert_eq!(h1, h2);
        let h3 = content_hash_with_model("text", "model-b");
        assert_ne!(h1, h3);
    }

    #[test]
    fn empty_text_returns_empty_skip_http() {
        let p = OllamaEmbedding::new(None, Some("http://localhost:11434"), 768).unwrap();
        let vec = p.embed("").unwrap();
        assert!(vec.is_empty());
    }

    #[test]
    fn ollama_embed_with_mock_http() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/api/embed");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(r#"{"embeddings": [[0.1, 0.2, 0.3]]}"#);
        });
        let p = OllamaEmbedding::new(Some("test"), Some(&server.url("")), 3).unwrap();
        let vec = p.embed("hello").unwrap();
        assert_eq!(vec.len(), 3);
        assert!((vec[0] - 0.1).abs() < 1e-6);
        mock.assert();
    }

    #[test]
    fn ollama_embed_http_error() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/api/embed");
            then.status(500)
                .body("Internal Server Error");
        });
        let p = OllamaEmbedding::new(Some("test"), Some(&server.url("")), 3).unwrap();
        let result = p.embed("hello");
        assert!(result.is_err());
        mock.assert();
    }

    #[test]
    fn ollama_embed_batch_with_mock_http() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/api/embed");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(r#"{"embeddings": [[0.1, 0.2], [0.3, 0.4]]}"#);
        });
        let p = OllamaEmbedding::new(Some("test"), Some(&server.url("")), 2).unwrap();
        let batch = p.embed_batch(&["a", "b"]).unwrap();
        assert_eq!(batch.count, 2);
        assert_eq!(batch.dims, 2);
        mock.assert();
    }

    #[test]
    fn openai_embed_with_mock_http() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/embeddings")
                .header("Authorization", "Bearer sk-test");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(r#"{"data": [{"embedding": [0.1, 0.2, 0.3], "index": 0}]}"#);
        });
        let p = OpenAiEmbedding::new(
            Some("text-embedding-3-small"),
            Some(&server.url("")),
            Some("sk-test"),
            3,
        ).unwrap();
        let vec = p.embed("hello").unwrap();
        assert_eq!(vec.len(), 3);
        mock.assert();
    }

    #[test]
    fn openai_embed_http_error() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/embeddings");
            then.status(401)
                .body("Unauthorized");
        });
        let p = OpenAiEmbedding::new(
            None,
            Some(&server.url("")),
            Some("sk-bad"),
            3,
        ).unwrap();
        let result = p.embed("hello");
        assert!(result.is_err());
        mock.assert();
    }

    #[test]
    fn openai_embed_batch_with_mock_http() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/embeddings");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(r#"{"data": [{"embedding": [0.1, 0.2], "index": 0}, {"embedding": [0.3, 0.4], "index": 1}]}"#);
        });
        let p = OpenAiEmbedding::new(
            None,
            Some(&server.url("")),
            Some("sk-test"),
            2,
        ).unwrap();
        let batch = p.embed_batch(&["a", "b"]).unwrap();
        assert_eq!(batch.count, 2);
        assert_eq!(batch.dims, 2);
        mock.assert();
    }

    #[test]
    fn parse_ollama_batch_empty() {
        let json = br#"{"embeddings": []}"#;
        let batch = parse_ollama_batch_response(json).unwrap();
        assert_eq!(batch.count, 0);
        assert_eq!(batch.dims, 0);
        assert!(batch.flat.is_empty());
    }

    #[test]
    fn parse_openai_batch_empty() {
        let json = br#"{"data": []}"#;
        let batch = parse_openai_batch_response(json).unwrap();
        assert_eq!(batch.count, 0);
        assert_eq!(batch.dims, 0);
        assert!(batch.flat.is_empty());
    }

    #[tokio::test]
    async fn test_ollama_embed_async_with_mock() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/api/embed");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(r#"{"embeddings": [[0.1, 0.2, 0.3]]}"#);
        });
        let p = OllamaEmbedding::new(Some("test"), Some(&server.url("")), 3).unwrap();
        let vec = p.embed_async("hello").await.unwrap();
        assert_eq!(vec.len(), 3);
        mock.assert();
    }

    #[tokio::test]
    async fn test_openai_embed_async_with_mock() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/embeddings")
                .header("Authorization", "Bearer sk-test");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(r#"{"data": [{"embedding": [0.1, 0.2, 0.3], "index": 0}]}"#);
        });
        let p = OpenAiEmbedding::new(
            Some("text-embedding-3-small"),
            Some(&server.url("")),
            Some("sk-test"),
            3,
        ).unwrap();
        let vec = p.embed_async("hello").await.unwrap();
        assert_eq!(vec.len(), 3);
        mock.assert();
    }
}
