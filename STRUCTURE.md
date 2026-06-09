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
└── src/
    ├── Cargo.lock
    ├── bin/
    │   ├── coral/
    │   │   ├── Cargo.toml
    │   │   └── src/
    │   │       └── main.rs  # use std::path::PathBuf;
    │   └── guidance/
    │       ├── Cargo.toml
    │       └── src/
    │           ├── main.rs  # use std::path::{Path, PathBuf};
    │           └── structure.rs  # use std::collections::BTreeMap;
    ├── common/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── constants.rs  # pub const MAX_VALUE_LEN: usize =
    │       ├── csr_graph.rs  # pub const CSR_MAGIC: u32 =
    │       ├── error.rs  # use thiserror::Error;
    │       ├── error_context.rs  # use std::fmt;
    │       ├── format.rs  # use std::fmt::Write as _;
    │       ├── freq_table.rs  # use std::fs;
    │       ├── hash.rs  # use blake3::Hasher;
    │       ├── index_header.rs  # pub const INDEX_HEADER_SIZE: usize =
    │       ├── io.rs  # pub const DEFAULT_MAX_FILE_SIZE: usize
    │       ├── lib.rs  # #![deny(warnings, clippy::all,
    │       ├── metrics.rs  # use std::sync::atomic::{AtomicU64,
    │       ├── query_cache.rs  # use crate::hash::fnv1a64;
    │       ├── shell.rs  # use std::process::Command;
    │       ├── shell_parser.rs  # use thiserror::Error;
    │       ├── string.rs  # use
    │       ├── terminal.rs  # use std::io::{self, BufRead,
    │       ├── tokenizer.rs  # pub struct WordTokenizer<'a> {
    │       ├── trigram_index.rs  # use crate::index_header::Header;
    │       └── word_index.rs  # use
    ├── concurrency-queue/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── config.rs  # #[derive(Debug, Clone)]
    │       ├── error.rs  # use thiserror::Error;
    │       ├── event_queue.rs  # use std::sync::Arc;
    │       └── lib.rs  # pub mod config;
    ├── content-node/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── doc_node.rs  # use std::any::Any;
    │       ├── file_node.rs  # use std::any::Any;
    │       ├── lib.rs  # #![deny(warnings, clippy::all,
    │       ├── lod.rs  # pub fn generate_lod_slices(full_text:
    │       ├── node.rs  # use std::any::Any;
    │       ├── source_node.rs  # use std::any::Any;
    │       └── wvr.rs  # use crate::node::{ContentNode,
    ├── coral/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── cache_l1.rs  # use lru::LruCache;
    │       ├── cache_reactor.rs  # use std::sync::Arc;
    │       ├── cache_router.rs  # use std::collections::HashSet;
    │       ├── db.rs  # use std::mem::size_of;
    │       ├── ingest.rs  # use std::sync::Arc;
    │       ├── lib.rs  # //! Coral: Context-graph library for
    │       ├── mcp.rs  # use std::io::{self, BufRead,
    │       ├── packer.rs  # use guidance_types::{ContextNode,
    │       └── wasm_runtime.rs  # use std::path::Path;
    ├── dag/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── adapter.rs  # use std::sync::Arc;
    │       ├── drift.rs  # use bitvec::prelude::*;
    │       ├── error.rs  # use thiserror::Error;
    │       ├── executor.rs  # use std::collections::HashMap;
    │       ├── interner.rs  # use bitvec::vec::BitVec;
    │       ├── lib.rs  # pub mod adapter;
    │       ├── middleware.rs  # use std::sync::Arc;
    │       ├── resolver.rs  # use std::collections::HashMap;
    │       ├── target.rs  # use bitvec::vec::BitVec;
    │       ├── type_inference.rs  # use bitvec::prelude::*;
    │       └── work_unit.rs  # use std::process::Command;
    ├── dag-executor/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── adapter.rs
    │       ├── executor.rs
    │       ├── lib.rs
    │       ├── middleware.rs
    │       ├── resolver.rs
    │       └── work_unit.rs
    ├── fixtures/
    │   └── sample-project/
    │       ├── doc.md  # # Sample Markdown file for AST parsing
    │       ├── main.py  # """Sample Python file for AST parsing
    │       ├── main.rs  # # Sample Rust file for AST parsing
    │       └── main.zig  # /// Sample Zig file for AST parsing
    ├── guidance/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── ast_parser.rs  # use std::path::Path;
    │   │   ├── config.rs  # use std::path::{Path, PathBuf};
    │   │   ├── enhancer.rs  # use guidance_types::GuidanceDoc;
    │   │   ├── guidance_string.rs
    │   │   ├── lib.rs  # //! Guidance: AST-guided vector search
    │   │   ├── plugin.rs  # use std::collections::HashMap;
    │   │   ├── query/
    │   │   │   ├── identifier.rs  # use guidance_types::GuidanceDoc;
    │   │   │   ├── llm_filter.rs  # use guidance_types::GuidanceDoc;
    │   │   │   ├── llm_filter_batch.rs  # use
    │   │   │   ├── mod.rs  # pub mod identifier;
    │   │   │   ├── snapshot.rs  # use std::fs;
    │   │   │   ├── strategy.rs  # use guidance_types::GuidanceDoc;
    │   │   │   └── synthesize.rs  # use guidance_types::{GuidanceDoc,
    │   │   ├── query_engine.rs  # use std::path::Path;
    │   │   ├── scanner.rs  # use
    │   │   ├── sync/
    │   │   │   ├── comments.rs  # use std::path::Path;
    │   │   │   ├── file_lock.rs  # use fs2::FileExt;
    │   │   │   ├── json_store.rs  # use std::path::{Path, PathBuf};
    │   │   │   ├── json_writer.rs  # use guidance_types::{GuidanceDoc,
    │   │   │   ├── mod.rs  # pub mod comments;
    │   │   │   └── staleness.rs  # use std::path::Path;
    │   │   ├── sync_engine.rs  # use std::path::{Path, PathBuf};
    │   │   └── vector/
    │   │       ├── mod.rs  # pub mod vector_db;
    │   │       └── vector_db.rs  # use std::path::Path;
    │   └── tests/
    │       └── e2e_gen_roundtrip.rs  # use guidance_types::MemberType;
    ├── llm/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── anonymize.rs  # use std::sync::LazyLock;
    │       ├── client.rs  # use std::sync::Arc;
    │       ├── constants.rs  # pub const MAX_EMBEDDING_DIMENSIONS:
    │       ├── context_packer.rs  # use crate::client::ChatMessage;
    │       ├── decomposer.rs  # use bon::Builder;
    │       ├── embeddings.rs  # use std::collections::HashMap;
    │       ├── error.rs  # use
    │       ├── lib.rs  # pub mod anonymize;
    │       ├── llm_queue.rs  # use std::sync::Arc;
    │       └── url.rs  # use thiserror::Error;
    ├── ontology/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── entity.rs  # use
    │       ├── inference.rs  # use std::collections::{HashMap,
    │       ├── lib.rs  # pub mod entity;
    │       ├── mapper.rs  # use std::collections::HashMap;
    │       ├── migration.rs  # #[derive(Debug, Clone)]
    │       └── yago.rs  # pub const NS_YAGO: &str =
    ├── rdf/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── lexer.rs  # use crate::RdfError;
    │       ├── lib.rs  # pub mod lexer;
    │       ├── normalize.rs  # pub struct BlankNodeScope;
    │       ├── nquads.rs  # use crate::lexer::{Lexer,
    │       └── parser.rs  # use std::collections::{HashMap,
    ├── registry/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── interner.rs
    │       └── lib.rs
    ├── requirements.txt
    ├── traits/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── lib.rs  # pub mod wrapper;
    │       └── wrapper.rs  # use std::time::Duration;
    ├── types/
    │   ├── Cargo.toml
    │   └── src/
    │       └── lib.rs  # use serde::{Deserialize,
    ├── vector-aliases/
    │   ├── Cargo.toml
    │   └── src/
    │       └── lib.rs  # use
    ├── vector-math/
    │   ├── Cargo.toml
    │   └── src/
    │       └── lib.rs  # pub fn cosine_similarity(a: &[f32], b:
    └── wasm_ipc/
        ├── Cargo.toml
        └── src/
            └── lib.rs  # //! WASM IPC — Binary schemas for
```
