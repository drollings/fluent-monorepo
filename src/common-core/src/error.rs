use thiserror::Error;

#[derive(Error, Debug)]
pub enum IoError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("file too large: {size} > {max}")]
    FileTooLarge { size: usize, max: usize },
    #[error("path not found: {0}")]
    PathNotFound(String),
    #[error("invalid path: {0}")]
    InvalidPath(String),
}

impl IoError {
    /// Returns the inner `std::io::Error::kind` if this wraps one, else `None`.
    #[must_use]
    pub fn kind(&self) -> Option<std::io::ErrorKind> {
        match self {
            IoError::Io(e) => Some(e.kind()),
            _ => None,
        }
    }
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

/// Shared SQLite error wrapper. Feature-gated on `sqlite` so the crate stays
/// zero-domain by default — `rusqlite` is a generic storage dep, not a
/// domain concern.
#[cfg(feature = "sqlite")]
#[derive(Error, Debug)]
#[error("sqlite error: {0}")]
pub struct SqliteError(#[from] pub rusqlite::Error);

/// Common error umbrella that consolidates the most common leaf error
/// types. Crate-level errors should `#[from]` into this where possible
/// (e.g. `CommonError::Io`) instead of redefining `Io(#[from] std::io::Error)`.
#[derive(Error, Debug)]
pub enum CommonError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[cfg(feature = "sqlite")]
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("constraint violation: {0}")]
    Constraint(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_from_std() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = IoError::Io(io_err);
        assert!(format!("{err}").contains("file not found"));
    }

    #[test]
    fn common_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err: CommonError = io_err.into();
        assert!(matches!(err, CommonError::Io(_)));
    }

    #[test]
    fn common_error_variants_display() {
        assert_eq!(
            format!("{}", CommonError::NotFound("x".into())),
            "not found: x"
        );
        assert_eq!(
            format!("{}", CommonError::Parse("bad".into())),
            "parse error: bad"
        );
        assert_eq!(
            format!("{}", CommonError::Constraint("oob".into())),
            "constraint violation: oob"
        );
    }
}
