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
    clippy::byte_char_slices,
    clippy::too_many_arguments
)]

pub mod constants;
pub mod error;
pub mod error_context;
pub mod format;
pub mod hash;
pub mod io;
pub mod metrics;
pub mod shell;
pub mod shell_parser;
pub mod string;
pub mod terminal;
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
        Self {
            dry_run: false,
            max_retries: 0,
            timeout_ms: 30000,
            metadata: Vec::new(),
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

pub use constants::{MAX_FILE_SIZE, MAX_JSON_DEPTH, MAX_VALUE_LEN};
pub use error::{CacheError, DbError, IoError};
pub use error_context::{ErrorContext, HeapErrorContext};
pub use format::{format_csv, format_json, format_size, parse_size, Column, Table};
pub use hash::{
    blake3_hash, blake3_hex, content_hash_with_model, fnv1a64, hash_batch, hash_file, sha256_hex,
    BatchHashResult, HashAlgorithm, HashState,
};
pub use io::{
    make_path_absolute, read_file_alloc, read_file_alloc_err, resolve_path, strip_path_prefix,
};
pub use metrics::LatencyHistogram;
pub use string::{
    contains_any, contains_any_word, contains_ident_word, contains_ignore_case, contains_word,
    first_comment_line, has_extension, is_noisy_comment, is_path_token, is_test_path,
    looks_like_identifier, lower_into, skill_name_from_ref, slugify, strip_boilerplate,
    strip_nl_prefix, trim_left, trim_right, truncate_at_sentence, STOP_WORDS,
};
pub use terminal::{get_terminal_height, get_terminal_width, is_terminal, Color, ProgressBar};

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
