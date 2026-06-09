use thiserror::Error;

#[derive(Error, Debug)]
pub enum RegistryError {
    #[error("target '{name}' already exists")]
    DuplicateTarget { name: String },
    #[error("target not found: {0}")]
    TargetNotFound(String),
    #[error("invalid capability reference: {0}")]
    InvalidCapability(String),
    #[error("bit index {0} out of range")]
    BitIndexOutOfRange(usize),
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
}

#[derive(Error, Debug)]
pub enum ResolverError {
    #[error("circular dependency detected")]
    CircularDependency,
    #[error("target not found: {0}")]
    TargetNotFound(String),
    #[error("missing dependency: {0}")]
    MissingDependency(String),
    #[error("execution failed: {0}")]
    ExecutionFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_error_display() {
        let err = RegistryError::DuplicateTarget {
            name: "build".into(),
        };
        assert!(format!("{err}").contains("build"));
    }

    #[test]
    fn resolver_error_circular() {
        let err = ResolverError::CircularDependency;
        assert_eq!(format!("{err}"), "circular dependency detected");
    }
}
