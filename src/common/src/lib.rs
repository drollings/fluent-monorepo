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
    clippy::non_std_lazy_statics,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::zero_sized_map_values,
    clippy::unnecessary_literal_bound,
    clippy::cast_possible_wrap,
    clippy::unreadable_literal,
    clippy::similar_names,
    clippy::single_char_pattern,
    clippy::byte_char_slices,
)]
pub mod builder_error;
pub mod constants;
pub mod content_node;
pub mod csr_graph;
pub mod drift;
pub mod embeddings;
pub mod entity;
pub mod error;
pub mod error_context;
pub mod file_lock;
pub mod format;
pub mod freq_table;
pub mod frozen_snapshot;
pub mod hash;
pub mod index_header;
pub mod interner;
pub mod io;
pub mod metrics;
pub mod pattern;
pub mod query_cache;
pub mod registry;
pub mod shell;
pub mod shell_parser;
pub mod source;
pub mod string;
pub mod terminal;
pub mod tokenizer;
pub mod traits;
pub mod trigram_index;
pub mod type_inference;
pub mod types;
pub mod url;
pub mod word_index;
pub mod wrapper;

#[allow(deprecated)]
pub use builder_error::{BuilderError, Phase};
pub use constants::*;
pub use content_node::{generate_lod_slices, ContentNode};
pub use csr_graph::CsrGraph;
pub use drift::BitSetDrift;
pub use embeddings::{
    create_embedding_provider, parse_ollama_batch_response, parse_ollama_response,
    parse_openai_batch_response, parse_openai_response, BatchEmbedding, EmbeddingProvider,
    NoopEmbedding, OllamaEmbedding, OpenAiEmbedding,
};
pub use entity::{extract_entities, EntityFreq, EntityType};
pub use error_context::{ErrorContext, HeapErrorContext};
pub use file_lock::FileLock;
pub use format::{format_csv, format_json, format_size, parse_size, Column, Table};
pub use freq_table::{build_frequency_table, default_frequency_table, pair_weight};
pub use frozen_snapshot::FrozenSnapshot;
pub use hash::{
    blake3_hash, blake3_hex, content_hash_with_model, fnv1a64, hash_batch, hash_file, sha256_hex,
    BatchHashResult, HashAlgorithm, HashState,
};
pub use index_header::Header as IndexHeader;
pub use io::{make_path_absolute, read_file_alloc, read_file_alloc_err, resolve_path, strip_path_prefix};
pub use metrics::LatencyHistogram;
pub use pattern::{
    detect_decorator, detect_proxy, detect_ring_buffer, detect_strategy, detect_template_method,
    Pattern, PatternType,
};
pub use query_cache::QueryCache;
pub use source::{extract_excerpt, extract_simple_excerpt, NodeType};
pub use string::{
    contains_any, contains_any_word, contains_ident_word, contains_ignore_case, contains_word,
    first_comment_line, has_extension, is_noisy_comment, is_path_token, is_test_path,
    lang_from_path, looks_like_identifier, lower_into, slugify, strip_boilerplate, strip_nl_prefix,
    trim_left, trim_right, truncate_at_sentence, STOP_WORDS,
};
pub use terminal::{get_terminal_height, get_terminal_width, is_terminal, Color, ProgressBar};
pub use types::{
    ASTAnalysis, CapabilityEval, ContextNode, EdgeType, ExecutorKind, FileMatch, FileType,
    GraphNode, GuidanceDoc, GuidanceInfo, KnnHit, Member, MemberType, Meta, NodeId, Param,
    QueryResult, SessionId, Skill, Stage, StageKind, SyncResult, TargetId, TargetType, WasmTool,
};
pub use url::{is_local_host, is_private_ip, validate_https_or_local_http, UrlError};
pub use wrapper::{retry_call, wrap_if, Instrumented, Pipeline, RetryResult, WithRetry, WrapperKind};
pub use error::{RegistryError, EmbedError, IoError, ResolverError, DbError, CacheError};
pub use traits::{Describable, FieldAccess, FieldError, WorkContext, WorkError, WorkOutput, WorkUnit};
pub use interner::CapabilityRegistry;
pub use registry::{Target, TargetRegistry};
