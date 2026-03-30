# Coral Context: Architectural Vision

**A Deterministic-First Neurosymbolic Orchestration Engine for Edge Intelligence**

---

## Executive Summary

Coral Context is a **deterministic-first AI orchestration framework** designed for extreme edge efficiency. It separates LLM queries into non-deterministic elements to be cached as context "nodes", and isolates logic, routing, and tool orchestration to compose deterministic tools, knowledge bases, and Directed Acyclic Graph (DAG) executed in Zig. LLMs are relegated to the role of unstructured data compilers, while the core system resolves semantic relationships using an embedded graph database and executes logic securely via WebAssembly.

### The Core Goal

Traditional AI systems invoke probabilistic models for every query, incurring latency, cost, and unpredictability. Coral Context inverts this relationship:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                    DETERMINISTIC-FIRST EXECUTION MODEL                       │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│   Traditional AI:       Query → LLM → Response (slow, expensive, variable)  │
│                                                                             │
│   Coral Context:       Query → DAG Resolution → Result                      │
│                            ↓ (if needed)                                    │
│                       Local Model Decomposition                             │
│                            ↓ (if needed)                                    │
│                       Frontier LLM (last resort)                            │
│                                                                             │
│   Result: Sub-100ms for cached patterns, zero marginal cost, auditability  │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Key Outcomes**:
- **Sub-100ms latency** for deterministic execution paths
- **Zero marginal cost** for cached patterns (no LLM API calls)
- **Full auditability** through deterministic replay
- **Continuous improvement** as solutions become permanent DAG capabilities
- **Edge-native** design for resource-constrained environments

---

## Design Philosophy: Goals Over Implementation

### Goal 1: Replace LLM Reasoning with Bitwise Graph Traversal

The fundamental innovation: **DAGs over prompts**. Instead of asking an LLM to "figure out" the next step, the system uses bitwise operations on TraitSet bitmasks to deterministically resolve workflow paths:

- **TraitSet Matching**: Hardware-accelerated `@popCount` operations measure capability distance
- **Topological Execution**: Kahn's algorithm guarantees correct dependency ordering
- **No Prompt Engineering Required**: The DAG encodes the logic explicitly

### Goal 2: Edge-First Efficiency

Every component is designed for resource-constrained environments:

- **Memory Safety**: Zig's zero-cost abstractions prevent leaks
- **Single-Process Embedding**: SQLite runs in-process, no separate database server
- **Binary IPC**: Zero-copy serialization between Zig and WASM using `extern struct align(1)`
- **Token Optimization**: LOD packing reduces context window requirements by 80%+

### Goal 3: Neurosymbolic Learning Loop

When deterministic paths fail, the system learns from the solution:

```
Novel Query → Local Model Decomposition → Frontier LLM (if needed)
                            ↓
                    Solution Cached as New DAG Capability
                            ↓
        Next Time: Deterministic Execution (< 50ms)
```

The expensive probabilistic step becomes a **one-time cost**, not a recurring one.

### Goal 4: Security Through Sandboxing

Dynamic tools (LLM-generated or user-provided) run in isolation:

- **WASM Sandboxing**: Extism provides memory-safe execution
- **No Host Access**: Filesystem and network access blocked by default
- **Strict Type Boundaries**: Binary IPC prevents injection attacks

---

## Architectural Components

While the focus is on goals, the implementation uses these components:

### Component 1: The Deterministic Core (Zig + DAG)

The execution engine that replaces LLM reasoning:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         DAG RESOLUTION PIPELINE                             │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│   Query + Context → TraitSet Extraction                                    │
│                            ↓                                                │
│   Capability Registry → Bitmask Intersection                               │
│                            ↓                                                │
│   Distance = popCount(needed_mask & ~available_mask)                      │
│                            ↓                                                │
│   Distance = 0? → Execute DAG deterministically                            │
│   Distance > 0? → Partial match: decompose into sub-tasks                 │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Key Capabilities**:
- Hardware-accelerated TraitSet matching
- Kahn's algorithm for topological sorting
- Cycle detection for DAG validation
- Parallel execution of independent nodes
- **Reflection-based field access**: All Target and ContextNode fields accessible via unified schema

### Component 2: SQLite as Unified Backend

Single persistent store replacing dual-engine (pgvector + LadybugDB):

