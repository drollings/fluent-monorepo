//! ## Fluent WVR — Framework Trait Crate
//!
//! This is a **framework trait** crate — the Rust equivalent of a header-only
//! interface.  It defines the core `Component`, `WorkUnit`, `FieldAccess`, and
//! `Describable` traits that the DAG executor, Coral, and ContentNode crates
//! implement and consume.
//!
//! **Design contract:**
//! - No implementation logic beyond blanket impls and helper types
//! - No domain-specific dependencies (no rusqlite, no LLM, no guidance-types)
//! - The thinness is intentional — value is in the trait boundaries
//! - If a derive macro (`#[derive(FieldAccess)]`) is added later, it goes here
//!
//! Consumers: `guidance-dag`, `coral-context`, `guidance-content-node`

#![deny(warnings, clippy::all, clippy::pedantic)]
#![allow(
    clippy::module_name_repetitions,
    clippy::must_use_candidate,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::too_many_lines,
    clippy::large_stack_arrays,
    clippy::non_std_lazy_statics,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::zero_sized_map_values,
    clippy::unnecessary_literal_bound,
    clippy::cast_possible_wrap,
    clippy::unreadable_literal,
    clippy::similar_names,
    clippy::single_char_pattern,
    clippy::byte_char_slices
)]

pub mod wrapper;

pub use fluent_wvr_macros::FieldAccess;
pub use internment::ArcIntern;
use serde::{Deserialize, Serialize};
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::task::JoinHandle;

#[derive(Error, Debug)]
pub enum ConcurrencyError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub trait Capability: Send + Sync + 'static {
    fn name(&self) -> &'static str;
}

#[derive(Default, Debug)]
pub struct CapabilitySet {
    caps: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl Clone for CapabilitySet {
    fn clone(&self) -> Self {
        Self {
            caps: self.caps.clone(),
        }
    }
}

impl CapabilitySet {
    pub fn new() -> Self {
        Self {
            caps: HashMap::new(),
        }
    }

    #[must_use]
    pub fn with<C: Capability>(mut self, cap: C) -> Self {
        self.caps.insert(TypeId::of::<C>(), Arc::new(cap));
        self
    }

    pub fn get<C: Capability>(&self) -> Option<&C> {
        self.caps
            .get(&TypeId::of::<C>())
            .and_then(|arc| (&**arc as &dyn Any).downcast_ref::<C>())
    }
}

pub struct Reserve {
    counter: Arc<std::sync::atomic::AtomicUsize>,
    committed: bool,
}

impl Reserve {
    pub fn new(counter: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        counter.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        Self {
            counter,
            committed: false,
        }
    }

