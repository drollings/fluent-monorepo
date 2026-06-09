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
├── AGENTS.md  # # Agent Bootloader —
├── Cargo.toml
├── LICENSE
├── LICENSE-Commercial-Requirement
├── LICENSE-Contributor-Agreement
├── Makefile
├── README.md  # # guidance
├── STRUCTURE.md  # # AST-Guidance Project Structure
├── bin/
│   └── gen_simhash_projections.py  # #!/usr/bin/env python3
├── doc/
│   ├── SUBAGENT.md  # # REVIEW_20260418_LOCAL_SUBAGENT.
│   ├── capabilities/
│   │   ├── ast-indexing/
│   │   │   └── CAPABILITY.md  # ---
│   │   ├── config-system/
│   │   │   └── CAPABILITY.md  # ---
│   │   ├── coral-cache/
│   │   │   └── CAPABILITY.md  # ---
│   │   ├── coral-database/
│   │   │   └── CAPABILITY.md  # ---
│   │   ├── coral-ingestion/
│   │   │   └── CAPABILITY.md  # ---
│   │   ├── coral-mcp/
│   │   │   └── CAPABILITY.md  # ---
│   │   ├── embedding-providers/
│   │   │   └── CAPABILITY.md  # ---
│   │   ├── explain-query/
│   │   │   └── CAPABILITY.md  # ---
│   │   ├── llm-client/
│   │   │   └── CAPABILITY.md  # ---
│   │   ├── local-model-decomposition/
│   │   │   └── CAPABILITY.md  # ---
│   │   ├── ontology/
│   │   │   └── CAPABILITY.md  # ---
│   │   ├── plugin-system/
│   │   │   └── CAPABILITY.md  # ---
│   │   ├── rdf-parsing/
│   │   │   └── CAPABILITY.md  # ---
│   │   ├── reflection/
│   │   │   └── CAPABILITY.md  # ---
│   │   ├── sync-pipeline/
│   │   │   └── CAPABILITY.md  # ---
│   │   ├── target-registry/
│   │   │   └── CAPABILITY.md  # ---
│   │   ├── vector-search/
│   │   │   └── CAPABILITY.md  # ---
│   │   └── wasm-tools/
│   │       └── CAPABILITY.md  # ---
│   ├── coral/
│   │   ├── CHANGELOG.md  # # Changelog
│   │   ├── DETAILS.md  # # Coral Context: Detailed Engineering
│   │   ├── OVERVIEW.md  # # Coral Context: Architectural Design
│   │   └── VISION.md  # # Coral Context: Architectural
│   ├── guidance/
│   │   ├── DESIGN.md  # Comprehensive Analysis: Agentic
│   │   ├── MCP.md  # # guidance MCP Server
│   │   ├── VISION.md  # # guidance: Vision Document
│   │   └── schemas/
│   │       └── guidance.schema.json
│   └── skills/
│       ├── fluent-wvr/
│       │   └── SKILL.md  # # Fluent WVR in Rust — The Synthesis
│       ├── gof-patterns/
│       │   └── SKILL.md  # ---
│       ├── subagent/
│       │   └── SKILL.md  # ---
│       ├── zig-current/
│       │   └── SKILL.md  # ---
│       └── zig-to-rust/
│           └── SKILL.md  # # Zig to Rust Practices: Master
├── env/
│   └── mk/
│       ├── common.mk
│       ├── target_language.mk
│       └── targets/
│           ├── go.mk
│           ├── php.mk
│           ├── pine.mk
│           ├── py.mk
│           ├── rust.mk
│           └── zig.mk
├── src/
│   ├── Cargo.lock
│   ├── bin/
│   │   ├── coral/
│   │   │   ├── Cargo.toml
│   │   │   └── src/
│   │   │       └── main.rs  # use std::path::PathBuf;
│   │   └── guidance/
│   │       ├── Cargo.toml
│   │       └── src/
│   │           └── main.rs  # use std::path::{Path, PathBuf};
│   ├── common/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── constants.rs  # pub const MAX_VALUE_LEN: usize =
│   │       ├── csr_graph.rs  # pub const CSR_MAGIC: u32 =
│   │       ├── error.rs  # use thiserror::Error;
│   │       ├── error_context.rs  # use std::fmt;
│   │       ├── format.rs  # use std::fmt::Write as _;
│   │       ├── freq_table.rs  # use std::fs;
│   │       ├── hash.rs  # use blake3::Hasher;
│   │       ├── index_header.rs  # pub const INDEX_HEADER_SIZE: usize =
│   │       ├── io.rs  # pub const DEFAULT_MAX_FILE_SIZE: usize
│   │       ├── lib.rs  # #![deny(warnings, clippy::all,
│   │       ├── metrics.rs  # use std::sync::atomic::{AtomicU64,
│   │       ├── query_cache.rs  # use crate::hash::fnv1a64;
│   │       ├── shell.rs  # use std::process::Command;
│   │       ├── shell_parser.rs  # use thiserror::Error;
│   │       ├── string.rs  # use
│   │       ├── terminal.rs  # use std::io::{self, BufRead,
│   │       ├── tokenizer.rs  # pub struct WordTokenizer<'a> {
│   │       ├── trigram_index.rs  # use crate::index_header::Header;
│   │       └── word_index.rs  # use
│   ├── content-node/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── doc_node.rs  # use std::any::Any;
│   │       ├── file_node.rs  # use std::any::Any;
│   │       ├── lib.rs  # #![deny(warnings, clippy::all,
│   │       ├── lod.rs  # pub fn generate_lod_slices(full_text:
│   │       ├── node.rs  # use std::any::Any;
│   │       ├── source_node.rs  # use std::any::Any;
│   │       └── wvr.rs  # use crate::node::{ContentNode,
│   ├── coral/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── cache_l1.rs  # use dashmap::DashMap;
│   │       ├── cache_reactor.rs  # use std::sync::Arc;
│   │       ├── cache_router.rs  # use std::collections::HashSet;
│   │       ├── db.rs  # use std::mem::size_of;
│   │       ├── ingest.rs  # use std::sync::Arc;
│   │       ├── lib.rs  # //! Coral: Context-graph library for
│   │       ├── mcp.rs  # use std::io::{self, BufRead,
│   │       ├── packer.rs  # use guidance_types::{ContextNode,
│   │       └── wasm_runtime.rs  # use std::path::Path;
│   ├── dag/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── adapter.rs  # use std::sync::Arc;
│   │       ├── drift.rs  # use bitvec::prelude::*;
│   │       ├── error.rs  # use thiserror::Error;
│   │       ├── executor.rs  # use std::collections::HashMap;
│   │       ├── interner.rs  # use bitvec::vec::BitVec;
│   │       ├── lib.rs  # pub mod adapter;
│   │       ├── middleware.rs  # use std::sync::Arc;
│   │       ├── resolver.rs  # use std::collections::HashMap;
│   │       ├── target.rs  # use bitvec::vec::BitVec;
│   │       ├── type_inference.rs  # use bitvec::prelude::*;
│   │       └── work_unit.rs  # use std::process::Command;
│   ├── dag-executor/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── adapter.rs  # use std::sync::Arc;
│   │       ├── executor.rs  # use std::collections::HashMap;
│   │       ├── lib.rs  # #![allow(clippy::should_implement_trait,
│   │       ├── middleware.rs  # use std::sync::Arc;
│   │       ├── resolver.rs  # use std::collections::HashMap;
│   │       └── work_unit.rs  # use std::process::Command;
│   ├── fixtures/
│   │   └── sample-project/
│   │       ├── doc.md  # # Sample Markdown file for AST parsing
│   │       ├── main.py  # """Sample Python file for AST parsing
│   │       ├── main.rs  # # Sample Rust file for AST parsing
│   │       └── main.zig  # /// Sample Zig file for AST parsing
│   ├── guidance/
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── ast_parser.rs  # use std::path::Path;
│   │   │   ├── config.rs  # use std::path::{Path, PathBuf};
│   │   │   ├── enhancer.rs  # use guidance_types::GuidanceDoc;
│   │   │   ├── guidance_string.rs  # pub fn is_path_token(s: &str) -> bool
│   │   │   ├── lib.rs  # //! Guidance: AST-guided vector search
│   │   │   ├── plugin.rs  # use std::collections::HashMap;
│   │   │   ├── query/
│   │   │   │   ├── identifier.rs  # use guidance_types::GuidanceDoc;
│   │   │   │   ├── llm_filter.rs  # use guidance_types::GuidanceDoc;
│   │   │   │   ├── llm_filter_batch.rs  # use
│   │   │   │   ├── mod.rs  # pub mod identifier;
│   │   │   │   ├── snapshot.rs  # use std::fs;
│   │   │   │   ├── strategy.rs  # use guidance_types::GuidanceDoc;
│   │   │   │   └── synthesize.rs  # use guidance_types::{GuidanceDoc,
│   │   │   ├── query_engine.rs  # use std::path::Path;
│   │   │   ├── scanner.rs  # use
│   │   │   ├── sync/
│   │   │   │   ├── comments.rs  # use std::path::Path;
│   │   │   │   ├── file_lock.rs  # use fs2::FileExt;
│   │   │   │   ├── json_store.rs  # use std::path::{Path, PathBuf};
│   │   │   │   ├── json_writer.rs  # use guidance_types::{GuidanceDoc,
│   │   │   │   ├── mod.rs  # pub mod comments;
│   │   │   │   └── staleness.rs  # use std::path::Path;
│   │   │   ├── sync_engine.rs  # use std::path::{Path, PathBuf};
│   │   │   └── vector/
│   │   │       ├── mod.rs  # pub mod vector_db;
│   │   │       └── vector_db.rs  # use std::path::Path;
│   │   └── tests/
│   │       └── e2e_gen_roundtrip.rs  # use guidance_types::MemberType;
│   ├── llm/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── anonymize.rs  # use std::sync::LazyLock;
│   │       ├── client.rs  # use std::sync::Arc;
│   │       ├── constants.rs  # pub const MAX_EMBEDDING_DIMENSIONS:
│   │       ├── context_packer.rs  # use crate::client::ChatMessage;
│   │       ├── decomposer.rs  # use bon::Builder;
│   │       ├── embeddings.rs  # use std::collections::HashMap;
│   │       ├── error.rs  # use
│   │       ├── lib.rs  # pub mod anonymize;
│   │       └── url.rs  # use thiserror::Error;
│   ├── ontology/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── entity.rs  # use
│   │       ├── inference.rs  # use std::collections::{HashMap,
│   │       ├── lib.rs  # pub mod entity;
│   │       ├── mapper.rs  # use std::collections::HashMap;
│   │       ├── migration.rs  # #[derive(Debug, Clone)]
│   │       └── yago.rs  # pub const NS_YAGO: &str =
│   ├── rdf/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lexer.rs  # use crate::RdfError;
│   │       ├── lib.rs  # pub mod lexer;
│   │       ├── normalize.rs  # pub struct BlankNodeScope;
│   │       ├── nquads.rs  # use crate::lexer::{Lexer,
│   │       └── parser.rs  # use std::collections::{HashMap,
│   ├── registry/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── interner.rs  # use bitvec::vec::BitVec;
│   │       └── lib.rs  # use bitvec::vec::BitVec;
│   ├── requirements.txt
│   ├── traits/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs  # pub mod wrapper;
│   │       └── wrapper.rs  # use std::time::Duration;
│   ├── types/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── lib.rs  # use serde::{Deserialize,
│   ├── vector-aliases/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── lib.rs  # use
│   ├── vector-math/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── lib.rs  # pub fn cosine_similarity(a: &[f32], b:
│   └── wasm_ipc/
│       ├── Cargo.toml
│       └── src/
│           └── lib.rs  # //! WASM IPC — Binary schemas for
├── zig/
│   ├── build.zig  # const std = @import("std");
│   ├── build.zig.zon
│   ├── libc.conf
│   ├── mise.toml
│   ├── src/
│   │   ├── common/
│   │   │   ├── args.zig  # const std = @import("std");
│   │   │   ├── builder_error.zig  # //! builder_error.zig — Structured
│   │   │   ├── builder_error_tests.zig  # //! Tests for builder_error.zig.
│   │   │   ├── cli.zig  # const std = @import("std");
│   │   │   ├── constants.zig  # /// constants.zig — Shared
│   │   │   ├── content_node.zig  # /// content_node.zig — ContentNode:
│   │   │   ├── csr_graph.zig  # //! csr_graph.zig — Compressed Sparse
│   │   │   ├── doc_registry.zig  # /// doc_registry.zig — Shared path
│   │   │   ├── drift.zig  # //! drift.zig — BitSet DRIFT:
│   │   │   ├── embeddings.zig  # //! Embedding providers — convert
│   │   │   ├── embeddings_tests.zig  # //! Tests for embeddings.zig.
│   │   │   ├── entity.zig  # const std = @import("std");
│   │   │   ├── error_context.zig  # /// error_context.zig — Structured
│   │   │   ├── file_lock.zig  # const std = @import("std");
│   │   │   ├── format.zig  # const std = @import("std");
│   │   │   ├── freq_table.zig  # const std = @import("std");
│   │   │   ├── frozen_snapshot.zig  # /// frozen_snapshot.zig — Frozen
│   │   │   ├── hash.zig  # /// hash.zig — Generic cryptographic
│   │   │   ├── hash_tests.zig  # //! Tests for hash.zig.
│   │   │   ├── index_header.zig  # /// index_header.zig — Binary file
│   │   │   ├── interner.zig  # /// interner.zig — String interning
│   │   │   ├── io.zig  # /// io.zig — Shared buffered I/O
│   │   │   ├── io_tests.zig  # //! Tests for io.zig.
│   │   │   ├── json.zig  # /// json.zig — Generic JSON
│   │   │   ├── json_tests.zig  # //! Tests for json.zig.
│   │   │   ├── log.zig  # //! Global logger with console + file
│   │   │   ├── metrics.zig  # /// metrics.zig — Generic latency
│   │   │   ├── pattern.zig  # /// pattern.zig — Design pattern
│   │   │   ├── pattern_tests.zig  # //! Tests for pattern.zig.
│   │   │   ├── query_cache.zig  # const std = @import("std");
│   │   │   ├── refcount.zig  # //! refcount.zig — Reference-counted
│   │   │   ├── root.zig  # //! common — Module umbrella root.
│   │   │   ├── shell.zig  # /// shell.zig — Shared shell command
│   │   │   ├── shell_parser.zig  # /// shell_parser.zig — Safe
│   │   │   ├── shell_parser_tests.zig  # //! Tests for shell_parser.zig.
│   │   │   ├── shell_tests.zig  # //! Tests for shell.zig.
│   │   │   ├── source.zig  # /// source.zig — Source code excerpt
│   │   │   ├── source_tests.zig  # //! Tests for source.zig.
│   │   │   ├── string.zig  # /// string.zig — Generic string
│   │   │   ├── string_tests.zig  # //! Tests for string.zig.
│   │   │   ├── terminal.zig  # const std = @import("std");
│   │   │   ├── tokenizer.zig  # const std = @import("std");
│   │   │   ├── trigram_index.zig  # const std = @import("std");
│   │   │   ├── type_inference.zig  # /// type_inference.zig — Type
│   │   │   ├── types.zig  # /// Number of LOD (Level of Detail)
│   │   │   ├── url.zig  # /// url.zig — Generic URL validation
│   │   │   ├── url_tests.zig  # //! Tests for url.zig.
│   │   │   ├── vaxis_stub/
│   │   │   │   └── root.zig  # pub const Window = struct {};
│   │   │   ├── word_index.zig  # const std = @import("std");
│   │   │   ├── wrapper.zig  # //! wrapper.zig — Conditional and
│   │   │   └── wrapper_tests.zig  # //! Tests for wrapper.zig.
│   │   ├── concurrency/
│   │   │   ├── backend.zig  # //! backend.zig — ExecutionBackend
│   │   │   ├── root.zig  # //! concurrency — lightweight
│   │   │   ├── work_unit.zig  # //! work_unit.zig — Type-erased work
│   │   │   └── work_unit_tests.zig  # //! Tests for work_unit.zig.
│   │   ├── coral/
│   │   │   ├── agent_loop.zig  # /// agent_loop.zig — Agent-Loop
│   │   │   ├── algorithm_runner.zig  # /// algorithm_runner.zig — Algorithm
│   │   │   ├── algorithms/
│   │   │   │   ├── degree_centrality.zig  # //! degree_centrality.
│   │   │   │   ├── edge_weights.zig  # //! edge_weights.zig — Co-occurrence
│   │   │   │   ├── louvain.zig  # //! louvain.zig — Louvain community
│   │   │   │   ├── pagerank.zig  # //! pagerank.zig — PageRank via power
│   │   │   │   ├── shortest_path.zig  # //! shortest_path.zig — Dijkstra's
│   │   │   │   └── union_find.zig  # //! union_find.zig — Union-Find with
│   │   │   ├── batch.zig  # /// batch.zig — Streaming Batch
│   │   │   ├── benchmark.zig  # /// benchmark.zig — G5 Performance
│   │   │   ├── cache.zig  # //! cache.zig — 5-Tier Cache
│   │   │   ├── cache_l1.zig  # //! cache_l1.zig — L1/L1Hash Cache
│   │   │   ├── cache_reactor.zig  # //! cache_reactor.zig —
│   │   │   ├── cache_router.zig  # //! cache_router.zig — ParallelRouter
│   │   │   ├── cache_test.zig  # /// cache_test.zig — Integration
│   │   │   ├── cli.zig  # /// cli.zig — Ingestion CLI Command
│   │   │   ├── config.zig  # /// Coral project configuration loader.
│   │   │   ├── context_node_schema.zig  # const std = @import("std");
│   │   │   ├── db.zig  # /// db.zig — Coral Context Database
│   │   │   ├── delegation.zig  # /// delegation.zig — Delegation
│   │   │   ├── executor.zig  # /// executor.zig — DAG Executor for
│   │   │   ├── frontier.zig  # /// frontier.zig — M6: L5 Frontier
│   │   │   ├── frontier_tool_compiler.zig  # /// frontier_tool_compiler.
│   │   │   ├── global_search.zig  # /// global_search.zig — GlobalSearch
│   │   │   ├── http_transport.zig  # /// http_transport.zig — M4.1/M4.
│   │   │   ├── http_transport_test.zig  # /// http_transport_test.
│   │   │   ├── main.zig  # const std = @import("std");
│   │   │   ├── main_tests.zig  # //! Tests for main.zig.
│   │   │   ├── mcp.zig  # /// mcp.zig — Coral MCP (Model
│   │   │   ├── metrics.zig  # /// metrics.zig — Coral Latency
│   │   │   ├── root.zig  # //! coral/root.zig — Public API
│   │   │   ├── schema.zig  # /// schema.zig — Coral Context SQLite
│   │   │   ├── session.zig  # /// session.zig — Coral Session
│   │   │   ├── targets.zig  # /// targets.zig — Ingestion DAG
│   │   │   ├── token_budget.zig  # /// token_budget.zig — Token
│   │   │   ├── tool_registry.zig  # /// tool_registry.zig — Tool Registry
│   │   │   ├── verify.zig  # /// verify.zig — Ingestion
│   │   │   └── yago_ingest.zig  # /// yago_ingest.zig — YAGO 4.
│   │   ├── dag/
│   │   │   ├── context.zig  # const std = @import("std");
│   │   │   ├── dag_executor.zig  # /// dag_executor.zig — M6.
│   │   │   ├── json_parser.zig  # const std = @import("std");
│   │   │   ├── registry.zig  # const std = @import("std");
│   │   │   ├── repl.zig  # const std = @import("std");
│   │   │   ├── resolver.zig  # const std = @import("std");
│   │   │   ├── root.zig  # //! dag — DAG execution engine for
│   │   │   ├── target.zig  # const std = @import("std");
│   │   │   └── target_state.zig  # //! target_state.zig — Execution-only
│   │   ├── guidance/
│   │   │   ├── agents_md.zig  # //! AGENTS.md content generator for
│   │   │   ├── ast_parser.zig  # //! AST parser for Zig source files —
│   │   │   ├── comments/
│   │   │   │   ├── core.zig  # //! comments/core.zig — Merged doc
│   │   │   │   ├── core_tests.zig  # //! Tests for core.zig.
│   │   │   │   ├── header.zig  # //! header_generator.zig — File
│   │   │   │   ├── header_tests.zig  # //! Tests for header.zig.
│   │   │   │   ├── inserter.zig  # //! comment_inserter.zig — Insert and
│   │   │   │   ├── inserter_tests.zig  # //! Tests for inserter.zig.
│   │   │   │   ├── sync.zig  # //! comment_sync.zig —
│   │   │   │   └── sync_tests.zig  # //! Tests for sync.zig.
│   │   │   ├── config.zig  # //! guidance project configuration
│   │   │   ├── core/
│   │   │   │   ├── drift.zig  # //! core/drift.zig — Drift follow-up
│   │   │   │   ├── excerpt.zig  # //! core/excerpt.zig — Unified source
│   │   │   │   ├── format.zig  # //! core/format.zig — Unified
│   │   │   │   ├── intent.zig  # //! core/intent.zig — Deterministic
│   │   │   │   ├── metadata.zig  # //! core/metadata.zig — Unified
│   │   │   │   ├── ranking.zig  # //! core/ranking.zig — Unified result
│   │   │   │   └── skill_loader.zig  # //! core/skill_loader.
│   │   │   ├── doc_parser.zig  # //! doc_parser.zig — Unified parser
│   │   │   ├── doc_parser_tests.zig  # //! Tests for doc_parser.zig.
│   │   │   ├── document_indexer.zig  # //! document_indexer.zig — Document
│   │   │   ├── document_indexer_tests.zig  # //! Tests for document_indexer.zig.
│   │   │   ├── enhancer.zig  # //! AI Docstring Enhancer for Zig
│   │   │   ├── enhancer_tests.zig  # //! Tests for enhancer.zig.
│   │   │   ├── git.zig  # //! Gitignore-aware file filtering for
│   │   │   ├── git_tests.zig  # //! Tests for git.zig.
│   │   │   ├── hash.zig  # //! Hash utilities for guidance —
│   │   │   ├── hash_tests.zig  # //! Tests for hash.zig.
│   │   │   ├── health/
│   │   │   │   ├── build_validation.zig  # //! build_validation.zig — Phase 1.
│   │   │   │   ├── build_validation_tests.zig  # //! Tests for build_validation.zig.
│   │   │   │   ├── extractor.zig  # //! call_extractor.zig — AST-based
│   │   │   │   ├── extractor_tests.zig  # //! Tests for extractor.zig.
│   │   │   │   ├── health.zig  # //! codehealth — detect unused
│   │   │   │   ├── health_tests.zig  # //! Tests for main.zig.
│   │   │   │   ├── orphan.zig  # //! orphan.zig — Phase 0: Orphaned
│   │   │   │   ├── orphan_tests.zig  # //! Tests for orphan.zig.
│   │   │   │   ├── test_audit.zig  # //! test_audit.zig — Phase 2: Test
│   │   │   │   ├── test_audit_tests.zig  # //! Tests for test_audit.zig.
│   │   │   │   ├── test_mover.zig  # //! test_mover.zig — Move inline
│   │   │   │   └── test_mover_tests.zig  # //! Tests for test_mover.zig.
│   │   │   ├── main.zig  # //! guidance — AST-guided SQLite
│   │   │   ├── mcp.zig  # //! mcp.zig — guidance MCP server
│   │   │   ├── pattern.zig  # //! Pattern detection for Zig AST nodes
│   │   │   ├── plugin.zig  # //! LanguagePlugin — interface for
│   │   │   ├── plugin_registry.zig  # //! PluginRegistry — maps file
│   │   │   ├── plugin_registry_tests.zig  # //! Tests for plugin_registry.zig.
│   │   │   ├── plugin_tests.zig  # //! Tests for plugin.zig.
│   │   │   ├── plugins/
│   │   │   │   ├── markdown_plugin.zig  # //! MarkdownPlugin — extracts
│   │   │   │   ├── markdown_plugin_tests.zig  # //! Tests for markdown_plugin.zig.
│   │   │   │   ├── treesitter_extractor.zig  # //! TreeSitterExtractor — walks
│   │   │   │   ├── treesitter_extractor_tests.zig  # //! Tests for treesitter_extractor.zig.
│   │   │   │   ├── treesitter_loader.zig  # //! TreeSitterLoader — loads and
│   │   │   │   ├── treesitter_loader_tests.zig  # //! Tests for treesitter_loader.zig.
│   │   │   │   ├── treesitter_plugin.zig  # //! TreeSitterPlugin — universal AST
│   │   │   │   ├── zig_plugin.zig  # //! ZigPlugin — wraps ast_parser.
│   │   │   │   └── zig_plugin_tests.zig  # //! Tests for zig_plugin.zig.
│   │   │   ├── provider_discovery.zig  # //! External language provider
│   │   │   ├── provider_discovery_tests.zig  # //! Tests for provider_discovery.zig.
│   │   │   ├── query/
│   │   │   │   ├── args.zig  # //! query/args.zig — Argument parsing
│   │   │   │   ├── identifier.zig  # //! identifier_match.zig — Identifier
│   │   │   │   ├── llm_filter.zig  # //! llm_filter.zig — LLM-based
│   │   │   │   ├── llm_filter_batch.zig  # //! llm_filter_batch.zig — Batch LLM
│   │   │   │   ├── strategy.zig  # //! query_strategy.zig — Query
│   │   │   │   ├── strategy_tests.zig  # //! Tests for strategy.zig.
│   │   │   │   └── synthesize.zig  # //! synthesize.zig — LLM-based
│   │   │   ├── query_engine.zig  # //! query_engine.zig — explain,
│   │   │   ├── schema_validator.zig  # //! schema_validator.zig —
│   │   │   ├── skeleton.zig  # //! skeleton.zig — File and struct
│   │   │   ├── stage_builder.zig  # //! stage_builder.zig — Stage builder
│   │   │   ├── stage_builder_tests.zig  # //! Tests for stage_builder.zig.
│   │   │   ├── staged.zig  # //! staged.zig — Staged explain
│   │   │   ├── staged_tests.zig  # //! Tests for staged.zig.
│   │   │   ├── structure.zig  # //! STRUCTURE.md generator.
│   │   │   ├── subdirectory_tests.zig  # //! Shim root for subdirectory test
│   │   │   ├── sync/
│   │   │   │   ├── capability_eval.zig  # //! capability_eval.zig — Per-file
│   │   │   │   ├── capability_eval_tests.zig  # //! Tests for capability_eval.
│   │   │   │   ├── commit.zig  # //! sync/commit.zig — Git commit
│   │   │   │   ├── dep_graph.zig  # //! Forward+reverse @import dependency
│   │   │   │   ├── dep_graph_tests.zig  # //! Tests for dep_graph.zig.
│   │   │   │   ├── fast_snapshot.zig  # //! Binary snapshot for warm-startup
│   │   │   │   ├── fast_snapshot_tests.zig  # //! Tests for fast_snapshot.
│   │   │   │   ├── gen_files.zig  # //! sync/gen_files.zig — Gen command,
│   │   │   │   ├── json_store.zig  # //! JSON store for guidance sync —
│   │   │   │   ├── json_writer.zig  # //! sync/json_writer.zig — JSON
│   │   │   │   ├── line_verify.zig  # //! line_verify.zig —
│   │   │   │   ├── line_verify_tests.zig  # //! Tests for line_verify.zig.
│   │   │   │   ├── marker.zig  # //! Mtime-based change detection for
│   │   │   │   └── marker_tests.zig  # //! Tests for marker.zig.
│   │   │   ├── sync.zig  # //! Sync engine for guidance —
│   │   │   ├── sync_engine.zig  # //! sync_engine.zig — init, commit,
│   │   │   ├── tests.zig  # //! Unit tests for src/guidance —
│   │   │   ├── todo.zig  # //! todo.zig — Work item lifecycle
│   │   │   ├── todo_tests.zig  # //! Tests for todo.zig.
│   │   │   ├── triage.zig  # //! Triage subcommand: generate TRIAGE.
│   │   │   ├── triage_tests.zig  # //! Tests for triage.zig.
│   │   │   ├── types.zig  # //! Shared types for guidance —
│   │   │   └── types_tests.zig  # //! Tests for types.zig.
│   │   ├── llm/
│   │   │   ├── anonymize.zig  # /// anonymize.zig — PII anonymization
│   │   │   ├── context_compressor.zig  # /// context_compressor.
│   │   │   ├── context_packer.zig  # /// context_packer.zig — Context
│   │   │   ├── llm.zig  # //! llm.zig — LLM client, response
│   │   │   ├── root.zig  # //! llm — General-purpose LLM
│   │   │   ├── root_tests.zig  # //! Tests for root.zig.
│   │   │   ├── token_budget.zig  # /// token_budget.zig — Token
│   │   │   └── token_budget_tests.zig  # //! Tests for token_budget.zig.
│   │   ├── ontology/
│   │   │   ├── inference.zig  # /// inference.zig — Ontology
│   │   │   ├── mapper.zig  # /// mapper.zig — Triple →
│   │   │   ├── migration.zig  # /// migration.zig — Ontology
│   │   │   ├── root.zig  # /// ontology/root.zig — Ontology
│   │   │   └── yago.zig  # /// yago.zig — YAGO 4.
│   │   ├── rdf/
│   │   │   ├── lexer.zig  # /// lexer.zig — Streaming Turtle
│   │   │   ├── lexer_tests.zig  # //! Tests for lexer.zig.
│   │   │   ├── normalize.zig  # /// normalize.zig — RDF Term
│   │   │   ├── nquads.zig  # /// nquads.zig — N-Quads / N-Triples
│   │   │   ├── parser.zig  # /// parser.zig — Streaming
│   │   │   └── root.zig  # /// rdf/root.zig — RDF parsing module
│   │   ├── reflection/
│   │   │   ├── accessor.zig  # /// accessor.zig — Accessor,
│   │   │   ├── binary.zig  # /// binary.zig — BinaryFieldCodec for
│   │   │   ├── constraint.zig  # /// constraint.zig —
│   │   │   ├── enum_registry.zig  # /// enum_registry.zig — EnumRegistry
│   │   │   ├── permissions.zig  # /// permissions.zig — Role-based
│   │   │   ├── root.zig  # /// reflection — Coral Context
│   │   │   ├── schema_version.zig  # //! schema_version.zig — Versioning
│   │   │   ├── schema_version_tests.zig  # //! Tests for schema_version.zig.
│   │   │   ├── sql.zig  # /// sql.zig — Schema-driven SQLite
│   │   │   ├── sql_tests.zig  # //! Tests for sql.zig.
│   │   │   ├── typed.zig  # /// typed.zig — TypedAccessorTable(T)
│   │   │   ├── validate.zig  # //! validate.zig — Runtime validation
│   │   │   └── validate_tests.zig  # //! Tests for validate.zig.
│   │   ├── subagent/
│   │   │   ├── builder.zig  # //! builder.zig — Fluent builder for
│   │   │   ├── classify.zig  # //! classify.zig — Deterministic
│   │   │   ├── execute.zig  # //! execute.zig — Tool dispatch via
│   │   │   ├── fsm.zig  # //! fsm.zig — Main FSM loop for the
│   │   │   ├── grammar.zig  # //! grammar.zig — GBNF grammar
│   │   │   ├── guardrails.zig  # //! guardrails.zig — Loop detection,
│   │   │   ├── reflect.zig  # //! reflect.zig — Scratchpad
│   │   │   ├── root.zig  # //! root.zig — Public re-exports for
│   │   │   ├── route.zig  # //! route.zig — Deterministic
│   │   │   ├── synthesize.zig  # //! synthesize.zig — Context-isolated
│   │   │   ├── todo.zig  # //! todo.zig — Work item lifecycle
│   │   │   ├── todo_tests.zig  # //! Tests for todo.zig.
│   │   │   ├── types.zig  # //! types.zig — Core type definitions
│   │   │   └── validate.zig  # //! validate.zig — Schema + path
│   │   ├── testing/
│   │   │   ├── mock_vtable.zig  # //! mock_vtable.zig — Mock
│   │   │   └── mock_vtable_tests.zig  # //! Tests for mock_vtable.zig.
│   │   ├── vector/
│   │   │   ├── hnsw.zig  # /// hnsw.zig — M5.1 HNSW
│   │   │   ├── math.zig  # //! Vector operations — cosine
│   │   │   ├── math_tests.zig  # //! Tests for math.zig.
│   │   │   ├── quantized_embedding.zig  # //! quantized_embedding.
│   │   │   ├── root.zig  # //! guidance vector module — cosine
│   │   │   ├── simhash.zig  # /// simhash.zig — Locality-sensitive
│   │   │   ├── simhash_projections.zig  # /// simhash_projections.
│   │   │   ├── simhash_tests.zig  # //! Tests for simhash.zig.
│   │   │   ├── vector_db.zig  # //! guidance SQLite vector search
│   │   │   └── vector_db_tests.zig  # //! Tests for vector_db.zig.
│   │   └── wasm/
│   │       ├── execution_request.zig  # /// execution_request.zig — M1.
│   │       ├── root.zig  # //! wasm — WebAssembly Sandboxing
│   │       └── wasm.zig  # /// wasm.zig — Milestone 4:
│   └── vendor/
│       └── sqlite3/
│           ├── sqlite3.c  # /***************************************
│           ├── sqlite3.h  # /*
│           └── sqlite3ext.h  # /*
└── zig-doc/
    └── capabilities/
        ├── INDEX.md  # # guidance — AST-guided Vector
        ├── ast-indexing/
        │   └── CAPABILITY.md  # ---
        ├── config-system/
        │   └── CAPABILITY.md  # ---
        ├── coral-cache/
        │   └── CAPABILITY.md  # ---
        ├── coral-database/
        │   └── CAPABILITY.md  # ---
        ├── coral-ingestion/
        │   └── CAPABILITY.md  # ---
        ├── coral-mcp/
        │   └── CAPABILITY.md  # ---
        ├── embedding-providers/
        │   └── CAPABILITY.md  # ---
        ├── explain-query/
        │   └── CAPABILITY.md  # ---
        ├── llm-client/
        │   └── CAPABILITY.md  # ---
        ├── local-model-decomposition/
        │   └── CAPABILITY.md  # ---
        ├── ontology/
        │   └── CAPABILITY.md  # ---
        ├── plugin-system/
        │   └── CAPABILITY.md  # ---
        ├── rdf-parsing/
        │   └── CAPABILITY.md  # ---
        ├── reflection/
        │   └── CAPABILITY.md  # ---
        ├── sync-pipeline/
        │   └── CAPABILITY.md  # ---
        ├── target-registry/
        │   └── CAPABILITY.md  # ---
        ├── vector-search/
        │   └── CAPABILITY.md  # ---
        └── wasm-tools/
            └── CAPABILITY.md  # ---
```
