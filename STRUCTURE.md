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
│   ├── MEMORY_PLUGIN.md  # # Memory Plugin Architecture —
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
│   │   ├── fluent-concurrency/
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
│       ├── fluent-concurrency/
│       │   └── SKILL.md  # # `fluent-concurrency` — Lightweight
│       ├── fluent-wvr/
│       │   └── SKILL.md  # # Fluent WVR in Rust — The Synthesis
│       ├── gof-patterns/
│       │   └── SKILL.md  # ---
│       ├── subagent/
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
└── src/
    ├── Cargo.lock
    ├── bin/
    │   ├── coral/
    │   │   ├── Cargo.toml
    │   │   └── src/
    │   │       └── main.rs  # use clap::{Parser, Subcommand};
    │   └── guidance/
    │       ├── Cargo.toml
    │       └── src/
    │           ├── main.rs  # use std::path::{Path, PathBuf};
    │           └── structure.rs  # use std::collections::BTreeMap;
    ├── common-core/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── constants.rs  # pub const MAX_VALUE_LEN: usize =
    │       ├── error.rs  # use thiserror::Error;
    │       ├── error_context.rs  # use std::fmt;
    │       ├── format.rs  # use std::fmt::Write as _;
    │       ├── hash.rs  # use blake3::Hasher;
    │       ├── io.rs  # use std::fs;
    │       ├── lib.rs  # //! common-core: Zero-domain generic
    │       ├── metrics.rs  # use std::sync::atomic::{AtomicU64,
    │       ├── shell.rs  # use std::process::Command;
    │       ├── shell_parser.rs  # use thiserror::Error;
    │       └── string.rs  # use std::collections::HashSet;
    ├── content-node/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── doc_node.rs  # use std::any::Any;
    │       ├── file_node.rs  # use std::any::Any;
    │       ├── lib.rs  # //! guidance-content-node:
    │       ├── lod.rs  # pub fn generate_lod_slices(full_text:
    │       ├── node.rs  # use guidance_types::LOD_COUNT;
    │       ├── source_node.rs  # use std::any::Any;
    │       └── wvr.rs  # //! Fluent WVR integration for
    ├── coral/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── cache_l1.rs  # use lru::LruCache;
    │       ├── cache_reactor.rs  # use std::sync::Arc;
    │       ├── cache_router.rs  # use std::sync::Arc;
    │       ├── db.rs  # use std::mem::size_of;
    │       ├── error.rs  # use thiserror::Error;
    │       ├── ingest.rs  # use std::sync::Arc;
    │       ├── lib.rs  # //! Coral: Context-graph library for
    │       ├── mcp.rs  # use std::io::{self, BufRead,
    │       ├── packer.rs  # use guidance_types::{ContextNode,
    │       ├── wasm_runtime.rs  # use std::path::Path;
    │       └── wvr.rs  # //! Fluent WVR integration for Coral
    ├── dag/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── adapter.rs  # use std::sync::Arc;
    │       ├── drift.rs  # use bitvec::prelude::*;
    │       ├── error.rs  # use thiserror::Error;
    │       ├── executor.rs  # use std::collections::HashMap;
    │       ├── interner.rs  # use bitvec::vec::BitVec;
    │       ├── lib.rs  # //! guidance-dag: DAG executor with
    │       ├── middleware.rs  # use std::sync::Arc;
    │       ├── resolver.rs  # use std::collections::HashMap;
    │       ├── target.rs  # use bitvec::vec::BitVec;
    │       ├── type_inference.rs  # use bitvec::prelude::*;
    │       ├── work_unit.rs  # use std::process::Command;
    │       └── wvr.rs  # //! Fluent WVR integration for DAG
    ├── fluent-concurrency/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── capability.rs  # //! Concrete capability tokens for
    │       ├── flow.rs  # //! Credit-based backpressure flow
    │       ├── io/
    │       │   ├── db.rs  # //! SQLite-backed database capability
    │       │   ├── fs.rs  # //! Capability-gated filesystem I/O
    │       │   ├── mod.rs  # //! Capability-gated I/O primitive
    │       │   └── net.rs  # //! Capability-gated network I/O (TCP
    │       ├── lib.rs  # #![forbid(unsafe_code)]
    │       ├── pool.rs  # //! Bounded async queue, worker pool,
    │       ├── queue.rs  # //! A priority queue with a fast path
    │       ├── router.rs  # //! A partitioned router that
    │       ├── runtime/
    │       │   ├── mod.rs  # //! Pluggable `Runtime` backends
    │       │   ├── test.rs  # //! Test `Runtime` implementation with
    │       │   └── tokio.rs  # //! Production `Runtime` implementation
    │       ├── scope.rs  # //! Structured concurrency via `Scope`
    │       └── zone.rs  # //! Supervision zone with async retry,
    ├── fluent-wvr/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── lib.rs  # //! ## Fluent WVR — Framework Trait
    │       └── wrapper.rs  # use std::sync::Arc;
    ├── fluent-wvr-macros/
    │   ├── Cargo.toml
    │   └── src/
    │       └── lib.rs  # use proc_macro::TokenStream;
    ├── guidance/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── ast_parser.rs  # use std::path::Path;
    │   │   ├── config.rs  # use std::collections::HashMap;
    │   │   ├── enhancer.rs  # use guidance_llm::client::{ChatMessage,
    │   │   ├── lib.rs  # //! Guidance: AST-guided vector search
    │   │   ├── plugin.rs  # use std::collections::HashMap;
    │   │   ├── query/
    │   │   │   ├── formatter.rs  # use std::fmt::Write;
    │   │   │   ├── identifier.rs  # use guidance_types::GuidanceDoc;
    │   │   │   ├── llm_filter.rs  # use guidance_types::GuidanceDoc;
    │   │   │   ├── llm_filter_batch.rs  # use
    │   │   │   ├── mod.rs  # pub mod formatter;
    │   │   │   ├── search_backend.rs  # use guidance_types::GuidanceDoc;
    │   │   │   ├── snapshot.rs  # use std::fs;
    │   │   │   ├── strategy.rs  # use guidance_types::GuidanceDoc;
    │   │   │   └── synthesize.rs  # use guidance_types::{GuidanceDoc,
    │   │   ├── query_engine.rs  # use std::path::Path;
    │   │   ├── runtime.rs  # use std::cell::RefCell;
    │   │   ├── scanner.rs  # use common_core::string::{contains_any,
    │   │   ├── sync/
    │   │   │   ├── comments.rs  # use std::path::Path;
    │   │   │   ├── json_store.rs  # use std::path::{Path, PathBuf};
    │   │   │   ├── json_writer.rs  # use guidance_types::{GuidanceDoc,
    │   │   │   ├── mod.rs  # pub mod comments;
    │   │   │   └── staleness.rs  # use std::path::Path;
    │   │   ├── sync_engine.rs  # use std::path::{Path, PathBuf};
    │   │   └── walk.rs  # use std::collections::HashSet;
    │   └── tests/
    │       └── e2e_gen_roundtrip.rs  # use
    ├── llm/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── anonymize.rs  # use std::sync::LazyLock;
    │       ├── client.rs  # use std::sync::{Arc, LazyLock};
    │       ├── constants.rs  # pub const MAX_EMBEDDING_DIMENSIONS:
    │       ├── context_packer.rs  # use crate::client::ChatMessage;
    │       ├── decomposer.rs  # use bon::Builder;
    │       ├── embeddings.rs  # use std::collections::HashMap;
    │       ├── error.rs  # use
    │       ├── lib.rs  # //! guidance-llm: LLM HTTP client
    │       ├── llm_queue.rs  # use std::sync::Arc;
    │       └── url.rs  # use thiserror::Error;
    ├── memory-plugin/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── capability.rs  # //! Capability token for explicit
    │       ├── lib.rs  # #![forbid(unsafe_code)]
    │       ├── plugins/
    │       │   ├── hindsight/
    │       │   │   └── mod.rs  # //! Hindsight memory plugin —
    │       │   ├── holographic/
    │       │   │   ├── hrr.rs  # //! Holographic Reduced Representations
    │       │   │   ├── mod.rs  # //! Holographic memory plugin — local
    │       │   │   └── store.rs  # //! SQLite-backed fact store with
    │       │   ├── honcho/
    │       │   │   └── mod.rs  # //! Honcho memory plugin —
    │       │   └── mod.rs  # //! Memory plugin implementations.
    │       ├── registry.rs  # //! Central memory plugin registry.
    │       ├── traits.rs  # //! Core trait definitions for the
    │       ├── types.rs  # //! Shared types for the memory plugin
    │       └── zone.rs  # //! Memory ingestion zone.
    ├── ontology/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── entity.rs  # use
    │       ├── inference.rs  # use std::collections::{HashMap,
    │       ├── lib.rs  # //! guidance-ontology: Entity
    │       ├── mapper.rs  # use std::collections::HashMap;
    │       ├── migration.rs  # #[derive(Debug, Clone)]
    │       └── yago.rs  # pub const NS_YAGO: &str =
    ├── project-knowledge/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── csr_graph.rs  # pub const CSR_MAGIC: u32 =
    │       ├── freq_table.rs  # use std::fs;
    │       ├── index_header.rs  # pub const INDEX_HEADER_SIZE: usize =
    │       ├── lib.rs  # //! guidance-project-knowledge:
    │       ├── query_cache.rs  # use common_core::hash::fnv1a64;
    │       ├── tokenizer.rs  # pub struct WordTokenizer<'a> {
    │       ├── trigram_index.rs  # use crate::index_header::Header;
    │       └── word_index.rs  # use
    ├── rdf/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── lexer.rs  # use crate::RdfError;
    │       ├── lib.rs  # //! guidance-rdf: RDF/Turtle/N-Quads
    │       ├── normalize.rs  # pub struct BlankNodeScope;
    │       ├── nquads.rs  # use crate::lexer::{Lexer,
    │       └── parser.rs  # use std::collections::{HashMap,
    ├── requirements.txt
    ├── search-vector/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── aliases.rs  # use
    │       ├── db.rs  # use std::path::Path;
    │       ├── error.rs  # use thiserror::Error;
    │       ├── lib.rs  # //! guidance-search-vector: SQLite
    │       └── math.rs  # pub fn cosine_similarity(a: &[f32], b:
    ├── types/
    │   ├── Cargo.toml
    │   └── src/
    │       └── lib.rs  # //! guidance-types: Shared data types
    └── wasm_ipc/
        ├── Cargo.toml
        └── src/
            └── lib.rs  # //! WASM IPC — Binary schemas for
```
