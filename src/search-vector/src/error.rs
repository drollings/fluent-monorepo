use thiserror::Error;

#[derive(Error, Debug)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] common_core::error::SqliteError),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("duplicate entry: {0}")]
    DuplicateEntry(String),
    #[error("invalid schema version: {0}")]
    InvalidSchemaVersion(u32),
}

impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> Self {
        DbError::Sqlite(common_core::error::SqliteError(e))
    }
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
