# Coral Context: Architectural Vision

**A Deterministic-First Context Graph Library with MCP Server and Multi-Tier Cache**

---

## Executive Summary

Coral Context is a **Rust-native context graph library** that provides a 6-tier intelligent cache, SQLite-backed graph database, MCP server interface, and WASM plugin runtime. It serves as the knowledge backbone for guidance, separating deterministic lookups from probabilistic inference.

### The Core Goal

Traditional AI systems invoke probabilistic models for every query, incurring latency, cost, and unpredictability. Coral Context inverts this relationship:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                    DETERMINISTIC-FIRST EXECUTION MODEL                       │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│   Traditional AI:       Query → LLM → Response (slow, expensive, variable)  │
│                                                                             │
│   Coral Context:       Query → Cache Tier Check → Result                    │
│                            ↓ (miss at each tier)                            │
│                       L1 Memory → L3 Graph → L4 Semantic →                  │
│                       L4.5 Decompose → L5 Frontier                          │
│                                                                             │
│   Result: Sub-100ms for cached patterns, zero marginal cost, auditability  │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Key Outcomes**:
- **Sub-100ms latency** for deterministic execution paths
- **Zero marginal cost** for cached patterns (no LLM API calls)
- **Full auditability** through deterministic replay
- **Continuous improvement** as solutions become permanent cached nodes
- **Edge-native** design for resource-constrained environments

---

## Design Philosophy: Goals Over Implementation

### Goal 1: Replace LLM Reasoning with Cache-Tier Resolution

The fundamental innovation: **cascading cache tiers** instead of prompt-based reasoning. Each tier is progressively more expensive, and the system stops at the first hit:

- **L1 Memory**: LRU in-memory cache for hot queries (<1ms)
- **L3 Graph**: SQLite keyword search + recursive CTE graph traversal (<10ms)
- **L4 Semantic**: Brute-force KNN cosine similarity over embeddings (<50ms)
- **L4.5 Decompose**: Local LLM decomposes complex queries into subtasks (200ms)
- **L5 Frontier**: External LLM fallback for genuinely novel problems (500ms+)

### Goal 2: Edge-First Efficiency

Every component is designed for resource-constrained environments:

- **Memory Safety**: Rust's ownership model prevents leaks
- **Single-Process Embedding**: SQLite runs in-process, no separate database server
- **Token Optimization**: LOD packing reduces context window requirements by 80%+
- **No Runtime Overhead**: Zero-cost abstractions via Rust's trait system

### Goal 3: Neurosymbolic Learning Loop

When deterministic paths fail, the system learns from the solution:

```
Novel Query → L4.5 Decomposition → L5 Frontier LLM (if needed)
                    ↓
            Solution Cached as New Node
                    ↓
    Next Time: Deterministic Execution (< 50ms)
```

The expensive probabilistic step becomes a **one-time cost**, not a recurring one.

### Goal 4: Security Through Sandboxing

Dynamic tools (LLM-generated or user-provided) run in isolation:

- **WASM Sandboxing**: Extism provides memory-safe execution
- **No Host Access**: Filesystem and network access blocked by default
- **SSRF Protection**: URL validation blocks private IPs and remote HTTP
- **PII Anonymization**: Regex-based redaction for sensitive data

---

## Architectural Components

### Component 1: SQLite Graph Database (`db.rs`)

The core storage layer — a single SQLite database replacing dual-engine architectures:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         SQLite GRAPH DATABASE                                │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│   Tables:                                                                   │
│   ├── context_nodes    (id, name, source, lod, embedding, capabilities)    │
│   ├── edges            (source_id, target_id, edge_type, weight)           │
│   ├── wasm_tools       (name, path, capabilities)                          │
│   ├── targets          (name, bit_index, depends, provides, command)       │
│   ├── embedding_cache  (query_hash, query_text, embedding)                 │
│   ├── entity_types     (node_id, type_iri)                                 │
│   └── entity_hierarchy (subclass_iri, superclass_iri)                      │
│                                                                             │
│   Query Modes:                                                              │
│   ├── SQL + recursive CTE   (topological traversal, BFS/DFS)               │
│   ├── Brute-force KNN       (cosine similarity over float32 BLOBs)         │
│   ├── Hybrid search         (keyword + vector with RRF merge)              │
│   └── Duck typing           (recursive CTE is_a hierarchy traversal)       │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Key Capabilities**:
- Thread-safe via `Mutex<rusqlite::Connection>`
- KNN search capped at 100K candidates
- Recursive CTE for graph traversal with depth limit
- Batch insert with transactional flush
- Embedding cache for repeated queries
- Ontology type hierarchy with transitive `is_a` queries

