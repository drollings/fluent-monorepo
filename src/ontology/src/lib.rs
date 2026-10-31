//! guidance-ontology: Entity extraction, capability inference, and YAGO
//! taxonomy integration for semantic knowledge representation.
#![forbid(unsafe_code)]

pub mod canonical;
pub mod entity;
pub mod inference;
pub mod mapper;
pub mod migration;
pub mod normalize;
pub mod plausibility;
pub mod prefix;
pub mod taxonomy;
pub(crate) mod turtle;
pub mod yago;
pub mod yago_loader;
pub mod yago_view;
pub mod ysm1;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum OntologyError {
    #[error("mapping error: {0}")]
    Mapping(String),
    #[error("inference error: {0}")]
    Inference(String),
    #[error("yago view error: {0}")]
    YagoView(String),
}

impl From<crate::mapper::MappingError> for OntologyError {
    fn from(e: crate::mapper::MappingError) -> Self {
        Self::Mapping(e.to_string())
    }
}

impl From<crate::inference::InferenceError> for OntologyError {
    fn from(e: crate::inference::InferenceError) -> Self {
        Self::Inference(e.to_string())
    }
}

impl From<crate::canonical::CanonicalFormError> for OntologyError {
    fn from(e: crate::canonical::CanonicalFormError) -> Self {
        Self::Mapping(e.to_string())
    }
}

pub use plausibility::{score_plausibility, PlausibilityInput};
pub use yago_loader::{canonical_class_name, yago_class_id, LoadStats, YaGoLoader};
pub use yago_view::YagoView;
