# fluent-db — Vision

> Status: Implemented. The authoritative contract lives in
> `doc/skills/db/SKILL.md` (the DB principle R12, import boundary D2); this
> file is a pointer. Crate directory is `src/db/`; package name is
> `fluent-db`.

## Mission

`fluent-db` is the workspace's single database-access owner: connection
lifecycle, statement execution, schema lifecycle, reusable store shapes
(single-connection, pooled, TTL/LRU, HNSW-backed vector), embedding vector
math, and the capability-gated async surface. Any code that opens a
`Connection`, builds a SQL `IN` clause, or blocks on `rusqlite` belongs
here — not in a domain crate.

## What belongs / what does not belong

Belongs: `SqlitePool` pooled store (`PooledConnection` RAII checkout),
`SqliteStore` single-connection store, typed `query::query_row` /
`query_rows` / `execute` / `transaction` helpers over `query::OwnedValue`,
`sqlite` raw mechanics (`open_wal`, `quote_ident`, `in_clause`),
`TtlCache`, `HnswIndex` (+ `AdaptiveHnsw` dispatch, `HnswIndexHandle`),
`vector` embedding math (`PlainBruteForceIndex` generic + `BruteForceIndex`
adapter, `knn_brute_force`, q8 quantized paths, `rrf_merge`),
`DbWorkUnit`, `DbCapability` token (exactly `open` / `open_with_config` /
`pool` — statement execution goes only through the typed helpers),
`outbox` durable queue, `migrate` versioning, `embedding_cache` schema.

Does not belong: domain semantics (what a row *is* stays in the consumer);
a second query spelling (the lossy string-map form is removed and pinned
absent by `db/tests/capability_surface.rs`); imports from
`fluent-concurrency`, `guidance`, `coral`, or the router.

## Consumers (verified `Cargo.toml` dependents)

`fluent-router` (ledger, charts), `guidance-core` (`DB_POOL`),
`coral-context`, `fluent-knowledge` (`QueryCache` → `TtlCache`),
`search-vector`, `memory-plugin`, `fluent-llm`,
`fluent-concurrency` (`db` feature re-export), `coral-router` binary.

## End-to-end capabilities

A crate needing durable state gets the full path without touching
`rusqlite` directly: versioned schema migrations, single-connection
and pooled stores with RAII checkout, typed row queries, TTL/LRU
caches, vector search with quantized and HNSW-backed indexes,
durable outbox queues, and capability-gated async writes — one
`DbError` taxonomy, no stringly queries, no second connection
discipline.

## Non-goals

- No raw `Mutex<rusqlite::Connection>` in consumers; raw `&Connection`
  appears only inside `with_conn` closures.
- No second `From<rusqlite::Error>`: `DbError` is the single taxonomy.
- Zero-cost optionality: `--no-default-features` pulls no `rusqlite`.
