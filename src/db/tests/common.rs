//! Crate-typed test fixtures shared by fluent-db Tier-1 suites.
//!
//! Migrated homes of the former `pool.rs` / `capability.rs` /
//! `query.rs` / `migrate.rs` duplicated helpers (see
//! `ROADMAP_20260816_TESTS.md` §1.3). Never copy these into a new test
//! module.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use fluent_wvr::CapabilitySet;
use rusqlite::Connection;

use crate::capability::DbCapability;
use crate::pool::{PoolConfig, SqlitePool};

/// A boxed `Send` future — lets the shared behavior assertions below accept
/// per-store-layer closures that mix sync (`Connection`/`SqliteStore`) and
/// async (`SqlitePool`) drivers uniformly.
type BoxedFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// An open in-memory `Connection` (via `crate::sqlite::open_in_memory`).
pub fn conn() -> Connection {
    crate::sqlite::open_in_memory().unwrap()
}

/// A `CapabilitySet` carrying a `DbCapability` token. The pool's `acquire`
/// path is capability-gated, so tests that check out a connection must
/// scope one of these into `CURRENT_CAPS`.
pub fn db_caps() -> CapabilitySet {
    CapabilitySet::new().with(DbCapability::open(":memory:").unwrap())
}

/// An empty in-memory `SqlitePool` (default config), wrapped in `Arc`.
pub fn in_memory_pool() -> Arc<SqlitePool> {
    Arc::new(SqlitePool::open_in_memory(&PoolConfig::default()).unwrap())
}

// ── Shared behavior assertions (one assertion set per behavior) ────────────
//
// These consolidate the transaction / execute-query round-trip / poison
// suites that were triplicated across the bare-`Connection` (query.rs),
// `SqliteStore` (store.rs), and `SqlitePool` (pool.rs) layers. Each layer
// provides a thin driver closure; the assertion body lives here once (see
// `ROADMAP_20260816_TESTS.md` M2.7). Never duplicate the assertion logic.

/// Transaction commit/rollback: `run(commit)` prepares a fresh store, executes
/// a transaction that either commits or returns an error, and returns the
/// post-run row count. Asserts commit persists (count 1) and error rolls back
/// (count 0).
pub async fn assert_transaction_commit_rollback(run: impl Fn(bool) -> BoxedFut<i64>) {
    let committed = run(true).await;
    assert_eq!(committed, 1, "committed transaction persists its writes");
    let rolled_back = run(false).await;
    assert_eq!(rolled_back, 0, "transaction must roll back on error");
}

/// Execute+query round-trip: `run()` writes `(1, 'hello')` and returns the
/// insert count, the name read back by id, and all rows. Asserts the write is
/// visible through every read path.
pub async fn assert_execute_query_round_trip(
    run: impl Fn() -> BoxedFut<(usize, Option<String>, Vec<(i64, String)>)>,
) {
    let (n, name, rows) = run().await;
    assert_eq!(n, 1);
    assert_eq!(name.as_deref(), Some("hello"));
    assert_eq!(rows, vec![(1, "hello".to_string())]);
}

/// Poison recovery: a panic while holding the layer's lock must not leave the
/// store/pool unusable. `poison_guard` panics while holding the lock;
/// `verify_usable` then exercises the same handle.
pub async fn assert_poison_recovery(
    poison_guard: impl FnOnce(),
    verify_usable: impl FnOnce() -> BoxedFut<()>,
) {
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(poison_guard));
    assert!(panic.is_err(), "expected a panic while holding the lock");
    verify_usable().await;
}
