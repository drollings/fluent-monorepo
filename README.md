# guidance monorepo

A Rust-native, deterministic-first AST-guided vector search and context graph
system for AI-assisted code navigation. Builds two projects — **guidance** (code
indexing + query) and **Coral Context** (knowledge graph + cache cascade) — on a
shared foundation of 15 reusable crates implementing the Fluent WVR component
model.

The LLM is treated as a fallback compiler for unstructured data, not as the
primary reasoning engine. Deterministic computation covers 80%+ of queries;
probabilistic inference is strictly additive.

## Quick start

```bash
# Build everything
cargo build --workspace

# Run all tests
cargo test --workspace

# Lint
cargo clippy --workspace -- -D warnings

# Build just the guidance binary
cargo build --bin guidance

# Initialize a guidance index for a Rust project
cargo run --bin guidance -- init .
cargo run --bin guidance -- sync .
cargo run --bin guidance -- explain "LLM integration"
```

## Projects

### guidance

AST-guided code navigation subagent. Parses source files (Zig, Python, Rust,
Markdown) via tree-sitter, produces `.guidance/src/**/*.json` metadata mirrors
and `.guidance.db` SQLite vector search databases. Optimized for token-efficient
subagent discovery workflows with sub-100ms deterministic queries.

**Core pipeline:**
```
Source files → tree-sitter AST parse → JSON metadata → SQLite vector DB
                                                              ↓
Query → Alias expansion → Hybrid search (vector + keyword) → Staged output
                                            ↓ (miss)
                                     Local LLM synthesis → cached for next time
```

**Subcommands:** `sync`, `explain`, `check`, `init`, `status`, `clean`,
`structure`, `health`, `benchmark`, `test`, `telemetry`, `cache-stats`, `todo`,
`diary`, `commit`

### Coral Context

A context graph library providing a 6-tier intelligent cache (L1 memory → L5
frontier LLM), SQLite-backed graph database, MCP server, and WASM plugin
runtime. Separates deterministic lookups from probabilistic inference with
sub-100ms latency for cached patterns and zero marginal cost.

**Cache cascade:**
```
L1 Memory (LRU, <1ms) → L3 Graph (SQLite, <10ms) → L4 Semantic (KNN, <50ms)
                                                        ↓ (miss)
                                                 L4.5 Decompose (local LLM, 200ms)
                                                        ↓ (miss)
                                                 L5 Frontier (external LLM, 500ms+)
```

## Library infrastructure

The Fluent WVR component model gives every unit of work — native structs, WASM
plugins, DB-driven configs — the same `Arc<dyn Component>` interface. The
orchestrator never branches on implementation type.

### Core traits (`fluent-wvr`)

| Trait | Purpose |
|-------|---------|
| `WorkUnit` | Uniform orchestration: `name`, `depends`, `provides`, `execute` |
| `FieldAccess` | Runtime field get/set by name with validation |
| `Describable` | JSON Schema generation for MCP/TUI integration |
| `Component` | Supertrait combining all three (blanket impl) |

### Workspace crates

```
src/
  bin/
    guidance/            guidance CLI binary (14+ subcommands)
    coral/               coral binary (MCP server + ingest CLI)
  guidance/              guidance-core: AST parser, sync engine, query engine
  coral/                 coral-context: graph DB, cache cascade, MCP server, WASM runtime
  dag/                   DAG executor: resolver, work_unit, adapter, middleware
  fluent-wvr/            Component, WorkUnit, FieldAccess, Describable traits
  fluent-wvr-macros/     #[derive(FieldAccess)] and #[derive(Derivable)] proc macros
  fluent-concurrency/    WorkerPool, Scope, Zone, Limiter, PriorityQueue, CreditFlow
  llm/                   LLM HTTP client + embeddings (Ollama, OpenAI)
  types/                 Shared domain types (GuidanceDoc, Member, FileType, etc.)
  common-core/           General utilities (hashing, formatting, shell, string ops)
  search-vector/         SQLite hybrid search (vector + keyword + RRF fusion)
  project-knowledge/     WordIndex, TrigramIndex, CsrGraph, QueryCache
  content-node/          LOD slicing and file content annotation
  ontology/              Entity extraction, YAGO taxonomy, capability inference
  rdf/                   Turtle/N-Quads parser and normalization
  wasm_ipc/              WASM IPC binary types (#[repr(C, packed)])
  memory-plugin/         Pluggable persistent memory backends
```

### Key capabilities

