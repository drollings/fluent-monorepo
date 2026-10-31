# fluent-db — Canonical Database-Access Layer

**Context**: `fluent-db` (`src/db/`) is the workspace's single database-access
crate. It owns connection lifecycle, statement execution, schema lifecycle,
reusable store shapes, embedding vector math, and the capability-gated async
surface. Raw SQLite mechanics (`open_wal`, `quote_ident`, `in_clause`,
`is_unique_violation`, embedding-cache DDL) are canonically owned by
`fluent_db :: sqlite` (the former `common-core :: sqlite` home was removed;
`lib.rs` documents the move).

## The DB principle (R12)

**Any code that opens a `Connection`, prepares a statement, builds a SQL
`IN` clause, maintains an HNSW index, evicts a TTL cache, or blocks on a
`rusqlite` call belongs in `fluent-db` (or `common-core::sqlite` for raw
mechanics) — not in a domain crate.** When you find yourself writing that in a
consumer, stop and promote/extend the shared component instead.

## Import boundary (D2)

`fluent-db` may import `common-core`, `fluent-wvr`,
`tokio`, `rusqlite`, `hnsw_rs`, `anndists`, `serde`, `thiserror`, `tracing`,
`bon`, `internment`, `fluent-types`, `vector-math`. It must NOT import `fluent-concurrency`, `guidance`,
`coral`, `fluent-router`, `search-vector`, `knowledge`, `ontology`, `rdf`,
or `wasm_ipc`. The capability-gating primitives
(`CURRENT_CAPS`, `check_capability`, `CapabilityError`) live in `fluent-wvr`
so both `fluent-db` and `fluent-concurrency` read the same task-local without
a cycle; `fluent-concurrency` re-exports `DbCapability` behind its `db`
feature (never the reverse).

## Modules

