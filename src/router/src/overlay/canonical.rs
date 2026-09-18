//! M9.2 migration shim: the canonical-form table lives in
//! `guidance_ontology::canonical` — this module re-exports it so existing
//! `crate::overlay::canonical::…` paths (including the binary's
//! `ENTITY_ROOT_IRI` reference) keep working. New code imports from
//! `guidance-ontology` directly.

pub use guidance_ontology::canonical::{CanonicalFormError, CanonicalFormTable, ENTITY_ROOT_IRI};