| Capability | Crate | Description |
|-----------|-------|-------------|
| AST indexing | `guidance` | Tree-sitter parsing for Zig, Python, Rust with incremental `match_hash` sync |
| Vector search | `search-vector` | Cosine similarity + keyword + RRF hybrid over SQLite, quantized embeddings |
| Concurrency | `fluent-concurrency` | Bounded pools, structured scopes, capability-gated I/O, credit flow backpressure |
| DAG execution | `dag` | Dependency-driven workflow with adapters, middleware, type inference |
| LLM client | `llm` | Ollama/OpenAI chat + embeddings with context packing and request queueing |
| Context packing | `coral` | Token-budget-aware LOD selection with BFS distance weighting |
| WASM runtime | `coral` + `wasm_ipc` | Extism plugin execution with binary IPC across the sandbox boundary |
| MCP server | `coral` | JSON-RPC 2.0 over STDIO for IDE integration |
| Graph database | `coral` | SQLite graph store with KNN search, recursive CTE traversal, duck typing |
| Ontology | `ontology` | YAGO taxonomy with transitive `is_a` inference for duck-typed capabilities |
| RDF ingestion | `rdf` | Turtle/N-Quads parsing with transactional batch flush |
| Content nodes | `content-node` | 6-level LOD pyramid (full text → keywords) for context window packing |
| Project knowledge | `project-knowledge` | Word/trigram inverted indexes, CSR graph, frequency tables |

## Design philosophy

1. **Deterministic-first**: AST parsing produces ground truth; LLM enhancement is
   strictly additive, never authoritative
2. **Cache over compute**: Every novel solution becomes a permanent cached node
3. **Edge-deployable**: Single-process SQLite, no external services, targets
   Raspberry Pi class hardware (<50MB binary, <500MB RAM)
4. **Capability-gated I/O**: All file/network/DB access requires explicit
   capability tokens — no ambient authority
5. **Structured concurrency**: Every spawned task belongs to a Scope whose close
   must be awaited; panics are contained within Zones
6. **Uniform interface**: Native Rust, WASM plugins, and DB-driven configs all
   present `Arc<dyn Component>` — the orchestrator never branches on origin

## Design patterns

The codebase implements twelve composable patterns documented in
`doc/skills/fluent-wvr/SKILL.md`:

| Pattern | Problem | Cost |
|---------|---------|------|
| Fluent Builder (`bon`) | Multi-parameter init | Zero |
| Trait-Based Reflection (`FieldAccess`) | Runtime config by name | Zero |
| Trait Composition (newtype wrappers) | Cross-cutting concerns | Zero |
| Trait Objects (`Arc<dyn Component>`) | Runtime polymorphism | One vtable ptr |
| Binary IPC (`#[repr(C, packed)]`) | WASM boundary messages | Memcpy |
| Scoped Ownership (RAII) | Repeated alloc/free | Zero |
| Newtype Handles | ID type confusion | Zero |
| Unit of Work | Uniform orchestration | One impl |
| Middleware Chain | Post-erasure composition | One alloc/layer |
| Component Adapter | Runtime type adaptation | One Arc |
| Structured Logging Context | Request-scoped observability | Thread-local |
| Runtime Composition | Full lifecycle | Zero |

## Configuration

`.guidance/guidance-config.json`:
```json
{
  "model": "llama3.2",
  "api_url": "http://localhost:11434",
  "providers": { "rs": { "extensions": [".rs"] } },
  "embedding_provider": "ollama",
  "embedding_model": "nomic-embed-text"
}
```

## Source layout

```
.guidance/              Generated guidance JSON, skills, docs
  guidance-config.json  Model / provider configuration
  .skills/              Design-pattern skill documents
  .doc/                 Capabilities, diary, inbox
  src/                  Generated guidance JSON (mirrors src/ tree)
.guidance.db            SQLite vector search database for queries
doc/
  ARCHITECTURE.md       System architecture reference
  capabilities/         19 capability documents
  guidance/VISION.md    guidance vision document
  coral/VISION.md       Coral Context vision document
  skills/               Fluent WVR and Fluent Concurrency skill docs
env/
  mk/                   Shared Makefile helpers
  mise/                 Language-specific mise.toml fragments
```

## Authorship

Authored by Daniel Rollings, February 2026, with elements from previous
hand-written projects in Python, C++, and Zig, ported to Rust.

## License

Dual-licensed under GNU GPLv3 and a Commercial License.

**GPLv3**: Free for open-source, hobby, and individual use. If you distribute
your software, you must open-source it under GPLv3.

**Commercial License** required for:
- Proprietary, closed-source products
- Organizations with gross annual revenue exceeding $1,000,000 USD
- More than one developer seat
- Technical support, indemnification, or liability waivers

See `LICENSE`, `LICENSE-Contributor-Agreement`, and `LICENSE-Commercial-Requirement`.
