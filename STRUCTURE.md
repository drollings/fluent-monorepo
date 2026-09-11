# AST-Guidance Project Structure

A fast, lightweight code navigation and orchestration framework friendly to
human and human-in-the-loop LLM agentic software engineering.  It is based
on enriched AST, and uses optional AI for documentation which is cached,
idempotent, and upcycled for lightweight searches and local agentic
intelligence.

## Quick Navigation (Coding Assistants)

| Purpose | File | Use When |
|---------|------|----------|
| **Find related code** | `make query QUERY="search terms"` | Searching for code |
| **Check Implementation** | `make explore QUERY="search terms"` | Before implementing anything |
| **Understand patterns** | `doc/capabilities/*.md` | Implementation examples + patterns |
| **Find existing code** | `mcp_grep` or `mcp_lsp_find_references` | Searching for implementations |

## **Attention**: Skills needed to understand files

Skills are referenced per-file in comments below.  The lookup path for the skills is:
`{guidance_dir}/skills/{skill}/SKILL.md`

So if you find a file you're looking for named file.rs:
`file.rs      # [zig-current, gof-patterns] Summary of files' contents` ,
Then you you must read

```
{guidance_dir}/skills/zig-current/SKILL.md
{guidance_dir}/skills/gof-patterns/SKILL.md
```

---

## Directory Tree (Git-Tracked Files Only)

