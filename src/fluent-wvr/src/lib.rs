#![forbid(unsafe_code)]

//! ## Fluent WVR — Framework Trait Crate
//!
//! This is a **framework trait** crate — the Rust equivalent of a header-only
//! interface.  It defines the core `Component`, `WorkUnit`, `FieldAccess`, and
//! `Describable` traits that the DAG executor and Coral crates implement and
//! consume.
//!
//! **Design contract:**
//! - No implementation logic beyond blanket impls and helper types
//! - No domain-specific dependencies (no rusqlite, no LLM, no fluent-types)
//! - The thinness is intentional — value is in the trait boundaries
//! - If a derive macro (`#[derive(FieldAccess)]`) is added later, it goes here
//!
//! Consumers: `fluent-dag`, `coral-context`

extern crate self as fluent_wvr;

pub mod boundary;
pub mod capability;
pub mod coerce;
pub mod dynamic;
pub mod macros;
pub mod metadata;
pub mod runtime;
pub mod store;
pub mod traits;
pub mod validate;
pub mod work;

pub mod prelude;
pub mod wrapper;

pub use boundary::{
    decode_boundary, decode_boundary_typed, extract_members, repair_boundary, BoundaryError,
    BoundaryOptions, BoundaryRepair, BoundarySchema,
};
pub use capability::{
    check_capability, run_gated_blocking, BlockingJoinError, Capability, CapabilityError,
    CapabilitySet, FsCapability, CURRENT_CAPS,
};
pub use coerce::{
    coerce, escape_control_chars, escaped_char, literal_kind, parse_bool, parse_int, parse_number,
    split_top_level, strip_outer_quotes, Coercion, LiteralKind,
};
pub use dynamic::{DynamicComponent, DynamicExecutor};
pub use fluent_wvr_macros::{Describable, FieldAccess};
pub use internment::ArcIntern;
pub use metadata::MetadataValue;
pub use runtime::{NoopRuntime, Runtime};
pub use store::OutputStore;
pub use traits::{
    component_downcast_mut, component_downcast_ref, describe_work_unit, Component, ComponentArcExt,
    Describable, FieldAccess, FieldError, FieldSchema, SchemaProvider, WorkUnit,
};
pub use validate::{check_for_execute, valid_output, Validate};
pub use work::{WorkContext, WorkError, WorkOutput};

// Re-export string utilities from common-core for use in derive macros and consumers.
pub use common_core::string::{
    contains_ident_word, contains_ignore_case, contains_word, looks_like_identifier, slugify,
    strip_html, truncate_at_sentence,
};

use std::any::Any;
use std::sync::Arc;

impl WorkUnit for Arc<dyn WorkUnit> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        (**self).depends()
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        (**self).provides()
    }
    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        (**self).execute(ctx)
    }
    fn default_timeout_ms(&self) -> u64 {
        (**self).default_timeout_ms()
    }
    fn type_name(&self) -> &'static str {
        (**self).type_name()
    }
}

impl WorkUnit for Arc<dyn Component> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        (**self).depends()
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        (**self).provides()
    }
    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        (**self).execute(ctx)
    }
    fn default_timeout_ms(&self) -> u64 {
        (**self).default_timeout_ms()
    }
    fn type_name(&self) -> &'static str {
        (**self).type_name()
    }
}

impl FieldAccess for Arc<dyn Component> {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        Arc::get_mut(self)
            .ok_or_else(|| {
                FieldError::ReadOnly(
                    name.to_string(),
                    "Arc has multiple owners; configure before wrapping".into(),
                )
            })?
            .set_field(name, value)
    }
    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        (**self).get_field(name)
    }
    fn field_names(&self) -> &'static [&'static str] {
        (**self).field_names()
    }
}

impl Describable for Arc<dyn Component> {
    fn describe(&self) -> serde_json::Value {
        (**self).describe()
    }
}

impl Component for Arc<dyn Component> {
    fn as_any(&self) -> &dyn Any {
        (**self).as_any()
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        // Asymmetric by necessity: `&mut` cannot reach the inner component
        // through a shared `Arc`. When uniquely owned, delegate to the inner
        // component (unchanged behavior). When shared, hand out the `Arc`
        // handle itself as `&mut dyn Any` — sound because
        // `Arc<dyn Component>: Any` — so a downcast on a shared handle yields
        // the `Arc`, not the inner. Use `ComponentArcExt::try_as_any_mut`
        // for fallible inner access.
        // (Two `get_mut` probes: the first borrow ends before the second
        // begins, so the shared-`None` arm can reborrow `self`.)
        if Arc::get_mut(self).is_some() {
            Arc::get_mut(self).expect("probed unique").as_any_mut()
        } else {
            self as &mut dyn Any
        }
    }
}

pub mod test_support;

#[cfg(test)]
mod tests;
