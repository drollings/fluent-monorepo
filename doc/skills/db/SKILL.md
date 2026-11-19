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
`IN` clause, evicts a TTL cache, or blocks on a
`rusqlite` call belongs in `fluent-db` (or `common-core::sqlite` for raw
mechanics) — not in a domain crate.** HNSW graph ownership and vector
math live in `search-vector` (and the `vector-math` leaf); when you find
yourself writing SQL or cache mechanics in a consumer, stop and
promote/extend the shared component instead.

## Import boundary (D2)

`fluent-db` may import `common-core`, `fluent-wvr`,
`tokio`, `rusqlite`, `serde`, `thiserror`, `tracing`,
`bon`, `internment`, `fluent-types`. It must NOT import `fluent-concurrency`, `guidance`,
`coral`, `fluent-router`, `search-vector`, `knowledge`, `fluent-concept`, `rdf`,
or `wasm_ipc`. The capability-gating primitives
(`CURRENT_CAPS`, `check_capability`, `CapabilityError`) live in `fluent-wvr`
so both `fluent-db` and `fluent-concurrency` read the same task-local without
a cycle; `fluent-concurrency` re-exports `DbCapability` behind its `db`
feature (never the reverse).

## Modules

| Module | Purpose | Path |
|--------|---------|------|
| `error` | `DbError` — the single database error taxonomy (`Sqlite|NotFound|DuplicateEntry|PermissionDenied|Busy|PoolExhausted|InvalidSchemaVersion|InvalidIdentifier|UnstampedContent|Io|Config|Spawn|Internal|Data|Serialization`; no `Other` catch-all) + the one `From<rusqlite::Error>` centralizing `is_unique_violation` → `DuplicateEntry` and `SQLITE_BUSY` → `Busy` (`From<IoError>` yields `Io`) | `src/db/src/error.rs` |
| `sqlite` | Raw mechanics: `open_wal`/`open_in_memory`/`open_shared_in_memory`/`quote_ident`/`run_batch`/`is_unique_violation`/`in_clause` (canonical home, moved from `common-core`; HNSW construction lives in `search-vector::hnsw::make_hnsw`) | `src/db/src/sqlite.rs` |
| `outbox` | Crash-redelivery outbox (`init_schema`/`append`/`claim_batch`/`ack`/`pending_count` with lease expiry + `attempts` bump; retry policy `is_claim_retryable` + `OUTBOX_CLAIM_MAX_ATTEMPTS`/`OUTBOX_CLAIM_RETRY_DELAY_MS`, plus the pool's async `outbox_*` wrappers) | `src/db/src/outbox.rs` |
| `embedding_cache` | `EMBEDDING_CACHE_SCHEMA` + `init_embedding_cache` DDL | `src/db/src/embedding_cache.rs` |
| `store` | `SqliteStore` — single-connection store (`Mutex<Connection>`, WAL, schema-init, migrations, typed helpers), poison-safe via `common_core::sync::lock` | `src/db/src/store.rs` |
| `pool` | `SqlitePool` — pooled async store (`Semaphore` + `spawn_blocking` + RAII `PooledConnection`, `PoolConfig { size, busy_timeout: Duration }`, `with_busy_timeout`) with typed async helpers. `acquire()` is **capability-gated** like every effect entry point (`check_db_capability` → `DbError::PermissionDenied`); internal pre-gated callers use the private `acquire_ungated`. Also owns an async `transaction<T>(&self, f)` (API parity with `SqliteStore::transaction`) | `src/db/src/pool.rs` |
| `query` | Free typed statement helpers shared by store and pool: `query_row`/`query_rows`/`execute`/`execute_batch`/`query_rows_from_iter` (the `in_clause` + `params_from_iter` combo)/`last_insert_rowid`/`transaction` (rollback-on-Err). rusqlite `QueryReturnedNoRows` → `None` on `query_row`, `vec![]` on `query_rows` | `src/db/src/query.rs` |
| `migrate` | `Migration` trait + `migrate()` via `PRAGMA user_version`, `ensure_column` (idempotent `ALTER TABLE`), `schema_version` | `src/db/src/migrate.rs` |
| `cache` | `SqliteTtlCache` — generic TTL/FIFO key-value store (`open`/`open_in_memory(default_ttl: Duration)`/`with_key`/`with_cap`, `get`/`put`/`evict_expired`/`evict_fifo`/`clear`/`stats() -> (usize, Duration, usize)`; row `Entry.ttl: Duration`, SQL column stays `ttl_seconds`), `hash_key: fn(&str) -> String` parameterized; schema stays `query_cache` | `src/db/src/cache.rs` |
| `hnsw` | No `db::hnsw` module ships: the HNSW-backed index store (`HnswIndex`: `RwLock<Option<Hnsw>>` + `id_map`, `insert`/`rebuild_from(rows, decode)`/`search`/`is_built`/`len`/`id_map_snapshot`, **lock order `hnsw → id_map`, never inverted (R9)**) lives in `search-vector::hnsw` (`src/search-vector/src/hnsw.rs`); `fluent-db` owns SQL only | — |
| `vector` | No `db::vector` module ships: embedding math (`cosine_similarity`, `knn_brute_force`, `vec_to_bytes`/`bytes_to_vec`/`try_bytes_to_vec`, `QuantizedEmbedding`, `cosine_similarity_q8`, `rrf_merge`) lives in `search-vector::math` and the `vector-math` leaf; consumers import those homes directly | — |
| `capability` | `DbCapability` — a `fluent_wvr::Capability` token over an `Arc<SqlitePool>`, exposing exactly `open` / `open_with_config` / `pool`. Statement execution goes only through the typed helpers (`query::query_row` / `query_rows` / `execute`, `pool::SqlitePool` borrowed helpers, `store::SqliteStore`); the old lossy string-map spelling is gone and must not be reintroduced (pinned by `db/tests/capability_surface.rs`) | `src/db/src/capability.rs` |
| `wvr` | ~~`DbWorkUnit<F>` + `store_unit` adapters~~ **pruned** (no production caller; offload via `common_core::runtime::block_on_shared` or scoped threads) | (`deleted`) |

## Zero-cost guarantee

The rusqlite surface is feature-gated on `sqlite` (default-on). A consumer that
only wants pools/scope/batch pays nothing for the database layer:
`cargo build -p fluent-db --no-default-features` pulls no `rusqlite`, and
`fluent-concurrency --no-default-features` has no `io`/`capability` modules.

## Consumers

`guidance` (`GuidanceDbStorage`, composed over `search-vector` primitives), `coral-context` (`Library`), `fluent-router`
(ledger + charts store), `fluent-knowledge` (`QueryCache` → `SqliteTtlCache`),
and `memory-plugin` (`HolographicStore`) all compose `fluent-db`
components. No consumer holds a raw
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
- When a blocking offload needs timing, use
  `common_core::metrics::LatencyHistogram` + `Instrumented::with_metrics`.