    pub fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for Reserve {
    fn drop(&mut self) {
        if !self.committed {
            self.counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

pub trait Runtime: Send + Sync + 'static {
    fn spawn(
        &self,
        future: Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> JoinHandle<()>;
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>>;
    fn now(&self) -> Instant;
}

pub struct NoopRuntime;

impl Runtime for NoopRuntime {
    fn spawn(
        &self,
        _future: Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> JoinHandle<()> {
        panic!("NoopRuntime::spawn called - no runtime configured");
    }

    fn sleep(&self, _duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        panic!("NoopRuntime::sleep called - no runtime configured");
    }

    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[derive(Error, Debug)]
pub enum FieldError {
    #[error("field not found: {0}")]
    NotFound(String),
    #[error("field parse error: {0}")]
    Parse(String),
    #[error("constraint violation: {0}")]
    Constraint(String),
}

#[derive(Error, Debug)]
pub enum WorkError {
    #[error("execution failed: {0}")]
    Execution(String),
    #[error("dependency not satisfied: {0}")]
    Dependency(String),
    #[error("timeout")]
    Timeout,
}

#[derive(Clone)]
pub struct WorkContext {
    pub dry_run: bool,
    pub max_retries: u32,
    pub timeout_ms: u64,
    pub metadata: Vec<(String, String)>,
    pub rt: Arc<dyn Runtime>,
    pub caps: CapabilitySet,
}

impl std::fmt::Debug for WorkContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkContext")
            .field("dry_run", &self.dry_run)
            .field("max_retries", &self.max_retries)
            .field("timeout_ms", &self.timeout_ms)
            .field("metadata", &self.metadata)
            .field("rt", &"<dyn Runtime>")
            .field("caps", &self.caps)
            .finish()
    }
}

impl Default for WorkContext {
    fn default() -> Self {
        Self {
            dry_run: false,
            max_retries: 0,
            timeout_ms: 30000,
            metadata: Vec::new(),
            rt: Arc::new(NoopRuntime),
            caps: CapabilitySet::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkOutput {
    pub success: bool,
    pub message: String,
    pub data: serde_json::Value,
}

impl WorkOutput {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
            data: serde_json::Value::Null,
        }
    }
    pub fn ok_with_data(message: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            success: true,
            message: message.into(),
            data,
        }
    }
    pub fn fail(message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: message.into(),
            data: serde_json::Value::Null,
        }
    }
}

pub trait FieldAccess {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError>;
    fn get_field(&self, name: &str) -> Result<String, FieldError>;
    fn field_names(&self) -> &'static [&'static str];
}

pub trait Describable {
    fn describe(&self) -> serde_json::Value;
}

pub trait WorkUnit: Send + Sync {
    fn name(&self) -> &str;
    fn depends(&self) -> &[ArcIntern<str>];
    fn provides(&self) -> &[ArcIntern<str>];
    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError>;
}

pub trait Component: FieldAccess + Describable + WorkUnit + Send + Sync {}
impl<T: FieldAccess + Describable + WorkUnit + Send + Sync> Component for T {}

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
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestComponent {
        name: ArcIntern<str>,
        value: i32,
    }

    impl FieldAccess for TestComponent {
        fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
            match name {
                "value" => {
                    self.value = value.parse().map_err(|_| FieldError::Parse(value.into()))?;
                    Ok(())
                }
                _ => Err(FieldError::NotFound(name.into())),
            }
        }
        fn get_field(&self, name: &str) -> Result<String, FieldError> {
            match name {
                "value" => Ok(self.value.to_string()),
                _ => Err(FieldError::NotFound(name.into())),
            }
        }
        fn field_names(&self) -> &'static [&'static str] {
            &["value"]
        }
    }

    impl Describable for TestComponent {
        fn describe(&self) -> serde_json::Value {
            serde_json::json!({"name": &*self.name, "value": self.value})
        }
    }

    impl WorkUnit for TestComponent {
        fn name(&self) -> &str {
            &self.name
        }
        fn depends(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn provides(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
            Ok(WorkOutput::ok(format!("computed: {}", self.value * 2)))
        }
    }

    #[test]
    fn test_field_access() {
        let mut comp = TestComponent {
            name: ArcIntern::from("test"),
            value: 42,
        };
        assert_eq!(comp.get_field("value").unwrap(), "42");
        comp.set_field("value", "99").unwrap();
        assert_eq!(comp.get_field("value").unwrap(), "99");
        assert!(comp.set_field("nonexistent", "x").is_err());
    }
    #[test]
    fn test_work_context_default() {
        let ctx = WorkContext::default();
        assert!(!ctx.dry_run);
        assert_eq!(ctx.timeout_ms, 30000);
    }
    #[test]
    fn test_work_output_helpers() {
        assert!(WorkOutput::ok("done").success);
        assert!(!WorkOutput::fail("error").success);
    }
    #[test]
    fn test_component_trait_object() {
        let comp = TestComponent {
            name: ArcIntern::from("test"),
            value: 10,
        };
        let boxed: Box<dyn Component> = Box::new(comp);
        assert_eq!(boxed.name(), "test");
    }
}