### Component 2: 6-Tier Cache Cascade (`cache_reactor.rs`, `cache_l1.rs`, `cache_router.rs`)

The intelligence layer that routes queries through progressively more expensive tiers:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         CACHE TIER CASCADE                                   │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│   Query                                                                     │
│     │                                                                       │
│     ▼                                                                       │
│   ┌─────────┐  hit  ┌─────────┐  hit  ┌─────────┐  hit  ┌─────────┐      │
│   │ L1:     │──────▶│ L2:     │──────▶│ L3:     │──────▶│ L4:     │      │
│   │ Memory  │       │ WASM    │       │ Graph   │       │Semantic │      │
│   │ (LRU)   │       │ Tool    │       │ (SQLite)│       │ (KNN)   │      │
│   └─────────┘       └─────────┘       └─────────┘       └─────────┘      │
│                                                       │                   │
│                                                       ▼                   │
│                                                 ┌─────────┐  hit         │
│                                                 │ L4.5:   │─────────▶    │
│                                                 │Decompose│              │
│                                                 │ (local) │              │
│                                                 └─────────┘              │
│                                                       │ miss             │
│                                                       ▼                   │
│                                                 ┌─────────┐              │
│                                                 │ L5:     │              │
│                                                 │Frontier │              │
│                                                 │ (LLM)   │              │
│                                                 └─────────┘              │
│                                                                             │
│   Every non-L1 hit is persisted as a solution node for future queries.     │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

| Tier | Name | Implementation | Latency | When |
|------|------|---------------|---------|------|
| L1 | Memory | LRU cache (10K entries) | <1ms | Hot queries |
| L2 | WASM Workflow | WASM tool matching via `wasm_tools` table | <5ms | Tool-capable queries |
| L3 | Graph | SQLite LIKE + recursive CTE traversal | <10ms | Structural queries |
| L4 | Semantic | Brute-force KNN cosine similarity | <50ms | Semantic queries |
| L4.5 | Decompose | Local LLM splits into subtasks, recurses | 200ms | Complex multi-step |
| L5 | Frontier | External LLM (Ollama/OpenAI) | 500ms+ | Novel problems |

### Component 3: MCP Server (`mcp.rs`)

JSON-RPC 2.0 server implementing the Model Context Protocol for AI agent integration:

| Method | Parameters | Behavior |
|--------|-----------|----------|
| `coral_query` | `{ "name": "..." }` | Node lookup by name |
| `coral_insert` | Full `ContextNode` JSON | Insert a node, return `node_id` |
| `coral_traverse` | `{ "node_id": N, "max_depth": N }` | Graph traversal |

**Transport**: STDIO (line-delimited JSON), max request size 10MB.

### Component 4: WASM Plugin Runtime (`wasm_runtime.rs`)

Dynamic tool execution via Extism WASM SDK, bridged to the fluent-wvr trait system:

```rust
// WasmComponent implements all fluent-wvr traits:
impl WorkUnit for WasmComponent { ... }
impl FieldAccess for WasmComponent { ... }
impl Describable for WasmComponent { ... }
// Automatic Component via blanket impl
```

**Security Model**:
- No filesystem access (unless explicitly granted)
- No network access from sandboxed tools
- Memory-safe execution via Extism
- Host functions: whitelisted only

### Component 5: Context Packing with LOD (`packer.rs`)

Token-budget-aware context packing using Level of Detail:

| LOD Level | Size | Use Case |
|-----------|------|----------|
| lod0 | Complete | Focal point of query |
| lod1 | ~800 chars | Primary context |
| lod2 | ~240 chars | Secondary context |
| lod3 | ~80 chars | Distant references |
| lod4 | ~10 chars | Peripheral mentions |

**Algorithm**:
1. BFS from focus node up to depth 5
2. Select LOD by effective graph distance (normalized by avg degree)
3. First-Fit Decreasing bin-pack into token budget

### Component 6: Batch Ingestion (`ingest.rs`)

RDF/Turtle/N-Quads ingestion pipeline with transactional flush:

```
File → Lexer → Parser → TripleMapper → PendingNode/PendingEdge
                                              ↓
                                    BatchIngestor (10K batch)
                                              ↓
                                    Transactional flush to SQLite
```

**Features**:
- Turtle and N-Quads format support
- YAGO ontology whitelist filtering
- Auto-discovery of neighbor edges via KNN (distance < 0.3)
- Embedding computation during ingestion

---

## Data Model

### ContextNode

