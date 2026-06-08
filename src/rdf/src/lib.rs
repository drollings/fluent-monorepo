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
