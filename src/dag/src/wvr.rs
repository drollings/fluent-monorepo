//! Fluent WVR integration for DAG crate.
//!
//! This module implements the `Component`, `WorkUnit`, `FieldAccess`, and
//! `Describable` traits from `fluent-wvr` for DAG-specific types.
//!
//! If you add a new DAG type that needs Fluent WVR integration, add it here.

pub use fluent_wvr::{
    Component, Describable, FieldAccess, FieldError, WorkContext, WorkError, WorkOutput, WorkUnit,
};
