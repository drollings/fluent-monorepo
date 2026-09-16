use super::*;
use crate::tests::common::conn;

struct TestMigration {
    version: u32,
    name: String,
    ddl: String,
}

impl TestMigration {
    fn new(version: u32, name: &str, ddl: &str) -> Self {
        Self {
            version,
            name: name.to_string(),
            ddl: ddl.to_string(),
        }
    }
}

impl Migration for TestMigration {
    fn version(&self) -> u32 {
        self.version
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn up(&self, tx: &mut rusqlite::Transaction<'_>) -> Result<(), DbError> {
        tx.execute_batch(&self.ddl).map_err(DbError::from)
    }
}

#[test]
fn ensure_column_adds_once_and_is_idempotent() {
    let conn = conn();
    conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .unwrap();
    ensure_column(&conn, "t", "label", "TEXT NOT NULL DEFAULT ''").unwrap();
    ensure_column(&conn, "t", "label", "TEXT NOT NULL DEFAULT ''").unwrap();
    let cols: Vec<String> = conn
        .prepare("PRAGMA table_info(t)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(cols, vec!["id".to_string(), "label".to_string()]);
}

#[test]
fn ensure_column_leaves_existing_columns_untouched() {
    let conn = conn();
    conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, label TEXT NOT NULL DEFAULT '')")
        .unwrap();
    // Data in the existing column must survive an ensure_column no-op.
    conn.execute("INSERT INTO t (id, label) VALUES (1, 'keep')", [])
        .unwrap();
    ensure_column(&conn, "t", "label", "TEXT NOT NULL DEFAULT ''").unwrap();
    let label = conn
        .query_row("SELECT label FROM t WHERE id = 1", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap();
    assert_eq!(label, "keep");
}

/// The identifier arguments must survive being spliced into `ALTER TABLE`
/// unquoted — anything carrying a `"` (statement terminator), `;`
/// (statement separator), a NUL byte, or emptiness is rejected as a typed
/// error before any SQL runs.
#[test]
fn ensure_column_rejects_quote_in_identifier() {
    let conn = conn();
    conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .unwrap();
    let err = ensure_column(&conn, "t", "la\"bel", "TEXT").unwrap_err();
    assert!(matches!(err, DbError::InvalidIdentifier(_)));
    let err = ensure_column(&conn, "t\"", "label", "TEXT").unwrap_err();
    assert!(matches!(err, DbError::InvalidIdentifier(_)));
    assert_column_absent(&conn, "t", "label");
}

#[test]
fn ensure_column_rejects_semicolon() {
    let conn = conn();
    conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .unwrap();
    let err = ensure_column(&conn, "t", "label;", "TEXT").unwrap_err();
    assert!(matches!(err, DbError::InvalidIdentifier(_)));
    let err = ensure_column(&conn, "t; DROP TABLE t; --", "label", "TEXT").unwrap_err();
    assert!(matches!(err, DbError::InvalidIdentifier(_)));
    assert_column_absent(&conn, "t", "label");
    // The table must still exist: the rejected identifier must not have
    // been executed as a second statement.
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn ensure_column_rejects_empty() {
    let conn = conn();
    conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .unwrap();
    let err = ensure_column(&conn, "t", "", "TEXT").unwrap_err();
    assert!(matches!(err, DbError::InvalidIdentifier(_)));
    let err = ensure_column(&conn, "", "label", "TEXT").unwrap_err();
    assert!(matches!(err, DbError::InvalidIdentifier(_)));
    assert_column_absent(&conn, "t", "label");
}

#[test]
fn ensure_column_rejects_nul() {
    let conn = conn();
    conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .unwrap();
    let err = ensure_column(&conn, "t", "la\u{0}bel", "TEXT").unwrap_err();
    assert!(matches!(err, DbError::InvalidIdentifier(_)));
    let err = ensure_column(&conn, "t\u{0}", "label", "TEXT").unwrap_err();
    assert!(matches!(err, DbError::InvalidIdentifier(_)));
    assert_column_absent(&conn, "t", "label");
}

/// Golden over the three live ledger column migrations: after each add, the
/// declared name, type, and default must be exactly what `PRAGMA
/// table_info` reports — the emitted DDL shape is pinned here.
#[test]
fn ensure_column_ledger_columns_golden() {
    let conn = conn();
    conn.execute_batch("CREATE TABLE ledger (id INTEGER PRIMARY KEY)")
        .unwrap();
    ensure_column(&conn, "ledger", "label", "TEXT NOT NULL DEFAULT ''").unwrap();
    ensure_column(&conn, "ledger", "lod", "TEXT NOT NULL DEFAULT '[]'").unwrap();
    ensure_column(
        &conn,
        "ledger",
        "content_json",
        "TEXT NOT NULL DEFAULT '{}'",
    )
    .unwrap();

    let cols: Vec<(String, String, Option<String>)> = conn
        .prepare("PRAGMA table_info(ledger)")
        .unwrap()
        .query_map([], |row| Ok((row.get(1)?, row.get(2)?, row.get(4)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        cols,
        vec![
            ("id".to_string(), "INTEGER".to_string(), None),
            (
                "label".to_string(),
                "TEXT".to_string(),
                Some("''".to_string())
            ),
            (
                "lod".to_string(),
                "TEXT".to_string(),
                Some("'[]'".to_string())
            ),
            (
                "content_json".to_string(),
                "TEXT".to_string(),
                Some("'{}'".to_string())
            ),
        ]
    );

    // The defaults are live: a row inserted without those columns picks
    // them up.
    conn.execute("INSERT INTO ledger (id) VALUES (1)", [])
        .unwrap();
    let label: String = conn
        .query_row("SELECT label FROM ledger WHERE id = 1", [], |r| r.get(0))
        .unwrap();
    let lod: String = conn
        .query_row("SELECT lod FROM ledger WHERE id = 1", [], |r| r.get(0))
        .unwrap();
    let content_json: String = conn
        .query_row("SELECT content_json FROM ledger WHERE id = 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(label, "");
    assert_eq!(lod, "[]");
    assert_eq!(content_json, "{}");
}

fn assert_column_absent(conn: &rusqlite::Connection, table: &str, column: &str) {
    let has: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info(?) WHERE name = ?",
            rusqlite::params![table, column],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has, 0, "column {column:?} must not exist in {table:?}");
}

#[test]
fn migrate_applies_in_order_and_tracks_version() {
    let conn = conn();
    let migrations: [&dyn Migration; 3] = [
        &TestMigration::new(1, "base", "CREATE TABLE t (id INTEGER PRIMARY KEY)"),
        &TestMigration::new(2, "col_a", "ALTER TABLE t ADD COLUMN a TEXT"),
        &TestMigration::new(3, "col_b", "ALTER TABLE t ADD COLUMN b TEXT"),
    ];
    assert_eq!(schema_version(&conn).unwrap(), 0);
    migrate(&conn, &migrations).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 3);

    let cols: Vec<String> = conn
        .prepare("PRAGMA table_info(t)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        cols,
        vec!["id".to_string(), "a".to_string(), "b".to_string(),]
    );
}

#[test]
fn migrate_skips_already_applied() {
    let conn = conn();
    let migrations: [&dyn Migration; 2] = [
        &TestMigration::new(1, "base", "CREATE TABLE t (id INTEGER PRIMARY KEY)"),
        &TestMigration::new(2, "col_a", "ALTER TABLE t ADD COLUMN a TEXT"),
    ];
    migrate(&conn, &migrations).unwrap();
    // Second run must be a no-op: re-running "CREATE TABLE t" would fail
    // with a duplicate-object error if it were not skipped.
    migrate(&conn, &migrations).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 2);
}

#[test]
fn migrate_applies_only_pending_migrations_from_later_version() {
    let conn = conn();
    let base: [&dyn Migration; 2] = [
        &TestMigration::new(1, "base", "CREATE TABLE t (id INTEGER PRIMARY KEY)"),
        &TestMigration::new(2, "col_a", "ALTER TABLE t ADD COLUMN a TEXT"),
    ];
    migrate(&conn, &base).unwrap();

    // A later migration set re-applies only what is above user_version.
    let more: [&dyn Migration; 3] = [
        &TestMigration::new(1, "base", "CREATE TABLE t (id INTEGER PRIMARY KEY)"),
        &TestMigration::new(2, "col_a", "ALTER TABLE t ADD COLUMN a TEXT"),
        &TestMigration::new(3, "col_b", "ALTER TABLE t ADD COLUMN b TEXT"),
    ];
    migrate(&conn, &more).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 3);

    let has_b: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('t') WHERE name = 'b'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(has_b, 1);
}
