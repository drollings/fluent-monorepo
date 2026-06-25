//! Re-export of `BitSetDrift` from `common-core`.
//!
//! `BitSetDrift` was promoted to `common_core::drift` so that any
//! crate in the workspace can use it without depending on `fluent-dag`.
//! This module exists for backward compatibility with
//! `dag::drift::BitSetDrift`; new code should reference
//! `common_core::drift::BitSetDrift` directly.

pub use common_core::drift::BitSetDrift;
