use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Init,
    Build,
    Register,
    Validate,
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Phase::Init => write!(f, "init"),
            Phase::Build => write!(f, "build"),
            Phase::Register => write!(f, "register"),
            Phase::Validate => write!(f, "validate"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BuilderError {
    pub phase: Phase,
    pub field: Option<String>,
    pub value: Option<String>,
    pub constraint: Option<String>,
    pub message: String,
    pub cause: Option<String>,
}

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
            field: field.map(|s| s.to_string()),
            value: value.map(|s| s.to_string()),
            constraint: constraint.map(|s| s.to_string()),
            message: message.to_string(),
            cause: None,
        }
    }

    pub fn chain(mut self, cause: impl fmt::Display) -> Self {
        self.cause = Some(cause.to_string());
        self
    }
}

impl fmt::Display for BuilderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}]", self.phase)?;
        if let Some(ref field) = self.field {
            write!(f, " field={}", field)?;
        }
        if let Some(ref value) = self.value {
            let truncated = if value.len() > 64 {
                format!("{}...", &value[..64])
            } else {
                value.clone()
            };
            write!(f, " value={}", truncated)?;
        }
        if let Some(ref constraint) = self.constraint {
            write!(f, " constraint={}", constraint)?;
        }
        write!(f, ": {}", self.message)?;
        if let Some(ref cause) = self.cause {
            write!(f, " (cause: {})", cause)?;
        }
        Ok(())
    }
}

pub fn join_string_slice(items: &[String], separator: &str) -> String {
    items.join(separator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_captures_all_fields() {
        let err = BuilderError::new(
            Phase::Build,
            Some("name"),
            Some("my-target"),
            Some("unique"),
            "duplicate target name",
        );
        assert_eq!(err.phase, Phase::Build);
        assert_eq!(err.field.as_deref(), Some("name"));
        assert_eq!(err.value.as_deref(), Some("my-target"));
        assert_eq!(err.constraint.as_deref(), Some("unique"));
    }

    #[test]
    fn format_writes_message() {
        let err = BuilderError::new(Phase::Init, None, None, None, "hello");
        let s = format!("{}", err);
        assert!(s.contains("hello"));
        assert!(s.contains("[init]"));
    }

    #[test]
    fn chain_appends_parent_message() {
        let err = BuilderError::new(Phase::Build, None, None, None, "child")
            .chain("parent error");
        let s = format!("{}", err);
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
        let err = BuilderError::new(Phase::Validate, None, None, None, "ok");
        let s = format!("{}", err);
        assert_eq!(s, "[validate]: ok");
    }

    #[test]
    fn value_truncated_to_max_len() {
        let long = "a".repeat(100);
        let err = BuilderError::new(Phase::Init, None, Some(&long), None, "too long");
        let s = format!("{}", err);
        assert!(s.len() < 200);
    }
}
