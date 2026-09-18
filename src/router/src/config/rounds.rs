//! M3.3 migration shim: the canonical definitions live in
//! `fluent_types::rounds` — this module re-exports them so existing
//! `crate::config::{BoundedRounds, …}` paths keep working. New code imports
//! from `fluent_types` directly.

pub use fluent_types::rounds::{BoundedRounds, EscalationConfidence, SeverityThreshold};
