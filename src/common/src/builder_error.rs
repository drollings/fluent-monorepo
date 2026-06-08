use std::fmt;

use crate::constants::MAX_VALUE_LEN;

#[allow(deprecated)]
#[deprecated(note = "Use bon-generated builders with Result instead. See ZIG_TO_RUST_PRACTICES.md §1.2")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Depends,
    Provides,
    Command,
    Registration,
    Validation,
    Initialization,
}

#[allow(deprecated)]
impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Phase::Depends => write!(f, "depends"),
            Phase::Provides => write!(f, "provides"),
            Phase::Command => write!(f, "command"),
            Phase::Registration => write!(f, "registration"),
            Phase::Validation => write!(f, "validation"),
            Phase::Initialization => write!(f, "initialization"),
        }
    }
}

#[allow(deprecated)]
#[deprecated(note = "Use bon-generated builders with Result instead. See ZIG_TO_RUST_PRACTICES.md §1.2")]
#[derive(Debug, Clone)]
pub struct BuilderError {
    pub phase: Phase,
    pub field: Option<String>,
    pub value: Option<String>,
    pub constraint: Option<String>,
    pub message: String,
    pub cause: Option<String>,
}

#[allow(deprecated)]
impl BuilderError {
    pub fn new(
        phase: Phase,
        field: Option<&str>,
        value: Option<&str>,
        constraint: Option<&str>,
        message: &str,
    ) -> Self {
        Self {
            phase,
            field: field.map(ToString::to_string),
            value: value.map(|s| {
                if s.len() > MAX_VALUE_LEN {
                    format!("{}...", &s[..MAX_VALUE_LEN])
                } else {
                    s.to_string()
                }
            }),
            constraint: constraint.map(ToString::to_string),
            message: message.to_string(),
            cause: None,
        }
    }

    #[must_use]
    pub fn chain(self, cause: impl fmt::Display) -> Self {
        Self {
            cause: Some(cause.to_string()),
            ..self
        }
    }
}

#[allow(deprecated)]
impl fmt::Display for BuilderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}]", self.phase)?;
        if let Some(ref field) = self.field {
            write!(f, " field={field}")?;
        }
        if let Some(ref value) = self.value {
            write!(f, " value={value}")?;
        }
        if let Some(ref constraint) = self.constraint {
            write!(f, " constraint={constraint}")?;
        }
        write!(f, ": {}", self.message)?;
        if let Some(ref cause) = self.cause {
            write!(f, " (cause: {cause})")?;
        }
        Ok(())
    }
}

#[allow(deprecated)]
impl std::error::Error for BuilderError {}

pub fn join_string_slice(items: &[String], separator: &str) -> String {
    items.join(separator)
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;

    #[test]
    fn init_captures_all_fields() {
        let err = BuilderError::new(
            Phase::Depends,
            Some("name"),
            Some("my-target"),
            Some("unique"),
            "duplicate target name",
        );
        assert_eq!(err.phase, Phase::Depends);
        assert_eq!(err.field.as_deref(), Some("name"));
        assert_eq!(err.value.as_deref(), Some("my-target"));
        assert_eq!(err.constraint.as_deref(), Some("unique"));
    }

    #[test]
    fn format_writes_message() {
        let err = BuilderError::new(Phase::Initialization, None, None, None, "hello");
        let s = format!("{err}");
        assert!(s.contains("hello"));
        assert!(s.contains("[initialization]"));
    }

    #[test]
    fn chain_appends_parent_message() {
        let err = BuilderError::new(Phase::Depends, None, None, None, "child")
            .chain("parent error");
        let s = format!("{err}");
        assert!(s.contains("parent error"));
    }

    #[test]
    fn join_string_slice_empty() {
        assert_eq!(join_string_slice(&[], ", "), "");
    }

    #[test]
    fn join_string_slice_single() {
        assert_eq!(join_string_slice(&["a".into()], ", "), "a");
    }

    #[test]
    fn join_string_slice_multiple() {
        assert_eq!(
            join_string_slice(&["a".into(), "b".into(), "c".into()], ", "),
            "a, b, c"
        );
    }

    #[test]
    fn null_field_and_constraint_format_correctly() {
        let err = BuilderError::new(Phase::Validation, None, None, None, "ok");
        let s = format!("{err}");
        assert_eq!(s, "[validation]: ok");
    }

    #[test]
    fn value_truncated_to_max_len() {
        let long = "a".repeat(200);
        let err = BuilderError::new(Phase::Registration, None, Some(&long), None, "too long");
        let s = format!("{err}");
        assert!(s.len() < 300);
        assert!(err.value.unwrap().ends_with("..."));
    }

    #[test]
    fn implements_error() {
        let err = BuilderError::new(Phase::Command, None, None, None, "test");
        let _: &dyn std::error::Error = &err;
    }
}
