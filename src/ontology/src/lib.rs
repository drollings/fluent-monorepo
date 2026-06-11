//! guidance-ontology: Entity extraction, capability inference, and YAGO
//! taxonomy integration for semantic knowledge representation.

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

pub mod entity;
pub mod inference;
pub mod mapper;
pub mod migration;
pub mod yago;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum OntologyError {
    #[error("mapping error: {0}")]
    Mapping(String),
    #[error("inference error: {0}")]
    Inference(String),
}
