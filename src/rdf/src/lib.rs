//! guidance-rdf: RDF/Turtle/N-Quads parser, lexer, normalizer, and
//! blank-node hashing for RDF dataset processing.

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
    clippy::byte_char_slices
)]

pub mod lexer;
pub mod normalize;
pub mod nquads;
pub mod parser;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum RdfError {
    #[error("unterminated IRI")]
    UnterminatedIRI,
    #[error("unterminated literal")]
    UnterminatedLiteral,
    #[error("invalid escape sequence")]
    InvalidEscape,
    #[error("unexpected character at line {line} col {col}")]
    UnexpectedChar { line: u32, col: u32 },
    #[error("unexpected EOF")]
    UnexpectedEOF,
    #[error("unexpected token at line {line} col {col}: expected {expected}, got {got}")]
    UnexpectedToken {
        line: u32,
        col: u32,
        expected: String,
        got: String,
    },
    #[error("invalid prefix declaration")]
    InvalidPrefix,
    #[error("out of memory")]
    OutOfMemory,
}

pub const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
pub const XSD_NS: &str = "http://www.w3.org/2001/XMLSchema#";
