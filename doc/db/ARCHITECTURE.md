# fluent-db — Architecture

> Pointer to `doc/skills/db/SKILL.md` (authoritative). Every type named
> below exists in `src/db/src/`; the stale-name guard
> (`src/db/tests/doc_stubs.rs`) fails on invented names.

## Module table (from the SKILL, verified against source)

| Module | Purpose | Path |
|--------|---------|------|
| `pool` | `SqlitePool` pooled async store (`Semaphore` + `spawn_blocking`, RAII `PooledConnection`, capability-gated `acquire`) | `src/db/src/pool.rs` |
| `store` | `SqliteStore` single-connection store | `src/db/src/store.rs` |
| `query` | `query_row`, `query_rows`, `execute`, `transaction` typed helpers over `OwnedValue` (single source of truth) | `src/db/src/query.rs` |
| `sqlite` | `open_wal`, `quote_ident`, `in_clause`, `is_unique_violation` raw mechanics (canonical home) | `src/db/src/sqlite.rs` |
| `capability` | `DbCapability` token (`open`, `open_with_config`, `pool`) | `src/db/src/capability.rs` |
| `migrate` | `Migration` trait, `PRAGMA user_version` versioning | `src/db/src/migrate.rs` |
| `cache` | `SqliteTtlCache` TTL/FIFO store | `src/db/src/cache.rs` |
| `hnsw` | No `db::hnsw` module ships: the HNSW-backed index store lives in `search-vector::hnsw`; `fluent-db` owns SQL only | — |
| `vector` | No `db::vector` module ships: embedding math lives in `search-vector::math` and the `vector-math` leaf | — |
| `wvr` | ~~`DbWorkUnit`/`store_unit` orchestration adapters~~ **pruned** (no production caller) | (`deleted`) |
| `outbox` | durable `outbox` queue (`init_schema`, `append`, `claim_batch`, `ack`) | `src/db/src/outbox.rs` |
| `embedding_cache` | embedding-cache schema (`EMBEDDING_CACHE_SCHEMA`, `init_embedding_cache`) | `src/db/src/embedding_cache.rs` |
| `error` | `DbError` single taxonomy | `src/db/src/error.rs` |

## Load-bearing invariants

- `#![forbid(unsafe_code)]`; feature-gated `sqlite` (default-on).
- Gating primitives (`CURRENT_CAPS`, `check_capability`) live in
  `fluent-wvr` so both `fluent-db` and `fluent-concurrency` read the same
  task-local without a dependency cycle.
- Blocking work never runs inline: blocking rusqlite work is offloaded
  (never blocks the executor), and `ctx.caps` are scoped on every
  offload path.
