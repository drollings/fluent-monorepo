//! guidance-project-knowledge: Word/trigram index, CSR graph, frequency
//! tables, and tokenizer for project-level knowledge representation.

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

pub mod csr_graph;
pub mod freq_table;
pub mod index_header;
pub mod query_cache;
pub mod tokenizer;
pub mod trigram_index;
pub mod word_index;

pub use csr_graph::CsrGraph;
pub use freq_table::{build_frequency_table, default_frequency_table, pair_weight};
pub use index_header::Header as IndexHeader;
pub use query_cache::QueryCache;
pub use tokenizer::WordTokenizer;
pub use word_index::{DocRegistry, WordHit, WordIndex};
