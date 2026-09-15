//! The `fluent_db::sqlite` connection/setup helpers: WAL mode, shared-cache
//! lifetime, HNSW construction, violation classification, placeholder lists.

use crate::sqlite::{
    in_clause, is_unique_violation, make_hnsw, open_in_memory, open_shared_in_memory, open_wal,
    run_batch,
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
    conn.execute("INSERT INTO t (v) VALUES (?1)", ["hello"]).unwrap();
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
