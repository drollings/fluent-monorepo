//! Read-plane types, readiness, and the intent-routed engine.
//!
//! `doc_cards` owns the single markdown chunker (`DocChunk`) plus the
//! external capability-corpus import. `project_map` owns the
//! deterministic workspace/crate/domain cards. Both are model-free:
//! every row derives from repository bytes plus the external corpus.
//! `engine` is the single DB-backed read path.

pub mod admission;
pub mod budget;
pub mod calibration;
pub mod claims;
pub mod decide;
pub mod doc_cards;
pub mod engine;
pub mod evidence;
pub mod intent;
pub mod planes;
pub mod project_map;
pub mod readiness;
pub mod render;

#[cfg(test)]
#[path = "../../tests/read_perf.rs"]
mod read_perf_tests;
pub mod vocabulary;