| Function | Implementation |
|----------|----------------|
| Node payloads | LOD text pyramid + embeddings (BLOB) |
| Target DAG | DynamicBitSet word arrays (BLOB) |
| Graph edges | SQL + recursive CTEs for BFS traversal |
| WASM cache | Base64-encoded binaries |
| Embeddings | Float32 BLOBs, cosine similarity in Zig |

**Query Modes**:
- **SQL + recursive CTE**: Topological traversal, dependency resolution
- **Cosine Similarity**: In-process KNN over float32 BLOBs (≤100K nodes)
- **Hybrid**: Weighted fusion of vector (0.65) + keyword (0.35) scores

### Component 3: Reflection Layer — Single Source of Truth

A unified reflection system ensures that field access, serialization, IPC, and database persistence all derive from the same schema definition:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      REFLECTION ARCHITECTURE                                │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│   ┌─────────────────────────────────────────────────────────────────┐       │
│   │                    STRUCT DEFINITION                            │       │
│   │         Target { id, name, depends, provides, ... }            │       │
│   │         ContextNode { id, lod[6], embedding, ... }            │       │
│   └────────────────────────────┬────────────────────────────────────┘       │
│                                │                                            │
│                    ┌───────────┴───────────┐                               │
│                    │    ACCESSOR TABLE     │                               │
│                    │  (comptime-generated) │                               │
│                    └───────────┬───────────┘                               │
│                                │                                            │
│         ┌──────────────────────┼──────────────────────┐                      │
│         │                      │                      │                      │
│         ▼                      ▼                      ▼                      │
│   ┌───────────┐        ┌───────────┐         ┌───────────┐                │
│   │ TUI Editor│        │  WASM IPC │         │  SQLite   │                │
│   │ Role perms│        │ Binary ser│         │ Hydration │                │
│   └───────────┘        └───────────┘         └───────────┘                │
│                                                                             │
│   All paths use the same field offsets, types, and permissions            │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Key Capabilities**:

| Feature | Implementation |
|---------|---------------|
| **Unified Schema** | `Accessor` table defines name, offset, type, permissions |
| **Role-Based Access** | 18-bit `RolePermissions` (6 roles × 3 ops) enforced on every `set`/`get` |
| **Binary IPC** | `setBinaryFn`/`getBinaryFn` vtable entries for zero-copy WASM serialization |
| **String Path** | `setCtxFn`/`getCtxFn` for TUI editors, RPC handlers, config loading |
| **Ownership Tracking** | `releaseFn` with `lod_owned` bitmask prevents double-free on string fields |
| **Cross-Module Translation** | `convertFn` translates bitsets between different StringInterners |

**Schema Types**:

| Schema | Host Type | Use Case |
|--------|-----------|----------|
| `Editable(T)` | Comptime-known structs | `WasmTool.editable`, `ToolCompilerConfig.editable` |
| `DynamicEditable` | Runtime-defined rows | SQLite hydration, WASM tool parameters |
| `TargetSchema` | `Target` | DAG targets with `depends`/`provides` bitsets |
| `ContextNodeSchema` | `ContextNode` | Semantic entities with LOD pyramid |

**Single Source of Truth Principle**:

The struct definition is the schema. Reflection derives:
- Field offsets for binaryIPC
- String setters/getters for TUI and RPC
- Permission masks for role enforcement
- Ownership modes for memory safety
- Type tags for heterogeneous storage

Dynamic tool execution in isolated sandboxes:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      WASM EXECUTION ARCHITECTURE                            │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│   Tool Source (Zig/Rust/Generated) → Compile to WASM                       │
│                            ↓                                                │
│   Extism Sandbox: 16MB heap limit, 30s timeout                            │
│                            ↓                                                │
│   Binary IPC: extern struct align(1) (no JSON)                            │
│       └─ Reflection-driven: BinaryContextNode, BinaryTarget               │
│                            ↓                                                │
│   Host Functions: Whitelisted only (graph query, node fetch)               │
│                                                                             │
│   Security: No filesystem, no network, no subprocesses                      │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Reflection-Powered IPC**:

Binary serialization avoids string parsing overhead:
- `BinaryContextNode` serializes `lod[6]` as offset/length pairs
- `BinaryExecutionResult` uses word arrays for unbounded bitsets
- Field order matches `Accessor` table — drift impossible

### Component 5: ContextNodes with LOD Packing

Semantic entities stored at multiple detail levels:

