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
├── audit/
│   ├── FINDINGS.md  # # Audit Findings — fluent-wvr / flu...
│   └── adr/
│       ├── 001-schema-numbers.md  # # ADR-001: Schema numeric bounds as JSON
│       ├── 002-strict-int-parse.md  # # ADR-002: Strict native integer parse (
│       ├── 003-wrapper-timeout.md  # # ADR-003: Wrapper timeout delegation (H
│       ├── 004-sync-timeout.md  # # ADR-004: Sync execute timeouts (H5)...
│       ├── 005-shared-mutation.md  # # ADR-005: Shared-mutation model (H1/H11
│       └── 006-first-ok.md  # # ADR-006: First-Ok semantics (H8) ...
├── bin/
│   ├── classifier-probe.py  # #!/usr/bin/env python3
│   ├── coral-router-test.py  # #!/usr/bin/env python3
│   ├── download_yago_taxonomy.sh
│   ├── gate-run.sh
│   ├── gen_simhash_projections.py  # #!/usr/bin/env python3
│   ├── gen_yago_classes.py  # #!/usr/bin/env python3
│   ├── live-ai-guard.sh
│   ├── llm-boundary-check.sh
│   ├── parse_yago_taxonomy_to_json.py  # #!/usr/bin/env python3
│   ├── protopyne-build-matrix.sh
│   ├── prune_yago_taxonomy.py  # #!/usr/bin/env python3
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
│   ├── common-core/
│   │   ├── ARCHITECTURE.md  # # common-core — Architecture
│   │   └── VISION.md  # # common-core — Vision
│   ├── coral/
│   │   ├── ARCHITECTURE.md  # # coral-context — Architecture
│   │   ├── CHANGELOG.md  # # Changelog
│   │   ├── DETAILS.md  # # Coral Context: Detailed Engineering Sp
│   │   ├── OVERVIEW.md  # # Coral Context: Architectural Design Do
│   │   └── VISION.md  # # Coral Context — Vision
│   ├── dag/
│   │   ├── ARCHITECTURE.md  # # fluent-dag — Architecture
│   │   └── VISION.md  # # Unified Dependency Resolver — Vision
│   ├── db/
│   │   ├── ARCHITECTURE.md  # # fluent-db — Architecture
│   │   └── VISION.md  # # fluent-db — Vision
│   ├── fluent-concurrency/
│   │   ├── ARCHITECTURE.md  # # fluent-concurrency — Architecture
│   │   ├── CREDITFLOW_TIER_FEED.md  # # CreditFlow — control-plane wiring...
│   │   └── VISION.md  # # fluent-concurrency — Vision
│   ├── fluent-onnx/
│   │   └── ARCHITECTURE.md  # # fluent-onnx — Architecture
│   ├── fluent-wvr/
│   │   ├── ARCHITECTURE.md  # # fluent-wvr — Architecture
│   │   ├── COMPOSABILITY.md  # # Composability Guide
│   │   └── VISION.md  # # fluent-wvr — Vision
│   ├── guidance/
│   │   ├── ARCHITECTURE.md  # # guidance — Architecture
│   │   ├── OVERVIEW.md  # # guidance — Reverse-Engineering Re...
│   │   ├── VISION.md  # # guidance — Vision
│   │   └── schemas/
│   │       └── guidance.schema.json
│   ├── memory-plugin/
│   │   └── ARCHITECTURE.md  # # Memory Plugin Architecture — Clea...
│   ├── ontology/
│   │   ├── ARCHITECTURE.md  # # guidance-ontology — Architecture
│   │   └── VISION.md  # # guidance-ontology — Vision
│   ├── orator/
│   │   ├── ARCHITECTURE.md  # # Orator — Architecture
│   │   ├── CALIBRATION.md  # # Orator calibration register
│   │   ├── DESKTOP_WIRING.md  # # Desktop wiring plan (Murmur parity, Q1
│   │   ├── HANDOFF.md  # # Orator handoff
│   │   ├── HANDOFF_Q.md  # # Orator handoff (red-team hardening, Q0
│   │   ├── PARITY_GAPS.md  # # Orator parity gaps (Moshi C01–C18...
│   │   ├── REDTEAM_FINDINGS.md  # # Orator Red-Team Findings
│   │   └── VISION.md  # # Orator — Vision
│   ├── protopyne-rs/
│   │   ├── ARCHITECTURE.md  # # protopyne-rs — Architecture Map &...
│   │   ├── README.md  # # protopyne-rs
│   │   ├── TO_WIRE.md  # # protopyne-rs — Unwired Surfaces I...
│   │   ├── UI_VISION.md  # Here is an optimized technical design de
│   │   └── VISION.md  # # protopyne-rs — Vision
│   ├── router/
│   │   ├── ARCHITECTURE.md  # # Coral Router — Architecture
│   │   ├── INSTANCES.md  # # Coral Router: /instances API - shared-
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
│   │   ├── fluent-egui/
│   │   │   └── SKILL.md  # # fluent-egui — Domain-Blind Native...
│   │   ├── fluent-wvr/
│   │   │   └── SKILL.md  # # Fluent WVR = Fluent, Wrapped, Verified
│   │   └── interlingua/
│   │       └── SKILL.md  # # Interlingua — unified, disambigua...
│   └── spacy-rs/
│       ├── ARCHITECTURE.md  # # spacy-rs — Architecture
│       ├── BLOB_SPEC.md  # # Taxonomy Blob Specification — Dec...
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
    │   │       ├── component_gate_cli.rs  # //! `guidance component-gate` is a read-
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
    │   │   ├── capacity.rs  # //! Shared bounded-capacity guard: zero 
    │   │   ├── checksum.rs  # //! Zero-domain checksum primitives (no 
    │   │   ├── cite.rs  # //! `file:line` citation scanner — ...
    │   │   ├── config.rs  # //! JSON config loaders: `load_json_or_d
    │   │   ├── constants.rs  # //! Cross-crate magic numbers (size caps
    │   │   ├── drift.rs  # //! Bit-set drift analysis: compute "mis
    │   │   ├── envelope.rs  # //! Playout envelope math: zero-domain g
    │   │   ├── error.rs  # //! Shared leaf error types: `IoError`, 
    │   │   ├── error_context.rs  # //! Contextual error wrappers: `ErrorCon
    │   │   ├── format.rs  # //! Human-readable output: `format_json`
    │   │   ├── git.rs  # //! Git operations — thin wrappers ...
    │   │   ├── golden.rs  # //! Golden comparison and per-stage timi
    │   │   ├── hash.rs  # //! Hashing utilities: `blake3_*`, `sha2
    │   │   ├── http.rs  # //! Process-wide shared HTTP client (and
    │   │   ├── interner.rs  # //! Capability registry: thread-safe str
    │   │   ├── io.rs  # use std::fs;
    │   │   ├── jsonrpc.rs  # //! Shared JSON-RPC 2.
    │   │   ├── label_match.rs  # //! Hash-parameterized label matching: "
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── metrics.rs  # //! Lock-free latency histogram with 12 
    │   │   ├── path.rs  # //! Pure lexical path helpers (no I/O): 
    │   │   ├── pcm.rs  # //! PCM sample math: zero-domain `f32...
    │   │   ├── prelude.rs  # //! The common-core prelude — impor...
    │   │   ├── registry.rs  # //! Generic keyed registry — the ca...
    │   │   ├── retry.rs  # //! Retry and backoff primitives — ...
    │   │   ├── runtime.rs  # //! Sync→async runtime bridge: run ...
    │   │   ├── score.rs  # use std::collections::HashMap;
    │   │   ├── shell.rs  # //! Subprocess helpers: `run_capture`, `
    │   │   ├── shell_parser.rs  # //! Safe shell parser: whitespace+quote 
    │   │   ├── sort.rs  # //! Deterministic ordering: the single c
    │   │   ├── string.rs  # //! 20+ string utilities: case-insensiti
    │   │   ├── sync.rs  # //! Poison-safe locking helpers for `std
    │   │   ├── telemetry.rs  # //! Failure taxonomy — the generic ...
    │   │   ├── time.rs  # //! Time utilities: epoch-second helpers
    │   │   ├── walk.rs  # //! Directory walker: `walk_files` (call
    │   │   └── watchdog.rs  # use std::collections::VecDeque;
    │   └── tests/
    │       ├── ascii_tokens.rs  # //! Golden outputs for [`common_core::st
    │       ├── blob_cursor.rs  # //! Focused unit tests for `common_core:
    │       ├── blob_spec.rs  # use common_core::blob_spec::*;
    │       ├── cache.rs  # // NOTE (ROADMAP_20260903_LLM M11): the 
    │       ├── cache_singleflight.rs  # //! Single-flight `LoadCache::get_or_loa
    │       ├── calibration.rs  # // Exact asserts on harness outputs are 
    │       ├── capacity.rs  # use common_core::capacity::nonzero_capac
    │       ├── checksum.rs  # use common_core::checksum::luhn_valid;
    │       ├── cite.rs  # use common_core::cite::*;
    │       ├── config.rs  # use common_core::config::*;
    │       ├── constants.rs  # use common_core::constants::*;
    │       ├── doc_stubs.rs  # //! Stale-name guard for the `common-cor
    │       ├── drift.rs  # //! Bit-set drift analysis: compute "mis
    │       ├── envelope.rs  # //! `common_core::envelope` unit tests: 
    │       ├── error.rs  # use common_core::error::*;
    │       ├── error_context.rs  # use common_core::error_context::*;
    │       ├── fingerprint_framing.rs  # //! Baseline pins for the fingerprint fr
    │       ├── fingerprint_primitives.rs  # //! Call-site-independent pins for the s
    │       ├── format.rs  # use common_core::format::*;
    │       ├── fts_match.rs  # //! Golden outputs for [`common_core::st
    │       ├── git.rs  # use common_core::git::*;
    │       ├── golden.rs  # use common_core::golden::{compare_series
    │       ├── hash.rs  # use common_core::hash::*;
    │       ├── http.rs  # // NOTE (ROADMAP_20260903_LLM M11): the 
    │       ├── interner.rs  # use common_core::interner::*;
    │       ├── io.rs  # use common_core::io::*;
    │       ├── jsonrpc.rs  # use common_core::jsonrpc::*;
    │       ├── label_match.rs  # //! Focused unit tests for `common_core:
    │       ├── lib.rs  # #[allow(unused_imports)]
    │       ├── metrics.rs  # use common_core::metrics::*;
    │       ├── path.rs  # use common_core::path::{
    │       ├── pcm.rs  # //! `common_core::pcm` unit tests: scale
    │       ├── prelude.rs  # //! Prelude completeness: every symbol t
    │       ├── registry.rs  # use common_core::registry::*;
    │       ├── retry.rs  # use common_core::retry::*;
    │       ├── runtime.rs  # use common_core::runtime::*;
    │       ├── score.rs  # // Exact asserts on pure-function output
    │       ├── shell.rs  # use common_core::shell::*;
    │       ├── shell_parser.rs  # use common_core::shell_parser::*;
    │       ├── sort.rs  # use std::cmp::Ordering;
    │       ├── span_splice.rs  # //! Golden outputs for [`common_core::st
    │       ├── sqlite.rs  # //! Consumer-perspective coverage for th
    │       ├── string.rs  # use common_core::string::*;
    │       ├── sync.rs  # use common_core::sync::*;
    │       ├── telemetry.rs  # // NOTE (ROADMAP_20260903_LLM M11): the 
    │       ├── walk.rs  # use common_core::walk::{sniff_is_binary,
    │       └── watchdog.rs  # use common_core::watchdog::*;
    ├── concept/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── annotation_store.rs  # //! `AnnotationStore` — the tiered ...
    │   │   ├── concept_store.rs  # //! The single source of truth for conce
    │   │   ├── concept_store_mem.rs  # //! The **hermetic in-memory** [`Concept
    │   │   ├── content_store.rs  # //! Content-node store seam + eager LOD 
    │   │   ├── lib.rs  # //! The neutral shared home for concept 
    │   │   └── plausibility.rs  # //! Text-half / knowledge-half bridge fo
    │   └── tests/
    │       ├── annotation_store.rs  # use super::*;
    │       ├── concept_store.rs  # use super::*;
    │       ├── concept_store_mem.rs  # use super::*;
    │       └── content_store.rs  # use super::*;
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
    │   │   ├── hierarchy.rs  # //! The one hierarchy-closure index for 
    │   │   ├── lib.rs  # //! fluent-dag: DAG executor with resolv
    │   │   ├── middleware.rs  # use std::sync::Arc;
    │   │   ├── narrowing.rs  # use std::collections::HashSet;
    │   │   ├── resolver.rs  # //! Capability-aware dependency resolver
    │   │   ├── target.rs  # use bitvec::vec::BitVec;
    │   │   ├── target_work_unit.rs  # //! `Target → WorkUnit` bridge.
    │   │   ├── type_inference.rs  # //! Ontology type-hierarchy inference.
    │   │   ├── version.rs  # //! Version-constraint resolution for pa
    │   │   ├── work_unit.rs  # use bon::Builder;
    │   │   ├── wvr.rs  # //! Fluent WVR integration for DAG crate
    │   │   └── yamake_loader.rs  # use bitvec::vec::BitVec;
    │   └── tests/
    │       ├── checkpointed.rs  # use super::*;
    │       ├── closure.rs  # use super::*;
    │       ├── common.rs  # //! Crate-typed test fixtures shared by 
    │       ├── dep_graph.rs  # use super::*;
    │       ├── error.rs  # use super::*;
    │       ├── hierarchy.rs  # use super::*;
    │       ├── middleware.rs  # use super::*;
    │       ├── mod.rs  # //! Tier-1 test support for fluent-dag, 
    │       ├── narrowing.rs  # use super::*;
    │       ├── resolver.rs  # use super::*;
    │       ├── scan_sort_equiv.rs  # //! Equivalence + golden coverage for th
    │       ├── target.rs  # use super::*;
    │       ├── target_work_unit.rs  # use super::*;
    │       ├── type_inference.rs  # use super::*;
    │       ├── version.rs  # use crate::resolver::{DependencyResolver
    │       ├── work_unit.rs  # use super::*;
    │       └── yamake_loader.rs  # use super::*;
    ├── db/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── cache.rs  # //! Generic TTL/LRU key-value cache stor
    │   │   ├── capability.rs  # //! The capability-gated async database 
    │   │   ├── embedding_cache.rs  # //! Embedding-cache DDL — the singl...
    │   │   ├── error.rs  # //! The single database error taxonomy f
    │   │   ├── hnsw.rs  # //! The canonical HNSW-backed vector ind
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── migrate.rs  # //! Idempotent schema migrations.
    │   │   ├── outbox.rs  # //! Generic append-only transactional ou
    │   │   ├── pool.rs  # //! The canonical pooled SQLite store (D
    │   │   ├── query.rs  # //! Typed statement helpers shared by `S
    │   │   ├── sqlite.rs  # //! SQLite connection setup, WAL mode, s
    │   │   ├── store.rs  # //! The canonical single-connection SQLi
    │   │   ├── vector.rs  # //! Embedding vector math.
    │   │   └── wvr.rs  # //! Database `Component`/`WorkUnit` adap
    │   └── tests/
    │       ├── bind_path.rs  # //! The `OwnedValue` bind path: type mat
    │       ├── cache.rs  # use super::*;
    │       ├── capability.rs  # use super::*;
    │       ├── capability_surface.rs  # //! Guard: `DbCapability` stays a minima
    │       ├── common.rs  # //! Crate-typed test fixtures shared by 
    │       ├── doc_stubs.rs  # //! Stale-name guard for the `fluent-db`
    │       ├── embedding_cache.rs  # //! Embedding-cache DDL goldens (moved, 
    │       ├── error.rs  # use super::*;
    │       ├── hnsw.rs  # use super::*;
    │       ├── hnsw_concurrent.rs  # //! Concurrent HNSW behavior: generation
    │       ├── migrate.rs  # use super::*;
    │       ├── mod.rs  # //! Tier-1 test support for fluent-db, w
    │       ├── outbox.rs  # use crate::outbox;
    │       ├── pool.rs  # use super::*;
    │       ├── query.rs  # use super::*;
    │       ├── sqlite.rs  # //! The `fluent_db::sqlite` connection/s
    │       ├── store.rs  # use super::*;
    │       ├── vector.rs  # // Exact asserts on pure-function output
    │       └── wvr.rs  # use std::sync::Arc;
    ├── fluent-concurrency/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── affinity.rs  # //! Affinity-aware priority scheduler.
    │   │   ├── batch.rs  # //! Supervised batch runner with async r
    │   │   ├── capability.rs  # //! Concrete capability tokens for files
    │   │   ├── close.rs  # //! Shared close-wakes-waiters core for 
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
    │   │   ├── pool/
    │   │   │   ├── common.rs  # //! Shared worker-loop internals for eve
    │   │   │   ├── limiter.rs  # //! Semaphore-based concurrency limiter 
    │   │   │   ├── priority.rs  # //! Priority-ordered result pool (`Prior
    │   │   │   ├── queue.rs  # //! Bounded close-wakes-waiters queue (`
    │   │   │   ├── result.rs  # //! Result-returning worker pool (`Resul
    │   │   │   ├── traits.rs  # //! Pool error and control-plane pool tr
    │   │   │   └── worker.rs  # //! Fire-and-forget worker pool (`Worker
    │   │   ├── pool.rs  # //! Bounded async queue, worker pool, an
    │   │   ├── queue.rs  # //! A priority queue with a fast path fo
    │   │   ├── reconnect.rs  # //! Supervised reconnect-with-backfill w
    │   │   ├── retry.rs  # //! Retry/backoff drivers for concurrenc
    │   │   ├── runtime/
    │   │   │   ├── mod.rs  # //! Pluggable `Runtime` backends (produc
    │   │   │   ├── test.rs  # //! Test `Runtime` implementation with p
    │   │   │   └── tokio.rs  # //! Production `Runtime` implementation 
    │   │   ├── scope.rs  # //! Structured concurrency via `Scope...
    │   │   ├── stream.rs  # //! Cooperative cancellation for long-li
    │   │   ├── sync.rs  # //! Poison-safe locking for `std::sync` 
    │   │   └── thread_resource.rs  # //! Per-thread lazy-initialized resource
    │   └── tests/
    │       ├── affinity.rs  # use super::*;
    │       ├── affinity_calibration.rs  # //! M2c — Calibration for affinity ...
    │       ├── audit_h5.rs  # //! Audit H5 + H8-ladder + H13-boundary 
    │       ├── credit_pool.rs  # use super::*;
    │       ├── doc_stubs.rs  # //! Stale-name guard for the `fluent-con
    │       ├── e2e.rs  # use super::*;
    │       ├── feed_worker.rs  # use super::*;
    │       ├── flow.rs  # use super::*;
    │       ├── ladder.rs  # use super::*;
    │       ├── m-affinity-purity.rs  # //! `effective_priority` purity + priori
    │       ├── m-credit-pool-race.rs  # //! Submit/drain race coverage: concurre
    │       ├── m1.rs  # use super::*;
    │       ├── m2.rs  # use super::*;
    │       ├── m3.rs  # use super::*;
    │       ├── m4.rs  # use super::*;
    │       ├── m5.rs  # // Exercises the capability-gated I/O en
    │       ├── mod.rs  # use std::sync::atomic::{AtomicUsize, Ord
    │       ├── pool.rs  # use super::*;
    │       ├── pool_paths.rs  # //! Facade compatibility: every public i
    │       ├── pool_submit_all.rs  # use std::time::Duration;
    │       ├── reconnect.rs  # use super::*;
    │       ├── retry_reexport.rs  # //! The `crate::retry` surface is the si
    │       ├── scope_caps.rs  # use std::sync::Arc;
    │       ├── scoped.rs  # use fluent_concurrency::scope::scoped;
    │       ├── stream.rs  # use super::*;
    │       └── thread_resource.rs  # use super::*;
    ├── fluent-egui/
    │   ├── Cargo.toml
    │   ├── benches/
    │   │   └── frame_budget.rs  # //! Frame-budget baselines for the gener
    │   ├── src/
    │   │   ├── annotations.rs  # //! Append-only annotations with bounded
    │   │   ├── config.rs  # //! Generic control plane: pane layout, 
    │   │   ├── context_menu.rs  # //! Pane-scoped secondary-click detectio
    │   │   ├── dialog.rs  # //! Staged dialog lifecycle: drafts that
    │   │   ├── error.rs  # //! Typed errors for the paint edge.
    │   │   ├── feed.rs  # //! Concurrency perimeter: feeds, waves,
    │   │   ├── fields.rs  # //! Schema-driven control-plane field ed
    │   │   ├── fonts.rs  # //! Bundled-font installation over egui'
    │   │   ├── hittest.rs  # //! Pure hit-test and crosshair mapping 
    │   │   ├── icons.rs  # //! Shell-owned icon cache: fingerprinte
    │   │   ├── images.rs  # //! Gated image ingestion: pool-bound fe
    │   │   ├── interaction.rs  # //! Pure pointer-interaction state machi
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── modal.rs  # //! Generic searchable, categorized cata
    │   │   ├── notice.rs  # //! Bounded notice queue: one live list 
    │   │   ├── paint.rs  # //! Paint edge: headless geometry plus t
    │   │   ├── plan.rs  # //! DAG-ordered pane compute plan plus a
    │   │   ├── replay.rs  # //! Replay and checkpoints over a caller
    │   │   ├── slot.rs  # //! Hot-slotted indicator control stack:
    │   │   ├── style.rs  # //! Per-indicator paint overrides: the S
    │   │   ├── table.rs  # //! Virtualized table plan: column regis
    │   │   ├── tokens.rs  # //! Material token theme system: a valid
    │   │   ├── view.rs  # //! Headless pane view-models over conve
    │   │   ├── viewport.rs  # //! Viewport geometry on the canonical s
    │   │   └── wheel.rs  # //! Region-aware wheel routing: where a 
    │   └── tests/
    │       ├── alpha_opacity.rs  # use super::*;
    │       ├── budget.rs  # //! Audit ledger: allocation shape, depe
    │       ├── config.rs  # use super::*;
    │       ├── context_menu.rs  # use super::*;
    │       ├── dialog.rs  # use super::*;
    │       ├── error.rs  # use super::*;
    │       ├── feed.rs  # use super::*;
    │       ├── fields.rs  # //! Schema-driven control-plane editors:
    │       ├── fonts.rs  # use super::*;
    │       ├── history.rs  # use super::*;
    │       ├── hittest.rs  # use super::*;
    │       ├── icons.rs  # use super::*;
    │       ├── images.rs  # use super::*;
    │       ├── interaction.rs  # use super::*;
    │       ├── lib.rs  # use super::*;
    │       ├── modal.rs  # //! Searchable, categorized catalog moda
    │       ├── notice.rs  # use super::*;
    │       ├── paint.rs  # use super::*;
    │       ├── plan.rs  # use super::*;
    │       ├── replay.rs  # use super::*;
    │       ├── slot.rs  # //! Hot-slot control stack: registration
    │       ├── style.rs  # //! Per-indicator paint overrides: the S
    │       ├── table.rs  # use super::*;
    │       ├── tokens.rs  # use super::*;
    │       ├── ui/
    │       │   ├── no_generic_undo.rs  # // Compile-fail ratchet for the no-undo 
    │       │   └── no_generic_undo.stderr
    │       ├── view.rs  # use super::*;
    │       ├── viewport.rs  # use super::*;
    │       └── wheel.rs  # //! Region-aware wheel routing: a wheel 
    ├── fluent-onnx/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── align.rs  # //! LFM ↔ spacy-rs token alignment.
    │   │   ├── annotate.rs  # //! The trained-encoder annotation rung.
    │   │   ├── colbert.rs  # //! ColBERT late-interaction retrieve...
    │   │   ├── config.rs  # //! ONNX model configuration schema ...
    │   │   ├── context.rs  # //! The per-context KV surface: `PastSta
    │   │   ├── context_pool.rs  # //! `OnnxContextPool` — the onnx ha...
    │   │   ├── encoder.rs  # //! `OrtEncoder` — the base Encoder...
    │   │   ├── error.rs  # //! Error type for `fluent-onnx` — ...
    │   │   ├── grammar.rs  # //! Grammar-constrained decoding primiti
    │   │   ├── lib.rs  # //! `fluent-onnx` — ONNX / `ort` wo...
    │   │   ├── llm.rs  # //! The generative `CausalLm` decoder...
    │   │   ├── ort_loader.rs  # //! Real session loader backed by ONNX R
    │   │   ├── overlay.rs  # //! Overlay data types and the `Residual
    │   │   ├── pii.rs  # //! PII detection: the BILUO decode, the
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
    │       └── two_tower.rs  # // Exact asserts on stub similarity outp
    ├── fluent-wvr/
    │   ├── Cargo.toml
    │   ├── benches/
    │   │   └── tier_costs.rs  # //! Soak-tier cost table for the reflect
    │   ├── fuzz/
    │   │   ├── Cargo.toml
    │   │   └── fuzz_targets/
    │   │       └── repair.rs  # #![no_main]
    │   ├── src/
    │   │   ├── boundary.rs  # //! Schema-driven tolerant decoding of b
    │   │   ├── capability.rs  # use std::any::{Any, TypeId};
    │   │   ├── coerce.rs  # //! Boundary-string coercion: turning un
    │   │   ├── dynamic.rs  # //! `DynamicComponent` — a `Compone...
    │   │   ├── ladder.rs  # //! First-accept-wins combinator — ...
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── macros.rs  # /// Eliminates the 7-line `as_any`/`as_a
    │   │   ├── metadata.rs  # use serde::{Deserialize, Serialize};
    │   │   ├── prelude.rs  # //! The fluent-wvr prelude — import...
    │   │   ├── runtime.rs  # use std::future::Future;
    │   │   ├── store.rs  # //! Typed in-process handoff accumulator
    │   │   ├── test_support.rs  # //! Crate-local test support for fluent-
    │   │   ├── tests.rs  # use crate::test_support::TestComponent;
    │   │   ├── traits.rs  # use std::any::Any;
    │   │   ├── validate.rs  # use crate::{WorkError, WorkOutput};
    │   │   ├── work.rs  # use std::collections::HashMap;
    │   │   └── wrapper.rs  # use std::collections::HashMap;
    │   └── tests/
    │       ├── audit_h1.rs  # //! AUDIT-H1: shared-handle mutation inc
    │       ├── audit_h13.rs  # //! AUDIT-H13: boundary decoder guarante
    │       ├── audit_h2.rs  # //! AUDIT-H2: describe() schema fidelity
    │       ├── audit_h3.rs  # //! AUDIT-H3: numeric parsing goes throu
    │       ├── audit_h4.rs  # //! AUDIT-H4: derive coverage of non-sca
    │       ├── audit_h6.rs  # //! AUDIT-H6: wrapper delegation complet
    │       ├── audit_h8.rs  # //! AUDIT-H8: cascade first-Ok vs ladder
    │       ├── audit_ignore_hygiene.rs  # //! Audit-ignore hygiene: every `#[ignor
    │       ├── boundary.rs  # #![allow(unused_imports)]
    │       ├── boundary_contract.proptest-regressions
    │       ├── boundary_contract.rs  # //! Boundary contract: the guarantees `r
    │       ├── capability.rs  # #![allow(unused_imports)]
    │       ├── cascade_acceptance.rs  # //! Golden calibration corpus for the ca
    │       ├── coerce.rs  # #![allow(unused_imports)]
    │       ├── doc_stubs.rs  # //! Stale-name guard for the `fluent-wvr
    │       ├── dynamic.rs  # #![allow(unused_imports)]
    │       ├── ladder.rs  # //! Infallible ordered-`Option` ladder c
    │       ├── macros.rs  # #![allow(unused_imports)]
    │       ├── metadata.rs  # #![allow(unused_imports)]
    │       ├── runtime.rs  # #![allow(unused_imports)]
    │       ├── schema_edges.rs  # //! Schema edges: `pattern`/`clamp` keyw
    │       ├── skill_doc_pins.rs  # //! Compiled pins for skill-doc claims t
    │       ├── store.rs  # #![allow(unused_imports)]
    │       ├── traits.rs  # #![allow(unused_imports)]
    │       ├── validate.rs  # use fluent_wvr::validate::{check_for_exe
    │       ├── work.rs  # #![allow(unused_imports)]
    │       └── wrapper.rs  # #![allow(unused_imports)]
    ├── fluent-wvr-macros/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   └── unknown_attr_tests.rs  # //! Unknown `#[field(...
    │   └── tests/
    │       ├── derive_expansion.rs  # //! Expansion tests for the fluent-wvr d
    │       ├── derive_ui.rs  # //! Compile-time expansion ratchet for t
    │       └── ui/
    │           ├── shapes.rs  # //! Pass case: every composition shape t
    │           ├── unknown_attr.rs  # //! Compile-fail case: the field-attribu
    │           └── unknown_attr.stderr
    ├── fluent-wvr-testutil/
    │   ├── Cargo.toml
    │   └── src/
    │       └── lib.rs  # //! Test utilities for Fluent WVR crates
    ├── guidance/
    │   ├── Cargo.toml
    │   ├── benches/
    │   │   └── recall_depth_sweep.rs  # //! P6 recall-depth sweep: fused recall 
    │   ├── component_snapshot.json
    │   ├── corpora/
    │   │   ├── excerpt_seed.json
    │   │   ├── intent_seed.json
    │   │   ├── nav_seed.json
    │   │   ├── staleness_seed.json
    │   │   ├── sufficiency_seed.json
    │   │   └── triage_seed.json
    │   ├── fixture_graph/
    │   │   ├── g_caller.rs  # fn g_call() {
    │   │   ├── ground_truth.json
    │   │   ├── h_a.rs  # pub fn dup() {}
    │   │   ├── h_b.rs  # pub fn dup() {}
    │   │   ├── h_c.rs  # pub fn dup() {}
    │   │   ├── h_caller.rs  # fn h_call() {
    │   │   ├── i_base.rs  # pub fn foundation() {}
    │   │   ├── i_user.rs  # mod i_base;
    │   │   ├── u_callee.rs  # pub fn u_target() {}
    │   │   └── u_caller.rs  # fn u_call() {
    │   ├── scorecard_baseline.json
    │   ├── src/
    │   │   ├── ast_parser.rs  # use std::path::Path;
    │   │   ├── change_set.rs  # //! P3 change tracking (port of zvec-gre
    │   │   ├── component_map.rs  # //! Workspace file → narrowest-gate...
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
    │   │   ├── model_lanes.rs  # //! Model-call priority lanes: interacti
    │   │   ├── offload.rs  # //! Deterministic offload applier: the n
    │   │   ├── plugin.rs  # use std::path::{Path, PathBuf};
    │   │   ├── query/
    │   │   │   ├── concept_ground.rs  # //! Taxonomy groundedness for concept se
    │   │   │   ├── db_storage.rs  # //! `GuidanceDb` as recall storage: conv
    │   │   │   ├── decision_points.rs  # //! Decision points for the guidance que
    │   │   │   ├── diagnose.rs  # //! Recall diagnosis: why an entity/file
    │   │   │   ├── formatter.rs  # use std::fmt::Write;
    │   │   │   ├── fts_backend.rs  # //! Lexical backend: single-route FTS re
    │   │   │   ├── fusion.rs  # //! Candidate fusion: score sums, orderi
    │   │   │   ├── glob.rs  # //! rg-style glob + path + file-type mat
    │   │   │   ├── hybrid.rs  # //! Hybrid orchestration: query → p...
    │   │   │   ├── identifier.rs  # use common_core::string::{contains_ignor
    │   │   │   ├── ingest.rs  # //! P1 minimal ingestion: whole-file fra
    │   │   │   ├── lemma_backend.rs  # //! Lemma backend: query lemmas looked u
    │   │   │   ├── llm_filter.rs  # use common_core::string::contains_ignore
    │   │   │   ├── llm_filter_batch.rs  # //! Chunked synchronous scoring over one
    │   │   │   ├── mod.rs  # pub mod concept_ground;
    │   │   │   ├── plan.rs  # //! Search-plan validation: closed-enum 
    │   │   │   ├── recall.rs  # //! Hybrid recall pipeline: plan validat
    │   │   │   ├── rerank.rs  # //! LLM reranker response parsing: order
    │   │   │   ├── rg_backend.rs  # //! L0 minimal: capability-gated managed
    │   │   │   ├── search_backend.rs  # use common_core::string::contains_ignore
    │   │   │   ├── shadow.rs  # //! Shadow decision hooks for the guidan
    │   │   │   ├── snapshot.rs  # use std::path::Path;
    │   │   │   ├── strategy.rs  # use super::identifier;
    │   │   │   ├── structure_enrich.rs  # //! Structure enrichment (P5): lexical h
    │   │   │   ├── synthesize.rs  # use fluent_types::{GuidanceDoc, Member, 
    │   │   │   └── vector_backend.rs  # //! Vector backend: single-route embeddi
    │   │   ├── query_engine.rs  # use fluent_types::GuidanceDoc;
    │   │   ├── runtime.rs  # use std::path::PathBuf;
    │   │   ├── scanner.rs  # use common_core::string::{contains_any, 
    │   │   ├── scheduler.rs  # //! P3 reconcile job queue: the coalesci
    │   │   ├── scorecard.rs  # //! Hermetic scorecard: the measuring in
    │   │   ├── search_constants.rs  # //! Tested search/index constants (P0 co
    │   │   ├── search_types.rs  # //! Search value types + pure search hel
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
    │   │   └── watcher.rs  # //! P3 file watcher (port of zvec-grep `
    │   └── tests/
    │       ├── capability_decision.rs  # use super::{grounded_first, member_groun
    │       ├── change_set.rs  # //! P3 `change_set` tests (port of zvec-
    │       ├── common/
    │       │   └── mod.rs  # //! Tier-2 (crate-root `tests/`) shared 
    │       ├── component_gate_map.rs  # use super::{component_gate_for_path, sna
    │       ├── coordinator.rs  # //! P3 coordinator tests (port of zvec-g
    │       ├── corpus_seeds.rs  # use fluent_llm::decision::{gate_for_poin
    │       ├── decision_metrics.rs  # use super::{evaluate_with_metrics, Decis
    │       ├── decision_points.rs  # use super::{
    │       ├── e2e_gen_roundtrip.rs  # use fluent_types::MemberType;
    │       ├── excerpt_decision.rs  # use super::{excerpt_spec, relevance_gate
    │       ├── extract_code.rs  # //! P2 code-extraction tests (ports `tes
    │       ├── extract_markdown.rs  # //! P2 markdown-extraction tests: headin
    │       ├── extract_text.rs  # //! P2 text-extraction tests: window bud
    │       ├── extract_vector_content.rs  # //! P2 `vector_content` tests: metadata 
    │       ├── freshness.rs  # //! P3 freshness-matrix tests: `fresh` /
    │       ├── graph_accuracy.rs  # use super::{GraphIndex, HarvestInput, Re
    │       ├── graph_index.rs  # //! P3 `graph_index` tests: per-language
    │       ├── graph_resolution_labels.rs  # use super::{GraphIndex, HarvestInput, Re
    │       ├── hybrid_parity.rs  # //! Hermetic known-item parity matrix (P
    │       ├── index_diff.rs  # //! P2 diff tests: added/modified/pendin
    │       ├── index_pipeline.rs  # //! P2 pipeline tests (ports `service.
    │       ├── index_selection.rs  # //! P2.4 scanner-utils port: discovery r
    │       ├── intent_decision.rs  # use crate::query::decision_points::{inte
    │       ├── l2_probe.rs  # //! P6 target (4) probe: inflected-query
    │       ├── lane_priority.rs  # use super::{Lane, LaneConfig, LaneError,
    │       ├── live/
    │       │   ├── README.md  # # guidance-core — Live-AI tests
    │       │   ├── decision_live.rs  # //! Live canary probe for decision-model
    │       │   └── smoke_live.rs  # //! Opt-in live-AI smoke test for the gu
    │       ├── live.rs  # //! Live-AI integration test crate for g
    │       ├── model_roles.rs  # use super::super::config::{
    │       ├── offload_apply.rs  # use super::apply_verified;
    │       ├── offload_draft.rs  # use super::DEFAULT_MAX_ATTEMPTS;
    │       ├── offload_stale_only.rs  # use super::SyncEngine;
    │       ├── offload_summary.rs  # use super::{apply_draft, EscalationPacke
    │       ├── offload_triage.rs  # use super::{classify_finding, triage_spe
    │       ├── parity/
    │       │   ├── corpus_cjk.rs  # //! CJK-heavy fixture corpus (Gate 0 ...
    │       │   └── mod.rs  # //! P0 fixture corpora for legacy parity
    │       ├── query_concept_ground.rs  # //! Hermetic pins for `query::concept_gr
    │       ├── query_db_storage.rs  # //! Row-translator characterization (inc
    │       ├── query_fts_backend.rs  # use super::*;
    │       ├── query_fusion.rs  # use super::*;
    │       ├── query_glob.rs  # use super::*;
    │       ├── query_hybrid.rs  # use super::*;
    │       ├── query_ingest.rs  # //! Lazy lemma-pipeline boundary tests (
    │       ├── query_lemma_backend.rs  # use super::*;
    │       ├── query_plausibility.rs  # //! Hermetic pins for `query::concept_gr
    │       ├── query_recall.rs  # use super::*;
    │       ├── query_rg_backend.rs  # use super::*;
    │       ├── query_structure_enrich.rs  # //! Ported `structure-enrichment.test.
    │       ├── query_vector_backend.rs  # use super::*;
    │       ├── scheduler.rs  # //! P3 scheduler tests: the coalescing c
    │       ├── scorecard.rs  # use super::{render_human, score_metrics,
    │       ├── scorecard_ratchet.rs  # use super::{
    │       ├── search_constants.rs  # use super::*;
    │       ├── search_types.rs  # use super::*;
    │       ├── staleness_decision.rs  # use super::{normalize_signature, stalene
    │       ├── sufficiency_decision.rs  # use super::{route_sufficiency, sufficien
    │       ├── sync_fixpoint.rs  # use super::absolute_or;
    │       ├── synthesis_gate.rs  # use super::{can_synthesize, stage_conten
    │       ├── synthesis_gate_wiring.rs  # use super::{QueryEngine, QueryEngineErro
    │       └── watcher.rs  # //! P3 watcher tests (port of zvec-grep 
    ├── knowledge/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── assembler.rs  # //! The deterministic prompt assemble...
    │   │   ├── csr_graph.rs  # pub const CSR_MAGIC: u32 = 0x4752_5343;
    │   │   ├── freq_table.rs  # use std::fs;
    │   │   ├── index_header.rs  # pub const INDEX_HEADER_SIZE: usize = 10;
    │   │   ├── lib.rs  # //! fluent-knowledge: Word/trigram index
    │   │   ├── lod.rs  # //! Level of detail, 0..
    │   │   ├── lod_policy.rs  # //! Pure LOD policy: fidelity bands, bud
    │   │   ├── query_cache.rs  # //! TTL/LRU query cache delegating to `f
    │   │   ├── selector.rs  # //! Fidelity selection over the content 
    │   │   ├── tokenizer.rs  # pub struct WordTokenizer<'a> {
    │   │   ├── trigram_index.rs  # use crate::index_header::Header;
    │   │   ├── views.rs  # //! Reference-only view layer over the c
    │   │   └── word_index.rs  # use std::collections::HashMap;
    │   └── tests/
    │       ├── assembler.rs  # use super::*;
    │       ├── csr_graph.rs  # use super::*;
    │       ├── freq_table.rs  # use super::*;
    │       ├── index_header.rs  # use super::*;
    │       ├── lod.rs  # use super::*;
    │       ├── lod_policy.rs  # use super::*;
    │       ├── query_cache.rs  # use super::*;
    │       ├── selector.rs  # use super::*;
    │       ├── tokenizer.rs  # use super::*;
    │       ├── trigram_index.rs  # use super::*;
    │       ├── views.rs  # use super::*;
    │       └── word_index.rs  # use super::*;
    ├── llm/
    │   ├── CALIBRATION.md  # # fluent-llm calibration report — R...
    │   ├── Cargo.toml
    │   ├── calibration/
    │   │   └── seed_intent.json
    │   ├── src/
    │   │   ├── anonymize.rs  # /// The ONE request-path anonymize entry
    │   │   ├── artifact.rs  # //! Pinned model-artifact plane: cache-f
    │   │   ├── artifact_lock.rs  # //! Directory-based mutual exclusion for
    │   │   ├── backend.rs  # //! Backend plugin layer: one base trait
    │   │   ├── cache.rs  # //! LLM response cache — the single...
    │   │   ├── catalog.rs  # //! Pinned embedding-model catalog: the 
    │   │   ├── client.rs  # use std::sync::Arc;
    │   │   ├── constants.rs  # //! LLM-domain constants — the sing...
    │   │   ├── context_packer.rs  # use crate::tokens::{estimate_tokens, Tok
    │   │   ├── decision/
    │   │   │   ├── backend.rs  # //! Backend substitution seam for bounde
    │   │   │   ├── breaker.rs  # //! Minimal consecutive-failure breaker 
    │   │   │   ├── bridge.rs  # //! Detached shadow bridge: bounded queu
    │   │   │   ├── bulk.rs  # //! Bulk helpers over concrete types wit
    │   │   │   ├── cache.rs  # //! Bounded in-process cache (no persist
    │   │   │   ├── canary.rs  # //! Fixed canary prompts with expected d
    │   │   │   ├── decode.rs  # //! Single-token logprobs decoder + text
    │   │   │   ├── eval.rs  # //! Threshold calibration harness (no ne
    │   │   │   ├── ladder.rs  # //! Ladder composition: deterministic ru
    │   │   │   ├── mod.rs  # //! Decision-model support: bounded Choi
    │   │   │   ├── question.rs  # //! Two-layer question API: spec Compone
    │   │   │   ├── registry.rs  # //! Calibration registry: thresholds key
    │   │   │   ├── shadow.rs  # //! Shadow guidance consumers (logs only
    │   │   │   ├── transport.rs  # //! Remote opt-in boundary: redaction ho
    │   │   │   ├── types.rs  # //! Decision types + pure policy (no I/O
    │   │   │   └── wvr.rs  # //! Pure render/decode/policy `WorkUnit`
    │   │   ├── decomposer.rs  # use bon::Builder;
    │   │   ├── embeddings.rs  # use std::num::NonZeroUsize;
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
    │   │   ├── pii_calibration.rs  # //! PII calibration harness — contr...
    │   │   ├── pii_label.rs  # //! Closed PII label vocabulary for the 
    │   │   ├── pii_patterns.rs  # use std::sync::LazyLock;
    │   │   ├── prompt.rs  # //! Pure LLM prompt builders shared acro
    │   │   ├── protocol.rs  # //! LLM protocol + request queue — ...
    │   │   ├── qwen.rs  # //! Remote Qwen embedding backends behin
    │   │   ├── resolution.rs  # //! Model-reference resolution order: ex
    │   │   ├── roles.rs  # //! Chat role vocabulary — the clos...
    │   │   ├── runtime.rs  # //! `fluent-llm::runtime` — the sha...
    │   │   ├── sse.rs  # //! SSE line framing — the single o...
    │   │   ├── telemetry.rs  # //! LLM telemetry vocabulary + structure
    │   │   ├── testutil.rs  # //! Shared hermetic doubles for inferenc
    │   │   ├── thinking.rs  # //! Think-block stripping — the sin...
    │   │   ├── tokens.rs  # //! Token budgets — the single owner.
    │   │   └── url.rs  # use thiserror::Error;
    │   └── tests/
    │       ├── anonymize.rs  # use super::*;
    │       ├── anonymize_map.rs  # use super::*;
    │       ├── artifact.rs  # //! Ported from zvec-grep `test/unit/mod
    │       ├── artifact_lock.rs  # //! Ported from zvec-grep `test/unit/mod
    │       ├── backend.rs  # //! Backend plugin layer tests: registry
    │       ├── backend_identity.rs  # use super::*;
    │       ├── cache.rs  # //! LLM response-cache goldens.
    │       ├── calibration.rs  # //! Calibration gate (tests + measuremen
    │       ├── catalog.rs  # //! Ported from zvec-grep `test/unit/mod
    │       ├── client.rs  # use super::*;
    │       ├── constants.rs  # //! LLM constants goldens.
    │       ├── context_packer.rs  # use super::*;
    │       ├── decision_backend.rs  # //! Backend substitution seam tests (tes
    │       ├── decision_boundary.rs  # //! Remote opt-in boundary tests (tests-
    │       ├── decision_bridge.rs  # //! Detached shadow bridge tests (tests-
    │       ├── decision_bulk.rs  # //! Bulk helpers tests (tests-first).
    │       ├── decision_cache.rs  # //! Bounded in-process cache tests (test
    │       ├── decision_canary.rs  # //! Canary fingerprint tests (tests-firs
    │       ├── decision_decode.rs  # //! Single-token logprobs decoder + text
    │       ├── decision_doc_stubs.rs  # //! Stale-name guard for the decision mo
    │       ├── decision_eval.rs  # //! Threshold calibration harness tests 
    │       ├── decision_ladder.rs  # //! Breaker + ladder composition tests (
    │       ├── decision_parity.rs  # //! Backward-compat parity tests (tests-
    │       ├── decision_question.rs  # //! Two-layer question API tests (tests-
    │       ├── decision_registry.rs  # //! Calibration registry tests (tests-fi
    │       ├── decision_shadow.rs  # //! Shadow wrapper tests (tests-first).
    │       ├── decision_spike_fixtures.rs  # //! Shared hermetic fixtures for decisio
    │       ├── decision_transport.rs  # //! Async decide seam tests over `httpmo
    │       ├── decision_types.rs  # //! Decision types + pure policy tests (
    │       ├── decomposer.rs  # use super::*;
    │       ├── embed_cache.rs  # //! `CachedEmbeddingProvider` behavior: 
    │       ├── embeddings.rs  # use super::*;
    │       ├── error.rs  # use super::*;
    │       ├── factory.rs  # //! Ported from zvec-grep `test/unit/mod
    │       ├── gguf.rs  # //! Ported from zvec-grep `test/unit/mod
    │       ├── grants.rs  # //! Ported from zvec-grep authorization 
    │       ├── http_class.rs  # use super::*;
    │       ├── live/
    │       │   ├── README.md  # # fluent-llm — Live-AI tests
    │       │   ├── decision_spike.rs  # //! Throwaway live spike: Choice + Noul 
    │       │   ├── embed_live.rs  # //! Opt-in live-AI test for the `llama:`
    │       │   ├── p4b_live.rs  # //! Live artifact-plane + remote-embeddi
    │       │   └── smoke_live.rs  # //! Opt-in live-AI smoke test for the fl
    │       ├── live.rs  # //! Live-AI integration test crate for f
    │       ├── llm_queue.rs  # use super::*;
    │       ├── no_domain_imports.rs  # //! No-domain-imports guard.
    │       ├── onnx_config.rs  # use super::*;
    │       ├── onnx_session.rs  # use super::*;
    │       ├── openai.rs  # //! OpenAI envelope goldens.
    │       ├── parse.rs  # use super::*;
    │       ├── pii.rs  # use super::*;
    │       ├── pii_calibration.rs  # use crate::backend::{PiiSpanDetector, Re
    │       ├── pii_label.rs  # use super::*;
    │       ├── pii_patterns.rs  # use super::*;
    │       ├── prompt.rs  # use super::*;
    │       ├── protocol_parity.rs  # //! Protocol-ownership parity.
    │       ├── qwen.rs  # //! Ported from zvec-grep `test/unit/mod
    │       ├── residency_state.rs  # use super::*;
    │       ├── resolution.rs  # //! Ported from zvec-grep `resolution.
    │       ├── roles.rs  # use super::*;
    │       ├── runtime.rs  # use super::*;
    │       ├── sse.rs  # //! SSE framing goldens.
    │       ├── telemetry.rs  # //! LLM telemetry-vocabulary goldens.
    │       ├── testutil.rs  # //! Shared stub-backend contract: route-
    │       ├── thinking.rs  # //! Think-block stripping goldens.
    │       ├── tokens.rs  # //! Token-budget goldens.
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
    │   │   ├── entity_stoplist.txt
    │   │   └── yago_classes.json
    │   ├── src/
    │   │   ├── bin/
    │   │   │   └── ysm1_encode.rs  # //! Operator tool: encode a YaGO taxonom
    │   │   ├── canonical.rs  # //! The versioned canonical-form table.
    │   │   ├── entity.rs  # use std::collections::HashMap;
    │   │   ├── inference.rs  # use std::collections::{HashMap, HashSet}
    │   │   ├── lib.rs  # //! guidance-ontology: Entity extraction
    │   │   ├── mapper.rs  # use std::collections::HashMap;
    │   │   ├── migration.rs  # #[derive(Debug, Clone)]
    │   │   ├── normalize.rs  # //! Single source for YaGO name normaliz
    │   │   ├── plausibility.rs  # //! Taxonomy plausibility scoring —...
    │   │   ├── prefix.rs  # //! Well-known CURIE prefixes behind a c
    │   │   ├── taxonomy.rs  # //! YaGO TTL → JSON taxonomy pipeli...
    │   │   ├── turtle.rs  # //! The one Turtle `rdfs:subClassOf` rea
    │   │   ├── yago.rs  # pub const NS_YAGO: &str = "http://yago-k
    │   │   ├── yago_loader.rs  # //! The YaGO taxonomy loader (ROADMAP...
    │   │   ├── yago_view.rs  # //! YagoView — runtime file, safe `...
    │   │   └── ysm1.rs  # //! YSM1 v1 — the binary view of th...
    │   └── tests/
    │       ├── canonical.rs  # // Moved verbatim with `CanonicalFormTab
    │       ├── curie_disambiguation.rs  # use crate::taxonomy::normalize_taxonomy_
    │       ├── doc_stubs.rs  # //! Stale-name guard for the `guidance-o
    │       ├── entity.rs  # use super::*;
    │       ├── entity_calibration.rs  # use super::*;
    │       ├── fixtures/
    │       │   └── yago_mini.ttl
    │       ├── inference.rs  # use super::*;
    │       ├── inference_equiv.rs  # //! Equivalence coverage for the subclas
    │       ├── loader.rs  # //! `YaGoLoader` behavior: duplicate-IRI
    │       ├── mapper.rs  # use super::*;
    │       ├── migration.rs  # use super::*;
    │       ├── normalize.rs  # use super::*;
    │       ├── plausibility.rs  # //! Moved with the scoring (M5.
    │       ├── prefix.rs  # use crate::prefix::{split_prefix, CurieP
    │       ├── taxonomy.rs  # //! Parity with the deprecated `src/onto
    │       ├── taxonomy_equivalence.rs  # //! Equivalence between [`crate::taxonom
    │       ├── yago.rs  # use super::*;
    │       ├── yago_interlingua.rs  # //! ROADMAP M11.8 — the YaGO taxono...
    │       ├── yago_loader.rs  # use super::*;
    │       ├── yago_view.rs  # //! Moved with the view (M5.
    │       └── ysm1.rs  # //! YSM1 codec suite: control group (mus
    ├── orator-audio/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── archive.rs  # //! Lossless archive tap: Opus-in-Ogg se
    │   │   ├── backend.rs  # //! Capture and playout backends.
    │   │   ├── codec.rs  # //! Streaming audio codec seam: PCM edge
    │   │   ├── doctor.rs  # //! Environment diagnosis: devices, code
    │   │   ├── envelope.rs  # //! Playout envelopes: duck on barge-in 
    │   │   ├── error.rs  # //! Audio error taxonomy.
    │   │   ├── jitter.rs  # //! Adaptive drop-oldest jitter buffer.
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── merge.rs  # //! Time-correct mic/sys word merge for 
    │   │   ├── playback.rs  # //! Blocking local playout: one output s
    │   │   ├── resample.rs  # //! Resampling: device rates to the 16 k
    │   │   ├── source.rs  # //! Capture sources: cpal bring-up, synt
    │   │   └── wav.rs  # //! Minimal WAV reader: PCM16 and IEEE-f
    │   └── tests/
    │       ├── archive.rs  # //! Archive tests: segment rotation, man
    │       ├── backend.rs  # //! Backend tests: enumeration never fai
    │       ├── codec.rs  # //! Codec seam goldens: the PCM adapter 
    │       ├── doctor.rs  # //! Doctor tests: the report never fails
    │       ├── envelope.rs  # //! Envelope tests: duck ramps to the fl
    │       ├── error.rs  # //! Unit tests for [`crate::error`].
    │       ├── jitter.rs  # //! Jitter tests: drop-oldest on overflo
    │       ├── merge.rs  # //! Merge tests: interleave goldens unde
    │       ├── playback.rs  # //! Blocking playout contract: silent no
    │       ├── resample.rs  # //! Resample tests: exact ratios, length
    │       ├── source.rs  # //! Capture-source tests: synthetic alwa
    │       └── wav.rs  # //! WAV tests: hand-built fixtures, exac
    ├── orator-bench/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── events.rs  # //! Compatibility re-export: the bench l
    │   │   ├── gates.rs  # //! Regression gates over replayed numbe
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── replay.rs  # //! Deterministic replay: scripts in, ti
    │   │   ├── scenarios.rs  # //! Scripted bench scenarios over the li
    │   │   └── stats.rs  # //! Compatibility re-export: histograms 
    │   └── tests/
    │       ├── events.rs  # //! Event tests: stat-file round-trip, o
    │       ├── gates.rs  # //! Gate tests: green replay passes, eve
    │       ├── replay.rs  # //! Replay tests: determinism, fault cos
    │       ├── scenarios.rs  # //! Scripted bench scenarios: single-tur
    │       ├── soak.rs  # //! Fault soak (heavy tier): periodic fa
    │       └── stats.rs  # //! Stats tests: histogram rendering nam
    ├── orator-cli/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── ask.rs  # //! Ask preflight: strict port parsing, 
    │   │   ├── ask_cmd.rs  # //! Ask dispatch: one supervised questio
    │   │   ├── chat.rs  # //! `chat` turn helpers: slash parsing, 
    │   │   ├── chatlog.rs  # //! Scrolling-log events for `orator cha
    │   │   ├── config.rs  # //! Configuration helpers: backend resol
    │   │   ├── dictate.rs  # //! Dictation bounds: one place for the 
    │   │   ├── doctor.rs  # //! Full environment diagnosis: audio pl
    │   │   ├── error.rs  # //! One error envelope: every CLI failur
    │   │   ├── fallback.rs  # //! Local-first voice fallback: the daem
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── main.rs  # #![forbid(unsafe_code)]
    │   │   ├── models.rs  # //! Honest `models verify` reporting: ev
    │   │   ├── models_cmd.rs  # //! Models dispatch: list, pull, rm, ver
    │   │   ├── narration.rs  # #![forbid(unsafe_code)]
    │   │   ├── ptt.rs  # #![forbid(unsafe_code)]
    │   │   ├── say.rs  # //! Speech dispatch: daemon, audio-serve
    │   │   ├── say_chain.rs  # #![forbid(unsafe_code)]
    │   │   ├── shell.rs  # #![forbid(unsafe_code)]
    │   │   ├── socket.rs  # //! Daemon socket client: one credential
    │   │   ├── stats.rs  # //! Local stat-file rendering: the `stat
    │   │   ├── stream.rs  # //! Chunked playout orchestration: synth
    │   │   ├── transcribe.rs  # #![forbid(unsafe_code)]
    │   │   └── tui.rs  # //! Terminal duplex client: mic level, s
    │   └── tests/
    │       ├── ask.rs  # //! Ask preflight goldens: strict ports,
    │       ├── chat.rs  # //! `chat` helper goldens: slash parsing
    │       ├── chatlog.rs  # //! `chatlog` goldens: timestamp shape, 
    │       ├── commands.rs  # //! Unit tests for the CLI surface: cano
    │       ├── config.rs  # //! Configuration helper tests: backend 
    │       ├── dictate.rs  # //! Dictate bounds: seconds validation, 
    │       ├── doctor.rs  # //! Full-doctor tests: every section ren
    │       ├── error.rs  # //! Error-envelope tests: every builder 
    │       ├── fallback.rs  # //! Local-fallback tests: refusal classi
    │       ├── it_end_to_end.rs  # //! End-to-end goldens: the cascade flow
    │       ├── it_parity.rs  # //! Daemon/CLI parity: the clap surface 
    │       ├── live.rs  # //! Live barge/quit harness: a scripted 
    │       ├── models.rs  # //! `models verify` reporting: named unv
    │       ├── models_cmd.rs  # //! Characterization for the cross-famil
    │       ├── narration.rs  # //! Narration level goldens: parsing, se
    │       ├── progress.rs  # //! Long-op progress goldens: the real b
    │       ├── ptt.rs  # //! PTT goldens: parsing, session starts
    │       ├── say_chain.rs  # //! Voice chain goldens: advance exactly
    │       ├── shell.rs  # //! Shell tests: scripts mention the ent
    │       ├── signal.rs  # //! Signal-cancellation tests: a real SI
    │       ├── socket.rs  # //! Socket client tests: envelope round-
    │       ├── stats.rs  # //! Stat-file rendering tests: missing f
    │       ├── stream.rs  # //! Overlap goldens: chunked playout str
    │       ├── transcribe.rs  # //! Transcribe tests: golden outputs per
    │       └── tui.rs  # //! Headless TUI tests: keystroke mappin
    ├── orator-core/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── bench.rs  # //! Bench log and render: timestamped st
    │   │   ├── config.rs  # //! Layered JSON configuration with runt
    │   │   ├── error.rs  # //! Engine and protocol error types.
    │   │   ├── gpu.rs  # //! GPU device presence: one probe for e
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── paths.rs  # //! Trusted path resolution: one home lo
    │   │   ├── prompts.rs  # //! One prompt home for every orator sur
    │   │   ├── protocol.rs  # //! The single wire-protocol registry.
    │   │   ├── signal.rs  # //! Process shutdown signal: SIGTERM or 
    │   │   ├── socket.rs  # //! Credentialed socket framing: length,
    │   │   ├── traits.rs  # //! Engine traits: one bounded step at a
    │   │   ├── truncate.rs  # //! One char-count truncator for every t
    │   │   ├── types.rs  # //! Shared audio and transcript vocabula
    │   │   └── untrusted.rs  # //! Untrusted content: tool output, fetc
    │   └── tests/
    │       ├── bench.rs  # //! Bench-log tests: stat-file round-tri
    │       ├── config.rs  # //! Unit tests for [`crate::config`]: do
    │       ├── error.rs  # //! Unit tests for [`crate::error`]: dis
    │       ├── gpu.rs  # //! Device-probe tests: consistent answe
    │       ├── it_config.rs  # //! Load-or-default goldens: missing fil
    │       ├── it_source_guard.rs  # //! Source hygiene guards: project code 
    │       ├── paths.rs  # //! `paths` resolution-helper tests: exp
    │       ├── prompts.rs  # //! Prompt-home tests: every surface com
    │       ├── protocol.rs  # //! Unit tests for [`crate::protocol`]: 
    │       ├── signal.rs  # //! Shutdown-signal test: a real SIGTERM
    │       ├── socket.rs  # //! Socket framing tests: round-trip, tr
    │       ├── traits.rs  # //! Unit tests for [`crate::traits`]: en
    │       ├── truncate.rs  # //! Truncator tests: one char-count cap 
    │       ├── types.rs  # //! Unit tests for [`crate::types`]: tim
    │       └── untrusted.rs  # //! Untrusted-wrap tests: delimiters hol
    ├── orator-daemon/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── admission.rs  # //! Admission policy: bounded concurrenc
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── main.rs  # #![forbid(unsafe_code)]
    │   │   ├── preset.rs  # //! Session presets: dependency graphs o
    │   │   ├── session.rs  # //! Session registry: one entry per live
    │   │   ├── socket.rs  # //! Credentialed socket serving: one acc
    │   │   ├── voice.rs  # //! Daemon-owned voice server: one long-
    │   │   └── ws.rs  # //! WebSocket listener: the same contrac
    │   └── tests/
    │       ├── admission.rs  # //! Admission harness: saturation refuse
    │       ├── dispatch.rs  # //! Unit tests for dispatch: the served 
    │       ├── export.rs  # //! Meeting export tests: a scripted ses
    │       ├── leases.rs  # //! Session multiplexing: real leases on
    │       ├── loopback.rs  # //! Loopback test: synthetic source thro
    │       ├── preset.rs  # //! Preset tests: every preset resolves,
    │       ├── scope.rs  # //! Unit tests for the owned scope tree:
    │       ├── session.rs  # //! Session factory tests: lifecycle, Sa
    │       ├── socket.rs  # //! Socket serving tests: contract call 
    │       ├── voice.rs  # //! Voice-owner tests: disabled fails cl
    │       └── ws.rs  # //! WebSocket listener tests: transport 
    ├── orator-desktop/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── bind.rs  # //! Desktop binding walks: probe each su
    │   │   ├── clipboard.rs  # //! Clipboard transactions: change-count
    │   │   ├── continuation.rs  # //! Continuation spacing: back-to-back d
    │   │   ├── dbus.rs  # //! Control API model for `org.fluent.
    │   │   ├── hotkey.rs  # //! Hotkey chord codec: parse, validate,
    │   │   ├── indicators.rs  # //! Session indicators: mandatory record
    │   │   ├── krunner.rs  # //! KRunner runner: `orator` plus a ques
    │   │   ├── ladder.rs  # //! Injection ladder: ordered fallback a
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── notify.rs  # //! Notifications with approve/deny/stop
    │   │   ├── refine.rs  # //! Dictation refine chain: LLM polish, 
    │   │   └── tray.rs  # //! Tray status model: what the indicato
    │   └── tests/
    │       ├── bind.rs  # //! Binding-walk tests: every surface re
    │       ├── clipboard.rs  # //! Clipboard tests: restore discipline 
    │       ├── continuation.rs  # //! Continuation tests: the 10 s same-wi
    │       ├── dbus.rs  # //! Control-API tests: method parity, lo
    │       ├── hotkey.rs  # //! Chord codec tests: parse, pretty, re
    │       ├── indicators.rs  # //! Indicator tests: mandatory recording
    │       ├── krunner.rs  # //! Runner tests: prefix match, offers, 
    │       ├── ladder.rs  # //! Ladder tests: forced failures land o
    │       ├── notify.rs  # //! Notify tests: content-free summaries
    │       ├── refine.rs  # //! Refine-chain goldens: LLM polish win
    │       └── tray.rs  # //! Tray model tests: labels and the non
    ├── orator-diary/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── align.rs  # //! Alignment and naming: speaker segmen
    │   │   ├── cluster.rs  # //! Agglomerative clustering on cosine d
    │   │   ├── embed.rs  # //! Speaker embeddings: batched extracti
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── pipeline.rs  # //! Checkpointed post-processing: resuma
    │   │   └── segment.rs  # //! Speaker-turn segmentation: fixed win
    │   └── tests/
    │       ├── align.rs  # //! Alignment tests: midpoint attributio
    │       ├── cluster.rs  # //! Clustering tests: two voices separat
    │       ├── embed.rs  # //! Embedding tests: order-preserving ba
    │       ├── pipeline.rs  # //! Pipeline tests: rewind golden and cr
    │       ├── result_pool_shape.rs  # //! Test-only harness proving the `Resul
    │       └── segment.rs  # //! Segmentation tests: window math, ove
    ├── orator-lfm/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── arch.rs  # //! Architecture constants for LFM3-Audi
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── manifest.rs  # //! Weight-layout manifest for LFM3-Audi
    │   │   ├── params.rs  # //! Generation defaults for LFM3-Audio t
    │   │   └── template.rs  # //! Chat-template assembly for LFM3-Audi
    │   └── tests/
    │       ├── arch.rs  # //! `arch` pins: layer interleave, token
    │       ├── fixtures/
    │       │   ├── probe_reference.py  # #!/usr/bin/env python3
    │       │   └── reference_expansion.json
    │       ├── live.rs  # //! Live reference agreement: re-runs th
    │       ├── manifest.rs  # //! `manifest` checks: layout shape plus
    │       ├── params.rs  # //! `params` pins: generation defaults a
    │       ├── reference.rs  # //! Reference anchoring: every expectati
    │       └── template.rs  # //! `template` goldens: marker expansion
    ├── orator-llm/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── probe.rs  # //! Boot health probe: `GET {base}/model
    │   │   ├── session.rs  # //! Endpoint session: one turn's confirm
    │   │   └── supervisor.rs  # //! Supervised local inference endpoint:
    │   └── tests/
    │       ├── live.rs  # //! Live ask: real endpoint, real infere
    │       ├── prefix_calibration.rs  # //! Adversarial prefix check: abort at e
    │       ├── probe.rs  # //! Endpoint probe contract: `/models` s
    │       ├── session.rs  # //! Session contract: chunked prefill, a
    │       └── supervisor.rs  # //! Supervisor contract: resolution orde
    ├── orator-models/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── model.rs  # //! Model catalog: what the assistant ma
    │   │   ├── ops.rs  # //! Model file operations: list, verify,
    │   │   └── residency.rs  # //! Model residency: budget enforcement 
    │   └── tests/
    │       ├── calibration.rs  # //! Model-verification calibration: the 
    │       ├── model.rs  # //! Model catalog tests: structure, look
    │       ├── ops.rs  # //! Model-operation tests: list shape, v
    │       └── residency.rs  # //! Residency tests: eviction order, bud
    ├── orator-notes/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── export.rs  # //! Meeting file export: deterministic m
    │   │   ├── facts.rs  # //! Markdown facts and the rolling summa
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── live.rs  # //! Live-notes scheduler stub: when the 
    │   │   ├── search.rs  # //! Keyword-honest notes search over the
    │   │   └── summarize.rs  # //! Map-reduce summarizer with validated
    │   └── tests/
    │       ├── export.rs  # //! Export goldens: the renderer is byte
    │       ├── facts.rs  # //! Facts tests: append shape, rolling b
    │       ├── live.rs  # //! Live-notes scheduler tests: window r
    │       ├── search.rs  # //! Search tests: keyword honesty, ranki
    │       └── summarize.rs  # //! Summarizer tests: ordered map, citat
    ├── orator-s2s/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── config.rs  # //! Audio-endpoint configuration and ses
    │   │   ├── ensure.rs  # //! Audio endpoint assurance: use the co
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── protocol.rs  # //! Audio-endpoint wire codec: WAV frami
    │   │   ├── session.rs  # //! Audio endpoint session: single-shot 
    │   │   ├── voice.rs  # //! Supervised voice server: stock `llam
    │   │   └── worker.rs  # //! Supervised vocoder speech: text in, 
    │   └── tests/
    │       ├── calibration.rs  # //! Readiness/fallback calibration: dead
    │       ├── config.rs  # //! Speech-config tests: bundle presence
    │       ├── duplex_calibration.rs  # //! Duplex threshold calibration: script
    │       ├── ensure.rs  # //! Assurance tests: shared when served,
    │       ├── live.rs  # //! Live speech: real audio endpoint, re
    │       ├── protocol.rs  # //! Wire-codec tests: WAV bytes, message
    │       ├── session.rs  # //! Session tests: scripted SSE turns (t
    │       ├── voice.rs  # //! Voice-server tests: resolution order
    │       └── worker.rs  # //! Worker tests: stub-binary synthesis 
    ├── orator-store/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── events.rs  # //! Append-only event log with paged rep
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── schema.rs  # //! Durable event-log schema on the shar
    │   │   └── store.rs  # //! Single-writer feed and instance/back
    │   └── tests/
    │       ├── events.rs  # //! Event-log tests: append cursors, pag
    │       ├── schema.rs  # //! Migration goldens: version table, ne
    │       └── writer.rs  # //! Writer and gate tests: credit-gated 
    ├── orator-stt/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── moonshine.rs  # //! Moonshine backend: local streaming-A
    │   │   ├── streaming.rs  # //! Rolling-window streaming wrapper: co
    │   │   ├── wer.rs  # //! Word error rate: the STT accuracy ya
    │   │   └── whisper.rs  # //! whisper.cpp backend: lazy model load
    │   └── tests/
    │       ├── corpus/
    │       │   └── README.md  # # WER corpus contract
    │       ├── live.rs  # //! Live decode: real model, real infere
    │       ├── moonshine.rs  # //! Moonshine backend tests: refusal sha
    │       ├── streaming.rs  # //! Streaming tests: agreement commits, 
    │       ├── wer.rs  # //! WER goldens: identical, substitution
    │       └── whisper.rs  # //! Backend tests: unloaded backends fai
    ├── orator-testkit/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── fakes.rs  # //! Deterministic fakes: scripted clocks
    │   │   └── lib.rs  # #![forbid(unsafe_code)]
    │   └── tests/
    │       └── fakes.rs  # //! Fake tests: clock math, fault cadenc
    ├── orator-tools/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── approve.rs  # //! Approvals: a rendezvous with a deadl
    │   │   ├── builtins.rs  # //! The v1 tool catalog: two read-only t
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── loop_.rs  # //! The tool-call loop: at most five too
    │   │   ├── parse.rs  # //! Tool-call parsing: strict JSON prima
    │   │   ├── policy.rs  # //! Policy engine: `decide(call, ctx) ->
    │   │   ├── sandbox.rs  # //! Guarded execution: the process half 
    │   │   ├── tool.rs  # //! One `Tool` trait: the single source 
    │   │   └── untrusted.rs  # //! Compatibility re-export: the `<data>
    │   └── tests/
    │       ├── approve.rs  # //! Approval tests: rounds resolve, dead
    │       ├── builtins.rs  # //! Builtin tests: calculator math, arg 
    │       ├── loop.rs  # //! Loop tests: round bound, preamble ca
    │       ├── parse.rs  # //! Parse tests: both paths, preamble ca
    │       ├── policy.rs  # //! Policy tests: tier table, ambient de
    │       ├── redteam.rs  # //! Red-team calibration: planted instru
    │       ├── sandbox.rs  # //! Sandbox tests: timeout kills the gro
    │       ├── tool.rs  # //! Registry tests: single definitions, 
    │       └── untrusted.rs  # //! Untrusted-wrap tests: delimiters hol
    ├── orator-tts/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── kokoro.rs  # //! Kokoro backend: lazy model load, ser
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── normalize.rs  # //! Spoken-text normalizer: markdown and
    │   │   ├── pipeline.rs  # //! Speaking pipeline: normalized text i
    │   │   └── splitter.rs  # //! Sentence streamer: LLM tokens in, sp
    │   └── tests/
    │       ├── kokoro.rs  # //! Backend tests: missing files fail un
    │       ├── live.rs  # //! Live synth: real Kokoro weights, rea
    │       ├── normalize.rs  # //! Normalizer goldens: code never spoke
    │       ├── pipeline.rs  # //! Pipeline tests: prebuffer lead, stal
    │       └── splitter.rs  # //! Splitter goldens: buffer-end holds, 
    ├── orator-turn/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── driver.rs  # //! Turn driver: epoch discipline, barge
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   └── machine.rs  # //! The turn-taking state machine: a pur
    │   └── tests/
    │       ├── barge_in_calibration.rs  # //! Barge-in calibration: scripted evide
    │       ├── driver.rs  # //! Driver tests: epoch stickiness at th
    │       ├── hold_calibration.rs  # //! CAL: endpoint-hold calibration on a 
    │       └── machine.rs  # //! Machine tests: the transition table 
    ├── orator-vad/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── energy.rs  # //! Zero-dependency energy voice activit
    │   │   ├── gate.rs  # //! Commit gate for transcription: wheth
    │   │   └── lib.rs  # #![forbid(unsafe_code)]
    │   └── tests/
    │       ├── energy.rs  # //! VAD tests: attack/hangover timing, h
    │       └── gate.rs  # //! Commit-gate tests: tonal isolation p
    ├── protopyne-chart-model/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── annotations.rs  # //! Domain-keyed annotations: `(bar_time
    │   │   ├── chart_type.rs  # //! Chart visualization taxonomy: the tw
    │   │   ├── convert.rs  # //! Display-boundary conversion: `Decima
    │   │   ├── display_only.rs  # //! Display-purity fence: `Decimal -> f6
    │   │   ├── error.rs  # //! Typed errors for headless chart-mode
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── range.rs  # //! Preset date-range selectors for fetc
    │   │   ├── session.rs  # //! Trading-session boundaries over clos
    │   │   └── transform.rs  # //! Pure bar transforms: time-based smoo
    │   └── tests/
    │       ├── annotations.rs  # use super::*;
    │       ├── budget.rs  # //! Workspace wiring ledger: paint-edge 
    │       ├── chart_type.rs  # use super::*;
    │       ├── convert.rs  # use super::*;
    │       ├── display_boundary.rs  # use super::*;
    │       ├── display_only.rs  # use super::*;
    │       ├── range.rs  # use super::*;
    │       ├── session.rs  # use super::*;
    │       └── transform.rs  # use super::*;
    ├── protopyne-chart-scale/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── error.rs  # //! Typed errors for scale construction 
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── mapping.rs  # //! Price-scale modes, ranges, and index
    │   │   └── ticks.rs  # //! Heckbert "nice number" ticks for axe
    │   └── tests/
    │       ├── mapping.rs  # use super::*;
    │       └── ticks.rs  # use super::*;
    ├── protopyne-egui/
    │   ├── Cargo.toml
    │   ├── benches/
    │   │   └── chart_budget.rs  # //! Trading-domain budget baselines for 
    │   ├── examples/
    │   │   ├── demo.rs  # //! Chart-shell example (demo): a thin c
    │   │   ├── shared/
    │   │   │   └── seed.rs  # //! Shared seeded-history switching for 
    │   │   └── terminal.rs  # //! Composition root: the trading termin
    │   ├── fonts/
    │   │   ├── NotoSans-Regular.ttf
    │   │   ├── NotoSansMono-Regular.ttf
    │   │   └── README.md  # # Terminal fonts
    │   ├── src/
    │   │   ├── annotations.rs  # //! Pine drawings enter the operator's a
    │   │   ├── backtest.rs  # //! Schema-driven backtest authoring: ru
    │   │   ├── chart.rs  # //! Trading chart control plane: chart c
    │   │   ├── delta_volume_profile.rs  # //! Native Rust equivalency for `_DeltaV
    │   │   ├── export.rs  # //! Replay export in the server shapes, 
    │   │   ├── fast_mas.rs  # //! "Fast MAs": a decoupled moving-avera
    │   │   ├── feed.rs  # //! Trading recompute pool: heavy oscill
    │   │   ├── fonts.rs  # //! Terminal typeface: Noto Sans over th
    │   │   ├── grant_form.rs  # //! Schema-driven grant arming form.
    │   │   ├── indicators.rs  # //! Indicator plugin system: the searcha
    │   │   ├── integration_lite.rs  # //! IntegrationLite parity pane plus the
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── oscillator_panel.rs  # //! Oscillator kind defaults: the dock p
    │   │   ├── oscillators.rs  # //! Trading oscillator vocabulary plus t
    │   │   ├── overlay.rs  # //! Backtest chart overlay: trade marker
    │   │   ├── pine_indicator.rs  # //! Pine-script indicator plugin: a cata
    │   │   ├── pine_overlay.rs  # //! Pine overlay: engine primitives beco
    │   │   ├── prefs.rs  # //! Terminal preferences: the serialized
    │   │   ├── regime_labels.rs  # //! Quiet regime labels for display and 
    │   │   ├── replay.rs  # //! Terminal replay: the scrub over the 
    │   │   ├── session.rs  # //! Session-break overlays over closed b
    │   │   ├── topology.rs  # //! Trading chart topology: the standard
    │   │   ├── views/
    │   │   │   ├── broker.rs  # //! Read-only broker panel: venues, acco
    │   │   │   ├── health.rs  # //! Health/trading-state view-model: wha
    │   │   │   ├── inbox.rs  # //! Approval-inbox projection: which row
    │   │   │   ├── journal.rs  # //! Proposal-provenance timeline: journa
    │   │   │   ├── mod.rs  # //! Read-only terminal views over `&dyn 
    │   │   │   ├── portfolio.rs  # //! Portfolio view-model: a thin read-on
    │   │   │   └── tickets.rs  # //! Manual-ticket projection: which tick
    │   │   ├── widgets.rs  # //! Trading terminal widgets: multi-time
    │   │   └── workspace.rs  # //! Central chart workspace: the termina
    │   └── tests/
    │       ├── add_preserves_view.rs  # use super::*;
    │       ├── annotations.rs  # //! Pine drawings enter the operator's a
    │       ├── auto_height.rs  # use super::*;
    │       ├── backtest.rs  # //! Backtest form, run control, and resu
    │       ├── beginning_reset.rs  # use super::*;
    │       ├── broker_view.rs  # //! Broker panel view-model: unsupported
    │       ├── budget.rs  # //! Audit ledger: allocation shape, depe
    │       ├── chart.rs  # use super::*;
    │       ├── chart_types.rs  # //! Chart-type wiring goldens: the termi
    │       ├── common/
    │       │   └── fakes.rs  # //! Configurable recording gateway doubl
    │       ├── context_panes.rs  # use super::*;
    │       ├── data/
    │       │   ├── demo_check.txt
    │       │   ├── demo_csv_check.txt
    │       │   ├── gain_threshold_mapping.json
    │       │   ├── oscillator_threshold_calibration.json
    │       │   ├── per_kind_threshold_calibration.json
    │       │   ├── pine_input_coverage.json
    │       │   ├── pivot_calibration.json
    │       │   └── terminal_mock_check.txt
    │       ├── delta_volume_profile.rs  # //! Native Delta Volume Profile: binning
    │       ├── demo_shell.rs  # //! Shell-contract pins for the demo col
    │       ├── derived_panes.rs  # use super::*;
    │       ├── export.rs  # use super::*;
    │       ├── fast_mas.rs  # //! Fast MAs module: one isolated `Compo
    │       ├── feed.rs  # use super::*;
    │       ├── fetch_limit.rs  # use super::*;
    │       ├── fetch_window.rs  # use super::*;
    │       ├── fonts.rs  # use super::*;
    │       ├── gateway.rs  # //! Composition-root parity from the ter
    │       ├── grant_form.rs  # //! Schema-driven grant arming form, tes
    │       ├── heikin_cache.rs  # use super::*;
    │       ├── indicator_manifest.rs  # //! Manifest-driven indicator catalog: m
    │       ├── indicators.rs  # //! Indicator plugin system: the searcha
    │       ├── integration_lite.rs  # use super::*;
    │       ├── kernel_plugins.rs  # //! Schema-driven kernel plugins: multi-
    │       ├── lib.rs  # use super::*;
    │       ├── lite_display_flags.rs  # //! Display-flag dials behind the Integr
    │       ├── live_chart.rs  # //! Live chart: feed-frame parsing, the 
    │       ├── live_replay.rs  # //! Interactive replay controls: start r
    │       ├── oscillator_catalog.rs  # use super::*;
    │       ├── oscillator_display.rs  # use super::*;
    │       ├── oscillator_lift.rs  # use super::*;
    │       ├── oscillator_panel.rs  # use super::*;
    │       ├── oscillators.rs  # use super::*;
    │       ├── overlay.rs  # //! Backtest overlay geometry: markers l
    │       ├── pane_routing.rs  # use super::*;
    │       ├── pane_taxonomy.rs  # use super::*;
    │       ├── pickers.rs  # //! Catalog-backed pickers: smoothing an
    │       ├── pine_indicator.rs  # //! Pine indicator plugin: header overla
    │       ├── pine_input_coverage.rs  # //! Pine-input coverage table: every liv
    │       ├── pine_overlay.rs  # //! Overlay geometry goldens: `PlotPrimi
    │       ├── prefs.rs  # use super::*;
    │       ├── recompute_gate.rs  # use super::*;
    │       ├── regime_labels.rs  # use super::*;
    │       ├── replay.rs  # //! Terminal replay: the scrub over the 
    │       ├── schema_coverage.rs  # use fluent_egui::{schema_fields, FieldKi
    │       ├── series_source.rs  # //! Indicator outputs as inputs: `Series
    │       ├── session.rs  # use super::*;
    │       ├── settings_dials.rs  # use super::*;
    │       ├── settings_style.rs  # use super::*;
    │       ├── shared_oscillator.rs  # use super::*;
    │       ├── source_options.rs  # use super::*;
    │       ├── switch_order.rs  # use super::*;
    │       ├── taxonomy_single_truth.rs  # use super::*;
    │       ├── threshold_calibration.rs  # use super::*;
    │       ├── topology.rs  # use super::*;
    │       ├── views.rs  # //! Read-only views over `&dyn TerminalG
    │       ├── widgets.rs  # use super::*;
    │       ├── window_alignment.rs  # use super::*;
    │       ├── workspace.rs  # //! Central chart workspace: boot state,
    │       ├── workspace_symbols.rs  # //! Gateway-less symbol switches: the de
    │       └── workspace_tape.rs  # //! The workspace seeds and paints headl
    ├── protopyne-kernels/
    │   ├── Cargo.toml
    │   ├── benches/
    │   │   ├── fetch_tick.rs  # //! Per-tick serve cost at the resident-
    │   │   ├── mtf_streaming.rs  # //! Streaming-store baselines: append-on
    │   │   └── pine_eval.rs  # //! Pine evaluation baselines: the fixtu
    │   ├── examples/
    │   │   └── production_refusal_probe.rs  # //! Production-shape refusal probe (buil
    │   ├── src/
    │   │   ├── averages.rs  # //! Weighted and adaptive moving average
    │   │   ├── bars.rs  # //! Native tick-to-bar ingestion: ring, 
    │   │   ├── buffers.rs  # //! Native `BufferFloat` mirror: the shi
    │   │   ├── calibration.rs  # //! Global calibration gate: universal c
    │   │   ├── candles.rs  # //! Single-bar candle compositions and t
    │   │   ├── compat.rs  # //! Kernel compatibility matrix: per-ker
    │   │   ├── component.rs  # //! Indicator-as-`Component`: native ker
    │   │   ├── composed.rs  # //! The chained kernel shell: one child 
    │   │   ├── context.rs  # //! Explicit per-run evaluation context:
    │   │   ├── drawing.rs  # //! Per-run drawings: labels and lines a
    │   │   ├── ema.rs  # //! Exponential moving average over clos
    │   │   ├── error.rs  # //! Kernel-side typed errors: invalid in
    │   │   ├── external.rs  # //! External parity artifacts: third-par
    │   │   ├── filters.rs  # //! Adaptive and predictive filters: the
    │   │   ├── greeks.rs  # //! Black-Scholes pricing and sensitivit
    │   │   ├── indicator.rs  # //! Indicator contract, registry, and ex
    │   │   ├── inputs.rs  # //! Validated per-run inputs: declared b
    │   │   ├── iv_surface.rs  # //! One-expiry implied-volatility surfac
    │   │   ├── kernel_eval.rs  # use std::sync::Arc;
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── lite.rs  # //! One market-leg batch, one oscillator
    │   │   ├── manifest_files.rs  # //! Boot-time manifest driver: the files
    │   │   ├── market_context.rs  # //! Native `MarketContext`: the `libApex
    │   │   ├── math.rs  # //! Shared indicator math: one home for 
    │   │   ├── mtf.rs  # //! Multi-timeframe alignment primitives
    │   │   ├── native_libs.rs  # //! Native library registry: publisher/n
    │   │   ├── order.rs  # //! One topological sort over a registry
    │   │   ├── oscillators.rs  # //! Oscillators: `rsi`, `macd`, `stoch`,
    │   │   ├── parity.rs  # //! Research-only expression fixtures pl
    │   │   ├── pine/
    │   │   │   ├── bind.rs  # //! One argument binder for every call s
    │   │   │   ├── builtins_core.rs  # //! Scalar and structural script namespa
    │   │   │   ├── builtins_state.rs  # //! Run-scoped builtin state: the one ow
    │   │   │   ├── builtins_ta.rs  # //! `ta.*` builtin bridge: script calls 
    │   │   │   ├── bytecode.rs  # //! Bytecode program representation for 
    │   │   │   ├── bytecode_compile.rs  # //! Bytecode compiler: pure lowering fro
    │   │   │   ├── bytecode_exec.rs  # //! Bytecode execution loop (scalar): ru
    │   │   │   ├── closure.rs  # //! Directory-driven Pine closure discov
    │   │   │   ├── display.rs  # //! Pine program display: canonical sour
    │   │   │   ├── drawing.rs  # //! Headless drawing capture: one regist
    │   │   │   ├── egui_parity.rs  # //! Reference-overlap parity matrix: eve
    │   │   │   ├── engine.rs  # //! Engine facade, runtime plumbing, run
    │   │   │   ├── eval.rs  # //! Compatibility surface for the split 
    │   │   │   ├── home.rs  # //! One semantic home model shared by bo
    │   │   │   ├── lexer.rs  # //! Pine lexer: one scanner for the egui
    │   │   │   ├── manifest.rs  # //! Boot-time Pine indicator manifest: d
    │   │   │   ├── mod.rs  # //! Pine script surface: source loading 
    │   │   │   ├── modules.rs  # //! Pine import graph: multi-file progra
    │   │   │   ├── oracle/
    │   │   │   │   └── mod.rs  # //! Tree-walk interpreter over the runti
    │   │   │   ├── projection.rs  # //! Overlay projection: engine captures 
    │   │   │   ├── request.rs  # //! Headless request datasets: the run-s
    │   │   │   ├── semantics.rs  # //! Shared evaluation leaves both engine
    │   │   │   ├── source.rs  # //! Pine source identities: file loading
    │   │   │   ├── strategy.rs  # //! Headless `strategy.
    │   │   │   ├── syntax.rs  # //! Pine parser: egui-overlap grammar pl
    │   │   │   └── trigger.rs  # //! Trigger and classifier registry for 
    │   │   ├── price_source.rs  # //! Canonical price-source registry: one
    │   │   ├── research.rs  # //! Research-frame helpers: trimmed wind
    │   │   ├── runtime.rs  # //! Bar-by-bar Pine runtime: configurati
    │   │   ├── scores.rs  # //! Featureless slice scorers shared by 
    │   │   ├── series.rs  # //! Backward-indexed views over closed-b
    │   │   ├── sma.rs  # //! Simple moving average over closes.
    │   │   ├── smoothing.rs  # //! Single moving-average truth: every `
    │   │   ├── ta_momentum.rs  # //! Momentum remainder: price oscillator
    │   │   ├── ta_overlap.rs  # //! Overlap remainder: Bollinger-style b
    │   │   ├── ta_price.rs  # //! Price-transform remainder: average, 
    │   │   ├── ta_stat.rs  # //! Statistic remainder: population devi
    │   │   ├── ta_volume.rs  # //! Volume remainder: accumulation/distr
    │   │   ├── timed.rs  # //! Supervised kernel execution with per
    │   │   ├── util.rs  # //! Two-series and window utilities: `cr
    │   │   ├── value.rs  # //! Pine scalar values: one owned enum i
    │   │   ├── vars.rs  # //! Bar-persistent `var` versus intrabar
    │   │   ├── volatility.rs  # //! Range and band kernels: `tr`, `atr`,
    │   │   ├── windowed.rs  # //! The windowed kernel shell: one leg f
    │   │   ├── wq_comb.rs  # //! WorldQuant-style scalar/vector combi
    │   │   ├── wq_half_life.rs  # //! Half-life exponential weighting as f
    │   │   └── wq_series.rs  # //! WorldQuant-style time-series combina
    │   └── tests/
    │       ├── averages.rs  # use super::*;
    │       ├── bars.rs  # //! Native bar-path goldens: hand-comput
    │       ├── bars_calibration.rs  # //! Calibration suite for the bar trigge
    │       ├── bars_history.rs  # //! History-source adapter tests: the co
    │       ├── cal.rs  # //! M5 calibration: must-fire and must-N
    │       ├── cal_global.rs  # //! Global calibration gate: universal c
    │       ├── candles.rs  # use super::*;
    │       ├── capacity_overflow.rs  # //! Capacity proof: `max_candles` (chart
    │       ├── closure.rs  # //! The shipping closure: what a default
    │       ├── common/
    │       │   └── fixture.rs  # //! Shared Pine link-fixture constructor
    │       ├── compat.rs  # use crate::compat::{
    │       ├── component.rs  # use super::*;
    │       ├── context.rs  # use super::*;
    │       ├── data/
    │       │   ├── SPY_15M.csv
    │       │   ├── SPY_1M.csv
    │       │   ├── SPY_5M.csv
    │       │   ├── bytecode_contract.json
    │       │   ├── ema14.json
    │       │   ├── external/
    │       │   │   └── README.md  # This directory holds third-party export 
    │       │   ├── golden/
    │       │   │   ├── alma14.json
    │       │   │   ├── atr14.json
    │       │   │   ├── bb14.json
    │       │   │   ├── fastmas_baseline_alma8.csv
    │       │   │   ├── fastmas_baseline_ema21.csv
    │       │   │   ├── fastmas_baseline_ema34.csv
    │       │   │   ├── fastmas_baseline_ema55.csv
    │       │   │   ├── fastmas_baseline_ema8.csv
    │       │   │   ├── fastmas_baseline_sma100.csv
    │       │   │   ├── fastmas_baseline_sma200.csv
    │       │   │   ├── fastmas_baseline_sma50.csv
    │       │   │   ├── fastmas_baseline_wma3.csv
    │       │   │   ├── fastmas_spy5m.csv
    │       │   │   ├── hma14.json
    │       │   │   ├── kc14.json
    │       │   │   ├── lex_FastMAs.txt
    │       │   │   ├── lex_libMAs.txt
    │       │   │   ├── lex_libUtil.txt
    │       │   │   ├── macd12269.json
    │       │   │   ├── native_freeze_spy_15m.json
    │       │   │   ├── native_freeze_spy_1m.json
    │       │   │   ├── native_freeze_spy_5m.json
    │       │   │   ├── parse_FastMAs.txt
    │       │   │   ├── parse_libMAs.txt
    │       │   │   ├── parse_libUtil.txt
    │       │   │   ├── rma14.json
    │       │   │   ├── rsi14.json
    │       │   │   ├── stoch14.json
    │       │   │   ├── tr.json
    │       │   │   ├── vwma14.json
    │       │   │   └── wma14.json
    │       │   ├── parity_sma10.csv
    │       │   ├── refusal_inventory.json
    │       │   ├── sma14.json
    │       │   ├── tick_harness_baselines.json
    │       │   └── ticks/
    │       │       ├── window_out_of_order.csv
    │       │       ├── window_pull.csv
    │       │       ├── window_pull_remainder.csv
    │       │       ├── window_quiet.csv
    │       │       └── window_sparse.csv
    │       ├── delta_calibration.rs  # //! Delta calibration gate: earn trust f
    │       ├── delta_honesty.rs  # //! Honest delta estimation: every buy-v
    │       ├── drawing.rs  # use super::*;
    │       ├── ema.rs  # use super::*;
    │       ├── external.rs  # //! External parity artifacts, hermetic 
    │       ├── filters.rs  # use super::*;
    │       ├── golden.rs  # use super::*;
    │       ├── greeks.rs  # //! Black-Scholes parity and agreement: 
    │       ├── ignore_policy.rs  # //! Repository policy: every `#[ignore]`
    │       ├── indicator.rs  # use super::*;
    │       ├── ingest_unified.rs  # //! Differential contract for the unifie
    │       ├── inputs.rs  # use super::*;
    │       ├── iv_surface.rs  # //! Surface precision and refusal: the f
    │       ├── kernel_eval.rs  # use crate::indicator::IndicatorResult;
    │       ├── kernel_identity.rs  # //! All-kernel identity fold: `batch` an
    │       ├── lite.rs  # //! The one market-leg batch and the one
    │       ├── market_context_calibration.rs  # //! Regime-trigger calibration: labels a
    │       ├── market_context_native.rs  # //! Native `MarketContext` parity: the s
    │       ├── math.rs  # use super::*;
    │       ├── math_selection.rs  # //! Golden pins for the selection-based 
    │       ├── mtf.rs  # //! Grid-math goldens: exact integer exp
    │       ├── mtf_dedup.rs  # //! Parametric dependency edges: compose
    │       ├── mtf_incremental.rs  # //! Incremental frames: an append-at-a-t
    │       ├── mtf_store.rs  # //! Shared multi-timeframe store: one ca
    │       ├── native_apex.rs  # //! Native `libApex` provider: engine ag
    │       ├── native_bridge.rs  # //! Native `Bars` execution through the 
    │       ├── native_buffers.rs  # //! Native `BufferFloat` execution: the 
    │       ├── native_call_contract.rs  # //! Triple-state contract for native met
    │       ├── native_libs.rs  # use super::*;
    │       ├── native_override.rs  # //! Whole-library native override wiring
    │       ├── native_parity_freeze.rs  # //! Native-vs-Pine parity freeze: every 
    │       ├── native_ring_lease.rs  # //! Ring-lease contract: the engine may 
    │       ├── order.rs  # //! One topological sort, exercised thro
    │       ├── oscillators.rs  # use super::*;
    │       ├── param_fingerprint.rs  # //! Canonical parameter fingerprint: `Pa
    │       ├── parity.rs  # use super::*;
    │       ├── pine_builtins_core.rs  # //! Scalar and structural namespace test
    │       ├── pine_builtins_state.rs  # //! Run-scoped builtin store behavior: o
    │       ├── pine_builtins_ta.rs  # //! `ta.*` bridge tests: every bridged c
    │       ├── pine_bytecode_adversarial.rs  # #![cfg(feature = "bytecode")]
    │       ├── pine_bytecode_audit.rs  # //! Enum-categorization audit for the by
    │       ├── pine_bytecode_calibration.rs  # #![cfg(feature = "bytecode")]
    │       ├── pine_bytecode_closeout.rs  # //! Closeout proof: the fallback audit, 
    │       ├── pine_bytecode_compile.rs  # //! Compiler tests: the M0 corpus and th
    │       ├── pine_bytecode_contract.rs  # #![cfg(feature = "bytecode")]
    │       ├── pine_bytecode_diff.rs  # #![cfg(feature = "bytecode")]
    │       ├── pine_bytecode_exec.rs  # //! Execution-loop ownership tests: link
    │       ├── pine_bytecode_integration.rs  # //! Engine selection behavior: the compi
    │       ├── pine_bytecode_repr.rs  # //! Bytecode representation tests: every
    │       ├── pine_bytecode_series.rs  # #![cfg(feature = "bytecode")]
    │       ├── pine_calibration.rs  # //! Pine calibration gate: frozen bridge
    │       ├── pine_closure.rs  # //! Fixture closure files: the fixture i
    │       ├── pine_closure_parse.rs  # //! Fixture-file parse conformance: ever
    │       ├── pine_draw_captures.rs  # //! Drawing/model shims as captured data
    │       ├── pine_dvp_security.rs  # //! Headless `request.
    │       ├── pine_egui_parity.rs  # //! Reference-overlap matrix checks: eve
    │       ├── pine_eval.rs  # //! Bar-by-bar engine tests: transcribed
    │       ├── pine_fastmas.rs  # //! Headless `_FastMAs` proof: the real 
    │       ├── pine_home.rs  # //! Home-model behavior: the shared nest
    │       ├── pine_lexer.rs  # //! Lexer tests: egui differential corpu
    │       ├── pine_libapex.rs  # //! Market-core library goldens: the shi
    │       ├── pine_libbars.rs  # //! Real tick-engine library goldens: th
    │       ├── pine_libbuffers.rs  # //! Buffer-library goldens: the shipped 
    │       ├── pine_libmas.rs  # //! Real moving-average-library goldens:
    │       ├── pine_libutil.rs  # //! Real utility-library goldens: the sh
    │       ├── pine_manifest.rs  # //! Indicator manifest: boot-time discov
    │       ├── pine_modules.rs  # //! Import graph tests: the fixture `_Fa
    │       ├── pine_parity_freeze.rs  # //! Overlap freeze: every parity-matrix 
    │       ├── pine_projection.rs  # //! Projection goldens: `RunOutcome` cap
    │       ├── pine_runtime_shims.rs  # //! Closed-bar runtime shims: driver cur
    │       ├── pine_scope.rs  # //! Persistent-binding scoping: a `var`/
    │       ├── pine_source.rs  # use super::*;
    │       ├── pine_syntax.rs  # //! Parser tests: transcribed egui round
    │       ├── pine_ta_coverage.rs  # //! Closed-coverage probe over the disco
    │       ├── pine_task_closure.rs  # //! Task-indicator closures: every disco
    │       ├── pine_task_parity.rs  # //! Kernel-expression cross-checks for t
    │       ├── pine_triage.rs  # //! Directory-driven triage pins: discov
    │       ├── pine_v6.rs  # //! v6 surface tests: `switch`, enums, c
    │       ├── price_source.rs  # use crate::indicator::{BarTick, OwnedOhl
    │       ├── remainder_signed.rs  # //! Signed remainder: the reference-vs-r
    │       ├── research.rs  # use crate::parity::{assert_expr_parity, 
    │       ├── research_rsrs_parity.rs  # use super::*;
    │       ├── resident_ring.rs  # //! Resident-ring proof: the `Bars` ring
    │       ├── resolved_cache.rs  # //! The per-state resolved-scalar cache.
    │       ├── runtime.rs  # use super::*;
    │       ├── schema.rs  # //! Every builtin kernel declares a usab
    │       ├── scores.rs  # //! Focused checks for the featureless s
    │       ├── series.rs  # use super::*;
    │       ├── shared_registry.rs  # //! One immutable kernel registry per pr
    │       ├── sma.rs  # use super::*;
    │       ├── smoothing.rs  # use crate::indicator::{OwnedOhlcv, Param
    │       ├── stored_opens.rs  # //! Stored opens: the ring persists each
    │       ├── ta_momentum.rs  # use super::*;
    │       ├── ta_overlap.rs  # use super::*;
    │       ├── ta_price.rs  # use super::*;
    │       ├── ta_stat.rs  # use super::*;
    │       ├── ta_volume.rs  # use super::*;
    │       ├── timed.rs  # use std::sync::Arc;
    │       ├── tv_spot.rs  # //! TradingView spot check: `sma` / `ema
    │       ├── util.rs  # use crate::util::{
    │       ├── validate.rs  # use super::*;
    │       ├── value.rs  # use super::*;
    │       ├── vars.rs  # use super::*;
    │       ├── volatility.rs  # use super::*;
    │       ├── warmup.rs  # //! Warmup honesty for the shell-built f
    │       ├── wq_comb.rs  # use super::*;
    │       ├── wq_half_life.rs  # use super::*;
    │       └── wq_series.rs  # use super::*;
    ├── protopyne-server/
    │   ├── Cargo.toml
    │   ├── Containerfile
    │   ├── benches/
    │   │   ├── backtest_path.rs  # //! Backtest decide-path and sweep fan-o
    │   │   └── history_load.rs  # //! History-load economics: Parquet, CSV
    │   ├── smoke-container.sh
    │   ├── src/
    │   │   ├── backtest.rs  # //! Versioned-history venue behind the s
    │   │   ├── backtest_registry.rs  # //! Stored backtest runs: the registry b
    │   │   ├── boot.rs  # //! Boot and wiring: the server config, 
    │   │   ├── chain_history.rs  # //! Historical option-chain ingest: file
    │   │   ├── csv.rs  # //! CSV ingest for versioned bar history
    │   │   ├── equity.rs  # //! Shared equity-run types and per-bar 
    │   │   ├── error.rs  # //! Server-side ingest and service error
    │   │   ├── gateway.rs  # //! Server-backed gateway: the same view
    │   │   ├── history.rs  # //! One history-loading surface: extensi
    │   │   ├── history_root.rs  # //! Directory history roots: `TICKER/YYY
    │   │   ├── ingest.rs  # //! Always-on candle ingestion: closed b
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── live.rs  # //! Live loop authority and driver: jour
    │   │   ├── main.rs  # //! Prototype HTTP server: history in, R
    │   │   ├── marks.rs  # //! Option-mark selection: where a packa
    │   │   ├── metrics.rs  # //! Risk-adjusted performance metrics ov
    │   │   ├── operator.rs  # //! Operator command execution: fold jou
    │   │   ├── parquet.rs  # //! Parquet ingest for market OHLCV hist
    │   │   ├── parquet_history.rs  # //! A bounded [`HistoryProvider`] over t
    │   │   ├── portfolio.rs  # //! Portfolio-level PnL aggregation over
    │   │   ├── rehearsal.rs  # //! The rehearsal composition: one fixed
    │   │   ├── replay_session.rs  # //! Interactive replay session: the batc
    │   │   ├── report.rs  # //! Run manifest and honest backtest rep
    │   │   ├── routes/
    │   │   │   └── mod.rs  # //! HTTP routes: thin wrappers over the 
    │   │   ├── runner.rs  # //! Backtest runner: the same code path 
    │   │   ├── service.rs  # //! Service composition root: the HTTP s
    │   │   ├── simple_backtest.rs  # //! Simple long/short backtest: the refe
    │   │   ├── trading_loop.rs  # //! The live trading loop: a session-dri
    │   │   ├── transpile.rs  # //! Scoped script-to-kernel spike: accep
    │   │   ├── views.rs  # //! Shared view builders: every projecti
    │   │   └── walk_forward.rs  # //! Walk-forward / out-of-sample evaluat
    │   └── tests/
    │       ├── api.rs  # use super::*;
    │       ├── arm_journal.rs  # //! Server arming through the command jo
    │       ├── arm_worst_case.rs  # //! Arm-screen worst-case projection: a 
    │       ├── backtest.rs  # use super::*;
    │       ├── backtest_api.rs  # //! Backtest result contract over HTTP a
    │       ├── backtest_registry.rs  # //! The stored-run registry: publish, re
    │       ├── broker.rs  # //! Broker reads end to end: a fake acco
    │       ├── chain_history.rs  # //! Chain history ingest: CSV shape, fai
    │       ├── chart_contract.rs  # //! Chart-bar contract: the server route
    │       ├── common/
    │       │   ├── fixtures.rs  # //! Shared builders for the server integ
    │       │   ├── golden.rs  # //! Shared canonical-golden dumper for t
    │       │   ├── mtf.rs  # //! Shared multi-timeframe proof harness
    │       │   └── tape.rs  # //! Shared helper for the tape-backed in
    │       ├── composition_scan.rs  # //! Control-plane audits: strategy selec
    │       ├── conversion_guard.rs  # //! Single display-conversion guard: the
    │       ├── csv.rs  # use super::*;
    │       ├── data/
    │       │   ├── SPY_15M.csv
    │       │   ├── SPY_5M.csv
    │       │   ├── SPY_CHAIN_15M.csv
    │       │   ├── backtest_identity_canonical.json
    │       │   ├── backtest_identity_tearsheet.txt
    │       │   ├── baseline/
    │       │   │   ├── live_loop_vectors.json
    │       │   │   ├── multi_backtest.txt
    │       │   │   ├── paper_decisions.json
    │       │   │   ├── preview_response.json
    │       │   │   └── single_backtest.txt
    │       │   ├── manifest_no_tape.json
    │       │   ├── pine_closure/
    │       │   │   ├── indicators/
    │       │   │   │   ├── _DeltaVolumeProfileProtopyne.pine
    │       │   │   │   ├── _FastMAs.pine
    │       │   │   │   ├── _IntegrationLite.pine
    │       │   │   │   └── _Regressions.pine
    │       │   │   ├── libraries/
    │       │   │   │   ├── _libApex.pine
    │       │   │   │   ├── _libBars.pine
    │       │   │   │   ├── _libBuffers.pine
    │       │   │   │   ├── _libCandles.pine
    │       │   │   │   ├── _libDrawing.pine
    │       │   │   │   ├── _libMAs.pine
    │       │   │   │   └── _libUtil.pine
    │       │   │   └── strategies/
    │       │   │       └── _IntegrationLiteStrategy.pine
    │       │   ├── pine_integration_lite_15m.json
    │       │   ├── pine_integration_lite_1m.json
    │       │   ├── pine_integration_lite_5m.json
    │       │   ├── pine_integration_lite_strategy_15m.json
    │       │   ├── pine_integration_lite_strategy_1m.json
    │       │   ├── pine_integration_lite_strategy_5m.json
    │       │   ├── preview_worst_case.json
    │       │   ├── slot_decision_tally.json
    │       │   ├── strategy_coverage_matrix.json
    │       │   ├── ticks/
    │       │   │   └── window_sparse.csv
    │       │   └── views/
    │       │       ├── arm.json
    │       │       ├── chart_bars.json
    │       │       ├── health.json
    │       │       ├── journal.json
    │       │       ├── overlay_sma_14.json
    │       │       ├── portfolio.json
    │       │       ├── portfolio_booked.json
    │       │       ├── portfolio_fresh.json
    │       │       ├── portfolio_rehearsal.json
    │       │       ├── preview.json
    │       │       ├── proposals.json
    │       │       ├── proposals_journaled.json
    │       │       └── replay.json
    │       ├── doc_claims.rs  # //! Documentation-claim guards: the READ
    │       ├── equity.rs  # //! Equity backtest contract: sizing mod
    │       ├── fallback_latch.rs  # //! FallbackLatch and TapeServeStats cal
    │       ├── fanout_serial.rs  # use std::time::{Duration, Instant};
    │       ├── gateway.rs  # //! Gateway-equals-routes: `ServerGatewa
    │       ├── gateway_replay.rs  # //! Interactive replay through the gatew
    │       ├── gateway_tape_parity.rs  # //! The store-backed gateway read and th
    │       ├── health_view_store.rs  # //! Store facts on the health route: pop
    │       ├── history.rs  # //! One history-loading surface: the lad
    │       ├── history_root.rs  # //! Directory history roots: ticker disc
    │       ├── history_source_backward_compatible.rs  # //! The control group for the tape arm: 
    │       ├── history_source_tape.rs  # //! The tape arm is a substrate, not a s
    │       ├── ingest.rs  # //! Ingest service fault-injection suite
    │       ├── labels.rs  # //! The toolbar-label bridge agrees ever
    │       ├── ladder_characterization.rs  # //! Ladder characterization: the one sto
    │       ├── live_loop.rs  # //! Live-loop authority and the loop's d
    │       ├── live_loop_baseline.rs  # //! Live-loop decision-stream pin.
    │       ├── manifest_backward_compatible.rs  # //! The backward-compatibility gate: a m
    │       ├── manifest_rerun_equality.rs  # //! Rerun equality over tape data: the s
    │       ├── marks.rs  # //! Mark selection: chain mid, fill-pric
    │       ├── metrics.rs  # //! Risk-adjusted metric calibration vec
    │       ├── mtf_fanout.rs  # //! Shared-`MtfStore` per-timeframe fan-
    │       ├── multi_symbol_tape.rs  # //! A tape-backed leg rides the multi-sy
    │       ├── neutrality.rs  # //! Adapter-boundary audit: no server/vi
    │       ├── operator_close.rs  # //! Operator close/cancel wiring: the ga
    │       ├── parity_soak.rs  # //! Four-driver parity: one tape through
    │       ├── parquet.rs  # //! Parquet history ingest over the Marc
    │       ├── parquet_history.rs  # //! Parquet history provider: horizon, s
    │       ├── pine_integration_lite.rs  # //! Headless end-to-end proof for the sh
    │       ├── pine_integration_lite_strategy.rs  # //! Headless proof for the shipped `_Int
    │       ├── pine_parity.rs  # //! Pine-parity characterization: the re
    │       ├── pine_task_indicators.rs  # //! Headless fail-closed proof for versi
    │       ├── portfolio.rs  # //! Portfolio aggregation fixtures: clos
    │       ├── rehearsal_baseline.rs  # //! Rehearsal baseline pins.
    │       ├── replay_driver.rs  # use crate::common_fixtures::money;
    │       ├── replay_session.rs  # use super::*;
    │       ├── report.rs  # use super::*;
    │       ├── report_data_limits.rs  # //! The data-limits rendering gate: `bui
    │       ├── runner.rs  # use super::*;
    │       ├── simple_backtest.rs  # //! Simple-backtest contract: entries fi
    │       ├── stats_generic_spike.rs  # //! Feasibility spike: can one generic r
    │       ├── stats_spike.rs  # use super::*;
    │       ├── store_halt_end_to_end.rs  # //! A store outage halts entries end to 
    │       ├── submit_gates.rs  # //! Gate-order pins for the shared submi
    │       ├── tape_boot.rs  # //! Tape boot: a file-backed store behin
    │       ├── tickets.rs  # //! Manual ticket surface: the gateway v
    │       ├── trading_loop.rs  # use super::*;
    │       ├── trading_status.rs  # //! Trading-status truth table: every co
    │       ├── transpile.rs  # use super::*;
    │       ├── trigger_filter_calibration.rs  # //! Trigger and filter calibration: cont
    │       ├── validate.rs  # use super::*;
    │       ├── views_golden.rs  # //! Route-payload goldens: every read ro
    │       └── walk_forward.rs  # use super::*;
    ├── protopyne-store/
    │   ├── Cargo.toml
    │   ├── benches/
    │   │   └── coverage_scale.rs  # //! Read-path scale: one-day reads, torn
    │   ├── src/
    │   │   ├── append_log.rs  # //! Durable CSV record log: the one appe
    │   │   ├── arrow_decode.rs  # //! One Arrow decode layer for both Parq
    │   │   ├── capability.rs  # //! One capability token per store root:
    │   │   ├── catalog.rs  # //! Durable instrument identity: markets
    │   │   ├── commit.rs  # //! The provisional/committed bar lifecy
    │   │   ├── coverage.rs  # //! What the tape holds, and whether it 
    │   │   ├── error.rs  # //! Typed store failures with an explici
    │   │   ├── fold.rs  # //! Bar folding: coarser spacings derive
    │   │   ├── health.rs  # //! Serving-time store liveness: what th
    │   │   ├── health_edge.rs  # //! The stream-health hysteresis: whethe
    │   │   ├── history.rs  # //! History providers: where repair bars
    │   │   ├── identity.rs  # //! Verified content identity: a fast di
    │   │   ├── key.rs  # //! Durable symbol identity.
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── manifest.rs  # //! Sealed-snapshot manifest: one durabl
    │   │   ├── market_parquet.rs  # //! Market-data Parquet ingest: read ven
    │   │   ├── plan.rs  # //! Read planning: resolve what was aske
    │   │   ├── range.rs  # //! Millisecond spans with open ends: wh
    │   │   ├── registry.rs  # //! File-backed market registry: instrum
    │   │   ├── repair.rs  # //! Watermark repair: classify, fetch, f
    │   │   ├── snapshot.rs  # //! Sealed Parquet snapshots: a byte-sta
    │   │   ├── store.rs  # //! The synchronous candle interface: wh
    │   │   └── tape.rs  # //! The file-backed working store: one r
    │   └── tests/
    │       ├── append_idempotent.rs  # //! Append idempotence: re-delivery is c
    │       ├── append_log.rs  # //! The durable append-log contract: rou
    │       ├── capability_gate.rs  # //! The capability gate: ungranted tasks
    │       ├── commit_lifecycle.rs  # //! The lifecycle control group: both di
    │       ├── commit_merge_refusal.rs  # //! Merge refusal: what a restatement ma
    │       ├── commit_seal.rs  # //! Sealing: the one way a held bar beco
    │       ├── common/
    │       │   ├── append_log_fixtures.rs  # //! Test-only append-log record: a two-c
    │       │   ├── calibration.rs  # //! The freshness-calibration harness: c
    │       │   ├── fold.rs  # //! The fold-calibration harness: golden
    │       │   ├── history.rs  # //! row parser behind the test adapter, 
    │       │   ├── market_tape_setup.rs  # //! Shared file-tape test setup: an in-m
    │       │   ├── oracle.rs  # //! The whole-tape read algorithm, rebui
    │       │   ├── registry_setup.rs  # //! Shared registry-test setup: capabili
    │       │   ├── scale.rs  # //! Shared price-scale representability 
    │       │   ├── snapshot.rs  # //! Shared snapshot-test setup: an in-me
    │       │   ├── tape_conformance.rs  # //! Shared `CandleStore` conformance hel
    │       │   └── tapes.rs  # //! Shared bar constructors for the stor
    │       ├── coverage_holes.rs  # //! The planner and the runtime agree ab
    │       ├── coverage_stamps.rs  # //! Stamp-derived coverage: the same run
    │       ├── data/
    │       │   ├── fold_goldens.json
    │       │   ├── gap_fixtures.json
    │       │   └── seal_economics.json
    │       ├── demand_fill.rs  # //! On-demand fill: the seed rule, both 
    │       ├── demand_fill_bounded.rs  # //! Bounded demand fill: spans wider tha
    │       ├── error.rs  # //! Store error taxonomy: the three-way 
    │       ├── exactness_versus_f64.rs  # //! Exactness versus `f64`: the store ma
    │       ├── fold_basic.rs  # //! Bar-folding basics: coarser spacings
    │       ├── fold_calibration.rs  # //! The fold-calibration gate: frozen bo
    │       ├── fold_calibration_negative.rs  # //! The fold-calibration negative contro
    │       ├── fold_calibration_stability.rs  # //! The fold-calibration harness is dete
    │       ├── fold_edges.rs  # //! Edge policy on the one fold core: wh
    │       ├── gap_calibration.rs  # //! The freshness-calibration gate: the 
    │       ├── gap_calibration_stability.rs  # //! The calibration harness is determini
    │       ├── gap_classify.rs  # //! The gap-classification control group
    │       ├── health_edge.rs  # //! The stream-health transition table: 
    │       ├── key_newtype.rs  # //! The durable symbol identity is a new
    │       ├── manifest_json.rs  # //! The JSON seal manifest contract: rou
    │       ├── market_parquet.rs  # //! Market-data Parquet ingest: the anno
    │       ├── market_tape_conformance.rs  # //! `CandleStore` conformance instantiat
    │       ├── plan_read_end_to_end.rs  # //! Seed → resolve → read: the pl...
    │       ├── plan_resolve.rs  # //! Read-plan resolution: what was asked
    │       ├── plan_unbounded.rs  # //! Open-ended reads: a limit bounds the
    │       ├── price_scale_calibration.rs  # //! Price-scale calibration: the round-t
    │       ├── range_span.rs  # //! Span arithmetic: construction refuse
    │       ├── read_windowed_oracle.rs  # //! Windowed stamp planning serves what 
    │       ├── registry.rs  # //! The file-backed registry contract: r
    │       ├── rejected_is_counted.rs  # //! Rejection is counted: an unrepresent
    │       ├── repair_backoff.rs  # //! Repair backoff: the shared schedule,
    │       ├── repair_classification.rs  # //! Repair classification: the status ca
    │       ├── repair_idempotent.rs  # //! Repair idempotence: filling the same
    │       ├── repair_single_flight.rs  # //! Concurrent repair idempotence: the p
    │       ├── snapshot_digest_stability.rs  # //! Snapshot digest stability: same byte
    │       ├── snapshot_manifest_crash.rs  # //! Snapshot crash recovery: a missing f
    │       ├── snapshot_market_parity.rs  # //! Sealed-artifact parity: the snapshot
    │       ├── snapshot_price_scale.rs  # //! Snapshot price-scale representabilit
    │       ├── snapshot_roundtrip.rs  # //! Snapshot round-trip: the control gro
    │       ├── snapshot_seal_economics.rs  # //! Seal economics: the `min_part_rows` 
    │       ├── store_coverage.rs  # //! Coverage answers, including the answ
    │       ├── store_health_derivation.rs  # //! `StoreHealth` derivation pins: every
    │       ├── store_ordering.rs  # //! The ordering regression test: the st
    │       ├── tape_budget.rs  # //! File-tape read-path budget: served r
    │       ├── tape_capacity.rs  # //! Capacity-bound calibration: the one 
    │       ├── tape_compact.rs  # //! Explicit compaction: seal, then trun
    │       ├── verified_id_collision.rs  # //! The collision control group: a diges
    │       └── warmup_roundtrip.rs  # //! The fetch-bounds round-trip pin: wid
    ├── protopyne-strategy/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── activity.rs  # //! Options-activity scoring: an optiona
    │   │   ├── axes.rs  # //! The two measurement axes, kept apart
    │   │   ├── economics.rs  # //! Spread economics: one pure pricer fo
    │   │   ├── elements.rs  # //! Strategy elements: one required sign
    │   │   ├── equity.rs  # //! Equity trade management: sizing mode
    │   │   ├── error.rs  # //! Typed errors for strategy compositio
    │   │   ├── fixtures.rs  # //! One grant fixture for every test tar
    │   │   ├── generator.rs  # //! RSRS-gated vertical generator and re
    │   │   ├── grant.rs  # //! Arming grants and their pure evaluat
    │   │   ├── integration_lite/
    │   │   │   ├── migrate.rs  # //! The nested composition view and its 
    │   │   │   ├── mod.rs  # //! The IntegrationLite composition: one
    │   │   │   ├── signal.rs  # //! The signal element of the Integratio
    │   │   │   └── spec.rs  # //! IntegrationLite strategy configurati
    │   │   ├── ledger.rs  # //! The grant ledger: one reserve/consum
    │   │   ├── legs.rs  # //! Leg selection: turn a multi-leg stru
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── position.rs  # //! One position, one manager, one place
    │   │   ├── proposal.rs  # //! Proposals and their journaled lifecy
    │   │   ├── risk.rs  # //! Mandatory risk engine: the non-bypas
    │   │   ├── signals_kernel.rs  # //! Declarative, multi-line kernel signa
    │   │   ├── signals_lite.rs  # //! Native IntegrationLite signal: close
    │   │   ├── signals_pine.rs  # //! Pine-backed signal: one declared plo
    │   │   ├── slots/
    │   │   │   ├── composition.rs  # //! Whole-composition slots: the strateg
    │   │   │   ├── envelope.rs  # //! Uniform `Component` envelopes over t
    │   │   │   ├── exit.rs  # //! Exit intents: what a rule asks the r
    │   │   │   ├── filter.rs  # //! Filter slots: admission of an entry 
    │   │   │   ├── mod.rs  # //! Runtime-assignable strategy slots.
    │   │   │   ├── sizing.rs  # //! Sizer slots: the existing sizing pol
    │   │   │   └── trigger.rs  # //! Trigger slots: the six Pine entry br
    │   │   ├── tape.rs  # //! A point-in-time window that cannot s
    │   │   └── view.rs  # //! Point-in-time decision input.
    │   └── tests/
    │       ├── activity.rs  # //! Options-activity seam: the chain pro
    │       ├── activity_calibration.rs  # //! Chain-activity calibration: a quiet 
    │       ├── adjustment.rs  # //! Adjustment rules: drafts, not exits.
    │       ├── assignment_guard.rs  # //! Assignment and ex-dividend protectio
    │       ├── axes.rs  # //! Unit tests for the measurement-axis 
    │       ├── calibration.rs  # //! Grant threshold calibration: must-fi
    │       ├── data/
    │       │   ├── chain_activity_labels.json
    │       │   ├── chain_activity_tally.json
    │       │   ├── exhaustion_tighten_tally.json
    │       │   ├── exit_rule_tally.json
    │       │   ├── loss_tally.json
    │       │   ├── regime_corpus_tally.json
    │       │   ├── signal_rule_tally.json
    │       │   ├── sizing_tally.json
    │       │   ├── trigger_delta_window.json
    │       │   ├── trigger_exhaustion.json
    │       │   ├── trigger_extension.json
    │       │   ├── trigger_regime.json
    │       │   └── trigger_volume.json
    │       ├── doc_axis.rs  # //! Every gate, trigger, and threshold s
    │       ├── economics.rs  # //! Spread-economics identities, refusal
    │       ├── elements.rs  # use super::*;
    │       ├── envelope_pins.rs  # //! Envelope pins: the uniform handle ag
    │       ├── equity.rs  # //! Equity management contract: sizing m
    │       ├── error.rs  # use super::*;
    │       ├── exit.rs  # //! Exit intents: the legacy-loss mappin
    │       ├── filter_pins.rs  # //! Composition pins for filter admissio
    │       ├── fixtures.rs  # //! The shared grant fixture: its baseli
    │       ├── generator.rs  # //! Generator and rebalancer calibration
    │       ├── grant.rs  # use super::*;
    │       ├── heuristic_calibration.rs  # //! Heuristic calibration: the template 
    │       ├── integration_lite.rs  # //! Strategy-group schema contract: ever
    │       ├── integration_lite_signal.rs  # //! Native IntegrationLite signal contra
    │       ├── kelly_sizing.rs  # //! Capped fractional-Kelly sizing: the 
    │       ├── layering.rs  # //! Crate-graph guards for the decision 
    │       ├── ledger.rs  # use super::*;
    │       ├── legs.rs  # //! Leg selection fixture matrix: exact 
    │       ├── loss_calibration.rs  # //! Loss-rule calibration: control group
    │       ├── migrate.rs  # //! The nested view and its compatibilit
    │       ├── position.rs  # //! The position manager: stop precedenc
    │       ├── proposal.rs  # use super::*;
    │       ├── regime_calibration.rs  # //! Regime-gate corpus: every label arm 
    │       ├── research_closure.rs  # //! The research closure: what a default
    │       ├── risk.rs  # use super::*;
    │       ├── signal_calibration.rs  # //! Signal-rule calibration: control gro
    │       ├── signals_kernel.rs  # //! Kernel-signal guards: the rule vocab
    │       ├── signals_pine.rs  # //! Pine signal guards: the version, par
    │       ├── sizing_axis_pins.rs  # //! Sizing axis pins: every catalog size
    │       ├── sizing_calibration.rs  # //! Sizing-policy calibration: control g
    │       ├── sizing_exit_pins.rs  # //! Composition pins for sizing and prot
    │       ├── slot_identity_pins.rs  # //! Slot-meaning identity: one id, one v
    │       ├── slots.rs  # //! The slot registries: ids, refusals, 
    │       ├── tape.rs  # //! Bounds and cursor behaviour of the p
    │       ├── trigger_calibration.rs  # //! Trigger-shape calibration: every M3 
    │       ├── trigger_pins.rs  # //! Composition pins for trigger verdict
    │       ├── validate.rs  # use super::*;
    │       ├── view.rs  # //! Point-in-time view guards: the futur
    │       └── volatility_sizing.rs  # //! Volatility-scaled sizing: the contro
    ├── protopyne-types/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── bar.rs  # //! Closed-bar market data and corporate
    │   │   ├── chain.rs  # //! Option-chain vocabulary: contracts, 
    │   │   ├── epoch.rs  # //! Epoch-millisecond instants: one nume
    │   │   ├── error.rs  # //! Error types for the base market-data
    │   │   ├── fold.rs  # //! The complete-bucket rule, one home.
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── mark.rs  # //! Option-mark basis: where a position'
    │   │   ├── money.rs  # //! `Decimal`-backed money and quantity 
    │   │   ├── series.rs  # //! Series and multi-timeframe context f
    │   │   ├── session.rs  # //! Single wall-clock predicate for exch
    │   │   ├── symbol.rs  # //! Scoped symbol registry: per-run stri
    │   │   ├── timeframe.rs  # //! Candle timeframe vocabulary shared b
    │   │   ├── warmup.rs  # //! Warmup fetch-bounds and output-trimm
    │   │   └── window.rs  # //! Closed-bar window with gap detection
    │   └── tests/
    │       ├── bar.rs  # use super::*;
    │       ├── chain.rs  # use super::*;
    │       ├── epoch.rs  # use super::*;
    │       ├── error.rs  # use super::*;
    │       ├── fold.rs  # //! The complete-bucket rule, one home: 
    │       ├── money.rs  # use super::*;
    │       ├── series.rs  # use super::*;
    │       ├── session.rs  # //! Wall-clock predicate goldens: sessio
    │       ├── symbol.rs  # use super::*;
    │       ├── timeframe.rs  # use super::*;
    │       ├── validate.rs  # use crate::{Adjustment, Bar, Money, Opti
    │       ├── warmup.rs  # use super::*;
    │       └── window.rs  # use super::*;
    ├── protopyne-venue/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── account.rs  # //! Broker account/order/deal reads: one
    │   │   ├── alert.rs  # //! Out-of-band alerting: breaker trips 
    │   │   ├── auth.rs  # //! Credential lifecycle: one authority,
    │   │   ├── backfill.rs  # //! Reconnect backfill: a dropped window
    │   │   ├── broker.rs  # //! Broker composition seam: one directo
    │   │   ├── cert_soak.rs  # //! Cert-sandbox soak harness: one deter
    │   │   ├── command.rs  # //! Portfolio registry, command plane, a
    │   │   ├── depth.rs  # //! Session-scoped depth book with dark 
    │   │   ├── error.rs  # //! Venue-side typed errors with an expl
    │   │   ├── facts.rs  # //! `PortfolioFacts` producer over a ven
    │   │   ├── feed.rs  # //! Market-data feeds and tick-to-bar ag
    │   │   ├── health.rs  # //! Health gate: trade only on fresh dat
    │   │   ├── journal.rs  # //! Journaled single-writer order state 
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── live.rs  # //! Authenticated live venue: order entr
    │   │   ├── market.rs  # //! Normalized market subscription model
    │   │   ├── order.rs  # //! Order vocabulary: intents, lifecycle
    │   │   ├── paper.rs  # //! `PaperVenue`: the rehearsal venue be
    │   │   ├── pipeline.rs  # //! NOT-A-PIPELINE (M16): full-record ga
    │   │   ├── position.rs  # //! Leg-aware position engine: pure fill
    │   │   ├── quote.rs  # //! Quotes, the liquidity filter, and th
    │   │   ├── reading.rs  # //! One bar's producer-confidence readin
    │   │   ├── reconcile.rs  # //! Reconciliation: the venue is the sou
    │   │   ├── registry.rs  # //! Execution routing registry: which ve
    │   │   ├── session.rs  # //! Supervised trading session: feeds, p
    │   │   ├── stream.rs  # //! Supervised stream: reconnect with ji
    │   │   ├── submit.rs  # //! Safe submission protocol: journal in
    │   │   ├── tastytrade.rs  # //! Tastytrade transport skeleton: marke
    │   │   ├── tastytrade_account.rs  # //! Tastytrade account reads: balances, 
    │   │   ├── tastytrade_feed.rs  # //! Tastytrade market-data feed: supervi
    │   │   ├── ticket.rs  # //! Manual ticket workflow: exact human-
    │   │   └── venue.rs  # //! The `Venue` trait: one code path for
    │   └── tests/
    │       ├── alert.rs  # //! Out-of-band alerting: breaker trips 
    │       ├── async_bridge_guard.rs  # //! No runtime bridge on the async venue
    │       ├── auth.rs  # //! Credential lifecycle tests: single-f
    │       ├── backfill.rs  # //! Backfill primitives: the missing-ran
    │       ├── book_calibration.rs  # //! Book-detector calibration: controls 
    │       ├── broker.rs  # //! Broker composition tests: substituti
    │       ├── calibration.rs  # //! Fill calibration: must-fill vs.
    │       ├── cert_soak.rs  # //! Cert-sandbox soak, hermetic twin: th
    │       ├── command.rs  # //! Command-plane calibration: registry 
    │       ├── command_arm.rs  # //! Journaled arming: `CommandUnit` arm/
    │       ├── command_plane.rs  # //! Live command plane wiring: journaled
    │       ├── corporate_actions.rs  # //! Corporate actions against the venue 
    │       ├── depth.rs  # use super::*;
    │       ├── error.rs  # use super::*;
    │       ├── facts.rs  # use super::*;
    │       ├── fee_schedule.rs  # //! Commission schedule: the flat arm, t
    │       ├── feed.rs  # //! Tick-to-bar adapter and feed vocabul
    │       ├── health.rs  # use super::*;
    │       ├── health_exits_survive_store_halt.rs  # //! Exits survive a store halt: a store 
    │       ├── health_gate_matrix.rs  # //! Gate-by-intent truth table: every he
    │       ├── health_serving.rs  # //! Serving-path gating: the command pla
    │       ├── health_store_gate.rs  # //! Store-health gate pins: the fourth `
    │       ├── ib_conformance.rs  # //! IB forward-compatibility conformance
    │       ├── journal.rs  # use super::*;
    │       ├── journal_characterization.rs  # //! Journal characterization: replay/out
    │       ├── journal_log.rs  # //! Journal over the event log: contract
    │       ├── library_only.rs  # //! Library-only markers: a subsystem wi
    │       ├── live/
    │       │   ├── cert_sandbox.rs  # //! Cert-sandbox readiness: production b
    │       │   ├── live_soak.rs  # //! Cert-sandbox bounded soak: the same 
    │       │   └── soak.rs  # //! Cert-sandbox soak: the long read-onl
    │       ├── live.rs  # //! Live cert-sandbox tests for protopyn
    │       ├── live_venue.rs  # //! Authenticated live-venue adapter: lo
    │       ├── market.rs  # use crate::journal::VenueStorage;
    │       ├── order.rs  # use super::*;
    │       ├── package_tolerance.rs  # //! Package-price tolerance calibration:
    │       ├── paper.rs  # use super::*;
    │       ├── paper_legs.rs  # use super::*;
    │       ├── pipeline.rs  # use super::*;
    │       ├── position.rs  # use super::*;
    │       ├── quote.rs  # use super::*;
    │       ├── reading.rs  # //! Producer-confidence reading pins: a 
    │       ├── reconcile.rs  # use super::*;
    │       ├── reconcile_driver.rs  # //! Reconciliation driver: the scheduled
    │       ├── registry.rs  # //! Execution routing registry: routing-
    │       ├── sensor_describe.rs  # use crate::{
    │       ├── session.rs  # use super::*;
    │       ├── soak.rs  # //! End-to-end fault soak: reconnect sto
    │       ├── spread_calibration.rs  # //! Spread fill-policy calibration: the 
    │       ├── stream.rs  # //! Streaming supervision: fault-scripte
    │       ├── stream_calibration.rs  # //! Stream-breaker calibration: must-not
    │       ├── submission_guard.rs  # //! No token-free order path: every prod
    │       ├── submit.rs  # use super::*;
    │       ├── submit_recorder.rs  # //! Recorder seam: the safe-submission p
    │       ├── symbol_index.rs  # use super::*;
    │       ├── tastytrade.rs  # //! Tastytrade transport skeleton calibr
    │       ├── tastytrade_account.rs  # //! Tastytrade transaction/balance trans
    │       ├── tastytrade_feed.rs  # //! Supervised Tastytrade streaming tran
    │       ├── ticket.rs  # //! Manual-ticket workflow: exact human 
    │       ├── transport_calibration.rs  # //! Reconnect/backoff + ladder policy ca
    │       ├── validate.rs  # use super::*;
    │       └── venue.rs  # use super::*;
    ├── protopyne-view/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── gateway.rs  # //! Read abstraction plus mock: render o
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── symbols.rs  # //! Shipped symbol catalog: the one inst
    │   │   └── views.rs  # //! Neutral read DTOs: the server's wire
    │   └── tests/
    │       ├── gateway.rs  # use super::*;
    │       ├── symbols.rs  # use super::*;
    │       └── views.rs  # use super::*;
    ├── rdf/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── lexer.rs  # use crate::RdfError;
    │   │   ├── lib.rs  # //! guidance-rdf: RDF/Turtle/N-Quads par
    │   │   ├── normalize.rs  # pub fn hash_iri(iri: &str) -> i64 {
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
    │   ├── benches/
    │   │   ├── stage_dispatch.rs  # //! Pipeline stage dispatch throughput: 
    │   │   └── tree.rs  # //! Classification-tree evaluation cost:
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
    │   │   │   ├── preset.rs  # //! Rendering of downstream serving conf
    │   │   │   ├── ps_table.rs  # //! `coral-router ps` table: one compact
    │   │   │   └── tables.rs  # //! Shared plain-text table renderer for
    │   │   ├── concept_store_sqlite.rs  # //! The router's durable [`ConceptStore`
    │   │   ├── config/
    │   │   │   ├── addr.rs  # //! Address parsing, host equivalence, a
    │   │   │   ├── audit.rs  # //! Audit-log file configuration — ...
    │   │   │   ├── builder.rs  # //! Pipeline builder - constructs pipeli
    │   │   │   ├── charts.rs  # //! Chart-store and post-processing conf
    │   │   │   ├── classification.rs  # //! Classification-tree configuration
    │   │   │   ├── compiled_tree.rs  # //! Precompiled classification tree.
    │   │   │   ├── escalation.rs  # //! Escalation-ladder configuration
    │   │   │   ├── filters.rs  # //! Filter types and reject patterns for
    │   │   │   ├── fleet.rs  # //! Fleet run-block types: the `roles.
    │   │   │   ├── frame.rs  # //! Frame-resolution worker configuratio
    │   │   │   ├── ledger_group.rs  # //! LedgerGroupConfig (M8).
    │   │   │   ├── models.rs  # //! Model-table entry types: `models` en
    │   │   │   ├── normalize.rs  # //! Pre-typing config normalizer legs: r
    │   │   │   ├── orchestrator.rs  # //! Ledger-agent coordinator configurati
    │   │   │   ├── overlay.rs  # //! Async overlay configuration — t...
    │   │   │   ├── refine_policy.rs  # //! Router DTO for `spacy_rs::RefinePoli
    │   │   │   ├── review.rs  # //! Async review configuration — th...
    │   │   │   ├── rigor.rs  # //! Rigor-route configuration — the...
    │   │   │   ├── root.rs  # //! Router configuration root types - th
    │   │   │   ├── rounds.rs  # //! M3.3 migration shim: the canonical d
    │   │   │   ├── routing.rs  # //! Route resolution and routing configu
    │   │   │   ├── runtime.rs  # //! RuntimeConfig (M8).
    │   │   │   ├── server.rs  # //! Server bind configuration — the...
    │   │   │   ├── serving.rs  # //! ServingConfig sub-config (M8) —...
    │   │   │   ├── session.rs  # //! Session composition section — t...
    │   │   │   ├── sidecar.rs  # //! Sidecar instance-management polic...
    │   │   │   └── tree_reload.rs  # //! Runtime classification-tree regenera
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
    │   │   │   ├── luhn.rs  # pub use common_core::checksum::luhn_vali
    │   │   │   ├── mod.rs  # //! Filter control-plane interface: the 
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
    │   │   │   ├── traits/
    │   │   │   │   ├── backend.rs  # //! The llama inference backend: a thin 
    │   │   │   │   ├── common.rs  # //! Shared llama-adapter error and resid
    │   │   │   │   ├── context.rs  # //! The `LlmContext` adapter for one nam
    │   │   │   │   ├── fleet.rs  # //! The unified `LlmFleet` facade over t
    │   │   │   │   ├── kv_cache.rs  # //! The `LlmKVCache` adapter for one lla
    │   │   │   │   └── weights.rs  # //! The `LlmWeights` adapter for a manag
    │   │   │   └── traits.rs  # //! The llama half of the shared `LlmWei
    │   │   ├── knowledge.rs  # //! `KnowledgeCapability` implementation
    │   │   ├── kv_cache.rs  # //! KV cache snapshot management - two-t
    │   │   ├── ledger/
    │   │   │   ├── annotations.rs  # //! M10.4 migration shim: `AnnotationSto
    │   │   │   ├── context_cache.rs  # //! Ledger-backed deterministic context 
    │   │   │   ├── correction_index.rs  # //! The router's [`CorrectionIndex`] ove
    │   │   │   ├── entity_promotion.rs  # //! Parse-time entity promotion (M16): t
    │   │   │   ├── frame_index.rs  # //! Frame resolution — the router's...
    │   │   │   ├── nlp.rs  # //! Ledger writer for the NLP parse (roa
    │   │   │   ├── node_annotation.rs  # //! `NodeAnnotation` — the fully-ma...
    │   │   │   ├── orchestrator.rs  # //! `LedgerAgentCoordinator` — the ...
    │   │   │   ├── overlay.rs  # //! The overlay/candidate plane.
    │   │   │   ├── overlay_acceptance.rs  # //! ArcReady overlay acceptance suite (O
    │   │   │   ├── overlay_worker.rs  # //! `OverlayWorker` — continuous ba...
    │   │   │   ├── prompt.rs  # //! M10.2 migration shim: the pure promp
    │   │   │   ├── span_cache.rs  # //! Span-level detail cache as a **read-
    │   │   │   ├── tiering.rs  # //! `LedgerTierWorker` — continuous...
    │   │   │   ├── workflow_replay.rs  # //! Workflow replay loop (M14a): record 
    │   │   │   └── workflow_store.rs  # //! Workflow extraction store (M8) ...
    │   │   ├── ledger.rs  # //! Full-detail content ledger with LOD 
    │   │   ├── ledger_guard.rs  # //! Irreversible write-path scrubber for
    │   │   ├── lib.rs  # //! LLM Router & Agent Orchestration Fra
    │   │   ├── logging.rs  # //! Structured logging infrastructure fo
    │   │   ├── metrics.rs  # //! Failure classification for the route
    │   │   ├── node_store.rs  # //! ContentNodeStore — the shared, ...
    │   │   ├── normalize.rs  # //! Request and response normalizatio...
    │   │   ├── ort/
    │   │   │   ├── backends.rs  # //! Onnx inference backends: the registr
    │   │   │   ├── prefilter.rs  # //! Onnx pre-filter and scorer builders:
    │   │   │   ├── registry.rs  # //! Onnx session-registry composition: t
    │   │   │   └── weights.rs  # //! The onnx half of the shared `LlmWeig
    │   │   ├── ort.rs  # //! Ort ONNX registry composition.
    │   │   ├── overlay/
    │   │   │   └── canonical.rs  # //! M9.2 migration shim: the canonical-f
    │   │   ├── overlay.rs  # //! Pure overlay-plane data.
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
    │   │   │   ├── compaction.rs  # //! Pre-dispatch context guard + LOD com
    │   │   │   ├── cors.rs  # //! CORS headers — the single owner.
    │   │   │   ├── dispatch.rs  # use std::collections::HashMap;
    │   │   │   ├── entity_link.rs  # //! The async entity-link overlay worker
    │   │   │   ├── handler.rs  # use std::collections::HashMap;
    │   │   │   ├── instances_api.rs  # //! Public `/instances` management API f
    │   │   │   ├── responses.rs  # use std::sync::atomic::AtomicU64;
    │   │   │   ├── review.rs  # //! The async review worker.
    │   │   │   └── session_step.rs  # //! Per-request session-step lifecycle: 
    │   │   ├── server.rs  # //! HTTP server exposing the router pipe
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
    │   │   │   ├── fleet.rs  # //! The supervisor process: one managed 
    │   │   │   ├── health.rs  # use std::sync::atomic::{AtomicBool, Orde
    │   │   │   └── spec.rs  # //! Llama-server spawn spec and pure spa
    │   │   ├── supervisor.rs  # //! Managed `llama-server` process super
    │   │   ├── target_match.rs  # //! Classifier-driven target matching...
    │   │   ├── telemetry.rs  # //! Structured telemetry events — t...
    │   │   ├── test_stubs.rs  # use std::collections::VecDeque;
    │   │   ├── test_support.rs  # //! Shared test logging capture.
    │   │   ├── testing/
    │   │   │   ├── calibration.rs  # //! Calibration control-group helpers.
    │   │   │   ├── fixtures/
    │   │   │   │   └── ledger_lod.json
    │   │   │   ├── ledger_golden.rs  # //! Golden ledger LOD snapshot (M4).
    │   │   │   ├── mock.rs  # use std::collections::HashMap;
    │   │   │   └── mod.rs  # pub mod calibration;
    │   │   └── transforms/
    │   │       ├── codeword_anonymize.rs  # use std::collections::HashMap;
    │   │       ├── decompose_hypothetical.rs  # use fluent_llm::anonymize;
    │   │       ├── decompose_subtasks.rs  # use fluent_llm::Decomposer;
    │   │       ├── mod.rs  # //! Transform control-plane interface: t
    │   │       ├── none.rs  # use crate::transforms::{TransformError, 
    │   │       ├── pii_anonymize.rs  # use std::collections::HashMap;
    │   │       ├── sanitize.rs  # use common_core::string::{filter_unsafe_
    │   │       └── secret_mask.rs  # use std::sync::LazyLock;
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
    │       ├── cli_commands_mod.rs  # use super::server::parse_prometheus_metr
    │       ├── cli_gguf.rs  # // Exact asserts on integer-valued snaps
    │       ├── cli_preset.rs  # use super::*;
    │       ├── cli_ps_table.rs  # use super::*;
    │       ├── common.rs  # //! Crate-typed HTTP test fixtures share
    │       ├── concept_store_sqlite.rs  # use super::*;
    │       ├── config_addr.rs  # use super::*;
    │       ├── config_boot_gate.rs  # use super::*;
    │       ├── config_builder.rs  # use super::*;
    │       ├── config_classification.rs  # // Tests assert float threshold values a
    │       ├── config_escalation.rs  # use super::*;
    │       ├── config_filters.rs  # use super::*;
    │       ├── config_normalize_parity.rs  # //! Byte-parity oracle for the config pr
    │       ├── config_refine_policy.rs  # use super::*;
    │       ├── config_root.rs  # // Tests assert float config values agai
    │       ├── config_route_tests.rs  # //! Config-synced routing integration te
    │       ├── config_routing.rs  # use super::*;
    │       ├── coordinator_gate.rs  # use super::*;
    │       ├── dag_session.rs  # use super::*;
    │       ├── data/
    │       │   ├── boot_invalid_member.json
    │       │   ├── boot_invalid_role.json
    │       │   ├── boot_valid.json
    │       │   ├── coherence_safety_reject_corpus.json
    │       │   ├── config_normalize/
    │       │   │   ├── explicit_null_params_never_filled.snap
    │       │   │   ├── fleet_run_block_sparse_merge.snap
    │       │   │   ├── models_default_feeds_fleet.snap
    │       │   │   ├── models_default_template_fill.snap
    │       │   │   ├── no_default_role_untouched.snap
    │       │   │   ├── no_models_default_untouched.snap
    │       │   │   ├── non_object_model_entry_skipped.snap
    │       │   │   ├── non_object_models_default_untouched.snap
    │       │   │   ├── sibling_declaring_every_key_unchanged.snap
    │       │   │   ├── template_becomes_fleet_without_default_params.snap
    │       │   │   └── template_key_inheritance.snap
    │       │   ├── config_overhaul_sketch.json
    │       │   ├── config_roles_split_fixture.json
    │       │   ├── ls_fixture.json
    │       │   ├── residency_engine_fleet_aggregate.json
    │       │   ├── residency_engine_fleet_models.json
    │       │   ├── residency_engine_pool_aggregate.json
    │       │   ├── residency_engine_pool_models.json
    │       │   ├── routing_fallback_corpus.json
    │       │   ├── routing_fallback_report.json
    │       │   ├── routing_role_fixture.json
    │       │   ├── routing_role_golden.json
    │       │   ├── routing_sentinel_corpus.json
    │       │   ├── routing_sentinel_report.json
    │       │   └── target_match_ladder_corpus.json
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
    │       ├── instances_mod.rs  # use super::stub::{StubHandler, StubServe
    │       ├── instances_traits.rs  # use super::*;
    │       ├── knowledge.rs  # use super::*;
    │       ├── kv_cache.rs  # use super::*;
    │       ├── ledger.rs  # use super::*;
    │       ├── ledger_annotations.rs  # //! Router-side annotation linkage tests
    │       ├── ledger_context_cache.rs  # use super::*;
    │       ├── ledger_correction_index.rs  # use super::*;
    │       ├── ledger_entity_promotion.rs  # //! End-to-end control-group tests for p
    │       ├── ledger_frame_index.rs  # use super::*;
    │       ├── ledger_frame_index_cal.rs  # use super::*;
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
    │       ├── ort.rs  # use std::sync::Arc;
    │       ├── overlay_calibration.rs  # // Exact asserts on measured overlay num
    │       ├── overlay_canonical.rs  # use super::*;
    │       ├── pipeline.rs  # use super::*;
    │       ├── pipeline_types.rs  # use super::*;
    │       ├── qualified_model_id_roundtrip.rs  # use super::*;
    │       ├── ranking.rs  # // Exact asserts on ranking scores are d
    │       ├── residency_engine_golden.rs  # //! Aggregation-envelope goldens for the
    │       ├── retrieval.rs  # use super::*;
    │       ├── routes_plan.rs  # use super::*;
    │       ├── routes_rigor_mod.rs  # // Exact asserts on calibrated round/lad
    │       ├── routing_context.rs  # use super::*;
    │       ├── routing_fallback_golden.rs  # //! Routing-fallback calibration corpus.
    │       ├── routing_role_golden.rs  # //! Role-first resolved-target golden.
    │       ├── routing_sentinel_golden.rs  # //! Sentinel fallback + late-binding cal
    │       ├── rubric_fixtures.rs  # //! Rubric-based test fixtures for `Resu
    │       ├── score_matrix.rs  # use super::*;
    │       ├── score_matrix_calibration.rs  # //! Calibration of `ScoreMatrix::router_
    │       ├── score_matrix_golden.rs  # use super::*;
    │       ├── seam_adapter.rs  # //! The single `ChatBackend` → fetc...
    │       ├── server_admin.rs  # use std::sync::Arc;
    │       ├── server_compaction.rs  # use super::*;
    │       ├── server_dispatch.rs  # use super::*;
    │       ├── server_entity_link.rs  # use super::*;
    │       ├── server_http_tests.rs  # //! HTTP-level integration tests for the
    │       ├── server_instances_api.rs  # use super::*;
    │       ├── server_responses.rs  # use super::*;
    │       ├── server_review.rs  # use super::*;
    │       ├── server_tests.rs  # #[cfg(test)]
    │       ├── stage_context.rs  # use super::*;
    │       ├── stage_tests.rs  # #[cfg(test)]
    │       ├── stages_classifier.rs  # // Exact asserts on parsed classifier sc
    │       ├── stages_classifier_action.rs  # use super::*;
    │       ├── stages_common.rs  # use super::*;
    │       ├── stages_nlp.rs  # use super::*;
    │       ├── stages_overlay.rs  # use super::*;
    │       ├── stages_pipeline_ref.rs  # use super::*;
    │       ├── stages_retry_classifier.rs  # use super::*;
    │       ├── stages_tree_mod.rs  # use std::collections::HashMap;
    │       ├── streaming.rs  # use super::*;
    │       ├── summarization.rs  # // Exact asserts on parsed scorer output
    │       ├── supervisor.rs  # use super::*;
    │       ├── supervisor_adopt.rs  # //! Orphan-adoption tests: argv parsing,
    │       ├── supervisor_integration_tests.rs  # //! End-to-end supervisor + sidecar inte
    │       ├── target_match.rs  # use std::collections::HashMap;
    │       ├── target_match_order.rs  # use std::collections::HashMap;
    │       ├── telemetry.rs  # use super::*;
    │       ├── testing_calibration.rs  # use super::*;
    │       ├── threshold_calibration.rs  # //! Calibration suite: stub goldens (M7b
    │       ├── transforms_codeword_anonymize.rs  # use super::*;
    │       ├── transforms_sanitize.rs  # use super::*;
    │       ├── transforms_secret_mask.rs  # use super::*;
    │       ├── transforms_tests.rs  # #[cfg(test)]
    │       ├── tree_compiled.rs  # //! Equivalence corpus for the precompil
    │       ├── tree_reload.rs  # use super::*;
    │       ├── tree_verdict.rs  # use super::*;
    │       ├── types.rs  # use fluent_types::openai_wire::*;
    │       ├── vector_index.rs  # use super::*;
    │       ├── views.rs  # use std::collections::HashSet;
    │       ├── workflow_replay.rs  # use super::*;
    │       ├── workflow_replay_cal.rs  # use crate::ledger::workflow_replay::{
    │       └── workflow_store_cal.rs  # //! Calibration of the workflow **write*
    ├── search-vector/
    │   ├── Cargo.toml
    │   ├── benches/
    │   │   └── hnsw_vs_brute.rs  # //! P6 calibration bench: HNSW vs int8 b
    │   ├── src/
    │   │   ├── aliases.rs  # use std::collections::HashMap;
    │   │   ├── db/
    │   │   │   ├── fragments.rs  # //! Fragment recall rows, filter pushdow
    │   │   │   ├── graph_rows.rs  # //! Graph recall rows: persisted import 
    │   │   │   └── schema.rs  # //! Fragment/graph schema: DDL, init, an
    │   │   ├── db.rs  # use std::collections::HashMap;
    │   │   ├── fusion.rs  # //! Score fusion: RRF kernel surface, bo
    │   │   ├── lib.rs  # #![forbid(unsafe_code)]
    │   │   ├── salience.rs  # //! Candidate-centrality scoring: a mini
    │   │   ├── tokens.rs  # //! Token helpers for the search pipelin
    │   │   └── vector_index.rs  # //! Test-support vector-store seam: the 
    │   └── tests/
    │       ├── error_hops.rs  # //! Error-hop tests (M7): every `rusqlit
    │       ├── file_status.rs  # //! P2 file-status tests: `replace_file`
    │       ├── fragments.rs  # use super::*;
    │       ├── fusion.rs  # //! M6.2 unit pins for `search_vector::f
    │       ├── graph_edges.rs  # use super::*;
    │       ├── hnsw_calibration.rs  # use std::collections::HashSet;
    │       ├── hnsw_dispatch.rs  # use super::*;
    │       ├── node_sync.rs  # //! Node-sync change-gate goldens: `sync
    │       ├── salience.rs  # use super::*;
    │       ├── schema.rs  # //! Schema snapshot golden: opening a `G
    │       └── vector_index.rs  # use super::*;
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
    │   │   ├── entity_gate.rs  # //! Two-gate promotion for entity-link c
    │   │   ├── error.rs  # //! Error taxonomy for the spaCy core.
    │   │   ├── frame.rs  # //! Frame extraction — spacy-rs as ...
    │   │   ├── hash.rs  # //! spaCy-compatible hashing: MurmurHash
    │   │   ├── interlingua.rs  # //! The pure hash→ID bridge.
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
    │   │   │   ├── genesis.rs  # //! Rule genesis for POS/NER.
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
    │   │   ├── review.rs  # //! The async review mechanism.
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
    │   │   ├── entity_gate.rs  # use std::sync::Arc;
    │   │   ├── entity_gate_cal.rs  # //! Calibration of `EntityStampGate.
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
    │   │   ├── sentence_roles.rs  # use super::*;
    │   │   ├── sentencizer.rs  # use super::*;
    │   │   ├── strings.rs  # use super::*;
    │   │   ├── tag_map.rs  # use super::*;
    │   │   ├── taxonomy_blob.rs  # use super::*;
    │   │   ├── tokenizer.rs  # use super::*;
    │   │   ├── tokenizer_snapshot.rs  # use super::*;
    │   │   ├── triple.rs  # use super::*;
    │   │   ├── validate.rs  # use super::*;
    │   │   ├── vocab.rs  # use super::*;
    │   │   └── yago_resolve.rs  # //! M5.1 pins for `YagoResolveStage`: na
    │   └── tools/
    │       ├── gen_en_exceptions.py  # #!/usr/bin/env python3
    │       ├── gen_en_lemma_data.py  # #!/usr/bin/env python3
    │       ├── gen_en_regexes.py  # #!/usr/bin/env python3
    │       ├── gen_en_tag_map.py  # #!/usr/bin/env python3
    │       └── gen_golden_corpus.py  # #!/usr/bin/env python3
    ├── types/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── file_kind.rs  # //! Canonical file-kind classification: 
    │   │   ├── instance_id.rs  # use internment::ArcIntern;
    │   │   ├── interlingua.rs  # //! The disambiguated interlingua: a por
    │   │   ├── knowledge.rs  # //! KnowledgeCapability — the cross...
    │   │   ├── label_enum.rs  # //! Closed-vocabulary label enums: one t
    │   │   ├── lib.rs  # //! fluent-types: Shared data types (Gui
    │   │   ├── model_key.rs  # //! Canonical routing model-key split: t
    │   │   ├── model_ref.rs  # //! Canonical model-reference parsing: t
    │   │   ├── openai_wire.rs  # //! OpenAI-compatible chat wire types...
    │   │   ├── provenance.rs  # //! Ledger annotation provenance — ...
    │   │   └── rounds.rs  # //! Bounded config newtypes — valid...
    │   └── tests/
    │       ├── file_kind.rs  # //! `file_kind` unit pins: canonical ext
    │       ├── label_enum.rs  # //! Macro unit tests for `fluent_types::
    │       ├── model_key.rs  # //! Characterization table for the three
    │       └── model_ref.rs  # //! `model_ref` unit pins: canonical `pr
    ├── vector-math/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── lib.rs  # //! Scalar embedding-vector math — ...
    │       └── tests/
    │           ├── leaf.rs  # //! Dependency-leaf invariant: this crat
    │           └── math.rs  # //! Contract tests for the scalar kernel
    ├── wasm_ipc/
    │   ├── Cargo.toml
    │   └── src/
    │       └── lib.rs  # //! WASM IPC — Binary schemas for E...
    └── xtask/
        ├── Cargo.toml
        └── src/
            ├── main.rs  # //! xtask for taxonomy operator pipeline
            └── main.rs.json
```
