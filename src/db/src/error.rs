//! The single database error taxonomy for the workspace.
//!
//! Consumers' error enums wrap `DbError` via a single `#[from]` variant instead
//! of hand-writing `From<rusqlite::Error>`. The `From<rusqlite::Error>` impl here
//! centralizes the `is_unique_violation` → `DuplicateEntry` mapping and the
//! `SQLITE_BUSY` → `Busy` mapping (generalizing coral's `db/mod.rs:36-45` and the
//! memory-plugin inline check).

/// Canonical database error.
///
/// Variants mirror the failure classes that the workspace's hand-rolled error
/// enums each re-derived separately: raw sqlite failures, missing rows/keys,
/// duplicate-key constraints, busy/timeout conditions, pool exhaustion, and
/// schema-version mismatches.
#[cfg(feature = "sqlite")]
#[derive(thiserror::Error, Debug)]
pub enum DbError {
    /// A raw `rusqlite` failure (wrapped in the shared `SqliteError` leaf).
    #[error("sqlite error: {0}")]
    Sqlite(#[from] common_core::error::SqliteError),
    /// A row or key that was expected to exist did not.
    #[error("not found: {0}")]
    NotFound(String),
    /// A `UNIQUE`/`PRIMARY KEY` constraint violation.
    #[error("duplicate entry: {0}")]
    DuplicateEntry(String),
    /// The caller's task-local capability set is missing the required token.
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// The database reported `SQLITE_BUSY`.
    #[error("database busy: {0}")]
    Busy(String),
    /// The connection pool had no free connections.
    #[error("connection pool exhausted")]
    PoolExhausted,
    /// The database schema version was not what a migration expected.
    #[error("invalid schema version: {0}")]
    InvalidSchemaVersion(u32),
    /// An identifier (table/column name) failed validation before being
    /// spliced into DDL — empty, or containing `"`, `;`, or a NUL byte.
    #[error("invalid identifier: {0}")]
    InvalidIdentifier(String),
    /// An annotation write was attempted against an unstamped content hash
    /// (no node version to key against).
    /// Content without a stamped hash: the concept↔db stamping contract.
    /// Annotations key on [`ContentHash`](fluent_types::ContentHash); an
    /// unstamped node has no version to key against, so the write is
    /// rejected instead of storing an unjoinable row.
    #[error("unstamped content hash")]
    UnstampedContent,
    /// Any other database-layer error.
    #[error("io error: {0}")]
    Io(#[from] common_core::error::IoError),
    /// Invalid pool/store configuration (e.g. zero capacity).
    #[error("invalid config: {0}")]
    Config(String),
    /// A blocking database task failed to join (panicked / shut down).
    #[error("blocking task failed: {0}")]
    Spawn(String),
    /// An internal invariant was violated (no row where one was required).
    #[error("internal error: {0}")]
    Internal(String),
    /// Stored data could not be interpreted (unknown enum text, bad row).
    #[error("corrupt data: {0}")]
    Data(String),
    /// Serialization of a value for storage failed.
    #[error("serialization error: {0}")]
    Serialization(String),
}

#[cfg(feature = "sqlite")]
impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> Self {
        // A UNIQUE/PRIMARY KEY constraint violation is a typed duplicate-key
        // error, not a raw sqlite failure — consumers surface it as such.
        if crate::sqlite::is_unique_violation(&e) {
            return DbError::DuplicateEntry(e.to_string());
        }
        // `SQLITE_BUSY` means a lock could not be acquired; classify it so
        // callers can back off and retry rather than treating it as a generic
        // failure.
        if let rusqlite::Error::SqliteFailure(ffi, _) = &e {
            if ffi.code == rusqlite::ErrorCode::DatabaseBusy {
                return DbError::Busy(e.to_string());
            }
        }
        DbError::Sqlite(common_core::error::SqliteError(e))
    }
}

#[cfg(all(test, feature = "sqlite"))]
#[path = "../tests/error.rs"]
mod tests;
