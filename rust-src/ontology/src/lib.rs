pub mod yago;
pub mod mapper;
pub mod inference;
pub mod migration;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum OntologyError {
    #[error("mapping error: {0}")]
    Mapping(String),
    #[error("inference error: {0}")]
    Inference(String),
    #[error("library error: {0}")]
    Library(#[from] guidance_coral::db::LibraryError),
}
