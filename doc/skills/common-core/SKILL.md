# common-core — Zero-Domain Utility Crate

**Context**: `common-core` (`src/common-core/`) is the **only permitted zero-domain**
crate in the workspace. It must NOT import any `guidance-*`, `coral-*`,
`fluent-*`, or `dag` crate. Generic utilities live here; domain logic does not.

## Reusable primitives — always use, never reimplement

| Concern | Module | Path |
|---------|--------|------|
| Hashing (blake3, sha256, fnv1a64, hex) + canonical fingerprint framing (`fingerprint_json` for serializable configs, `fingerprint_framed` for sorted length-framed name/payload pairs) + size-then-SHA-256 file verification (`verify_sha256_file` → `FileVerification::{Match, SizeMismatch, HashMismatch}`, size pinned via `Option<NonZeroU64>`, I/O errors returned unchanged) | `hash` | `src/common-core/src/hash.rs` |
| Text utilities (contains_ignore_case, truncate_at_sentence, strip_html, AnsiStripper, filter_unsafe_chars, trim_doc_prefix, detect_identifier_kind, slugify, fts_match_tokens, ascii_alnum_tokens, …) | `string` | `src/common-core/src/string.rs` |
| Path/fs helpers (mtime, read_file_alloc_err, write_atomic, ensure_dir, `bind_unix_listener_with_stale_reclaim` bounded stale-Unix-socket reclaim → `ReclaimBindError::{LiveHolder, Bind, Remove, Validate}` …) | `io` | `src/common-core/src/io.rs` |
| Lexical path helpers (`normalize_lexical` fail-open strings; `normalize_std_path` fail-closed `Path` component normalization for verified/jailed paths; `collapse_separators`, parent/file splits, `MAX_SYMLINK_HOPS`) | `path` | `src/common-core/src/path.rs` |
| Shared error leaf types (IoError, SqliteError, ResolverError) + `impl_from_io_error!` macro | `error` | `src/common-core/src/error.rs` |
| Cross-crate magic constants (MAX_FILE_SIZE, HnswParams, MAX_KNN_CANDIDATES, MAX_MCP_REQUEST_SIZE, …) | `constants` | `src/common-core/src/constants.rs` |
| Bitset / capability registry | `interner` | `src/common-core/src/interner.rs` |
| BitSetDrift | `drift` | `src/common-core/src/drift.rs` |
| Latency histograms / metrics (LatencyHistogram, bucket_counts, aggregate) | `metrics` | `src/common-core/src/metrics.rs` |
| Poison-safe mutex locking (lock, lock_read, lock_write — PoisonError::into_inner) | `sync` | `src/common-core/src/sync.rs` |
| SQLite open helpers + schemas (open_wal, make_hnsw, init_embedding_cache, is_unique_violation, in_clause) | `sqlite` | `src/db/src/sqlite.rs` (feature `sqlite`) — sqlite helpers are owned by `fluent-db`, not common-core (see `src/common-core/tests/sqlite.rs`) |
| JSON-RPC / MCP stdio loop | `jsonrpc` | `src/common-core/src/jsonrpc.rs` |
| Sync→async runtime bridge — `block_on` (single-threaded fallback: concurrent callers serialize) and `block_on_shared` (process-wide multi-thread fallback: concurrent callers do not serialize, and the runtime outlives the call so spawned continuations are not dropped with it) | `runtime` | `src/common-core/src/runtime.rs` |
| Token budget helpers | `tokens` | `src/llm/src/tokens.rs` (`fluent-llm`; LLM policy, not zero-domain — see `common_core::prelude` docs) |
| Directory walk / file scan | `walk` | `src/common-core/src/walk.rs` |
| Shell / subprocess helpers | `shell` | `src/common-core/src/shell.rs` |
| JSON config load-or-default | `config` | `src/common-core/src/config.rs` |
| LoadCache<K, V, E>, ArcLoadCache<K, V, E> (bounded get-or-load LRU) | `cache` | `src/common-core/src/cache.rs` |
| Weighted-LRU eviction engine (eviction_score, eviction_order, evict_until_fit) — "evict largest × coldest until under budget"; composes the InstancePool residency loop | `cache` | `src/common-core/src/cache.rs` |
| Generic keyed registry (insert/get/keys/remove/len) — `KeyedRegistry` (sync) and `ConcurrentRegistry` (thread-safe, `resolve_or_create` single-construction, Arc-shared) | `registry` | `src/common-core/src/registry.rs` |
| Retry with jittered exponential backoff (`retry_async`, `backoff_ms`, `capped_backoff_ms`, `capped_exp_backoff_ms`) + poll-with-backoff (`PollWithBackoff`, `PollResult`) + abort-aware verdict-loop composition recipe (tested doc example on `retry`: loop-top abort check, per-error-type `is_retryable` verdict, sleep raced against abort only where the loop's abort-latency contract requires it) | `retry` | `src/common-core/src/retry.rs` |
| PCM sample math (`f32_to_i16_sample`/`i16_to_f32_sample` under `Scale16::{Symmetric32767, Symmetric32768}` + `Rounding::{Nearest, TowardZero}`, `root_mean_square_f32` exact-`f32` RMS, `extend_mono_from_interleaved` channel averaging with trailing-partial remainder, `encode_wav_mono_pcm16` mono PCM16 WAV assembly, `wav_frame_header`, base64 framing) | `pcm` | `src/common-core/src/pcm.rs` |

## Import pattern

```rust
use common_core::prelude::*;        // 80% case — brings in hash, io, string
use common_core::metrics::LatencyHistogram;  // explicit for metrics
use fluent_db::sqlite::open_wal;             // feature-gated (fluent-db owns sqlite helpers)
use fluent_llm::tokens::TokenBudget;         // LLM policy lives in fluent-llm, not common-core
```

## What NOT to put here

- Anything that knows what a "node", "session", "target", "embedding",
  "WASM plugin", "Component", "WorkUnit", or "FieldAccess" is.
- Domain logic for guidance, coral, job-copilot, or fluent-router.

## Consolidation rule

When a cross-crate constant or utility has **two or more consumers**, it
MUST be promoted to `common-core`. Single-consumer items stay in their
domain crate temporarily until a second consumer appears — at which point
the *second* consumer must move to the shared home rather than copy the
consumer copy.

## Promotion candidates (canonical homes — compose, don't copy)

These consumer-side patterns are genuinely reusable but have **one** live
consumer today. Per the no-speculative-promotion rule, they stay in place
until a second consumer appears — at which point the *second* consumer must
move to the shared home below rather than copy the consumer copy.

| Pattern (current home) | Canonical home when promoted |
|---|---|
| `TelemetryEvent` + `TelemetrySink` PII-free observability contract (`src/router/src/telemetry.rs`) | `common-core::telemetry` (or `fluent-wvr::metrics` if it needs trait integration) |

> Completed: the `file:line` citation scanner now lives in `common-core::cite`
> (`extract_citations`/`extract_citation_at`) — do not reintroduce a second copy
> in `guidance` or `common-core::string`.

## Completed promotions (do not reintroduce a second copy)

- Weighted ranked-candidate scoring: the generic math (`ScoreMatrix::from_parts`,
  `ScoreMatrix::resolve`, `normalize_score`, `top_k_by_score` and the closed
  `ScoreDimension` enum) lives in `common_core::score`. The router's
  `src/router/src/score_matrix.rs::ScoreMatrix` is the thin `String`
  specialization plus the domain `router_default` (routes `plan`/`rigor`/`local`
  via the closed `RouterRouteKind` enum) — that file is the only place router
  routes appear as score categories. Do not promote `router_default` to
  `common-core`: it is domain. Do not copy the generic into a second home.
