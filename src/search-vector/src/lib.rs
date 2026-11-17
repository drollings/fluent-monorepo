#![forbid(unsafe_code)]

//! search-vector: generic node-document store plus KNN vector search.
//!
//! Scalar kernels live in the dependency-free `vector_math` leaf crate
//! and the `fluent_db::vector` owner — import those directly. Embedding
//! math, quantization, and top-K selection compose `fluent_db::vector`.
//! No vector kernel is implemented here twice.
//!
//! Guidance-specific schema (fragment/capability/summary/answer/graph
//! tables), RRF fusion surface, and token/alias helpers live in
//! `guidance-core::query` now; this crate keeps the reusable generic
//! store, the salience math, and the vector-index harness.

pub mod db;
pub mod salience;
pub mod vector_index;
pub mod vector_store;

pub use fluent_db::vector::QuantizedEmbedding;
pub use salience::{pagerank, NodeGraph, PAGERANK_ALPHA, PAGERANK_ITERATIONS};
pub use vector_index::{BruteForceStore, HnswVectorIndex, VectorStore};
pub use vector_store::{SearchResult, VectorDbError, VectorDocStore};
