//! guidance-search-vector: SQLite hybrid search engine — KNN vector search,
//! keyword search, RRF merge, quantized embeddings, and semantic aliases.

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
    clippy::byte_char_slices,
    clippy::too_many_arguments
)]

pub mod aliases;
pub mod db;
pub mod error;
pub mod math;

pub use aliases::SemanticAliases;
pub use db::GuidanceDb;
pub use math::QuantizedEmbedding;