| LOD Level | Size | Use Case |
|-----------|------|----------|
| lod0_full | Complete | Focal point of query |
| lod1_summary | ~800 chars | Primary context |
| lod2_brief | ~240 chars | Secondary context |
| lod3_tiny | ~80 chars | Distant references |
| lod4_name | ~10 chars | Peripheral mentions |

**Packing Algorithm**:
1. Identify semantic center via cosine similarity KNN
2. Compute graph distance via BFS
3. Assign LOD by distance (closer = more detail)
4. Fit to token budget, downgrading as needed

### Component 6: YAGO 4.5 Ontology

A sparse ontological foundation for semantic reasoning:

**Dual Purpose**:

1. **Duck Typing Support**: Type hierarchy enables capability inference
   - Tool built for "Person" → works for "Scientist", "Developer"
   - Property inheritance through subsumption

2. **Baseline Knowledge**: General world knowledge
   - Entity relationships from Wikipedia
   - Temporal grounding for facts

3. **Foundation for Custom Knowledge**:
   - User knowledge bases build on the baseline
   - Confidence scoring for merged information

### Component 7: System Boundaries

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         SYSTEM BOUNDARIES                                   │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│   ┌─────────────────────────────────────────────────────────────────┐      │
│   │                     CORAL CONTEXT                               │      │
│   │                                                                  │      │
│   │   ┌─────────────────┐    ┌─────────────────┐                  │      │
│   │   │  MCP Server     │    │  HTTP Client    │                  │      │
│   │   │  (JSON-RPC)     │    │  (LLM Access)   │                  │      │
│   │   │                 │    │                 │                  │      │
│   │   │ • STDIO transport    │ • Ollama/llama.cpp│                │      │
│   │   │ • HTTP/SSE    │    │ • Cloud APIs    │                  │      │
│   │   └────────┬────────┘    └────────┬────────┘                  │      │
│   │            │                       │                             │      │
│   │            ▼                       ▼                             │      │
│   │   ┌─────────────────────────────────────────────────────┐       │      │
│   │   │           INTERNAL BINARY EXECUTION                  │       │      │
│   │   │   • DAG Resolution  • WASM Execution                 │       │      │
│   │   │   • SQLite Queries • Context Packing                │       │      │
│   │   └─────────────────────────────────────────────────────┘       │      │
│   │                                                                  │      │
│   └─────────────────────────────────────────────────────────────────┘      │
│                                                                             │
│   JSON parsed ONLY at perimeter; internal ops use binary                   │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Integration Points**:
- **MCP Server**: For AI agent integration (Claude Code, Cursor, NullClaw)
- **HTTP Client**: For edge/cloud LLM access
- **STDIO**: For local low-latency scenarios

---

## Execution Pipeline

### Phase 1: Command Recognition (Deterministic)

```
Input: "/build src/main.zig"
  ↓
Pattern Match → Compiled Target "build_zig"
  ↓
Capability Check: needs {zig_compiler, build_zig}
  ↓
Resolution: Find providers via bitmask
  ↓
Execution: Native Zig execution
  ↓
Latency: < 5ms
```

### Phase 2: Semantic Search (Deterministic)

```
Input: "Extract tables from quarterly report"
  ↓
Embedding: 768-dim vector
  ↓
Cosine Similarity: In-process KNN over SQLite BLOBs
  ↓
Capability Filter: Verify provides_mask
  ↓
DAG Execution: Run matched tools
  ↓
Latency: 50-200ms
```

### Phase 3: Local Decomposition (Hybrid)

```
Input: Complex multi-step task
  ↓
Semantic Search: No complete match
  ↓
Local 3-4B Model: Decompose into sub-tasks
  ↓
Sub-task Resolution: Each matched via DAG
  ↓
Topological Execution: Run in order
  ↓
Latency: 200-800ms
```

### Phase 4: Frontier Consultation (Last Resort)

```
Input: Genuinely novel problem
  ↓
All prior phases failed
  ↓
Anonymize + Minimize context
  ↓
Frontier LLM: Generate solution
  ↓
Validate + Compile to WASM
  ↓
Index in SQLite
  ↓
**Next query: Deterministic execution**
```

---

## Data Model

### ContextNode

| Field | Type | Description |
|-------|------|-------------|
| id | i64 | Time-sortable identifier (SQLite INTEGER PRIMARY KEY) |
| lod | Array[6] | Level of Detail pyramid (lod0-lod5) |
| embedding | Float slice | 768-1536 dimensions |
| valid_from/to | Float64 | Temporal validity window |
| confidence | i32 | Quality score |
| provenance_id | i32 | Source reference |

