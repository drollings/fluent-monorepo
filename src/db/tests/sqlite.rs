//! The `fluent_db::sqlite` connection/setup helpers: WAL mode, shared-cache
//! lifetime, HNSW construction, violation classification, placeholder lists.

use crate::sqlite::{
    in_clause, is_unique_violation, make_hnsw, open_in_memory, open_shared_in_memory, open_wal,
    quote_ident, run_batch,
};
use common_core::constants::HnswParams;

#[test]
fn open_wal_enables_wal_and_roundtrips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wal.db");
    let conn = open_wal(&path).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode;", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
    run_batch(&conn, "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)").unwrap();
    conn.execute("INSERT INTO t (v) VALUES (?1)", ["hello"])
        .unwrap();
    let v: String = conn
        .query_row("SELECT v FROM t WHERE id = 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, "hello");
}

#[test]
fn shared_in_memory_connections_see_each_other_until_last_closes() {
    let name = format!("m2-shared-{}", std::process::id());
    let a = open_shared_in_memory(&name).unwrap();
    run_batch(&a, "CREATE TABLE t (id INTEGER PRIMARY KEY)").unwrap();
    {
        let b = open_shared_in_memory(&name).unwrap();
        b.execute("INSERT INTO t (id) VALUES (1)", []).unwrap();
        let n: i64 = a
            .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
    // `b` dropped, `a` still open: the database survives.
    let n: i64 = a
        .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn unique_and_pk_violations_classify_but_other_errors_do_not() {
    let conn = open_in_memory().unwrap();
    run_batch(
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT UNIQUE)",
    )
    .unwrap();
    conn.execute("INSERT INTO t (id, v) VALUES (1, 'a')", [])
        .unwrap();
    let dup_pk = conn
        .execute("INSERT INTO t (id, v) VALUES (1, 'b')", [])
        .unwrap_err();
    assert!(is_unique_violation(&dup_pk));
    let dup_unique = conn
        .execute("INSERT INTO t (id, v) VALUES (2, 'a')", [])
        .unwrap_err();
    assert!(is_unique_violation(&dup_unique));
    let other = conn
        .execute("INSERT INTO missing (id) VALUES (1)", [])
        .unwrap_err();
    assert!(!is_unique_violation(&other));
}

#[test]
fn in_clause_shapes() {
    assert_eq!(in_clause(0), "");
    assert_eq!(in_clause(1), "?");
    assert_eq!(in_clause(3), "?,?,?");
    // The empty case is a caller-guarded edge: `IN ()` matches nothing.
    let conn = open_in_memory().unwrap();
    run_batch(&conn, "CREATE TABLE t (id INTEGER PRIMARY KEY)").unwrap();
    conn.execute("INSERT INTO t (id) VALUES (1)", []).unwrap();
    let sql = format!("SELECT COUNT(*) FROM t WHERE id IN ({})", in_clause(2));
    let n: i64 = conn
        .query_row(&sql, rusqlite::params![1, 2], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn make_hnsw_builds_searchable_cosine_index() {
    let p = HnswParams::default();
    let hnsw = make_hnsw(&p, 16);
    let a = vec![1.0f32, 0.0, 0.0, 0.0];
    let b = vec![0.0f32, 1.0, 0.0, 0.0];
    hnsw.insert((&a, 0));
    hnsw.insert((&b, 1));
    let hits = hnsw.search(&a, 1, 16);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].d_id, 0);
}

#[test]
fn quote_ident_reject_list_and_round_trip() {
    assert_eq!(quote_ident("facts").unwrap(), "\"facts\"");
    assert_eq!(quote_ident("t").unwrap(), "\"t\"");
    // The reject list, not an allowlist: legitimate non-ASCII identifiers
    // pass through.
    assert_eq!(quote_ident("tabel_ä").unwrap(), "\"tabel_ä\"");
    for bad in [
        "",
        "t\"",
        "\"",
        "ta\"ble",
        "t;",
        ";",
        "t; DROP TABLE t; --",
        "a\u{0}b",
        "\u{0}",
    ] {
        assert!(
            quote_ident(bad).is_err(),
            "expected reject for identifier {bad:?}"
        );
    }
}

/// Table-valued functions accepting bound parameters is version-sensitive
/// SQLite behavior — the identifier-free pragma lookup in `ensure_column`
/// depends on it, so it is asserted, not assumed.
#[test]
fn pragma_table_info_accepts_bound_table_parameter() {
    let conn = open_in_memory().unwrap();
    run_batch(&conn, "CREATE TABLE facts (id INTEGER PRIMARY KEY, v TEXT)").unwrap();
    let hit: i64 = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info(?) WHERE name = ?",
            rusqlite::params!["facts", "id"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hit, 1);
    let miss: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info(?) WHERE name = ?",
            rusqlite::params!["facts", "nope"],
            |r| r.get::<_, i64>(0),
        )
        .ok();
    assert!(miss.is_none());
}

/// For every identifier the quoting accepts, splicing the quoted form into
/// a DDL batch is a single statement: the batch either succeeds creating
/// exactly one table with that identifier, or fails as a whole — never
/// parsing as two statements (checked via `sqlite_master`).
#[test]
fn quote_ident_accepted_ids_never_split_statements() {
    // Deterministic xorshift PRNG — reproducible corpus, no external dep.
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut accepted = 0usize;
    for _ in 0..2000 {
        let len = (next() % 12 + 1) as usize;
        let id: String = (0..len).map(|_| (next() % 95 + 32) as u8 as char).collect();
        let Ok(quoted) = quote_ident(&id) else {
            continue;
        };
        accepted += 1;
        let conn = open_in_memory().unwrap();
        let created = conn.execute_batch(&format!("CREATE TABLE {quoted} (x INTEGER)"));
        let objects: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        match created {
            Ok(()) => {
                assert_eq!(objects, 1, "accepted id {id:?} must create one table");
                let name: String = conn
                    .query_row("SELECT name FROM sqlite_master", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(name, id);
            }
            Err(_) => assert_eq!(
                objects, 0,
                "a failed batch over accepted id {id:?} must not leave a partial object"
            ),
        }
        if accepted >= 1000 {
            break;
        }
    }
    assert!(
        accepted >= 500,
        "corpus too thin: only {accepted} of 2000 draws were accepted identifiers"
    );
}
