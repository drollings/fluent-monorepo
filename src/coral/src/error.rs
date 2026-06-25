use thiserror::Error;

#[derive(Error, Debug)]
pub enum CacheError {
    #[error("library required but not configured")]
    LibraryRequired,
    #[error("embedder required but not configured")]
    EmbedderRequired,
    #[error("cache miss")]
    CacheMiss,
    #[error("database error: {0}")]
    Database(#[from] common_core::error::SqliteError),
    #[error("embedding error: {0}")]
    Embedding(String),
}

impl From<rusqlite::Error> for CacheError {
    fn from(e: rusqlite::Error) -> Self {
        CacheError::Database(common_core::error::SqliteError(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_error_library_required() {
        let err = CacheError::LibraryRequired;
        assert_eq!(format!("{err}"), "library required but not configured");
    }
}
