# Architecture Overview

## Crate Dependency Diagram

```
  ┌─────────────────────────────────────────────────────────────┐
  │                    Binary Layer                              │
  │                                                              │
  │  ┌─────────────────────┐    ┌─────────────────────┐          │
  │  │  guidance (bin)     │    │  coral (bin)        │          │
  │  │  CLI entry point    │    │  MCP server          │          │
  │  └──────────┬──────────┘    └──────────┬──────────┘          │
  │             │                          │                     │
  ├─────────────┼──────────────────────────┼─────────────────────┤
  │             │          Consumer Layer   │                     │
  │             │                          │                     │
  │  ┌──────────▼──────────┐    ┌──────────▼──────────┐          │
  │  │  guidance-core      │    │  coral-context      │          │
  │  │  (src/guidance)     │    │  (src/coral)        │          │
  │  │  app logic, CLI     │    │  context graph      │          │
  │  └──────────┬──────────┘    └──────────┬──────────┘          │
  │             │                          │                     │
  ├─────────────┼──────────────────────────┼─────────────────────┤
  │             │      Capability Layer     │                     │
  │             │                          │                     │
  │  ┌──────────▼──────────┐    ┌──────────▼──────────┐          │
  │  │  fluent-llm         │    │  fluent-dag         │          │
  │  │  (src/llm)          │    │  (src/dag)          │          │
  │  │  LLM HTTP client    │    │  DAG executor       │          │
  │  └─────────────────────┘    └─────────────────────┘          │
  │                                                              │
  │  ┌─────────────────────┐    ┌─────────────────────┐          │
  │  │  guidance-ontology  │    │  guidance-rdf       │          │
  │  │  (src/ontology)     │    │  (src/rdf)          │          │
  │  │  entity/inference   │    │  Turtle/N-Quads     │          │
  │  └─────────────────────┘    └─────────────────────┘          │
  │                                                              │
  │  ┌─────────────────────┐    ┌─────────────────────┐          │
  │  │  search-vector      │    │  fluent-knowledge   │          │
  │  │  (src/search-vector)│    │  (src/knowledge)    │          │
  │  │  hybrid search      │    │  word/trigram index │          │
  │  └─────────────────────┘    └─────────────────────┘          │
  │                                                              │
  │                                                              │
  │  ┌─────────────────────┐    ┌─────────────────────┐          │
  │  │  content LOD        │    │  fluent-wasm-ipc    │          │
  │  │  (src/knowledge/    │    │  (src/wasm_ipc)     │          │
  │  │   lod.rs)           │    │  Extism binary IPC  │          │
  │  │  LOD slicing        │    └─────────────────────┘          │
  │  └─────────────────────┘                                     │
  │                                                              │
  ├──────────────────────────────────────────────────────────────┤
  │                   Framework Layer                             │
  │                                                              │
  │             ┌─────────────────────────────┐                   │
  │             │  fluent-wvr (src/fluent-wvr)│                   │
  │             │  Component/WorkUnit traits  │                   │
  │             └─────────────┬───────────────┘                   │
  │                           │                                   │
  ├───────────────────────────┼───────────────────────────────────┤
  │                           │   Types Layer                     │
  │             ┌─────────────▼───────────────┐                   │
  │             │  fluent-types (src/types)    │                   │
  │             │  shared data types           │                   │
  │             └─────────────────────────────┘                   │
  │                                                              │
  ├──────────────────────────────────────────────────────────────┤
  │                    Common Layer                                │
  │                                                              │
  │  ┌─────────────────────┐    ┌─────────────────────┐          │
  │  │  common-core        │    │  fluent-            │          │
  │  │  (src/common-core)  │    │  concurrency        │          │
  │  │  zero-domain utils  │    │  (src/fluent-       │          │
  │  └─────────────────────┘    │   concurrency)      │          │
  │                             │  shared pools       │          │
  │                             └─────────────────────┘          │
  └──────────────────────────────────────────────────────────────┘
```

## Tier Descriptions

