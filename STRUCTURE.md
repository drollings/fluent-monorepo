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
├── AGENTS.md  # # Agent Bootloader — guidance
├── Cargo.toml
├── LICENSE
├── LICENSE-Commercial-Requirement
├── LICENSE-Contributor-Agreement
├── Makefile
├── README.md  # # The fluent monorepo
├── STRUCTURE.md  # # AST-Guidance Project Structure
├── bin/
│   └── gen_simhash_projections.py  # #!/usr/bin/env python3
├── doc/
│   ├── MEMORY_PLUGIN.md  # # Memory Plugin Architecture — Clea...
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
│   │   ├── DETAILS.md  # # Coral Context: Detailed Engineering Sp
│   │   ├── OVERVIEW.md  # # Coral Context: Architectural Design Do
│   │   └── VISION.md  # # Coral Context: Architectural Vision
│   ├── guidance/
│   │   ├── DESIGN.md  # Comprehensive Analysis: Agentic Document
│   │   ├── MCP.md  # # guidance MCP Server
│   │   ├── VISION.md  # # guidance: Vision Document
│   │   └── schemas/
│   │       └── guidance.schema.json
│   └── skills/
│       ├── fluent-concurrency/
│       │   └── SKILL.md  # # `fluent-concurrency` — Lightweigh...
│       ├── fluent-wvr/
│       │   └── SKILL.md  # # Fluent WVR in Rust — The Synthesi...
│       ├── gof-patterns/
│       │   └── SKILL.md  # ---
│       ├── subagent/
│       │   └── SKILL.md  # ---
│       └── zig-to-rust/
│           └── SKILL.md  # # Zig to Rust Practices: Master Guidelin
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
    │           ├── commit.rs  # //! Commit message generation — LLM...
    │           ├── editor.rs  # //! Editor interaction utilities for hum
    │           ├── main.rs  # use std::path::{Path, PathBuf};
    │           ├── mcp.rs  # //! MCP (Model Context Protocol) server 
    │           └── structure.rs  # use std::collections::BTreeMap;
    ├── common-core/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── config.rs  # use std::path::Path;
    │       ├── constants.rs  # pub const MAX_VALUE_LEN: usize = 128;
    │       ├── drift.rs  # use bitvec::prelude::*;
    │       ├── error.rs  # use thiserror::Error;
    │       ├── error_context.rs  # use std::fmt;
    │       ├── format.rs  # use std::fmt::Write as _;
    │       ├── git.rs  # //! Git operations — thin wrappers ...
    │       ├── hash.rs  # use blake3::Hasher;
    │       ├── interner.rs  # use bitvec::vec::BitVec;
    │       ├── io.rs  # use std::fs;
    │       ├── jsonrpc.rs  # //! Shared JSON-RPC 2.
    │       ├── lib.rs  # //! common-core: Zero-domain generic uti
    │       ├── metrics.rs  # use std::sync::atomic::{AtomicU64, Order
    │       ├── shell.rs  # use std::process::{Command, Output};
    │       ├── shell_parser.rs  # use thiserror::Error;
    │       ├── sqlite.rs  # //! Shared SQLite helpers — connect...
    │       ├── string.rs  # use std::collections::HashSet;
    │       ├── tokens.rs  # pub const DEFAULT_CHARS_PER_TOKEN: usize
    │       └── walk.rs  # use std::collections::HashSet;
    ├── content-node/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── doc_node.rs  # use std::any::Any;
    │       ├── file_node.rs  # use std::any::Any;
    │       ├── lib.rs  # //! guidance-content-node: Level-of-deta
    │       ├── lod.rs  # pub fn generate_lod_slices(full_text: &s
    │       ├── node.rs  # use guidance_types::LOD_COUNT;
    │       ├── source_node.rs  # use std::any::Any;
    │       └── wvr.rs  # //! Fluent WVR integration for `guidance
    ├── coral/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── cache_l1.rs  # use lru::LruCache;
    │       ├── cache_reactor.rs  # use std::sync::Arc;
    │       ├── cache_router.rs  # use std::sync::Arc;
    │       ├── db.rs  # use std::collections::HashMap;
    │       ├── error.rs  # use thiserror::Error;
    │       ├── ingest.rs  # use std::sync::Arc;
    │       ├── lib.rs  # //! Coral: Context-graph library for gui
    │       ├── mcp.rs  # use std::path::Path;
    │       ├── packer.rs  # use common_core::tokens::DEFAULT_CHARS_P
    │       ├── test_stubs.rs  # //! Test stubs for coral cache reactor t
    │       ├── tier_units.rs  # use std::sync::{Arc, Weak};
    │       ├── wasm_runtime.rs  # use std::num::NonZeroUsize;
    │       └── wvr.rs  # //! Fluent WVR integration for Coral cra
    ├── dag/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── adapter.rs  # //! Re-export of `ComponentAdapter` and 
    │       ├── error.rs  # use thiserror::Error;
    │       ├── executor.rs  # use std::collections::HashMap;
    │       ├── lib.rs  # //! fluent-dag: DAG executor with resolv
    │       ├── middleware.rs  # use std::sync::Arc;
    │       ├── resolver.rs  # use std::collections::HashMap;
    │       ├── target.rs  # use bitvec::vec::BitVec;
    │       ├── type_inference.rs  # use bitvec::prelude::*;
    │       ├── work_unit.rs  # use bon::Builder;
    │       └── wvr.rs  # //! Fluent WVR integration for DAG crate
    ├── fluent-concurrency/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── capability.rs  # //! Concrete capability tokens for files
    │       ├── flow.rs  # //! Credit-based backpressure flow contr
    │       ├── io/
    │       │   ├── db.rs  # //! SQLite-backed database capability wi
    │       │   ├── fs.rs  # //! Capability-gated filesystem I/O (rea
    │       │   ├── mod.rs  # //! Capability-gated I/O primitive engin
    │       │   └── net.rs  # //! Capability-gated network I/O (TCP co
    │       ├── lib.rs  # #![forbid(unsafe_code)]
    │       ├── pool.rs  # //! Bounded async queue, worker pool, an
    │       ├── queue.rs  # //! A priority queue with a fast path fo
    │       ├── router.rs  # //! A partitioned router that distribute
    │       ├── runtime/
    │       │   ├── mod.rs  # //! Pluggable `Runtime` backends (produc
    │       │   ├── test.rs  # //! Test `Runtime` implementation with p
    │       │   └── tokio.rs  # //! Production `Runtime` implementation 
    │       ├── scope.rs  # //! Structured concurrency via `Scope...
    │       └── zone.rs  # //! Supervision zone with async retry, d
    ├── fluent-wvr/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── lib.rs  # //! ## Fluent WVR — Framework Trait...
    │       └── wrapper.rs  # use std::sync::Arc;
    ├── fluent-wvr-macros/
    │   ├── Cargo.toml
    │   └── src/
    │       └── lib.rs  # use proc_macro::TokenStream;
    ├── fluent-wvr-testutil/
    │   ├── Cargo.toml
    │   └── src/
    │       └── lib.rs  # //! Test utilities for Fluent WVR crates
    ├── guidance/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── ast_parser.rs  # use std::path::Path;
    │   │   ├── config.rs  # use std::collections::HashMap;
    │   │   ├── enhancer.rs  # use guidance_llm::client::{ChatMessage, 
    │   │   ├── grounding.rs  # //! Grounding enforcement — ensures...
    │   │   ├── lib.rs  # //! Guidance: AST-guided vector search &
    │   │   ├── memory.rs  # //! Memory integration for the guidance 
    │   │   ├── plugin.rs  # use std::collections::HashMap;
    │   │   ├── query/
    │   │   │   ├── formatter.rs  # use std::fmt::Write;
    │   │   │   ├── identifier.rs  # use common_core::string::contains_ignore
    │   │   │   ├── llm_filter.rs  # use common_core::string::contains_ignore
    │   │   │   ├── llm_filter_batch.rs  # use super::llm_filter::{LlmFilterBackend
    │   │   │   ├── mod.rs  # pub mod formatter;
    │   │   │   ├── search_backend.rs  # use common_core::string::contains_ignore
    │   │   │   ├── snapshot.rs  # use std::path::Path;
    │   │   │   ├── strategy.rs  # use guidance_types::GuidanceDoc;
    │   │   │   └── synthesize.rs  # use guidance_types::{GuidanceDoc, Member
    │   │   ├── query_engine.rs  # use std::path::Path;
    │   │   ├── runtime.rs  # use std::cell::RefCell;
    │   │   ├── scanner.rs  # use common_core::string::{contains_any, 
    │   │   ├── sync/
    │   │   │   ├── comments.rs  # use std::path::Path;
    │   │   │   ├── json_store.rs  # use std::path::{Path, PathBuf};
    │   │   │   ├── json_writer.rs  # use guidance_types::{GuidanceDoc, Member
    │   │   │   ├── mod.rs  # pub mod comments;
    │   │   │   └── staleness.rs  # use std::path::Path;
    │   │   └── sync_engine.rs  # use std::path::{Path, PathBuf};
    │   └── tests/
    │       └── e2e_gen_roundtrip.rs  # use fluent_wvr_testutil::tempdir;
    ├── llm/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── anonymize.rs  # use std::sync::LazyLock;
    │       ├── client.rs  # use std::sync::{Arc, LazyLock};
    │       ├── constants.rs  # //! Cross-crate limit moved to `common-c
    │       ├── context_packer.rs  # use crate::client::ChatMessage;
    │       ├── decomposer.rs  # use bon::Builder;
    │       ├── embeddings.rs  # use std::num::NonZeroUsize;
    │       ├── error.rs  # use crate::embeddings::EmbeddingError;
    │       ├── lib.rs  # //! guidance-llm: LLM HTTP client provid
    │       ├── llm_queue.rs  # use std::sync::Arc;
    │       └── url.rs  # use thiserror::Error;
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
    │       ├── types.rs  # //! Shared types for the memory plugin s
    │       └── zone.rs  # //! Memory ingestion zone.
    ├── ontology/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── entity.rs  # use std::collections::HashMap;
    │       ├── inference.rs  # use std::collections::{HashMap, HashSet}
    │       ├── lib.rs  # //! guidance-ontology: Entity extraction
    │       ├── mapper.rs  # use std::collections::HashMap;
    │       ├── migration.rs  # #[derive(Debug, Clone)]
    │       └── yago.rs  # pub const NS_YAGO: &str = "http://yago-k
    ├── project-knowledge/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── csr_graph.rs  # pub const CSR_MAGIC: u32 = 0x4752_5343;
    │       ├── freq_table.rs  # use std::fs;
    │       ├── index_header.rs  # pub const INDEX_HEADER_SIZE: usize = 10;
    │       ├── lib.rs  # //! guidance-project-knowledge: Word/tri
    │       ├── query_cache.rs  # use common_core::hash::fnv1a64;
    │       ├── tokenizer.rs  # pub struct WordTokenizer<'a> {
    │       ├── trigram_index.rs  # use crate::index_header::Header;
    │       └── word_index.rs  # use std::collections::HashMap;
    ├── rdf/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── lexer.rs  # use crate::RdfError;
    │       ├── lib.rs  # //! guidance-rdf: RDF/Turtle/N-Quads par
    │       ├── normalize.rs  # pub struct BlankNodeScope;
    │       ├── nquads.rs  # use crate::lexer::{Lexer, TokenKind};
    │       └── parser.rs  # use std::collections::{HashMap, VecDeque
    ├── requirements.txt
    ├── search-vector/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── aliases.rs  # use std::collections::HashMap;
    │       ├── db.rs  # use std::path::Path;
    │       ├── error.rs  # use thiserror::Error;
    │       ├── lib.rs  # //! guidance-search-vector: SQLite hybri
    │       └── math.rs  # pub fn cosine_similarity(a: &[f32], b: &
    ├── types/
    │   ├── Cargo.toml
    │   └── src/
    │       └── lib.rs  # //! guidance-types: Shared data types (G
    └── wasm_ipc/
        ├── Cargo.toml
        └── src/
            └── lib.rs  # //! WASM IPC — Binary schemas for E...
```
