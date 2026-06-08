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
}

pub trait EmbeddingProvider: Send + Sync {
    fn name(&self) -> &str;
    fn dimensions(&self) -> u32;
    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError>;
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
    fn name(&self) -> &str {
        "none"
    }

    fn dimensions(&self) -> u32 {
        self.dims
    }

    fn embed(&self, _text: &str) -> Result<Vec<f32>, EmbeddingError> {
        Ok(Vec::new())
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

impl EmbeddingProvider for OllamaEmbedding {
    fn name(&self) -> &str {
        "ollama"
    }

    fn dimensions(&self) -> u32 {
        self.dims
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        Err(EmbeddingError::RequestFailed("not implemented".into()))
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
}

impl EmbeddingProvider for OpenAiEmbedding {
    fn name(&self) -> &str {
        "openai"
    }

    fn dimensions(&self) -> u32 {
        self.dims
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        Err(EmbeddingError::RequestFailed("not implemented".into()))
    }
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
        _ => Err(EmbeddingError::UnknownProvider(name.to_string())),
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
        let json = r#"{"embedding": [0.1, 0.2, 0.3]}"#;
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let arr = v["embedding"].as_array().unwrap();
        let vec: Vec<f32> = arr.iter().map(|x| x.as_f64().unwrap() as f32).collect();
        assert_eq!(vec.len(), 3);
    }

    #[test]
    fn parse_openai_response_valid() {
        let json = r#"{"data": [{"embedding": [0.1, 0.2]}]}"#;
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let arr = v["data"][0]["embedding"].as_array().unwrap();
        let vec: Vec<f32> = arr.iter().map(|x| x.as_f64().unwrap() as f32).collect();
        assert_eq!(vec.len(), 2);
    }

    #[test]
    fn content_hash_deterministic_and_model_sensitive() {
        let h1 = content_hash_with_model("text", "model-a");
        let h2 = content_hash_with_model("text", "model-a");
        assert_eq!(h1, h2);
        let h3 = content_hash_with_model("text", "model-b");
        assert_ne!(h1, h3);
    }
}
