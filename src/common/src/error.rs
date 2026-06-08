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
pub enum EmbedError {
    #[error("unknown provider: {0}")]
    UnknownProvider(String),
    #[error("embedding request failed: {0}")]
    RequestFailed(String),
    #[error("invalid API URL")]
    InvalidApiUrl,
    #[error("insecure API URL")]
    InsecureApiUrl,
    #[error("SSRF blocked URL")]
    SsrfBlockedUrl,
    #[error("no API key provided")]
    NoApiKey,
    #[error("parse error: {0}")]
    ParseError(String),
}

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

#[derive(Error, Debug)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("duplicate entry: {0}")]
    DuplicateEntry(String),
    #[error("invalid schema version: {0}")]
    InvalidSchemaVersion(u32),
}

#[derive(Error, Debug)]
pub enum CacheError {
    #[error("library required but not configured")]
    LibraryRequired,
    #[error("embedder required but not configured")]
    EmbedderRequired,
    #[error("cache miss")]
    CacheMiss,
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("embedding error: {0}")]
    Embedding(#[from] EmbedError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_error_display() {
        let err = RegistryError::DuplicateTarget { name: "build".into() };
        let msg = format!("{err}");
        assert!(msg.contains("build"));
    }

    #[test]
    fn embed_error_display() {
        let err = EmbedError::NoApiKey;
        assert_eq!(format!("{err}"), "no API key provided");
    }

    #[test]
    fn io_error_from_std() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = IoError::Io(io_err);
        assert!(format!("{err}").contains("file not found"));
    }

    #[test]
    fn resolver_error_circular() {
        let err = ResolverError::CircularDependency;
        assert_eq!(format!("{err}"), "circular dependency detected");
    }

    #[test]
    fn db_error_not_found() {
        let err = DbError::NotFound("test_node".into());
        assert!(format!("{err}").contains("test_node"));
    }

    #[test]
    fn cache_error_library_required() {
        let err = CacheError::LibraryRequired;
        assert_eq!(format!("{err}"), "library required but not configured");
    }
}
