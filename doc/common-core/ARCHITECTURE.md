# common-core — Architecture

> Pointer to `doc/skills/common-core/SKILL.md` (authoritative). Every type
> named below exists in `src/common-core/src/`; the stale-name guard
> (`src/common-core/tests/doc_stubs.rs`) fails on invented names.

## Module table (from the SKILL, verified against source)

| Module | Purpose | Path |
|--------|---------|------|
| `cache` | `LoadCache`, `ArcLoadCache` bounded get-or-load LRU; `eviction_score` / `evict_until_fit` weighted-LRU engine; `Budget` | `src/common-core/src/cache.rs` |
| `registry` | `KeyedRegistry`, `ConcurrentRegistry` (`resolve_or_create` single-construction) | `src/common-core/src/registry.rs` |
| `metrics` | `LatencyHistogram` lock-free `AtomicU64` buckets | `src/common-core/src/metrics.rs` |
| `retry` | `retry_async`, `backoff_ms`, `PollWithBackoff` jittered backoff | `src/common-core/src/retry.rs` |
| `runtime` | `block_on` (single-threaded fallback), `block_on_shared` (process-wide multi-thread fallback) | `src/common-core/src/runtime.rs` |
| `sync` | `lock`, `lock_read`, `lock_write` poison-recovering helpers | `src/common-core/src/sync.rs` |
| `hash` | `blake3_hex`, `sha256_hex`/`sha256_str`/`sha256_file`/`sha256_digest`, `fingerprint_framed`, chunked `hash_file` | `src/common-core/src/hash.rs` |
| `score` | `ScoreMatrix` generic math, `ScoreDimension` closed enum, `normalize_score`, `top_k_by_score` | `src/common-core/src/score.rs` |
| `interner` | `CapabilityRegistry` interned-name bitsets | `src/common-core/src/interner.rs` |
| `drift` | `BitSetDrift` follow-up tracking | `src/common-core/src/drift.rs` |
| `string` | byte-trim, case-insensitive search, HTML/ANSI strip, slug, sentence truncation, CJK bigrams, FTS quoting | `src/common-core/src/string.rs` |
| `io` | `mtime`, `write_atomic`, `read_to_string_err`, `read_file_alloc[_err]`, `resolve_path`, `ensure_dir`, `bind_unix_listener_with_stale_reclaim` + `ReclaimBindError` | `src/common-core/src/io.rs` |
| `path` | pure lexical cleaners (`normalize_lexical`, `collapse_separators`, `is_absolute_lexical`) — no I/O | `src/common-core/src/path.rs` |
| `format` | `format_json`/`format_csv`/`format_size`/`parse_size`, `Table`, generic `render_markdown_table` | `src/common-core/src/format.rs` |
| `calibration` | threshold sweeps, `CalibrationReport`, `render_calibration_table` (legacy `render_markdown_table` alias) | `src/common-core/src/calibration.rs` |
| `walk` | `walk_files`/`walk_files_filtered`, `WalkFilter`, `SkipReason`, `make_tree` test helper | `src/common-core/src/walk.rs` |
| `config` | `load_json[_or_default]`, `find_hierarchical` | `src/common-core/src/config.rs` |
| `shell` / `shell_parser` | subprocess capture (`shell_cmd`, `run_capture`) + `parse_command` | `src/common-core/src/shell.rs`, `shell_parser.rs` |
| `git` | `diff_staged`, `commit`, `rev_parse_head` subprocess wrappers | `src/common-core/src/git.rs` |
| `jsonrpc` | `JsonRpcRequest/Response/Error`, `serve_stdio` MCP loop | `src/common-core/src/jsonrpc.rs` |
| `http` | `shared_http_client`, `test_http_client`, `json_body` | `src/common-core/src/http.rs` |
| `error` / `error_context` | `IoError`, `ResolverError`, feature-gated `SqliteError`, `ErrorContext` | `src/common-core/src/error.rs`, `error_context.rs` |
| `constants` | `MAX_FILE_SIZE`, `HnswParams`, `MAX_KNN_CANDIDATES`, `MAX_MCP_REQUEST_SIZE`, … | `src/common-core/src/constants.rs` |
| `blob_spec` | lemma/YAGO blob framing (`BlobHeader`, `BlobCursor`) | `src/common-core/src/blob_spec.rs` |
| `golden` | `compare_series`, `Mismatch`, `StageTimer` | `src/common-core/src/golden.rs` |
| `cite` | `extract_citations`, `extract_citation_at` file:line scanner | `src/common-core/src/cite.rs` |
| `watchdog` | `WatchdogSet`, `Budget/ WallClock/Repetition` watchdogs | `src/common-core/src/watchdog.rs` |
| `sort` / `label_match` / `checksum` / `capacity` / `time` / `telemetry` | `sorted_vec`, `label_eq`, `luhn_valid`, `nonzero_capacity`, `now_secs`, `FailureClass` | `src/common-core/src/sort.rs` + siblings |

## Load-bearing invariants

- `#![forbid(unsafe_code)]`; zero TODO/FIXME markers.
- `block_on_shared` (never a per-call runtime) is the canonical bridge;
  pooled resources must never outlive the runtime that created them.
- Promotion rule: two live consumers trigger a move here; the second
  consumer moves rather than copies.
