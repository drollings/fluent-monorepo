use thiserror::Error;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_error_not_found() {
        let err = DbError::NotFound("test_node".into());
        assert!(format!("{err}").contains("test_node"));
    }
}
