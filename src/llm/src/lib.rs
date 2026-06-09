pub mod anonymize;
pub mod client;
pub mod constants;
pub mod context_packer;
pub mod decomposer;
pub mod embeddings;
pub mod error;
pub mod url;

pub use anonymize::anonymize;
pub use client::{
    extract_comment_tag, is_blank_or_plausible, is_malformed_response, model_name, strip_preamble,
    strip_think_block, ChatMessage, LlmClient, LlmConfig, LlmError,
};
pub use context_packer::ContextPacker;
pub use constants::MAX_EMBEDDING_DIMENSIONS;
pub use decomposer::{DecomposerConfig, LocalDecomposer};
pub use embeddings::{
    create_embedding_provider, BatchEmbedding, EmbeddingError, EmbeddingProvider, NoopEmbedding,
    OllamaEmbedding, OpenAiEmbedding,
};
pub use error::EmbedError;
