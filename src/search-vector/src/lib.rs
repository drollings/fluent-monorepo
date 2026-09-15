#![forbid(unsafe_code)]

//! search-vector: SQLite hybrid search engine — KNN vector search,
//! keyword search, RRF merge, quantized embeddings, and semantic aliases.
//!
//! Scalar kernels re-export from the dependency-free `vector_math` leaf
//! crate (`math::cosine_similarity_f32`); embedding math, quantization,
//! and top-K selection compose `fluent_db::vector`. No vector kernel is
//! implemented here twice.

pub mod aliases;
pub mod db;
pub mod fusion;
pub mod math;
pub mod tokens;

pub use aliases::SemanticAliases;
pub use db::GuidanceDb;
pub use fusion::{
    apply_boosts, rrf_fuse, score_postings, FusedHit, RrfScore, CALL_BOOST, DEPENDENT_BOOST,
    ROLE_EPSILON, SAME_FILE_BOOST, SCOPE_BOOST,
};
pub use tokens::{lower_dedup_push, split_tokens, symbol_name_from_token, symbol_names};
pub use fluent_db::vector::QuantizedEmbedding;