```rust
pub struct ContextNode {
    pub id: NodeId,                    // i64, time-sortable
    pub name: String,                  // unique identifier
    pub source: String,                // source reference
    pub lod: Vec<String>,              // Level of Detail pyramid (6 levels)
    pub embedding: Option<Vec<f32>>,   // 768-1536 dimensions
    pub capabilities: BitVec,          // capability bitmask
}
```

### Edge

| Field | Type | Description |
|-------|------|-------------|
| source_node_id | i64 | Source node |
| target_node_id | i64 | Target node |
| edge_type | String | Relationship type |
| weight | f64 | Edge weight |

### Target (DAG Node)

| Field | Type | Description |
|-------|------|-------------|
| name | String | Human-readable name |
| bit_index | usize | Capability bit position |
| depends | BitVec | Required capabilities |
| provides | BitVec | Provided capabilities |
| essential | bool | Must succeed |
| command | String | Shell command |

---

## Integration with guidance

### How Coral Serves guidance

```
guidance explain "query"
        │
        ▼
   QueryEngine.classify()
        │
        ▼
   WordIndex / GuidanceDb hybrid search
        │ (if vector search needed)
        ▼
   coral::db::Library::knn_search()
        │ (if graph traversal needed)
        ▼
   coral::db::Library::traverse_from()
        │ (if context packing needed)
        ▼
   coral::packer::ContextPacker::pack()
```

### Shared Types

| Type | Defined In | Used By |
|------|-----------|---------|
| `ContextNode` | `guidance-types` | coral, guidance |
| `NodeId` | `guidance-types` | coral, guidance |
| `KnnHit` | `guidance-types` | coral, guidance |
| `WasmTool` | `guidance-types` | coral |
| `Component` | `fluent-wvr` | coral, dag |
| `WorkUnit` | `fluent-wvr` | coral, dag |

---

## Implementation Status

### Completed

1. **SQLite Graph Database**: Full schema with 7 tables, KNN search, recursive CTE traversal, duck typing
2. **6-Tier Cache Cascade**: L1 (LRU) through L5 (Frontier), with solution persistence
3. **MCP Server**: JSON-RPC 2.0 over STDIO with 3 methods
4. **WASM Plugin Runtime**: Extism integration with fluent-wvr trait bridge
5. **Context Packing**: Token-budget-aware LOD selection with FFD bin-packing
6. **Batch Ingestion**: Turtle/N-Quads parsing with YAGO whitelist filtering
7. **Embedding Support**: Ollama and OpenAI embedding providers with caching
8. **SSRF Protection**: URL validation blocking private IPs and remote HTTP
9. **PII Anonymization**: Regex-based redaction for emails, credit cards, SSN, etc.
10. **Hybrid Search**: Reciprocal Rank Fusion (k=60) for keyword + vector

### In Progress

1. **Fluent WVR Pattern Adoption**: Wrapping cache tiers with WorkUnit for uniform orchestration
2. **Async I/O**: Replacing synchronous SQLite calls with async-friendly patterns

### Planned

1. **HNSW Index**: Replace brute-force KNN with approximate nearest neighbor for >100K nodes
2. **Persistent L1 Cache**: Disk-backed LRU for warm starts
3. **Graph Analytics**: PageRank, community detection for node importance

---

## Deployment Profiles

### Edge Profile (Raspberry Pi, Mobile)

- **Memory**: 4-8 GB RAM
- **Storage**: SQLite (WAL mode)
- **Max Nodes**: ~100K ContextNodes
- **Target Latency**: < 50ms deterministic

### Server Profile (Linux x86_64)

- **Memory**: 16-64 GB RAM
- **Storage**: SQLite (WAL mode, high concurrency)
- **Max Nodes**: ~1M+ ContextNodes
- **Target Latency**: < 20ms deterministic

---

## Success Metrics

| Metric | Target |
|--------|--------|
| Deterministic resolution rate | > 40% |
| Query latency (cached) | < 50ms |
| Memory footprint (edge) | < 500MB |
| Binary size (edge) | < 50MB |
| Frontier consultation rate | < 15% |

---

## Conclusion

Coral Context represents a shift in AI architecture: **deterministic execution first, probabilistic inference only when necessary**. By replacing prompt-based LLM reasoning with a cascading cache tier system, the system achieves:

- **Predictable performance**: Sub-100ms latency for known patterns
- **Zero marginal cost**: No API calls for cached solutions
- **Full auditability**: Every decision traceable through the cache tiers
- **Continuous improvement**: Each novel solution becomes a permanent cached node
- **Edge deployment**: Full functionality on Raspberry Pi-class hardware

The result is an AI system that grows more capable with every use, while remaining fast, cheap, and deterministic.

---

*Vision Document v3.0 — June 2026 (Rust codebase)*