| Module | Purpose | Path |
|--------|---------|------|
| `error` | `DbError` — the single database error taxonomy (`Sqlite|NotFound|DuplicateEntry|PermissionDenied|Busy|PoolExhausted|InvalidSchemaVersion|InvalidIdentifier|UnstampedContent|Other`) + the one `From<rusqlite::Error>` centralizing `is_unique_violation` → `DuplicateEntry` and `SQLITE_BUSY` → `Busy` | `src/db/src/error.rs` |
| `sqlite` | Raw mechanics: `open_wal`/`open_in_memory`/`quote_ident`/`run_batch`/`make_hnsw`/`is_unique_violation`/`in_clause` (canonical home, moved from `common-core`) | `src/db/src/sqlite.rs` |
| `outbox` | Crash-redelivery outbox (`init_schema`/`append`/`claim_batch`/`ack`/`pending_count` with lease expiry + `attempts` bump) | `src/db/src/outbox.rs` |
| `embedding_cache` | `EMBEDDING_CACHE_SCHEMA` + `init_embedding_cache` DDL | `src/db/src/embedding_cache.rs` |
| `store` | `SqliteStore` — single-connection store (`Mutex<Connection>`, WAL, schema-init, migrations, typed helpers), poison-safe via `common_core::sync::lock` | `src/db/src/store.rs` |
| `pool` | `SqlitePool` — pooled async store (`Semaphore` + `spawn_blocking` + RAII `PooledConnection`, `PoolConfig { size, busy_timeout_ms }`) with typed async helpers. `acquire()` is **capability-gated** like every effect entry point (`check_db_capability` → `DbError::PermissionDenied`); internal pre-gated callers use the private `acquire_ungated`. Also owns an async `transaction<T>(&self, f)` (API parity with `SqliteStore::transaction`) | `src/db/src/pool.rs` |
| `query` | Free typed statement helpers shared by store and pool: `query_row`/`query_rows`/`execute`/`execute_batch`/`query_rows_from_iter` (the `in_clause` + `params_from_iter` combo)/`last_insert_rowid`/`transaction` (rollback-on-Err). `QueryReturnedNoRows` → `None`/empty | `src/db/src/query.rs` |
| `migrate` | `Migration` trait + `migrate()` via `PRAGMA user_version`, `ensure_column` (idempotent `ALTER TABLE`), `schema_version` | `src/db/src/migrate.rs` |
| `cache` | `TtlCache` — generic TTL/LRU key-value store (`get`/`put`/`evict_expired`/`evict_lru`/`clear`/`stats`), `hash_key: fn(&str) -> String` parameterized; schema stays `query_cache` | `src/db/src/cache.rs` |
| `hnsw` | `HnswIndex` — generic HNSW-backed index store (`RwLock<Option<Hnsw>>` + `id_map`): `insert`/`rebuild_from(rows, decode)`/`search`/`is_built`/`len`/`id_map_snapshot`. **Lock order `hnsw → id_map`, never inverted (R9)** | `src/db/src/hnsw.rs` |
| `vector` | Embedding math: `cosine_similarity`, `knn_brute_force`, `vec_to_bytes`/`bytes_to_vec`/`try_bytes_to_vec`, `QuantizedEmbedding`, `cosine_similarity_q8`, `rrf_merge` (the former `search-vector::math` shim is deleted; consumers import this module directly) | `src/db/src/vector.rs` |
| `capability` | `DbCapability` — a `fluent_wvr::Capability` token over an `Arc<SqlitePool>`, exposing exactly `open` / `open_with_config` / `pool`. Statement execution goes only through the typed helpers (`query::query_row` / `query_rows` / `execute`, `pool::SqlitePool` borrowed helpers, `store::SqliteStore`); the old lossy string-map spelling is gone and must not be reintroduced (pinned by `db/tests/capability_surface.rs`) | `src/db/src/capability.rs` |
| `wvr` | `DbWorkUnit<F>` + `store_unit` — database `Component`/`WorkUnit` adapters whose `execute` offloads the blocking op via `tokio::task::block_in_place`/`spawn_blocking` (WorkUnit purity contract). `execute` scopes `ctx.caps` into `CURRENT_CAPS` on **both** offload paths (`block_in_place` and scoped-thread), so pool-backed units are capability-correct on multi-thread and current-thread runtimes alike. The pool-backed `DbStore` bridges sync→async via `common_core::runtime::block_on` | `src/db/src/wvr.rs` |

## Zero-cost guarantee

The rusqlite surface is feature-gated on `sqlite` (default-on). A consumer that
only wants pools/scope/batch pays nothing for the database layer:
`cargo build -p fluent-db --no-default-features` pulls no `rusqlite`, and
`fluent-concurrency --no-default-features` has no `io`/`capability` modules.

## Consumers

`search-vector` (`GuidanceDb`), `coral-context` (`Library`), `fluent-router`
(ledger + charts store), `fluent-knowledge` (`QueryCache` → `TtlCache`),
`memory-plugin` (`HolographicStore`), and `guidance-core` (`DB_POOL` →
`DbWorkUnit`) all compose `fluent-db` components. No consumer holds a raw
`Mutex<rusqlite::Connection>`; raw `&Connection` appears only inside
`with_conn` closures (documented exception), and the only
`From<rusqlite::Error>` in the tree is `fluent-db::error::DbError`.

## Rules

- Compose `fluent_db :: sqlite` mechanics; do not re-implement them.
- Keep `fluent-db` domain-free: parameterize new stores on plain data (`i64` ids,
  `&[f32]` embeddings, `ToSql`/`FromSql` row mappers). `NodeId` is a
  zero-cost `i64` newtype permitted as a legacy adapter; new index code
  prefers the plain-data generic (`PlainBruteForceIndex`).
- Use only the typed helpers (`query_rows`/`query_row`/`execute` and the
  `SqlitePool`/`SqliteStore` typed methods). The lossy string-map spelling
  is removed; do not reintroduce `query`/`execute` methods on
  `DbCapability`.
- New code must not introduce `unsafe`.
- When a `DbWorkUnit` needs timing, use
  `common_core::metrics::LatencyHistogram` + `Instrumented::with_metrics`.
