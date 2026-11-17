//! The canonical-form table lives in
//! `fluent_concept::canonical` — this module re-exports it so existing
//! `crate::overlay::canonical::…` paths (including the binary's
//! `ENTITY_ROOT_IRI` reference) keep working. New code imports from
//! `fluent-concept` directly.

pub use fluent_concept::canonical::{CanonicalFormError, CanonicalFormTable, ENTITY_ROOT_IRI};
