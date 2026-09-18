#![forbid(unsafe_code)]

//! search-vector: SQLite hybrid search engine — KNN vector search,
//! keyword search, RRF merge, quantized embeddings, and semantic aliases.
//!
//! Scalar kernels live in the dependency-free `vector_math` leaf crate
//! and the `fluent_db::vector` owner — import those directly. Embedding
//! math, quantization, and top-K selection compose `fluent_db::vector`.
//! No vector kernel is implemented here twice.

pub mod aliases;
pub mod db;
pub mod fusion;
pub mod salience;
pub mod tokens;
pub mod vector_index;

pub use aliases::SemanticAliases;
pub use db::GuidanceDb;
pub use fluent_db::vector::QuantizedEmbedding;
pub use fusion::{
    apply_boosts, rrf_fuse, score_postings, FusedHit, RrfScore, CALL_BOOST, DEPENDENT_BOOST,
    ROLE_EPSILON, SAME_FILE_BOOST, SCOPE_BOOST,
};
pub use salience::{pagerank, NodeGraph, PAGERANK_ALPHA, PAGERANK_ITERATIONS};
pub use tokens::{lower_dedup_push, split_tokens, symbol_name_from_token, symbol_names};
#[allow(deprecated)]
pub use vector_index::VectorIndex;
pub use vector_index::{BruteForceIndex, HnswVectorIndex, VectorStore};
