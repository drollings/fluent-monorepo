//! Scalar vector-search kernels for `search-vector` consumers — a re-export
//! surface, not a reimplementation.
//!
//! Canonical home is the dependency-free `vector_math` leaf crate (one
//! kernel, zero weight), so this module stays light no matter which
//! features are enabled. Higher-level selection and fusion live in
//! `fluent_db::vector` (`top_k_by_score`) and `fusion` (`rrf_fuse`) —
//! never reimplemented here.

pub use vector_math::cosine_similarity_f32;
