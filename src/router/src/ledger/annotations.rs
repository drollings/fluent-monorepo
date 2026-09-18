//! M10.4 migration shim: `AnnotationStore` + `ANNOTATIONS_TABLE` live in
//! `fluent_concept::annotation_store` — this module re-exports them so
//! existing `crate::ledger::annotations::…` paths keep working. The sqlite
//! home (DDL in `crate::ledger::ledger_migrations`) stays router-side.

pub use fluent_concept::annotation_store::{AnnotationStore, ANNOTATIONS_TABLE};
