//! Guard: `DbCapability` stays a minimal capability token.
//!
//! The lossy string-map `query`/`execute` spelling is gone: statement
//! execution goes only through the typed helpers (`query::query_row`,
//! `query::query_rows`, `query::execute`, `pool::SqlitePool` borrowed
//! helpers, `store::SqliteStore`). This suite pins the token surface
//! (`open`, `open_with_config`, `pool`) and asserts — via a source
//! assertion, since the property is the *absence* of an API — that no method
//! named `query` or `execute` is reintroduced on `DbCapability`.

use super::*;

#[test]
fn token_surface_is_open_open_with_config_and_pool() {
    let tok = DbCapability::open(":memory:").expect("open in-memory");
    let _ = tok.pool();
    let tok2 = DbCapability::open_with_config(":memory:", crate::pool::PoolConfig::default())
        .expect("open with config");
    let _ = tok2.pool();
}

#[test]
fn no_lossy_query_execute_spelling_on_token() {
    const SRC: &str = include_str!("../src/capability.rs");
    for line in SRC.lines() {
        let trimmed = line.trim();
        let is_fn = trimmed.starts_with("pub fn ") || trimmed.starts_with("pub async fn ");
        if is_fn {
            assert!(
                !trimmed.contains(" fn query(") && !trimmed.contains(" fn execute("),
                "DbCapability must not regain a lossy query/execute method: {trimmed}"
            );
        }
    }
}
