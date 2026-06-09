pub mod wrapper;

use internment::ArcIntern;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkContext {
    pub dry_run: bool,
    pub max_retries: u32,
    pub timeout_ms: u64,
    pub metadata: Vec<(String, String)>,
}

impl Default for WorkContext {
    fn default() -> Self {
        Self { dry_run: false, max_retries: 0, timeout_ms: 30000, metadata: Vec::new() }
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
        Self { success: true, message: message.into(), data: serde_json::Value::Null }
    }
    pub fn ok_with_data(message: impl Into<String>, data: serde_json::Value) -> Self {
        Self { success: true, message: message.into(), data }
    }
    pub fn fail(message: impl Into<String>) -> Self {
        Self { success: false, message: message.into(), data: serde_json::Value::Null }
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
    fn name(&self) -> &str { (**self).name() }
    fn depends(&self) -> &[ArcIntern<str>] { (**self).depends() }
    fn provides(&self) -> &[ArcIntern<str>] { (**self).provides() }
    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> { (**self).execute(ctx) }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestComponent { name: ArcIntern<str>, value: i32 }

    impl FieldAccess for TestComponent {
        fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
            match name {
                "value" => { self.value = value.parse().map_err(|_| FieldError::Parse(value.into()))?; Ok(()) },
                _ => Err(FieldError::NotFound(name.into())),
            }
        }
        fn get_field(&self, name: &str) -> Result<String, FieldError> {
            match name { "value" => Ok(self.value.to_string()), _ => Err(FieldError::NotFound(name.into())) }
        }
        fn field_names(&self) -> &'static [&'static str] { &["value"] }
    }

    impl Describable for TestComponent {
        fn describe(&self) -> serde_json::Value { serde_json::json!({"name": &*self.name, "value": self.value}) }
    }

    impl WorkUnit for TestComponent {
        fn name(&self) -> &str { &self.name }
        fn depends(&self) -> &[ArcIntern<str>] { &[] }
        fn provides(&self) -> &[ArcIntern<str>] { &[] }
        fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
            Ok(WorkOutput::ok(format!("computed: {}", self.value * 2)))
        }
    }

    #[test] fn test_field_access() {
        let mut comp = TestComponent { name: ArcIntern::from("test"), value: 42 };
        assert_eq!(comp.get_field("value").unwrap(), "42");
        comp.set_field("value", "99").unwrap();
        assert_eq!(comp.get_field("value").unwrap(), "99");
        assert!(comp.set_field("nonexistent", "x").is_err());
    }
    #[test] fn test_work_context_default() {
        let ctx = WorkContext::default();
        assert!(!ctx.dry_run); assert_eq!(ctx.timeout_ms, 30000);
    }
    #[test] fn test_work_output_helpers() {
        assert!(WorkOutput::ok("done").success);
        assert!(!WorkOutput::fail("error").success);
    }
    #[test] fn test_component_trait_object() {
        let comp = TestComponent { name: ArcIntern::from("test"), value: 10 };
        let boxed: Box<dyn Component> = Box::new(comp);
        assert_eq!(boxed.name(), "test");
    }
}
