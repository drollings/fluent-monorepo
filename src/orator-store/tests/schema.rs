//! Migration goldens: version table, newer-refuses, failure-is-noop.

use fluent_db::migrate::{migrate, schema_version, Migration};
use fluent_db::store::SqliteStore;

use crate::schema::{migrations, CURRENT_VERSION};

#[test]
fn fresh_database_lands_on_current_version() {
    let store = SqliteStore::open_in_memory().expect("memory store");
    store
        .with_conn(|conn| {
            migrate(conn, &migrations())?;
            Ok(())
        })
        .expect("migrate");
    let version = store.with_conn(schema_version).expect("version");
    assert_eq!(version, CURRENT_VERSION);
    // Idempotent: a second run is a no-op.
    store
        .with_conn(|conn| {
            migrate(conn, &migrations())?;
            Ok(())
        })
        .expect("re-migrate");
    assert_eq!(
        store.with_conn(schema_version).expect("version"),
        CURRENT_VERSION
    );
}

#[test]
fn all_v1_tables_exist() {
    let store = SqliteStore::open_in_memory().expect("memory store");
    store
        .with_conn(|conn| {
            migrate(conn, &migrations())?;
            Ok(())
        })
        .expect("migrate");
    for table in [
        "sessions",
        "events",
        "utterances",
        "words",
        "speakers",
        "meetings",
        "notes",
        "checkpoints",
        "tool_audit",
        "models",
        "kv",
    ] {
        let found: Option<String> = store
            .query_row(
                "SELECT name FROM sqlite_master WHERE type='table' AND name=?1",
                &[&table as &dyn rusqlite::ToSql],
                |row| row.get(0),
            )
            .expect("lookup");
        assert_eq!(found.as_deref(), Some(table), "table {table} must exist");
    }
}

#[test]
fn newer_schema_refuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("orator.db");
    {
        let store = SqliteStore::open(&path).expect("open");
        store
            .with_conn(|conn| {
                migrate(conn, &migrations())?;
                Ok(())
            })
            .expect("migrate");
        store
            .with_conn_mut(|conn| {
                conn.pragma_update(None, "user_version", CURRENT_VERSION + 5)
                    .map_err(fluent_db::error::DbError::from)?;
                Ok(())
            })
            .expect("bump");
    }
    let error = crate::store::open_store(&path).expect_err("newer must refuse");
    assert_eq!(
        error,
        crate::store::OpenError::NewerSchema {
            found: CURRENT_VERSION + 5,
            supported: CURRENT_VERSION,
        }
    );
}

struct FailingMigration;

impl Migration for FailingMigration {
    fn version(&self) -> u32 {
        CURRENT_VERSION + 1
    }

    fn name(&self) -> &str {
        "failing"
    }

    fn up(&self, tx: &mut rusqlite::Transaction<'_>) -> Result<(), fluent_db::error::DbError> {
        tx.execute_batch("CREATE TABLE probe_fail (id INTEGER PRIMARY KEY);")?;
        Err(fluent_db::error::DbError::Other("boom".to_string()))
    }
}

#[test]
fn failed_migration_leaves_the_database_untouched() {
    let store = SqliteStore::open_in_memory().expect("memory store");
    store
        .with_conn(|conn| {
            migrate(conn, &migrations())?;
            Ok(())
        })
        .expect("migrate");
    let before = store.with_conn(schema_version).expect("version");
    let failing = FailingMigration;
    let result = store.with_conn(|conn| migrate(conn, &[&failing as &dyn Migration]));
    assert!(result.is_err(), "failing migration must error");
    assert_eq!(store.with_conn(schema_version).expect("version"), before);
    let probe: Option<String> = store
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='probe_fail'",
            &[],
            |row| row.get(0),
        )
        .expect("lookup");
    assert_eq!(probe, None, "partial DDL must roll back");
}
