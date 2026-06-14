//! guidance-llm: LLM HTTP client provider — embeddings, chat completions,
//! prompt utilities, context packing, and request queueing.

#![deny(warnings, clippy::all, clippy::pedantic)]
#![allow(
    clippy::module_name_repetitions,
    clippy::must_use_candidate,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::too_many_lines,
    clippy::large_stack_arrays,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::zero_sized_map_values,
    clippy::unnecessary_literal_bound,
    clippy::cast_possible_wrap,
    clippy::unreadable_literal,
    clippy::similar_names,
    clippy::single_char_pattern,
    clippy::byte_char_slices
)]

pub mod anonymize;
pub mod client;
pub mod constants;
pub mod context_packer;
pub mod decomposer;
pub mod embeddings;
pub mod error;
pub mod llm_queue;
pub mod url;

pub use anonymize::anonymize;
pub use client::{
    chat_complete_http, extract_comment_tag, is_blank_or_plausible, is_malformed_response,
    model_name, strip_preamble, strip_think_block, ChatMessage, LlmClient, LlmConfig, LlmError,
};
pub use constants::MAX_EMBEDDING_DIMENSIONS;
pub use context_packer::ContextPacker;
pub use decomposer::{DecomposerConfig, LocalDecomposer};
pub use embeddings::{
    create_embedding_provider, BatchEmbedding, EmbeddingError, EmbeddingProvider, NoopEmbedding,
    OllamaEmbedding, OpenAiEmbedding,
};
pub use error::EmbedError;
