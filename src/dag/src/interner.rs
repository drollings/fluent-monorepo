//! Re-export of `CapabilityRegistry` from `common-core`.
//!
//! `CapabilityRegistry` was promoted to `common_core::interner` so that any
//! crate in the workspace can use it without depending on `fluent-dag`.
//! This module exists for backward compatibility with
//! `dag::interner::CapabilityRegistry`; new code should reference
//! `common_core::interner::CapabilityRegistry` directly.

pub use common_core::interner::CapabilityRegistry;