```
.
├── AGENTS.md  # # Coral Router — Development Guide
├── Cargo.toml
├── LICENSE
├── LICENSE-Commercial-Requirement
├── LICENSE-Contributor-Agreement
├── Makefile
├── README.md  # # Fluent Monorepo - deterministic-first
├── STRUCTURE.md  # # AST-Guidance Project Structure
├── bin/
│   ├── classifier-probe.py  # #!/usr/bin/env python3
│   ├── coral-router-test.py  # #!/usr/bin/env python3
│   ├── download_yago_taxonomy.sh
│   ├── gen_simhash_projections.py  # #!/usr/bin/env python3
│   ├── gen_yago_classes.py  # #!/usr/bin/env python3
│   ├── live-ai-guard.sh
│   ├── llm-boundary-check.sh
│   ├── router-wait-health.sh
│   ├── spacy/
│   │   └── benchmark/
│   │       ├── Cargo.lock
│   │       ├── Cargo.toml
│   │       ├── bench_py.py  # #!/usr/bin/env python3
│   │       ├── run.sh
│   │       ├── src/
│   │       │   └── main.rs  # //! Parity benchmark: spacy-rs English t
│   │       └── summarize.py  # #!/usr/bin/env python3
│   └── vector-check.sh
├── data/
│   └── yamake.json
├── doc/
│   ├── coral/
│   │   ├── CHANGELOG.md  # # Changelog
│   │   ├── DETAILS.md  # # Coral Context: Detailed Engineering Sp
│   │   ├── OVERVIEW.md  # # Coral Context: Architectural Design Do
│   │   └── VISION.md  # # Coral Context: Architectural Vision
│   ├── dag/
│   │   └── VISION.md  # # Unified Dependency Resolver — Fro...
│   ├── fluent-onnx/
│   │   └── ARCHITECTURE.md  # # fluent-onnx — Architecture
│   ├── guidance/
│   │   ├── ARCHITECTURE.md  # # Architecture Overview
│   │   ├── DESIGN.md  # Comprehensive Analysis: Agentic Document
│   │   ├── MCP.md  # # guidance MCP Server
│   │   ├── VISION.md  # # guidance: Vision Document
│   │   └── schemas/
│   │       └── guidance.schema.json
│   ├── memory-plugin/
│   │   └── ARCHITECTURE.md  # # Memory Plugin Architecture — Clea...
│   ├── router/
│   │   ├── ARCHITECTURE.md  # # Coral Router — Architecture
│   │   ├── MOCKROUTER.md  # # MockRouter — Audit: Redundancies,...
│   │   ├── TESTING.md  # # Workspace Testing Convention
│   │   └── VISION.md  # # Coral Router — Vision
│   ├── skills/
│   │   ├── common-core/
│   │   │   └── SKILL.md  # # common-core — Zero-Domain Utility...
│   │   ├── dag/
│   │   │   └── SKILL.md  # # fluent-dag — Dependency Graph & D...
│   │   ├── db/
│   │   │   └── SKILL.md  # # fluent-db — Canonical Database-Ac...
│   │   ├── fluent-concurrency/
│   │   │   └── SKILL.md  # # `fluent-concurrency` — Lightweigh...
│   │   ├── fluent-wvr/
│   │   │   └── SKILL.md  # # Fluent WVR = Fluent, Wrapped, Verified
│   │   └── interlingua/
│   │       └── SKILL.md  # # Interlingua — unified, disambigua...
│   └── spacy-rs/
│       ├── ARCHITECTURE.md  # # spacy-rs — Architecture
│       ├── ITERATIVE_PARSE_IMPROVEMENT.md  # # Iterative Deterministic Parse Improvem
│       └── VISION.md  # # spacy-rs — Vision
├── env/
│   ├── categories.json
│   ├── coral-router.json.example
│   ├── en_lemmatizer.json
│   ├── mk/
│   │   ├── common.mk
│   │   ├── target_language.mk
│   │   └── targets/
│   │       ├── go.mk
│   │       ├── php.mk
│   │       ├── pine.mk
│   │       ├── py.mk
│   │       ├── rust.mk
│   │       └── zig.mk
│   ├── mock-transcripts.json
│   ├── pii-patterns.json
│   └── workflows/
│       └── charts/
│           ├── bug_triage.md.json
│           └── draft_doc.md.json
└── src/
    ├── Cargo.lock
    ├── bin/
    │   ├── coral/
    │   │   ├── Cargo.toml
    │   │   └── src/
    │   │       └── main.rs  # use clap::{Parser, Subcommand};
    │   ├── coral-router/
    │   │   ├── Cargo.toml
    │   │   └── src/
    │   │       ├── boot.rs  # //! Boot-time YaGO taxonomy load + two-s
    │   │       └── main.rs  # //! coral-router — LLM Router & Age...
    │   ├── guidance/
    │   │   ├── Cargo.toml
    │   │   ├── src/
    │   │   │   ├── benchmark.rs  # //! `guidance benchmark` — query ac...
    │   │   │   ├── commit.rs  # //! Commit message generation — LLM...
    │   │   │   ├── editor.rs  # //! Editor interaction utilities for hum
    │   │   │   ├── embed.rs  # //! Embedding-backend resolution from pr
    │   │   │   ├── index_cmd.rs  # //! `index` command (P5): thin shell ove
    │   │   │   ├── main.rs  # #![forbid(unsafe_code)]
    │   │   │   ├── mcp.rs  # //! MCP (Model Context Protocol) server
    │   │   │   ├── search.rs  # //! `search` command (P5): thin shell ov
    │   │   │   └── structure.rs  # use std::collections::BTreeMap;
    │   │   └── tests/
    │   │       ├── cli_e2e.rs  # //! P5 acceptance: `cli.test.
    │   │       ├── common.rs  # //! Shared hermetic scaffold for the bin
    │   │       ├── explain_cli.rs  # //! M4 compat: explain not-found + `--he
    │   │       ├── index_cmd.rs  # //! Unit tests for the `index` command s
    │   │       ├── mcp_contract.rs  # //! P5 `mcp-contract` port: toolsets, ba
    │   │       ├── mcp_explain.rs  # //! Unit tests for the MCP `guidance_exp
    │   │       ├── rg_cli.rs  # //! P5 `rg-cli` port: the managed-rg byp
    │   │       ├── search_cmd.rs  # //! Unit tests for the `search` command
    │   │       └── sync_propagate.rs  # //! M3 acceptance: stale-dependent propa
    │   └── yamake-coral/
    │       ├── Cargo.toml
    │       └── src/
    │           └── main.rs  # use std::process;
    ├── common-core/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── blob_spec.rs  # //! Taxonomy blob spec — decoupled ...
    │   │   ├── cache.rs  # // ─── Load Cache ────...
    │   │   ├── calibration.rs  # //! Calibration harness for confidence v
    │   │   ├── cite.rs  # //! `file:line` citation scanner — ...
    │   │   ├── config.rs  # //! JSON config loaders: `load_json_or_d
    │   │   ├── constants.rs  # //! Cross-crate magic numbers (size caps
    │   │   ├── drift.rs  # //! Bit-set drift analysis: compute "mis
    │   │   ├── error.rs  # //! Shared leaf error types: `IoError`,
    │   │   ├── error_context.rs  # //! Contextual error wrappers: `ErrorCon
    │   │   ├── format.rs  # //! Human-readable output: `format_json`
    │   │   ├── git.rs  # //! Git operations — thin wrappers ...
    │   │   ├── hash.rs  # //! Hashing utilities: `blake3_*`, `sha2
    │   │   ├── http.rs  # //! Process-wide shared HTTP client (and
    │   │   ├── interner.rs  # //! Capability registry: thread-safe str
    │   │   ├── io.rs  # use std::fs;
    │   │   ├── jsonrpc.rs  # //! Shared JSON-RPC 2.
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── metrics.rs  # //! Lock-free latency histogram with 12
    │   │   ├── prelude.rs  # //! The common-core prelude — impor...
    │   │   ├── registry.rs  # //! Generic keyed registry — the ca...
    │   │   ├── retry.rs  # //! Retry and backoff primitives — ...
    │   │   ├── runtime.rs  # //! Sync→async runtime bridge: run ...
    │   │   ├── score.rs  # use std::collections::HashMap;
    │   │   ├── shell.rs  # //! Subprocess helpers: `run_capture`, `
    │   │   ├── shell_parser.rs  # //! Safe shell parser: whitespace+quote
    │   │   ├── sqlite.rs  # //! Shared SQLite helpers — connect...
    │   │   ├── string.rs  # //! 20+ string utilities: case-insensiti
    │   │   ├── sync.rs  # //! Poison-safe locking helpers for `std
    │   │   ├── telemetry.rs  # //! Failure taxonomy — the generic ...
    │   │   ├── time.rs  # //! Time utilities: epoch-second helpers
    │   │   ├── vector_math.rs  # //! Scalar embedding-vector math (P1 can
    │   │   ├── walk.rs  # //! Directory walker: `walk_files` (call
    │   │   ├── watchdog.rs  # use std::collections::VecDeque;
    │   │   ├── yago_normalize.rs  # //! Single source for YaGO name normaliz
    │   │   └── yago_taxonomy.rs  # //! YaGO TTL → JSON taxonomy pipeli...
    │   └── tests/
    │       ├── blob_spec.rs  # use common_core::blob_spec::*;
    │       ├── cache.rs  # // NOTE (ROADMAP_20260903_LLM M11): the
    │       ├── calibration.rs  # use common_core::calibration::*;
    │       ├── cite.rs  # use common_core::cite::*;
    │       ├── config.rs  # use common_core::config::*;
    │       ├── constants.rs  # use common_core::constants::*;
    │       ├── drift.rs  # //! Bit-set drift analysis: compute "mis
    │       ├── error.rs  # use common_core::error::*;
    │       ├── error_context.rs  # use common_core::error_context::*;
    │       ├── fixtures/
    │       │   └── yago_mini.ttl
    │       ├── format.rs  # use common_core::format::*;
    │       ├── git.rs  # use common_core::git::*;
    │       ├── hash.rs  # use common_core::hash::*;
    │       ├── http.rs  # // NOTE (ROADMAP_20260903_LLM M11): the
    │       ├── interner.rs  # use common_core::interner::*;
    │       ├── io.rs  # use common_core::io::*;
    │       ├── jsonrpc.rs  # use common_core::jsonrpc::*;
    │       ├── lib.rs  # #[allow(unused_imports)]
    │       ├── metrics.rs  # use common_core::metrics::*;
    │       ├── registry.rs  # use common_core::registry::*;
    │       ├── retry.rs  # use common_core::retry::*;
    │       ├── runtime.rs  # use common_core::runtime::*;
    │       ├── score.rs  # use common_core::score::*;
    │       ├── shell.rs  # use common_core::shell::*;
    │       ├── shell_parser.rs  # use common_core::shell_parser::*;
    │       ├── sqlite.rs  # #[cfg(feature = "sqlite")]
    │       ├── string.rs  # use common_core::string::*;
    │       ├── sync.rs  # use common_core::sync::*;
    │       ├── telemetry.rs  # // NOTE (ROADMAP_20260903_LLM M11): the
    │       ├── vector_math.rs  # use common_core::vector_math::cosine_sim
    │       ├── walk.rs  # use common_core::walk::{
    │       ├── watchdog.rs  # use common_core::watchdog::*;
    │       ├── yago_normalize.rs  # use common_core::yago_normalize::*;
    │       └── yago_taxonomy.rs  # //! Parity with the deprecated `src/onto
    ├── concept/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── concept_store.rs  # //! The single source of truth for conce
    │   │   ├── concept_store_mem.rs  # //! The **hermetic in-memory** [`Concept
    │   │   ├── lib.rs  # //! The neutral shared home for concept
    │   │   └── plausibility.rs  # //! Text-half / knowledge-half bridge fo
    │   └── tests/
    │       ├── concept_store.rs  # use super::*;
    │       └── concept_store_mem.rs  # use super::*;
    ├── content-node/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── doc_node.rs  # use crate::file_node::FileContentNode;
    │   │   ├── file_node.rs  # use std::fmt::Debug;
    │   │   ├── lib.rs  # //! content-node: Level-of-detail text s
    │   │   ├── lod.rs  # pub fn generate_lod_slices(full_text: &s
    │   │   ├── node.rs  # use fluent_types::LOD_COUNT;
    │   │   └── source_node.rs  # use crate::file_node::FileContentNode;
    │   └── tests/
    │       └── lod.rs  # use content_node::lod::*;
    ├── coral/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── cache/
    │   │   │   ├── mod.rs  # pub mod reactor;
    │   │   │   ├── reactor.rs  # use std::sync::Arc;
    │   │   │   └── stats.rs  # #[derive(Debug, Clone, serde::Serialize,
    │   │   ├── cache_l1.rs  # use serde::{Deserialize, Serialize};
    │   │   ├── cache_router.rs  # use std::sync::Arc;
    │   │   ├── db/
    │   │   │   ├── edges.rs  # use fluent_types::{GraphNode, NodeId};
    │   │   │   ├── embeddings.rs  # use fluent_db::vector::{try_bytes_to_vec
    │   │   │   ├── hnsw.rs  # use std::collections::HashMap;
    │   │   │   ├── mod.rs  # pub mod edges;
    │   │   │   ├── nodes.rs  # use std::collections::HashMap;
    │   │   │   └── schema.rs  # use fluent_db::error::DbError;
    │   │   ├── error.rs  # use thiserror::Error;
    │   │   ├── ingest.rs  # use std::sync::Arc;
    │   │   ├── knowledge.rs  # //! `KnowledgeCapability` implementation
    │   │   ├── lib.rs  # //! Coral: Context-graph library for gui
    │   │   ├── mcp.rs  # use std::path::Path;
    │   │   ├── packer.rs  # use fluent_types::{ContentNode, NodeId};
    │   │   ├── test_stubs.rs  # //! Test stubs for coral cache reactor t
    │   │   ├── tests/
    │   │   │   ├── common.rs  # //! Crate-typed test fixtures shared by
    │   │   │   └── mod.rs  # //! Tier-1 test support for coral-contex
    │   │   ├── tier_units.rs  # use std::sync::Arc;
    │   │   ├── wasm_runtime.rs  # use std::path::Path;
    │   │   └── wvr.rs  # //! Fluent WVR integration for Coral cra
    │   └── tests/
    │       └── error_hops.rs  # //! Error-hop tests (M7): every `rusqlit
    ├── dag/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── adapter.rs  # //! Re-export of `ComponentAdapter` and
    │   │   ├── checkpointed.rs  # //! Checkpoint/rewind over an ordered de
    │   │   ├── closure.rs  # use std::collections::HashSet;
    │   │   ├── dep_graph.rs  # //! Pure dependency-graph algorithms sha
    │   │   ├── error.rs  # use thiserror::Error;
    │   │   ├── lib.rs  # //! fluent-dag: DAG executor with resolv
    │   │   ├── middleware.rs  # use std::sync::Arc;
    │   │   ├── narrowing.rs  # use std::collections::HashSet;
    │   │   ├── resolver.rs  # //! Capability-aware dependency resolver
    │   │   ├── target.rs  # use bitvec::vec::BitVec;
    │   │   ├── target_work_unit.rs  # //! `Target → WorkUnit` bridge.
    │   │   ├── type_inference.rs  # //! Ontology type-hierarchy inference vi
    │   │   ├── work_unit.rs  # use bon::Builder;
    │   │   ├── wvr.rs  # //! Fluent WVR integration for DAG crate
    │   │   └── yamake_loader.rs  # use bitvec::vec::BitVec;
    │   └── tests/
    │       ├── checkpointed.rs  # use super::*;
    │       ├── closure.rs  # use super::*;
    │       ├── common.rs  # //! Crate-typed test fixtures shared by
    │       ├── dep_graph.rs  # use super::*;
    │       ├── error.rs  # use super::*;
    │       ├── middleware.rs  # use super::*;
    │       ├── mod.rs  # //! Tier-1 test support for fluent-dag,
    │       ├── narrowing.rs  # use super::*;
    │       ├── resolver.rs  # use super::*;
    │       ├── target.rs  # use super::*;
    │       ├── target_work_unit.rs  # use super::*;
    │       ├── type_inference.rs  # use super::*;
    │       ├── work_unit.rs  # use super::*;
    │       └── yamake_loader.rs  # use super::*;
    ├── db/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── cache.rs  # //! Generic TTL/LRU key-value cache stor
    │   │   ├── capability.rs  # //! The capability-gated async database
    │   │   ├── error.rs  # //! The single database error taxonomy f
    │   │   ├── hnsw.rs  # //! The canonical HNSW-backed vector ind
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── migrate.rs  # //! Idempotent schema migrations.
    │   │   ├── pool.rs  # //! The canonical pooled SQLite store (D
    │   │   ├── query.rs  # //! Typed statement helpers shared by `S
    │   │   ├── store.rs  # //! The canonical single-connection SQLi
    │   │   ├── vector.rs  # //! Embedding vector math.
    │   │   └── wvr.rs  # //! Database `Component`/`WorkUnit` adap
    │   └── tests/
    │       ├── cache.rs  # use super::*;
    │       ├── capability.rs  # use super::*;
    │       ├── common.rs  # //! Crate-typed test fixtures shared by
    │       ├── error.rs  # use super::*;
    │       ├── hnsw.rs  # use super::*;
    │       ├── migrate.rs  # use super::*;
    │       ├── mod.rs  # //! Tier-1 test support for fluent-db, w
    │       ├── pool.rs  # use super::*;
    │       ├── query.rs  # use super::*;
    │       ├── store.rs  # use super::*;
    │       ├── vector.rs  # use super::*;
    │       └── wvr.rs  # use std::sync::Arc;
    ├── fluent-concurrency/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── affinity.rs  # //! Affinity-aware priority scheduler.
    │   │   ├── batch.rs  # //! Supervised batch runner with async r
    │   │   ├── capability.rs  # //! Concrete capability tokens for files
    │   │   ├── credit_pool.rs  # //! Credit-gated bounded worker pool: a
    │   │   ├── feed_worker.rs  # //! `CreditedFeedWorker<Item>` — a ...
    │   │   ├── flow.rs  # //! Credit-based backpressure flow contr
    │   │   ├── io/
    │   │   │   ├── db.rs  # //! SQLite-backed database capability (p
    │   │   │   ├── fs.rs  # //! Capability-gated filesystem I/O (rea
    │   │   │   ├── mod.rs  # //! Capability-gated I/O primitive engin
    │   │   │   └── net.rs  # //! Capability-gated network I/O (TCP co
    │   │   ├── ladder.rs  # //! First-accept-wins combinator — ...
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── pool.rs  # //! Bounded async queue, worker pool, an
    │   │   ├── queue.rs  # //! A priority queue with a fast path fo
    │   │   ├── reserve.rs  # //! Available primitive: RAII permit on
    │   │   ├── router.rs  # //! A partitioned router that distribute
    │   │   ├── runtime/
    │   │   │   ├── mod.rs  # //! Pluggable `Runtime` backends (produc
    │   │   │   ├── test.rs  # //! Test `Runtime` implementation with p
    │   │   │   └── tokio.rs  # //! Production `Runtime` implementation
    │   │   ├── scope.rs  # //! Structured concurrency via `Scope...
    │   │   ├── stream.rs  # //! Cooperative cancellation for long-li
    │   │   └── thread_resource.rs  # //! Per-thread lazy-initialized resource
    │   └── tests/
    │       ├── affinity.rs  # use super::*;
    │       ├── affinity_calibration.rs  # //! M2c — Calibration for affinity ...
    │       ├── credit_pool.rs  # use super::*;
    │       ├── e2e.rs  # use super::*;
    │       ├── feed_worker.rs  # use super::*;
    │       ├── flow.rs  # use super::*;
    │       ├── ladder.rs  # use super::*;
    │       ├── m1.rs  # use super::*;
    │       ├── m2.rs  # use super::*;
    │       ├── m3.rs  # use super::*;
    │       ├── m4.rs  # use super::*;
    │       ├── m5.rs  # // Exercises the capability-gated I/O en
    │       ├── mod.rs  # use std::sync::atomic::{AtomicUsize, Ord
    │       ├── pool.rs  # use super::*;
    │       ├── reserve.rs  # use super::*;
    │       ├── stream.rs  # use super::*;
    │       └── thread_resource.rs  # use super::*;
    ├── fluent-onnx/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── align.rs  # //! LFM ↔ spacy-rs token alignment ...
    │   │   ├── annotate.rs  # //! The trained-encoder annotation rung
    │   │   ├── colbert.rs  # //! ColBERT late-interaction retrieve...
    │   │   ├── config.rs  # //! ONNX model configuration schema ...
    │   │   ├── context.rs  # //! The per-context KV surface (ROADMAP
    │   │   ├── context_pool.rs  # //! `OnnxContextPool` — the onnx ha...
    │   │   ├── encoder.rs  # //! `OrtEncoder` — the base Encoder...
    │   │   ├── error.rs  # //! Error type for `fluent-onnx` — ...
    │   │   ├── grammar.rs  # //! Grammar-constrained decoding primiti
    │   │   ├── lib.rs  # //! `fluent-onnx` — ONNX / `ort` wo...
    │   │   ├── llm.rs  # //! The generative `CausalLm` decoder...
    │   │   ├── ort_loader.rs  # //! Real session loader backed by ONNX R
    │   │   ├── overlay.rs  # //! Overlay data types and the `Residual
    │   │   ├── pii.rs  # //! PII detection (ROADMAP_20260827_ORT
    │   │   ├── session.rs  # //! ONNX session registry — re-expo...
    │   │   ├── tokenizer.rs  # //! LFM tokenizer wrapper — the fir...
    │   │   └── two_tower.rs  # //! Two-tower zero-shot worker: the shar
    │   └── tests/
    │       ├── align.rs  # use super::*;
    │       ├── annotate.rs  # use super::*;
    │       ├── colbert.rs  # use super::*;
    │       ├── context.rs  # use super::*;
    │       ├── context_pool.rs  # use super::*;
    │       ├── encoder.rs  # use super::mean_pool;
    │       ├── grammar.rs  # use super::*;
    │       ├── live/
    │       │   ├── README.md  # # fluent-onnx live-AI tests
    │       │   ├── encoder_annotate_live.rs  # //! Live-AI tests for the trained-encode
    │       │   ├── encoder_live.rs  # //! Live-AI tests for the ONNX Encoder (
    │       │   ├── fixtures/
    │       │   │   └── lfm25_26b_io.json
    │       │   ├── gpu_probe.rs  # //! Live-AI probe for the ONNX Runtime e
    │       │   ├── llm_live.rs  # //! Live-AI probe for the LFM2.5-2.
    │       │   ├── pii_live.rs  # //! Live-AI tests for the PII-Detector (
    │       │   ├── policy_linter_live.rs  # //! Live-AI tests for the Policy-Linter
    │       │   └── two_tower_live.rs  # //! Live-AI tests for the two-tower Prom
    │       ├── live.rs  # //! Live-AI integration test crate for f
    │       ├── llm.rs  # use super::*;
    │       ├── overlay.rs  # use super::*;
    │       ├── pii.rs  # use super::*;
    │       ├── session.rs  # #[cfg(feature = "onnx")]
    │       ├── tokenizer.rs  # use super::*;
    │       └── two_tower.rs  # use super::*;
    ├── fluent-wvr/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── boundary.rs  # //! Schema-driven tolerant decoding of b
    │   │   ├── capability.rs  # use std::any::{Any, TypeId};
    │   │   ├── coerce.rs  # //! Boundary-string coercion: turning un
    │   │   ├── dynamic.rs  # //! `DynamicComponent` — a `Compone...
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── macros.rs  # /// Eliminates the 7-line `as_any`/`as_a
    │   │   ├── metadata.rs  # use serde::{Deserialize, Serialize};
    │   │   ├── prelude.rs  # //! The fluent-wvr prelude — import...
    │   │   ├── runtime.rs  # use std::future::Future;
    │   │   ├── store.rs  # //! Typed in-process handoff accumulator
    │   │   ├── test_support.rs  # //! Crate-local test support for fluent-
    │   │   ├── tests.rs  # use crate::*;
    │   │   ├── traits.rs  # use std::any::Any;
    │   │   ├── work.rs  # use std::collections::HashMap;
    │   │   └── wrapper.rs  # use std::collections::HashMap;
    │   └── tests/
    │       ├── boundary.rs  # #![allow(unused_imports)]
    │       ├── capability.rs  # #![allow(unused_imports)]
    │       ├── coerce.rs  # #![allow(unused_imports)]
    │       ├── dynamic.rs  # #![allow(unused_imports)]
    │       ├── macros.rs  # #![allow(unused_imports)]
    │       ├── metadata.rs  # #![allow(unused_imports)]
    │       ├── runtime.rs  # #![allow(unused_imports)]
    │       ├── store.rs  # #![allow(unused_imports)]
    │       ├── traits.rs  # #![allow(unused_imports)]
    │       ├── work.rs  # #![allow(unused_imports)]
    │       └── wrapper.rs  # #![allow(unused_imports)]
    ├── fluent-wvr-macros/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   └── lib.rs  # #![forbid(unsafe_code)]
    │   └── tests/
    │       └── derive_expansion.rs  # //! Expansion tests for the fluent-wvr d
    ├── fluent-wvr-testutil/
    │   ├── Cargo.toml
    │   └── src/
    │       └── lib.rs  # //! Test utilities for Fluent WVR crates
    ├── guidance/
    │   ├── Cargo.toml
    │   ├── benches/
    │   │   └── recall_depth_sweep.rs  # //! P6 recall-depth sweep: fused recall
    │   ├── src/
    │   │   ├── ast_parser.rs  # use std::path::Path;
    │   │   ├── change_set.rs  # //! P3 change tracking (port of zvec-gre
    │   │   ├── config.rs  # use std::collections::HashMap;
    │   │   ├── coordinator.rs  # //! P3 index coordinator: watcher batche
    │   │   ├── diff.rs  # //! P2 diff: scanned files vs stored rec
    │   │   ├── enhancer.rs  # use fluent_llm::client::LlmClient;
    │   │   ├── extractor/
    │   │   │   ├── adapter.rs  # //! P2 language adapters (port of `extra
    │   │   │   ├── code.rs  # //! P2 code extraction (port of `extract
    │   │   │   ├── markdown.rs  # //! P2 markdown extraction (port of `ext
    │   │   │   ├── mod.rs  # //! P2 extraction core: shared fragment
    │   │   │   ├── text.rs  # //! P2 plain-text extraction (port of `e
    │   │   │   └── vector_content.rs  # //! P2 metadata-prefixed embedding text
    │   │   ├── freshness.rs  # //! P3 freshness contract (port of zvec-
    │   │   ├── graph_index.rs  # //! P3 dependency graph (L4): file-layer
    │   │   ├── grounding.rs  # //! Grounding enforcement — ensures...
    │   │   ├── index_pipeline.rs  # //! P2 incremental index pipeline: scan
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── memory.rs  # //! Memory integration for the guidance
    │   │   ├── plugin.rs  # use std::path::{Path, PathBuf};
    │   │   ├── query/
    │   │   │   ├── db_storage.rs  # //! `GuidanceDb` as recall storage: conv
    │   │   │   ├── formatter.rs  # use std::fmt::Write;
    │   │   │   ├── fts_backend.rs  # //! Lexical backend: single-route FTS re
    │   │   │   ├── fusion.rs  # //! Candidate fusion: score sums, orderi
    │   │   │   ├── glob.rs  # //! rg-style glob + path + file-type mat
    │   │   │   ├── hybrid.rs  # //! Hybrid orchestration: query → p...
    │   │   │   ├── identifier.rs  # use common_core::string::{contains_ignor
    │   │   │   ├── ingest.rs  # //! P1 minimal ingestion: whole-file fra
    │   │   │   ├── lemma_backend.rs  # //! Lemma backend: query lemmas looked u
    │   │   │   ├── llm_filter.rs  # use common_core::string::contains_ignore
    │   │   │   ├── llm_filter_batch.rs  # use super::llm_filter::{LlmFilterBackend
    │   │   │   ├── mod.rs  # pub mod db_storage;
    │   │   │   ├── recall.rs  # //! Hybrid recall pipeline: plan validat
    │   │   │   ├── rg_backend.rs  # //! L0 minimal: capability-gated managed
    │   │   │   ├── search_backend.rs  # use common_core::string::contains_ignore
    │   │   │   ├── snapshot.rs  # use std::path::Path;
    │   │   │   ├── strategy.rs  # use fluent_types::GuidanceDoc;
    │   │   │   ├── structure_enrich.rs  # //! Structure enrichment (P5): lexical h
    │   │   │   ├── synthesize.rs  # use fluent_types::{GuidanceDoc, Member,
    │   │   │   └── vector_backend.rs  # //! Vector backend: single-route embeddi
    │   │   ├── query_engine.rs  # use fluent_types::GuidanceDoc;
    │   │   ├── runtime.rs  # use std::path::PathBuf;
    │   │   ├── scanner.rs  # use common_core::string::{contains_any,
    │   │   ├── scheduler.rs  # //! P3 reconcile job queue: the coalesci
    │   │   ├── selection.rs  # //! P2 file selection: `FileSelection` (
    │   │   ├── sync/
    │   │   │   ├── comments.rs  # use std::path::Path;
    │   │   │   ├── json_store.rs  # use std::path::{Path, PathBuf};
    │   │   │   ├── json_writer.rs  # use fluent_types::{GuidanceDoc, Member};
    │   │   │   ├── mod.rs  # pub mod comments;
    │   │   │   └── staleness.rs  # use std::path::Path;
    │   │   ├── sync_engine.rs  # use std::path::{Path, PathBuf};
    │   │   ├── tests/
    │   │   │   ├── common.rs  # //! Crate-typed test fixtures shared by
    │   │   │   └── mod.rs  # //! Tier-1 test suites for guidance-core
    │   │   ├── watcher.rs  # //! P3 file watcher (port of zvec-grep `
    │   │   ├── zg_constants.rs
    │   │   └── zg_types.rs
    │   └── tests/
    │       ├── change_set.rs  # //! P3 `change_set` tests (port of zvec-
    │       ├── common/
    │       │   └── mod.rs  # //! Tier-2 (crate-root `tests/`) shared
    │       ├── coordinator.rs  # //! P3 coordinator tests (port of zvec-g
    │       ├── e2e_gen_roundtrip.rs  # use fluent_types::MemberType;
    │       ├── extract_code.rs  # //! P2 code-extraction tests (ports `tes
    │       ├── extract_markdown.rs  # //! P2 markdown-extraction tests: headin
    │       ├── extract_text.rs  # //! P2 text-extraction tests: window bud
    │       ├── extract_vector_content.rs  # //! P2 `vector_content` tests: metadata
    │       ├── freshness.rs  # //! P3 freshness-matrix tests: `fresh` /
    │       ├── graph_index.rs  # //! P3 `graph_index` tests: per-language
    │       ├── hybrid_parity.rs  # //! Hermetic known-item parity matrix (P
    │       ├── index_diff.rs  # //! P2 diff tests: added/modified/pendin
    │       ├── index_pipeline.rs  # //! P2 pipeline tests (ports `service.
    │       ├── index_selection.rs  # //! P2.4 scanner-utils port: discovery r
    │       ├── l2_probe.rs  # //! P6 target (4) probe: inflected-query
    │       ├── live/
    │       │   ├── README.md  # # guidance-core — Live-AI tests
    │       │   └── smoke_live.rs  # //! Opt-in live-AI smoke test for the gu
    │       ├── live.rs  # //! Live-AI integration test crate for g
    │       ├── query_fts_backend.rs  # use super::*;
    │       ├── query_fusion.rs  # use super::*;
    │       ├── query_glob.rs  # use super::*;
    │       ├── query_hybrid.rs  # use super::*;
    │       ├── query_ingest.rs  # //! Lazy lemma-pipeline boundary tests (
    │       ├── query_lemma_backend.rs  # use super::*;
    │       ├── query_recall.rs  # use super::*;
    │       ├── query_rg_backend.rs  # use super::*;
    │       ├── query_structure_enrich.rs  # //! Ported `structure-enrichment.test.
    │       ├── query_vector_backend.rs  # use super::*;
    │       ├── scheduler.rs  # //! P3 scheduler tests: the coalescing c
    │       ├── watcher.rs  # //! P3 watcher tests (port of zvec-grep
    │       ├── zg_constants.rs
    │       ├── zg_parity/
    │       │   ├── corpus_cjk.rs
    │       │   └── mod.rs
    │       └── zg_types.rs
    ├── knowledge/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── csr_graph.rs  # pub const CSR_MAGIC: u32 = 0x4752_5343;
    │   │   ├── freq_table.rs  # use std::fs;
    │   │   ├── index_header.rs  # pub const INDEX_HEADER_SIZE: usize = 10;
    │   │   ├── lib.rs  # //! fluent-knowledge: Word/trigram index
    │   │   ├── query_cache.rs  # //! TTL/LRU query cache delegating to `f
    │   │   ├── tokenizer.rs  # pub struct WordTokenizer<'a> {
    │   │   ├── trigram_index.rs  # use crate::index_header::Header;
    │   │   └── word_index.rs  # use std::collections::HashMap;
    │   └── tests/
    │       ├── csr_graph.rs  # use super::*;
    │       ├── freq_table.rs  # use super::*;
    │       ├── index_header.rs  # use super::*;
    │       ├── query_cache.rs  # use super::*;
    │       ├── tokenizer.rs  # use super::*;
    │       ├── trigram_index.rs  # use super::*;
    │       └── word_index.rs  # use super::*;
    ├── llm/
    │   ├── CALIBRATION.md  # # fluent-llm calibration report — R...
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── anonymize.rs  # /// The ONE request-path anonymize entry
    │   │   ├── artifact.rs  # //! Pinned model-artifact plane: cache-f
    │   │   ├── artifact_lock.rs  # //! Directory-based mutual exclusion for
    │   │   ├── backend.rs  # //! Backend plugin layer: one base trait
    │   │   ├── cache.rs  # //! LLM response cache — the single...
    │   │   ├── catalog.rs  # //! Pinned embedding-model catalog: the
    │   │   ├── client.rs  # use std::sync::Arc;
    │   │   ├── constants.rs  # //! LLM-domain constants — the sing...
    │   │   ├── context_packer.rs  # use crate::ChatMessage;
    │   │   ├── decomposer.rs  # use bon::Builder;
    │   │   ├── embeddings.rs  # use std::num::NonZeroUsize;
    │   │   ├── embeddings_cache.rs  # //! Embedding-cache DDL — the singl...
    │   │   ├── error.rs  # use crate::embeddings::EmbeddingError;
    │   │   ├── factory.rs  # //! Catalog-backed embedding-model facto
    │   │   ├── gguf.rs  # //! Local GGUF embedding backend behind
    │   │   ├── grants.rs  # //! Workspace grants for remote embeddin
    │   │   ├── http_class.rs  # /// HTTP status classification for LLM A
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── llm_queue.rs  # //! Default LLM request handler — w...
    │   │   ├── onnx_config.rs  # //! ONNX model configuration types ...
    │   │   ├── onnx_error.rs  # //! Error type for `fluent-onnx`.
    │   │   ├── onnx_session.rs  # //! `OrtSessionRegistry` — one ONNX...
    │   │   ├── openai.rs  # //! OpenAI-compatible chat-completion wi
    │   │   ├── parse.rs  # //! Tolerant JSON parsing for LLM output
    │   │   ├── pii_patterns.rs  # use std::sync::LazyLock;
    │   │   ├── protocol.rs  # //! LLM protocol + request queue — ...
    │   │   ├── qwen.rs  # //! Remote Qwen embedding backends behin
    │   │   ├── resolution.rs  # //! Model-reference resolution order: ex
    │   │   ├── runtime.rs  # //! `fluent-llm::runtime` — the sha...
    │   │   ├── sse.rs  # //! SSE line framing — the single o...
    │   │   ├── telemetry.rs  # //! LLM telemetry vocabulary + structure
    │   │   ├── testutil.rs  # //! Shared hermetic doubles for inferenc
    │   │   ├── thinking.rs  # //! Think-block stripping — the sin...
    │   │   ├── tokens.rs  # //! Token budgets — the single owne...
    │   │   └── url.rs  # use thiserror::Error;
    │   └── tests/
    │       ├── anonymize.rs  # use super::*;
    │       ├── anonymize_map.rs  # use super::*;
    │       ├── artifact.rs  # //! Ported from zvec-grep `test/unit/mod
    │       ├── artifact_lock.rs  # //! Ported from zvec-grep `test/unit/mod
    │       ├── backend.rs  # //! Backend plugin layer tests: registry
    │       ├── cache.rs  # //! ROADMAP_20260903_LLM M4.
    │       ├── calibration.rs  # //! ROADMAP_20260903_LLM M10 — cali...
    │       ├── catalog.rs  # //! Ported from zvec-grep `test/unit/mod
    │       ├── client.rs  # use super::*;
    │       ├── constants.rs  # //! ROADMAP_20260903_LLM M6.
    │       ├── context_packer.rs  # use super::*;
    │       ├── decomposer.rs  # use super::*;
    │       ├── embeddings.rs  # use super::*;
    │       ├── embeddings_cache.rs  # //! ROADMAP_20260903_LLM M5.
    │       ├── error.rs  # use super::*;
    │       ├── factory.rs  # //! Ported from zvec-grep `test/unit/mod
    │       ├── gguf.rs  # //! Ported from zvec-grep `test/unit/mod
    │       ├── grants.rs  # //! Ported from zvec-grep authorization
    │       ├── http_class.rs  # use super::*;
    │       ├── live/
    │       │   ├── README.md  # # fluent-llm — Live-AI tests
    │       │   ├── embed_live.rs  # //! Opt-in live-AI test for the `llama:`
    │       │   ├── p4b_live.rs  # //! Live artifact-plane + remote-embeddi
    │       │   └── smoke_live.rs  # //! Opt-in live-AI smoke test for the fl
    │       ├── live.rs  # //! Live-AI integration test crate for f
    │       ├── llm_queue.rs  # use super::*;
    │       ├── no_domain_imports.rs  # //! ROADMAP_20260903_LLM M0.
    │       ├── onnx_config.rs  # use super::*;
    │       ├── onnx_session.rs  # use super::*;
    │       ├── openai.rs  # //! ROADMAP_20260903_LLM M7.
    │       ├── parse.rs  # use super::*;
    │       ├── pii.rs  # use super::*;
    │       ├── pii_patterns.rs  # use super::*;
    │       ├── protocol_parity.rs  # //! ROADMAP_20260903_LLM M9.
    │       ├── qwen.rs  # //! Ported from zvec-grep `test/unit/mod
    │       ├── resolution.rs  # //! Ported from zvec-grep `resolution.
    │       ├── runtime.rs  # use super::*;
    │       ├── sse.rs  # //! ROADMAP_20260903_LLM M2.
    │       ├── telemetry.rs  # //! ROADMAP_20260903_LLM M8.
    │       ├── testutil.rs  # //! Shared stub-backend contract: route-
    │       ├── thinking.rs  # //! ROADMAP_20260903_LLM M1.
    │       ├── tokens.rs  # //! ROADMAP_20260903_LLM M3.
    │       └── url.rs  # use super::*;
    ├── memory-plugin/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── capability.rs  # //! Capability token for explicit memory
    │       ├── lib.rs  # #![forbid(unsafe_code)]
    │       ├── plugins/
    │       │   ├── hindsight/
    │       │   │   └── mod.rs  # //! Hindsight memory plugin — struc...
    │       │   ├── holographic/
    │       │   │   ├── hrr.rs  # //! Holographic Reduced Representations
    │       │   │   ├── mod.rs  # //! Holographic memory plugin — loc...
    │       │   │   └── store.rs  # //! SQLite-backed fact store with entity
    │       │   ├── honcho/
    │       │   │   └── mod.rs  # //! Honcho memory plugin — cross-se...
    │       │   └── mod.rs  # //! Memory plugin implementations.
    │       ├── registry.rs  # //! Central memory plugin registry.
    │       ├── traits.rs  # //! Core trait definitions for the memor
    │       └── types.rs  # //! Shared types for the memory plugin s
    ├── ontology/
    │   ├── Cargo.toml
    │   ├── build.rs  # //! ROADMAP M11.4 — build-time vali...
    │   ├── data/
    │   │   └── yago_classes.json
    │   ├── src/
    │   │   ├── entity.rs  # use std::collections::HashMap;
    │   │   ├── inference.rs  # use std::collections::{HashMap, HashSet}
    │   │   ├── lib.rs  # //! guidance-ontology: Entity extraction
    │   │   ├── mapper.rs  # use std::collections::HashMap;
    │   │   ├── migration.rs  # #[derive(Debug, Clone)]
    │   │   ├── plausibility.rs  # //! Taxonomy plausibility scoring —...
    │   │   ├── yago.rs  # pub const NS_YAGO: &str = "http://yago-k
    │   │   ├── yago_loader.rs  # //! The YaGO taxonomy loader (ROADMAP...
    │   │   └── yago_view.rs  # //! YagoView — runtime file, safe `...
    │   └── tests/
    │       ├── entity.rs  # use super::*;
    │       ├── inference.rs  # use super::*;
    │       ├── mapper.rs  # use super::*;
    │       ├── migration.rs  # use super::*;
    │       ├── plausibility.rs  # //! Moved with the scoring (M5.
    │       ├── yago.rs  # use super::*;
    │       ├── yago_interlingua.rs  # //! ROADMAP M11.8 — the YaGO taxono...
    │       ├── yago_loader.rs  # use super::*;
    │       └── yago_view.rs  # //! Moved with the view (M5.
    ├── rdf/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── lexer.rs  # use crate::RdfError;
    │   │   ├── lib.rs  # //! guidance-rdf: RDF/Turtle/N-Quads par
    │   │   ├── normalize.rs  # pub struct BlankNodeScope;
    │   │   ├── nquads.rs  # use crate::lexer::{Lexer, TokenKind};
    │   │   └── parser.rs  # use std::collections::{HashMap, VecDeque
    │   └── tests/
    │       ├── lexer.rs  # use super::*;
    │       ├── normalize.rs  # use super::*;
    │       ├── nquads.rs  # use super::*;
    │       └── parser.rs  # use super::*;
    ├── requirements.txt
    ├── router/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── audit.rs  # //! Canonical durable-audit surface for
    │   │   ├── charts/
    │   │   │   ├── binding.rs  # //! Entity binding layer — the dete...
    │   │   │   ├── compile.rs  # //! Chart compiler — turns a valida...
    │   │   │   ├── execute.rs  # //! SupervisedBatch-supervised execution
    │   │   │   ├── extract.rs  # //! Chart auto-extraction from dispatch
    │   │   │   ├── mod.rs  # //! Chart content model — a library...
    │   │   │   ├── render.rs  # //! Chart template rendering — mini...
    │   │   │   ├── rubric.rs  # //! Rubric acceptance gate for chart tar
    │   │   │   ├── select.rs  # //! Chart selection — deterministic...
    │   │   │   ├── stage.rs  # //! ChartPromptStage — a `Classifie...
    │   │   │   └── store.rs  # //! ChartStore — loads and holds a ...
    │   │   ├── cli/
    │   │   │   ├── commands/
    │   │   │   │   ├── filesystem.rs  # //! Filesystem admin commands (`list`, `
    │   │   │   │   ├── mod.rs  # //! Implementation of the `coral-router`
    │   │   │   │   └── server.rs  # //! Server admin commands (`ps`, `stop`,
    │   │   │   ├── gguf.rs  # //! GGUF directory scanning, caching, mo
    │   │   │   ├── mod.rs  # //! Admin CLI support for Coral Router.
    │   │   │   └── preset.rs  # //! Rendering of downstream serving conf
    │   │   ├── concept_store_sqlite.rs  # //! The router's durable [`ConceptStore`
    │   │   ├── config/
    │   │   │   ├── addr.rs  # //! Address parsing, host equivalence, a
    │   │   │   ├── builder.rs  # //! Pipeline builder - constructs pipeli
    │   │   │   ├── classification.rs  # //! Classification-tree configuration
    │   │   │   ├── escalation.rs  # //! Escalation-ladder configuration
    │   │   │   ├── filters.rs  # //! Filter types and reject patterns for
    │   │   │   ├── ledger_group.rs  # //! LedgerGroupConfig (M8).
    │   │   │   ├── refine_policy.rs  # //! Router DTO for `spacy_rs::RefinePoli
    │   │   │   ├── root.rs  # //! Router configuration root types - th
    │   │   │   ├── rounds.rs  # use serde::{Deserialize, Deserializer, S
    │   │   │   ├── routing.rs  # //! Route resolution and routing configu
    │   │   │   ├── runtime.rs  # //! RuntimeConfig (M8).
    │   │   │   └── serving.rs  # //! ServingConfig sub-config (M8) —...
    │   │   ├── config.rs  # //! Router configuration types - deseria
    │   │   ├── dag_session.rs  # //! Dependency-aware session with DAG st
    │   │   ├── dispatch/
    │   │   │   ├── backend.rs  # use std::future::Future;
    │   │   │   ├── escalation/
    │   │   │   │   ├── assemble.rs  # //! The parse/assemble/scorer helpers th
    │   │   │   │   ├── audit.rs  # //! Escalation audit-record building: on
    │   │   │   │   ├── mod.rs  # //! The escalation ladder: deterministic
    │   │   │   │   └── modes.rs  # //! The four escalation mode implementat
    │   │   │   ├── frontier.rs  # use serde_json::Value;
    │   │   │   └── mod.rs  # pub mod backend;
    │   │   ├── error.rs  # //! Server-level error type — the s...
    │   │   ├── filters/
    │   │   │   ├── injection_detect.rs  # use std::collections::HashSet;
    │   │   │   ├── luhn.rs  # pub fn luhn_valid(input: &str) -> bool {
    │   │   │   ├── mod.rs  # pub mod injection_detect;
    │   │   │   └── regex_filter.rs  # use std::collections::HashMap;
    │   │   ├── frontier/
    │   │   │   ├── mod.rs  # pub mod modes;
    │   │   │   └── modes.rs  # //! Frontier escalation ladder — VI...
    │   │   ├── instances/
    │   │   │   ├── api.rs  # //! The public `/instances` aggregation
    │   │   │   ├── client.rs  # //! The typed management client against
    │   │   │   ├── manager.rs  # //! The sidecar owner of instance lifecy
    │   │   │   ├── mod.rs  # //! Instance-pool grammar generation, ma
    │   │   │   ├── pool.rs  # //! The router's aggregate `/instances`
    │   │   │   └── traits.rs  # //! The llama half of the shared `LlmWei
    │   │   ├── knowledge.rs  # //! `KnowledgeCapability` implementation
    │   │   ├── kv_cache.rs  # //! KV cache snapshot management - two-t
    │   │   ├── ledger/
    │   │   │   ├── annotations.rs  # //! `AnnotationStore` — the ledger'...
    │   │   │   ├── correction_index.rs  # //! The router's [`CorrectionIndex`] ove
    │   │   │   ├── frame_index.rs  # //! Frame resolution — the router's...
    │   │   │   ├── nlp.rs  # //! Ledger writer for the NLP parse (roa
    │   │   │   ├── node_annotation.rs  # //! `NodeAnnotation` — the fully-ma...
    │   │   │   ├── orchestrator.rs  # //! `LedgerAgentCoordinator` — the ...
    │   │   │   ├── overlay.rs  # //! The overlay/candidate plane (ROADMAP
    │   │   │   ├── overlay_acceptance.rs  # //! ArcReady overlay acceptance suite (O
    │   │   │   ├── overlay_worker.rs  # //! `OverlayWorker` — continuous ba...
    │   │   │   ├── prompt.rs  # //! `LedgerPromptAssembler` — the p...
    │   │   │   ├── span_cache.rs  # //! Span-level detail cache as a **read-
    │   │   │   ├── tiering.rs  # //! `LedgerTierWorker` — continuous...
    │   │   │   ├── workflow.rs  # // M12 stub
    │   │   │   └── workflow_store.rs  # //! Workflow extraction store (M8) ...
    │   │   ├── ledger.rs  # //! Full-detail content ledger with LOD
    │   │   ├── ledger_guard.rs  # //! Irreversible write-path scrubber for
    │   │   ├── lib.rs  # //! LLM Router & Agent Orchestration Fra
    │   │   ├── logging.rs  # //! Structured logging infrastructure fo
    │   │   ├── metrics.rs  # //! Failure classification for the route
    │   │   ├── node_store.rs  # //! ContentNodeStore — the shared, ...
    │   │   ├── normalize.rs  # //! Request and response normalizatio...
    │   │   ├── ort.rs  # //! Ort ONNX registry composition (ROADM
    │   │   ├── overlay/
    │   │   │   └── canonical.rs  # //! The versioned canonical-form table (
    │   │   ├── overlay.rs  # //! Pure overlay-plane data (ROADMAP_202
    │   │   ├── pipeline.rs  # //! Pipeline orchestrator — sequenc...
    │   │   ├── pipeline_types.rs  # //! Pipeline decision types — struc...
    │   │   ├── ranking.rs  # //! Deterministic salience prefilter + m
    │   │   ├── retrieval.rs  # //! Live subagent retrieval service ...
    │   │   ├── routes/
    │   │   │   ├── mod.rs  # pub mod plan;
    │   │   │   ├── plan.rs  # use std::sync::atomic::Ordering;
    │   │   │   └── rigor/
    │   │   │       ├── mod.rs  # //! Rigor route - the fixed-pass blue/re
    │   │   │       └── prompts.rs  # //! Rigor route prompt constants, messag
    │   │   ├── routing_context.rs  # use serde::{Deserialize, Serialize};
    │   │   ├── score_matrix.rs  # //! Router-local `String` specialization
    │   │   ├── server/
    │   │   │   ├── admin.rs  # //! Admin endpoints for the CLI (`coral-
    │   │   │   ├── cors.rs  # //! CORS headers — the single owner...
    │   │   │   ├── dispatch.rs  # use std::collections::HashMap;
    │   │   │   ├── entity_link.rs  # //! The async entity-link overlay worker
    │   │   │   ├── handler.rs  # use std::collections::HashMap;
    │   │   │   ├── instances_api.rs  # //! Public `/instances` management API f
    │   │   │   ├── responses.rs  # use std::sync::atomic::AtomicU64;
    │   │   │   └── review.rs  # //! The async review worker (ROADMAP ...
    │   │   ├── server.rs  # //! HTTP server exposing the router pipe
    │   │   ├── session.rs  # //! Session context node schema — b...
    │   │   ├── stages/
    │   │   │   ├── classifier/
    │   │   │   │   └── action.rs  # //! ClassifierAction — typed dispat...
    │   │   │   ├── classifier.rs  # //! Stage 2: ClassifierStage — sing...
    │   │   │   ├── common.rs  # //! Shared helpers for pipeline stages.
    │   │   │   ├── deterministic.rs  # use std::collections::HashMap;
    │   │   │   ├── mod.rs  # pub mod classifier;
    │   │   │   ├── nlp.rs  # //! Stage: `NlpStage` — the determi...
    │   │   │   ├── overlay.rs  # //! Stage: `OverlayStage` — the det...
    │   │   │   ├── pipeline_ref.rs  # //! PipelineRefStage — a `WorkUnit`...
    │   │   │   ├── prompt_parse.rs  # //! Router-local LLM-JSON round-trip cod
    │   │   │   ├── retry_classifier.rs  # //! RetryClassifier — a `WorkUnit` ...
    │   │   │   └── tree/
    │   │   │       ├── decisions.rs  # //! Tree evaluation outcome types and th
    │   │   │       ├── engine.rs  # //! The classification-tree engine walk:
    │   │   │       ├── mod.rs  # //! Classification-tree engine
    │   │   │       └── verdict.rs  # //! The three-axis verdict a classifier
    │   │   ├── streaming.rs  # //! SSE streaming handler — transla...
    │   │   ├── summarization.rs  # //! Summarization and result acceptance.
    │   │   ├── supervisor/
    │   │   │   ├── adopt.rs  # //! Orphan adoption: rediscover live `ll
    │   │   │   └── health.rs  # use std::sync::atomic::{AtomicBool, Orde
    │   │   ├── supervisor.rs  # //! Managed `llama-server` process super
    │   │   ├── target_match.rs  # //! Classifier-driven target matching...
    │   │   ├── telemetry.rs  # //! Structured telemetry events — t...
    │   │   ├── test_stubs.rs  # use std::collections::VecDeque;
    │   │   ├── test_support.rs  # //! Shared test logging capture.
    │   │   ├── testing/
    │   │   │   ├── calibration.rs  # //! Calibration control-group helpers (R
    │   │   │   ├── fixtures/
    │   │   │   │   └── ledger_lod.json
    │   │   │   ├── ledger_golden.rs  # //! Golden ledger LOD snapshot (M4).
    │   │   │   ├── mock.rs  # use std::collections::HashMap;
    │   │   │   ├── mod.rs  # pub mod calibration;
    │   │   │   └── vector_index.rs  # //! Test-only vector indices — the ...
    │   │   ├── transforms/
    │   │   │   ├── codeword_anonymize.rs  # use std::collections::HashMap;
    │   │   │   ├── decompose_hypothetical.rs  # use fluent_llm::anonymize;
    │   │   │   ├── decompose_subtasks.rs  # use fluent_llm::Decomposer;
    │   │   │   ├── mod.rs  # pub mod codeword_anonymize;
    │   │   │   ├── none.rs  # use crate::transforms::{TransformError,
    │   │   │   ├── pii_anonymize.rs  # use std::collections::HashMap;
    │   │   │   ├── sanitize.rs  # use common_core::string::{filter_unsafe_
    │   │   │   └── secret_mask.rs  # use std::sync::LazyLock;
    │   │   ├── types.rs  # //! Unified request/response types ...
    │   │   └── views.rs  # //! Reference-only view layer over the s
    │   └── tests/
    │       ├── audit.rs  # use super::*;
    │       ├── backend_registry.rs  # //! Inference-registry routing goldens.
    │       ├── build_graph.rs  # //! Build-graph guards for optional `flu
    │       ├── charts_binding.rs  # use super::*;
    │       ├── charts_compile.rs  # use super::*;
    │       ├── charts_execute.rs  # use super::*;
    │       ├── charts_extract.rs  # use super::*;
    │       ├── charts_mod.rs  # use super::*;
    │       ├── charts_render.rs  # use super::*;
    │       ├── charts_rubric.rs  # use super::*;
    │       ├── charts_select.rs  # // Tests compare reordered HNSW/reranker
    │       ├── charts_stage.rs  # use super::*;
    │       ├── charts_store.rs  # use super::*;
    │       ├── cli_commands_mod.rs  # use super::*;
    │       ├── cli_gguf.rs  # use super::*;
    │       ├── cli_preset.rs  # use super::*;
    │       ├── common.rs  # //! Crate-typed HTTP test fixtures share
    │       ├── concept_store_sqlite.rs  # use super::*;
    │       ├── config_addr.rs  # use super::*;
    │       ├── config_builder.rs  # use super::*;
    │       ├── config_classification.rs  # use super::*;
    │       ├── config_escalation.rs  # use super::*;
    │       ├── config_filters.rs  # use super::*;
    │       ├── config_refine_policy.rs  # use super::*;
    │       ├── config_root.rs  # // Tests assert float config values agai
    │       ├── config_route_tests.rs  # //! Config-synced routing integration te
    │       ├── config_routing.rs  # use super::*;
    │       ├── dag_session.rs  # use super::*;
    │       ├── data/
    │       │   ├── residency_engine_fleet_aggregate.json
    │       │   ├── residency_engine_fleet_models.json
    │       │   ├── residency_engine_pool_aggregate.json
    │       │   ├── residency_engine_pool_models.json
    │       │   ├── routing_fallback_corpus.json
    │       │   ├── routing_fallback_report.json
    │       │   ├── routing_role_golden.json
    │       │   ├── routing_sentinel_corpus.json
    │       │   └── routing_sentinel_report.json
    │       ├── deprecated_baseline.rs  # //! Baseline characterization for the de
    │       ├── dispatch_backend.rs  # use super::*;
    │       ├── dispatch_escalation_mod.rs  # use super::*;
    │       ├── dispatch_frontier.rs  # use super::*;
    │       ├── e2e_tests.rs  # //! End-to-end tests for the router pipe
    │       ├── filters_injection_detect.rs  # use super::*;
    │       ├── filters_luhn.rs  # use super::*;
    │       ├── filters_mod.rs  # use super::*;
    │       ├── filters_regex_filter.rs  # use super::*;
    │       ├── frontier_modes.rs  # use super::*;
    │       ├── golden.rs  # //! Golden test set for the router pipel
    │       ├── instances_manager.rs  # use super::*;
    │       ├── instances_mod.rs  # use super::stub::StubServer;
    │       ├── instances_traits.rs  # use super::*;
    │       ├── knowledge.rs  # use super::*;
    │       ├── kv_cache.rs  # use super::*;
    │       ├── ledger.rs  # use super::*;
    │       ├── ledger_annotations.rs  # use super::*;
    │       ├── ledger_correction_index.rs  # use super::*;
    │       ├── ledger_frame_index.rs  # use super::*;
    │       ├── ledger_guard.rs  # use super::*;
    │       ├── ledger_guard_golden.rs  # use crate::test_support::capture_logs;
    │       ├── ledger_nlp.rs  # use super::*;
    │       ├── ledger_node_annotation.rs  # //! Seam round-trip for `NodeAnnotation`
    │       ├── ledger_orchestrator.rs  # use super::*;
    │       ├── ledger_overlay.rs  # use super::*;
    │       ├── ledger_overlay_worker.rs  # use super::*;
    │       ├── ledger_prompt.rs  # use super::*;
    │       ├── ledger_span_cache.rs  # use super::*;
    │       ├── ledger_tiering.rs  # use super::*;
    │       ├── ledger_workflow_store.rs  # use super::*;
    │       ├── live/
    │       │   ├── README.md  # # fluent-router — Live-AI tests
    │       │   ├── entity_link_live.rs  # //! Live-AI-gated entity-link overlay th
    │       │   └── smoke_live.rs  # //! Opt-in live-AI smoke test for the fl
    │       ├── live.rs  # //! Live-AI integration test crate for f
    │       ├── liveness_calibration.rs  # //! M4c — Calibration for liveness
    │       ├── logging.rs  # use super::*;
    │       ├── metrics.rs  # use super::*;
    │       ├── mod.rs  # //! Router test modules.
    │       ├── node_store.rs  # use super::*;
    │       ├── normalize.rs  # use super::*;
    │       ├── ort.rs  # use super::*;
    │       ├── overlay_calibration.rs  # use crate::config::builder::{
    │       ├── overlay_canonical.rs  # use super::*;
    │       ├── pipeline.rs  # use super::*;
    │       ├── pipeline_types.rs  # use super::*;
    │       ├── qualified_model_id_roundtrip.rs  # use super::*;
    │       ├── ranking.rs  # use super::*;
    │       ├── residency_engine_golden.rs  # //! Aggregation-envelope goldens for the
    │       ├── retrieval.rs  # use super::*;
    │       ├── routes_plan.rs  # use super::*;
    │       ├── routes_rigor_mod.rs  # use super::*;
    │       ├── routing_context.rs  # use super::*;
    │       ├── routing_fallback_golden.rs  # //! Routing-fallback calibration corpus.
    │       ├── routing_role_golden.rs  # //! Role-first resolved-target golden.
    │       ├── routing_sentinel_golden.rs  # //! Sentinel fallback + late-binding cal
    │       ├── rubric_fixtures.rs  # //! Rubric-based test fixtures for `Resu
    │       ├── score_matrix.rs  # use super::*;
    │       ├── score_matrix_golden.rs  # use super::*;
    │       ├── server_admin.rs  # use std::sync::Arc;
    │       ├── server_dispatch.rs  # use super::*;
    │       ├── server_entity_link.rs  # use super::*;
    │       ├── server_http_tests.rs  # //! HTTP-level integration tests for the
    │       ├── server_instances_api.rs  # use super::*;
    │       ├── server_responses.rs  # use super::*;
    │       ├── server_review.rs  # use super::*;
    │       ├── server_tests.rs  # #[cfg(test)]
    │       ├── stage_tests.rs  # #[cfg(test)]
    │       ├── stages_classifier.rs  # use super::*;
    │       ├── stages_classifier_action.rs  # use super::*;
    │       ├── stages_common.rs  # use super::*;
    │       ├── stages_nlp.rs  # use super::*;
    │       ├── stages_overlay.rs  # use super::*;
    │       ├── stages_pipeline_ref.rs  # use super::*;
    │       ├── stages_retry_classifier.rs  # use super::*;
    │       ├── stages_tree_mod.rs  # use std::collections::HashMap;
    │       ├── streaming.rs  # use super::*;
    │       ├── summarization.rs  # use super::*;
    │       ├── supervisor.rs  # use super::*;
    │       ├── supervisor_adopt.rs  # //! Orphan-adoption tests: argv parsing,
    │       ├── supervisor_integration_tests.rs  # //! End-to-end supervisor + sidecar inte
    │       ├── target_match.rs  # use std::collections::HashMap;
    │       ├── telemetry.rs  # use super::*;
    │       ├── testing_calibration.rs  # use super::*;
    │       ├── threshold_calibration.rs  # /// Calibration suite: stub goldens (M7b
    │       ├── transforms_codeword_anonymize.rs  # use super::*;
    │       ├── transforms_sanitize.rs  # use super::*;
    │       ├── transforms_secret_mask.rs  # use super::*;
    │       ├── transforms_tests.rs  # #[cfg(test)]
    │       ├── types.rs  # use super::*;
    │       ├── vector_index.rs  # use super::*;
    │       └── views.rs  # use super::*;
    ├── search-vector/
    │   ├── Cargo.toml
    │   ├── benches/
    │   │   └── hnsw_vs_brute.rs  # //! P6 calibration bench: HNSW vs int8 b
    │   ├── src/
    │   │   ├── aliases.rs  # use std::collections::HashMap;
    │   │   ├── db.rs  # use std::collections::HashMap;
    │   │   └── lib.rs  # #![forbid(unsafe_code)]
    │   └── tests/
    │       ├── error_hops.rs  # //! Error-hop tests (M7): every `rusqlit
    │       ├── file_status.rs  # //! P2 file-status tests: `replace_file`
    │       ├── fragments.rs  # use super::*;
    │       ├── graph_edges.rs  # use super::*;
    │       └── node_sync.rs  # //! Node-sync change-gate goldens: `sync
    ├── spacy-rs/
    │   ├── Cargo.toml
    │   ├── build.rs  # //! Compiles `../../env/en_lemmatizer.
    │   ├── examples/
    │   │   ├── parse.rs  # //! Interactive ArcEager parse inspector
    │   │   └── probe_cw.rs  # //! Temporary probe: per-item content er
    │   ├── src/
    │   │   ├── arc_eager.rs  # //! The deterministic transition parser
    │   │   ├── attrs.rs  # //! The attribute-id space and the `get_
    │   │   ├── cache.rs  # //! Span-level detail cache for refiner
    │   │   ├── doc.rs  # //! The doc model: contiguous `TokenReco
    │   │   ├── error.rs  # //! Error taxonomy for the spaCy core.
    │   │   ├── frame.rs  # //! Frame extraction — spacy-rs as ...
    │   │   ├── hash.rs  # //! spaCy-compatible hashing: MurmurHash
    │   │   ├── interlingua.rs  # //! The pure hash→ID bridge (ROADMA...
    │   │   ├── labels.rs  # //! The closed label vocabularies —...
    │   │   ├── lang/
    │   │   │   ├── en/
    │   │   │   │   ├── exceptions.rs  # // @generated by src/spacy-rs/tools/gen_
    │   │   │   │   ├── function_words.rs  # //! English closed-class function-word c
    │   │   │   │   ├── num_words.rs  # // @generated by src/spacy-rs/tools/gen_
    │   │   │   │   ├── patterns.rs  # // @generated by src/spacy-rs/tools/gen_
    │   │   │   │   ├── stop_words.rs  # // @generated by src/spacy-rs/tools/gen_
    │   │   │   │   └── tag_map.rs  # // @generated by src/spacy-rs/tools/gen_
    │   │   │   ├── en.rs  # //! English language data — the por...
    │   │   │   ├── genesis.rs  # //! Rule genesis for POS/NER (ROADMAP_20
    │   │   │   ├── mod.rs  # //! Language data: the deterministic inp
    │   │   │   ├── norm_exceptions.rs  # // @generated by src/spacy-rs/tools/gen_
    │   │   │   └── url.rs  # // @generated by src/spacy-rs/tools/gen_
    │   │   ├── lemma_blob.rs  # //! The versioned binary lemma blob: per
    │   │   ├── lemmatizer.rs  # //! The lookup/rule lemmatizer (walkthro
    │   │   ├── lex_attrs.rs  # //! Deterministic lexeme attribute funct
    │   │   ├── lexeme.rs  # //! The two-level lexicon: shared word-t
    │   │   ├── lib.rs  # //! # spacy-rs
    │   │   ├── llm.rs  # //! The LLM-JSON bridge (walkthrough ...
    │   │   ├── morph.rs  # //! The morphology table (walkthrough...
    │   │   ├── ortho.rs  # //! Tagger orthography: the string fragm
    │   │   ├── pipeline.rs  # //! The pipeline composition (walkthroug
    │   │   ├── retrieval.rs  # //! Pure lemma-grep helpers over a parse
    │   │   ├── review.rs  # //! The async review mechanism (ROADMAP
    │   │   ├── routing.rs  # //! DEP-as-routing-signal extraction (wa
    │   │   ├── sentencizer.rs  # //! The deterministic, punctuation-rule
    │   │   ├── strings.rs  # //! Bidirectional string ↔ hash sto...
    │   │   ├── tag_map.rs  # //! The fine-grained tag → UPOS der...
    │   │   ├── taxonomy_blob.rs  # //! LemmaView — compile-time embedd...
    │   │   ├── tokenizer.rs  # //! The deterministic two-pass tokenizer
    │   │   ├── triple.rs  # //! Deterministic RDF triple extraction
    │   │   ├── validate.rs  # //! The deterministic annotation validat
    │   │   ├── vocab.rs  # //! The vocabulary: the shared owner of
    │   │   └── yago_resolve.rs  # //! YagoResolveStage — Alt C, inser...
    │   ├── tests/
    │   │   ├── arc_eager.rs  # use super::*;
    │   │   ├── arceager_golden.rs  # //! The deterministic parser's golden co
    │   │   ├── attrs.rs  # use super::*;
    │   │   ├── cache.rs  # use super::*;
    │   │   ├── data/
    │   │   │   ├── en_tokenization.json
    │   │   │   ├── parse_bench.floors.json
    │   │   │   ├── parse_bench.json
    │   │   │   ├── parse_bench.refs.json
    │   │   │   └── pipeline_eager_golden.json
    │   │   ├── doc.rs  # use super::*;
    │   │   ├── en_tokenization.rs  # //! Golden tokenization test: replays th
    │   │   ├── frame.rs  # use super::*;
    │   │   ├── genesis.rs  # use super::*;
    │   │   ├── hash.rs  # use super::*;
    │   │   ├── interlingua.rs  # use super::*;
    │   │   ├── labels.rs  # use super::*;
    │   │   ├── lemma_blob.rs  # use super::*;
    │   │   ├── lemmatizer.rs  # use super::*;
    │   │   ├── lex_attrs.rs  # use super::*;
    │   │   ├── lexeme.rs  # use super::*;
    │   │   ├── live/
    │   │   │   ├── README.md  # # spacy-rs live-AI tests
    │   │   │   ├── parse_bench_live.rs  # //! Opt-in live-AI reference generator f
    │   │   │   ├── refine_live.rs  # //! Opt-in live-AI test for the span-sco
    │   │   │   └── smoke_live.rs  # //! Opt-in live-AI smoke test for the an
    │   │   ├── live.rs  # //! Live-AI integration test crate for s
    │   │   ├── llm.rs  # use super::*;
    │   │   ├── morph.rs  # use super::*;
    │   │   ├── ortho.rs  # use super::*;
    │   │   ├── parse_bench.rs  # //! Hermetic parse-accuracy bench (fully
    │   │   ├── pipeline.rs  # use super::*;
    │   │   ├── refine_calibration.rs  # //! Calibration corpus for task-value tr
    │   │   ├── retrieval.rs  # use super::*;
    │   │   ├── review.rs  # use super::*;
    │   │   ├── routing.rs  # use super::*;
    │   │   ├── sentencizer.rs  # use super::*;
    │   │   ├── strings.rs  # use super::*;
    │   │   ├── tag_map.rs  # use super::*;
    │   │   ├── taxonomy_blob.rs  # use super::*;
    │   │   ├── tokenizer.rs  # use super::*;
    │   │   ├── triple.rs  # use super::*;
    │   │   ├── validate.rs  # use super::*;
    │   │   └── vocab.rs  # use super::*;
    │   └── tools/
    │       ├── gen_en_exceptions.py  # #!/usr/bin/env python3
    │       ├── gen_en_lemma_data.py  # #!/usr/bin/env python3
    │       ├── gen_en_regexes.py  # #!/usr/bin/env python3
    │       ├── gen_en_tag_map.py  # #!/usr/bin/env python3
    │       └── gen_golden_corpus.py  # #!/usr/bin/env python3
    ├── types/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── instance_id.rs  # use internment::ArcIntern;
    │       ├── interlingua.rs  # //! The disambiguated interlingua: a por
    │       ├── knowledge.rs  # //! KnowledgeCapability — the cross...
    │       ├── lib.rs  # //! fluent-types: Shared data types (Gui
    │       └── provenance.rs  # //! Ledger annotation provenance — ...
    └── wasm_ipc/
        ├── Cargo.toml
        └── src/
            └── lib.rs  # //! WASM IPC — Binary schemas for E...
```