### Common Layer (no domain logic)
Crates in this layer contain zero domain-specific logic. They depend only on the
standard library and well-known third-party crates (serde_json, sha2, blake3).
A crate in this layer must never import any `guidance-*` or `coral-*` crate.

| Crate | Path | Responsibility |
|-------|------|----------------|
| `common-core` | `src/common-core/` | Generic utilities: hashing, formatting, I/O, shell, metrics, error context |
| `fluent-concurrency` | `src/fluent-concurrency/` | `Scope`, pools, credit flow, `StreamAbort` (shared with the router/assistant stacks) |

### Types Layer (shared data types)
Contains the canonical data types used across all higher layers. No business
logic — only type definitions, serialization, and validation.

| Crate | Path | Responsibility |
|-------|------|----------------|
| `fluent-types` | `src/types/` | `GuidanceDoc`, `Member`, `Param`, `FileType`, `NodeId`, etc. |

### Framework Layer (trait definitions)
Defines the core trait boundary between the DAG executor and its components.
This is the Rust equivalent of a header-only interface crate — traits and
blanket impls only, no implementation code.

| Crate | Path | Responsibility |
|-------|------|----------------|
| `fluent-wvr` | `src/fluent-wvr/` | `Component`, `WorkUnit`, `FieldAccess`, `Describable` traits |

### Capability Layer (reusable domain capabilities)
Each crate in this layer encapsulates a single domain concern. They depend
on the Common, Types, and Framework layers but NOT on each other (no
`fluent-dag` importing `coral-context`, etc.). Cross-capability integration
happens only in the Consumer layer.

| Crate | Path | Responsibility |
|-------|------|----------------|
| `fluent-llm` | `src/llm/` | LLM HTTP client, embeddings, chat completions, prompt utilities |
| `fluent-dag` | `src/dag/` | DAG executor, resolver, middleware, work unit abstractions |
| LOD slicing | `src/knowledge/lod.rs` | Level-of-detail text slicing over the word index |
| `search-vector` | `src/search-vector/` | SQLite hybrid search (KNN + keyword + RRF) |
| `fluent-knowledge` | `src/knowledge/` | Word/trigram index, CSR graph, frequency tables |
| `guidance-ontology` | `src/ontology/` | Entity extraction, capability inference, YAGO taxonomy |
| `guidance-rdf` | `src/rdf/` | RDF/Turtle/N-Quads parser, normalizer |
| `fluent-wasm-ipc` | `src/wasm_ipc/` | `#[repr(C)]` binary IPC schemas for Extism boundary |

### Consumer Layer (application logic)
Composes capabilities into application-level workflows and services.

| Crate | Path | Responsibility |
|-------|------|----------------|
| `guidance-core` | `src/guidance/` | CLI dispatch, sync engine, query pipeline, config, plugins, AST parsing |
| `coral-context` | `src/coral/` | Context graph cache tiers (L1–L5), MCP router, wasm runtime, ingest |

### Binary Layer (thin entry points)
Zero library exports — pure CLI entry points that parse arguments and delegate
to the consumer layer.

| Binary | Path | Responsibility |
|--------|------|----------------|
| `guidance` | `src/bin/guidance/` | CLI binary: `guidance explain`, `guidance gen`, etc. |
| `coral` | `src/bin/coral/` | MCP server binary: `coral mcp` |

## Design Contracts

### Layer violation rules
1. **Common** → nothing domain-specific. Never import `guidance-*`
2. **Types** → never import a capability crate
3. **Framework** → never import a capability crate
4. **Capability** → may import Common + Types + Framework; never import other capability crates
5. **Consumer** → may import any lower-tier crate
6. **Binary** → must be a pure consumer: no `[lib]`, no `pub` items

### Integration convention (wvr.rs)
Consumer and capability crates that need Fluent WVR framework integration
expose a `wvr.rs` module that re-exports the relevant traits. This follows
a discoverable convention: any crate with a `wvr.rs` is known to provide
Fluent WVR trait implementations.