**Reflection Access**: `ContextNodeSchema.viewOf()` provides schema-driven access for TUI editors, WASM IPC binary serialization, and SQLite hydration. The `lod_owned` bitmask tracks which string slots are allocator-owned.

### Target (DAG Node)

| Field | Type | Description |
|-------|------|-------------|
| id | i64 | Target identifier (SQLite INTEGER PRIMARY KEY) |
| name | String | Human-readable name |
| depends | DynamicBitSet | Required capabilities (unbounded) |
| provides | DynamicBitSet | Provided capabilities (unbounded) |
| executor | ExecutorKind | Native or WASM execution |

**Reflection Access**: `TargetSchema.viewOf()` enables schema-driven get/set for `depends`/`provides` bitsets with automatic StringInterner translation. Binary IPC uses word arrays for unbounded capability sets.

### Edge Relations

| Type | Purpose |
|------|---------|
| depends_on | DAG dependency tracking |
| provides_capability | Capability registry |
| neighbor_of | Semantic proximity |
| is_a | Ontology hierarchy |

---

## Implementation Status

### Completed ✅

1. **Deterministic Core**: Target registry, TraitSet bitmasks, Kahn's algorithm
2. **SQLite Integration**: Unified backend with SQL + recursive CTEs, cosine similarity in Zig
3. **Context Packing**: LOD pyramid, graph-distance algorithm
4. **WASM Sandboxing**: Extism integration, binary IPC
5. **Reflection Layer**: Unified schema-driven field access for Target and ContextNode
   - `Editable(T)` mixin for comptime-known structs
   - `DynamicEditable` for runtime-defined schemas
   - Role-based permissions enforced on every `set`/`get`
   - Binary and string serialization paths from same accessor table
6. **MCP Server**: JSON-RPC perimeter for agent integration (HTTP + STDIO transports)
7. **YAGO 4.5 Ingestion**: Sparse ontology with whitelist filtering, namespace prefix support
8. **Hybrid Search**: HNSW index + vector similarity, L1-L5 cache hierarchy
9. **Schema-Driven Hydration**: SQL binder and hydrator using accessor tables
10. **Performance Benchmarks**: L1 cache, HNSW build/search, comparison targets

### In Progress 🔄

1. **Local Model Integration**: 3-4B parameter inference for decomposition
2. **Frontier Protocol**: Anonymized LLM fallback for novel queries

### Planned 📋

1. **Evolutionary Improvement**: *Aspirational* - Genetic selection of high-value tools
2. **Advanced Caching**: LRU eviction policies for L1, semantic cache warming

---

## Deployment Profiles

### Edge Profile (Raspberry Pi, Mobile)

- **Memory**: 4-8 GB RAM
- **Storage**: SQLite (persistent)
- **Model**: 3-4B local inference
- **Max Nodes**: ~100K ContextNodes
- **Target Latency**: < 50ms deterministic

### Server Profile (Linux x86_64)

- **Memory**: 16-64 GB RAM
- **Storage**: SQLite (WAL mode, high concurrency)
- **Model**: 8-27B local + optional cloud
- **Max Nodes**: ~1M+ ContextNodes
- **Target Latency**: < 20ms deterministic

---

## Security Model

### WASM Isolation

- No filesystem access (unless explicitly granted)
- No network access from sandboxed tools
- 16MB heap limit, 30s timeout
- Memory-safe execution

### Approved Host Functions

| Function | Purpose |
|----------|---------|
| get_node_lod1 | Fetch node summary |
| get_neighbors | Graph traversal |
| insert_edge | Add relationship |

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

Coral Context represents a fundamental shift in AI architecture: **deterministic execution first, probabilistic inference only when necessary**. By replacing prompt-based LLM reasoning with bitwise DAG traversal, the system achieves:

- **Predictable performance**: Sub-100ms latency for known patterns
- **Zero marginal cost**: No API calls for cached solutions
- **Full auditability**: Every decision traceable through the DAG
- **Continuous improvement**: Each novel solution becomes a permanent capability
- **Edge deployment**: Full functionality on Raspberry Pi-class hardware

The result is an AI system that grows more capable with every use, while remaining fast, cheap, and deterministic.

---

*Vision Document v2.1 — March 2026*
