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
