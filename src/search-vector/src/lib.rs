#![forbid(unsafe_code)]

//! search-vector: generic node-document store plus KNN vector search.
//!
//! Scalar kernels live in the dependency-free `vector_math` leaf crate
//! and the `crate::math` owner — import those directly. Embedding
//! math, quantization, and top-K selection compose `crate::math`.
//! No vector kernel is implemented here twice.
//!
//! Guidance-specific schema (fragment/capability/summary/answer/graph
//! tables), RRF fusion surface, and token/alias helpers live in
//! `guidance-core::query` now; this crate keeps the reusable generic
//! store, the salience math, and the vector-index harness.

pub mod db;
pub mod hnsw;
pub mod math;
pub mod salience;
pub mod vector_index;
pub mod vector_store;

pub use math::QuantizedEmbedding;
pub use salience::{pagerank, NodeGraph, PAGERANK_ALPHA, PAGERANK_ITERATIONS};
pub use vector_index::{BruteForceStore, HnswVectorIndex, VectorStore};
pub use vector_store::{SearchResult, VectorDbError, VectorDocStore};

#[cfg(test)]
#[path = "../tests/cleanup_baseline.rs"]
mod cleanup_baseline_tests;

#[cfg(test)]
#[path = "../tests/canonical_paths.rs"]
mod canonical_paths_tests;
