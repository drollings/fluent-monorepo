//! Re-export of `ComponentAdapter` from `fluent-wvr`.
//!
//! `ComponentAdapter` was moved to `fluent_wvr::wrapper` so that any
//! `Component` consumer in the workspace can use it without taking
//! a dependency on `fluent-dag`. This module exists for backward
//! compatibility with the previous `dag::adapter::ComponentAdapter`
//! path; new code should reference
//! `fluent_wvr::wrapper::ComponentAdapter` directly.

pub use fluent_wvr::wrapper::ComponentAdapter;
