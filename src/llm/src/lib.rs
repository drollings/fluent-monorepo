pub mod anonymize;
pub mod client;
pub mod context_packer;
pub mod decomposer;
pub mod embeddings;

pub use anonymize::anonymize;
pub use client::{
    extract_comment_tag, is_blank_or_plausible, is_malformed_response, model_name, strip_preamble,
    strip_think_block, ChatMessage, LlmClient, LlmConfig, LlmError,
};
pub use context_packer::ContextPacker;
pub use decomposer::{DecomposerConfig, LocalDecomposer};
pub use embeddings::{
    create_embedding_provider, BatchEmbedding, EmbeddingError, EmbeddingProvider, NoopEmbedding,
    OllamaEmbedding, OpenAiEmbedding,
};
