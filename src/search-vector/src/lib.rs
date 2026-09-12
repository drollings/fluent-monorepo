#![forbid(unsafe_code)]

//! search-vector: SQLite hybrid search engine — KNN vector search,
//! keyword search, RRF merge, quantized embeddings, and semantic aliases.
//!
//! NOTE (M4): the `math` / `error` re-export shims are deleted. Canonical
//! homes are `fluent_db::vector` (embedding math, `QuantizedEmbedding`,
//! `distance_to_similarity`, `rrf_merge`) and `fluent_db::error::DbError`.
//! Do not reintroduce them here (enforced by the vector-check lint, M8);
//! `spacy-rs` consumes vector math from `common_core::vector_math` only.

pub mod aliases;
pub mod db;
pub mod fusion;
pub mod tokens;

pub use aliases::SemanticAliases;
pub use db::GuidanceDb;
pub use fusion::{
    apply_boosts, rrf_fuse, score_postings, FusedHit, RrfScore, CALL_BOOST, DEPENDENT_BOOST,
    ROLE_EPSILON, SAME_FILE_BOOST, SCOPE_BOOST,
};
pub use tokens::{lower_dedup_push, split_tokens, symbol_name_from_token, symbol_names};
pub use fluent_db::vector::QuantizedEmbedding;
