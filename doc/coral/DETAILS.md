# Coral Context: Detailed Engineering Specification

**Status:** Current (Zig/SQLite implementation, Milestones 3-8 complete)
**Last Updated:** March 2026
**Architecture:** SQLite backend with Zig deterministic core

---

## Executive Summary

Coral Context is a neurosymbolic orchestration framework optimized for edge environments. It systematically decouples probabilistic reasoning (LLMs) from deterministic execution (DAGs and Graph Databases). By utilizing Zig's high-performance, low-memory footprint alongside embedded SQLite, the system delegates workflow routing, knowledge retrieval, and tool orchestration to a strictly typed engine. LLMs are treated solely as unstructured data compilers accessed over HTTP, preventing hallucinations in the critical path and enabling complex agentic behaviors on devices as small as a Raspberry Pi.

**Core Technologies:**
- **Zig** for bitwise, deterministic DAG execution
- **SQLite** (embedded relational database) for semantic graphs and recursive CTE traversal
- **Extism** for secure WASM sandboxing
- **HTTP LLMs** for probabilistic inference

---

## Component 1: Deterministic Core (Zig + DAG)

### TargetRegistry

**File:** `src/common/registry.zig`

The TargetRegistry manages build targets and their dependency graph:

```
TargetRegistry
├── targets: HashMap(name → *Target)
├── interner: *StringInterner (shared string pool)
└── arena: ArenaAllocator (owns all Target memory)
```

**Key Operations:**
- `target("name", .file)` — Create a TargetBuilder for fluent configuration
- `add(target)` — Register a target in the registry
- `get(name)` — Look up target by name
- `validate()` — Check dependency graph for cycles and missing deps

**TraitSet Matching:**
Targets use `depends` and `provides` bitmasks for capability matching:

```
Target
├── id: i64 (time-sortable)
├── name: []const u8
├── depends: DynamicBitSetUnmanaged (required capabilities)
├── provides: DynamicBitSetUnmanaged (provided capabilities)
├── executor: ExecutorKind (.native, .wasm)
└── command: ?[]const u8
```

Bitmasks are backed by `StringInterner`, which assigns unique 32-bit IDs to strings, enabling O(1) capability intersection tests via `@popCount`.

### Kahn's Algorithm for Topological Sort

**File:** `src/common/registry.zig`

Dependency resolution uses Kahn's algorithm:

1. Compute indegree for each target (count of dependencies)
2. Initialize queue with zero-indegree targets
3. Pop target, decrement indegree of dependents
4. Add newly zero-indegree targets to queue
5. Detect cycle if any target remains unprocessed

**Time Complexity:** O(V + E) for V targets, E dependencies

---

## Component 2: SQLite Backend

### Schema

**File:** `src/coral/db.zig`

```sql
CREATE TABLE IF NOT EXISTS context_nodes (
    id INTEGER PRIMARY KEY,
    lod0_full TEXT,
    lod1_summary TEXT,
    lod2_brief TEXT,
    lod3_tiny TEXT,
    lod4_name TEXT,
    embedding BLOB,
    confidence INTEGER,
    provenance_id INTEGER,
    valid_from REAL,
    valid_to REAL
);

CREATE TABLE IF NOT EXISTS edges (
    id INTEGER PRIMARY KEY,
    source_id INTEGER,
    target_id INTEGER,
    relation TEXT,
    FOREIGN KEY (source_id) REFERENCES context_nodes(id),
    FOREIGN KEY (target_id) REFERENCES context_nodes(id)
);

CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_id);
CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_id);
CREATE INDEX IF NOT EXISTS idx_edges_relation ON edges(relation);
```

### Library API

```zig
pub const Library = struct {
    db: *c.sqlite3,
    
    // Node operations
    pub fn insertNode(self: *Self, node: ContextNode) !i64;
    pub fn fetchNode(self: *Self, id: i64) !?ContextNode;
    pub fn deleteNode(self: *Self, id: i64) !void;
    
    // Edge operations
    pub fn insertEdge(self: *Self, edge: Edge) !void;
    pub fn getNeighbors(self: *Self, id: i64) ![]i64;
    
    // Semantic search
    pub fn knnSearch(self: *Self, embedding: []const f32, k: usize) ![]SearchResult;
};
```

### Cosine Similarity (In-Process)

Vector similarity is computed in Zig, not delegated to SQLite:

```zig
fn cosineSimilarity(a: []const f32, b: []const f32) f32 {
    var dot: f32 = 0;
    var norm_a: f32 = 0;
    var norm_b: f32 = 0;
    for (a, b) |x, y| {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    return dot / (@sqrt(norm_a) * @sqrt(norm_b));
}
```

---

## Component 3: Reflection Layer

### Editable(T) — Zero-Size Mixin

**File:** `src/reflection/accessor.zig`

`Editable(T)` adds schema-driven field access to any struct at compile time:

```zig
const Config = struct {
    port: u16 = 8080,
    host: []const u8 = "localhost",
    enabled: bool = false,
    editable: Editable(Config) = .{},
};

var config: Config = .{};

// String path — for boundaries: JSON input, user input, DB rows
try config.editable.set(allocator, "port", "9000", .coder);
const s = try config.editable.get(allocator, "port", .coder);

// Fast path — for hot code: zero vtable call, zero allocation
config.editable.setFast("port", @as(u16, 9001));
const v = config.editable.getFast("port"); // returns u16 directly
```

### ConstraintVTable

**File:** `src/reflection/accessor.zig`

Type-erased access to any field:

```zig
pub const ConstraintVTable = struct {
    setFn: *const fn (Allocator, *anyopaque, []const u8) anyerror!void,
    getFn: *const fn (Allocator, *const anyopaque) anyerror![]const u8,
    // Optional paths
    context: ?*const anyopaque = null,
    releaseFn: ?*const fn (Allocator, *anyopaque) void = null,
    convertFn: ?*const fn (...) anyerror!void = null,
    setCtxFn: ?*const fn (...) anyerror!void = null,
    getCtxFn: ?*const fn (...) anyerror![]const u8 = null,
    setBinaryFn: ?*const fn (...) anyerror!usize = null,
    getBinaryFn: ?*const fn (...) anyerror!void = null,
};
```

### DynamicEditable

For runtime-defined schemas (DB rows, WASM tool configs):

```zig
var buffer align(4) = [_]u8{0} ** 8;
const accessors = [_]Accessor{
    .{ .name = "port", .offset = 0, .permissions = perm_coder, .constraint = &u16_vtable },
    .{ .name = "value", .offset = 4, .permissions = perm_coder, .constraint = &f32_vtable },
};

var dyn = try DynamicEditable.init(allocator, &buffer, &accessors);
defer dyn.deinit();

try dyn.set("port", "9000", .coder);
```

---

## Component 4: WASM Execution

### Binary IPC

**File:** `src/wasm/wasm.zig`

All host/guest communication uses `extern struct align(1)`:

```zig
pub const BinaryHeader = extern struct {
    magic: u32 align(1),      // 0xC04A_C0DE
    version: u8 align(1),     // BINARY_SCHEMA_VERSION
    payload_type: PayloadType align(1),
    _pad: [2]u8 align(1) = .{0, 0},
};

pub const BinaryExecutionRequest = extern struct {
    header: BinaryHeader align(1),
    target_id: i64 align(1),
    input_offset: u32 align(1),
    input_len: u32 align(1),
    flags: u32 align(1),
};
```

**Layout Rules:**
- All fields have `align(1)` — no padding between fields
- Variable-length data uses offset + length fields
- Offsets are absolute from buffer start
- Magic number validates integrity on receipt

### Extism Integration

```zig
const rc = extism_plugin_call(plugin, "execute", buf.items.ptr, buf.items.len);
if (rc != 0) {
    const err = extism_plugin_error(plugin);
    return error.WasmExecutionFailed;
}
const output = extism_plugin_output(plugin);
```

---

## Component 5: Cache Hierarchy (L1-L5)

### Tiering

| Tier | Latency | Coverage | Implementation |
|------|---------|----------|----------------|
| L1 | < 1ms | Exact query hash | In-memory HashMap |
| L2 | < 50ms | WASM tool | Extism sandbox |
| L3 | < 200ms | Graph traversal | SQLite recursive CTE |
| L4 | < 500ms | Semantic search | HNSW + cosine similarity |
| L5 | > 500ms | Frontier LLM | HTTP client |

### L1: Memory Cache

```zig
pub const L1Cache = struct {
    entries: HashMap(u64, CacheEntry),
    max_size: usize,
    
    pub fn get(self: *Self, query_hash: u64) ?CacheEntry;
    pub fn put(self: *Self, query_hash: u64, entry: CacheEntry) !void;
};
```

### L2: WASM Tool Execution

```zig
pub const WasmTool = struct {
    plugin: *extism.Plugin,
    editable: DynamicEditable,
    
    pub fn execute(self: *Self, input: []const u8) !ExecutionResult;
};
```

### L3: Graph Traversal

```sql
-- Recursive CTE for graph traversal
WITH RECURSIVE reachable(id, depth) AS (
    SELECT id, 0 FROM context_nodes WHERE id = ?
    UNION
    SELECT e.target_id, r.depth + 1
    FROM edges e
    JOIN reachable r ON e.source_id = r.id
    WHERE r.depth < ?
)
SELECT * FROM reachable;
```

### L4: HNSW Index

**File:** `src/vector/hnsw.zig`

Hierarchical Navigable Small World graph for approximate nearest neighbor search:

```zig
pub const HnswIndex = struct {
    nodes: AutoHashMapUnmanaged(i64, Node),
    entry_point: ?i64,
    m: usize = 16,              // max connections
    ef_construction: usize = 200,
    ef_search: usize = 50,
    
    pub fn add(self: *Self, id: i64, vector: []const f32) !void;
    pub fn search(self: *Self, query: []const f32, k: usize) ![]SearchResult;
};
```

**Performance Targets:**
- Build time < 100ms for 10K nodes (release mode)
- Search time < 1ms per query (release mode)
- Recall@10 > 95%

---

## Component 6: YAGO Ontology

### Namespace Filtering

**File:** `src/ontology/yago.zig`

```zig
pub const NS_YAGO = "http://yago-knowledge.org/resource/";
pub const NS_SCHEMA = "http://schema.org/";
pub const NS_RDF = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const NS_RDFS = "http://www.w3.org/2000/01/rdf-schema#";
pub const NS_OWL = "http://www.w3.org/2002/07/owl#";
pub const NS_SKOS = "http://www.w3.org/2004/02/skos/core#";

pub fn whitelistIRIs(out: [][]const u8) usize {
    var i: usize = 0;
    for (ALL_CLASSES) |cls| {
        if (i >= out.len) break;
        out[i] = cls.iri;
        i += 1;
    }
    return i;
}
```

### Capability Inference

**File:** `src/ontology/inference.zig`

```zig
pub const CapabilityInference = struct {
    classes: HashMap(IRI, ClassInfo),
    subclass_cache: HashMap(IRI, []IRI),
    
    /// Find all ancestors of a class (transitive closure).
    pub fn inferAncestors(self: *Self, class_iri: IRI) []IRI;
    
    /// Check if a class can use a tool designed for a parent class.
    pub fn duckType(self: *Self, subclass: IRI, parent_tool_class: IRI) bool;
};
```

---

## Component 7: HTTP Transport

### MCP Server

**File:** `src/coral/mcp.zig`

JSON-RPC over STDIO:

```
Client → Server:  {"jsonrpc":"2.0","method":"tools/list","id":1}
Server → Client:  {"jsonrpc":"2.0","result":{"tools":[...]},"id":1}
```

### HTTP Transport

**File:** `src/coral/http_transport.zig`

REST endpoints:

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/mcp` | POST | JSON-RPC handler |
| `/health` | GET | `{"status":"ok"}` |
| `/metrics` | GET | Prometheus format |
| `/events` | GET | SSE stream |

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

### Phase 2: Semantic Search (Cached)

```
Input: "Extract tables from quarterly report"
  ↓
Embedding: 768-dim vector (via EmbeddingProvider)
  ↓
L4 Cache: HNSW KNN search
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
Next query: Deterministic execution
```

---

## Sequence Diagrams

### Query Routing Flow

```
┌────────┐     ┌────────┐     ┌────────┐     ┌────────┐     ┌────────┐
│ Client │     │ Router │     │ L1     │     │ L4     │     │ L5     │
└───┬────┘     └───┬────┘     └───┬────┘     └───┬────┘     └───┬────┘
    │              │              │              │              │
    │ query        │              │              │              │
    │─────────────>│              │              │              │
    │              │              │              │              │
    │              │ hash(query)  │              │              │
    │              │─────────────>│              │              │
    │              │              │              │              │
    │              │              │ hit?         │              │
    │              │<─────────────│              │              │
    │              │              │              │              │
    │              │ [miss] embed │              │              │
    │              │─────────────────────────────>│              │
    │              │              │              │              │
    │              │              │              │ KNN search   │
    │              │              │              │─────────────>│
    │              │              │              │              │
    │              │              │              │ [fallback]  │
    │              │              │              │<─────────────│
    │              │              │              │              │
    │              │ result       │              │              │
    │<─────────────│              │              │              │
    │              │              │              │              │
```

---

## Performance Targets

| Metric | Target |
|--------|--------|
| L1 cache hit | < 1ms |
| L2 WASM execution | < 50ms |
| L3 graph traversal | < 200ms (100-node graph) |
| L4 semantic search | < 500ms (10K nodes) |
| L5 frontier (mock) | < 100ms |
| ContextNode hydration | < 1ms per node |
| Reflection set/get | < 10µs |
| Memory footprint (edge) | < 500MB |
| Binary size (edge) | < 50MB |

---

## File Reference

| Component | File | Description |
|-----------|------|-------------|
| TargetRegistry | `src/common/registry.zig` | DAG registry |
| StringInterner | `src/common/interner.zig` | Shared string pool |
| ContextNode | `src/coral/context_node.zig` | LOD text + embedding |
| Library | `src/coral/db.zig` | SQLite backend |
| HnswIndex | `src/vector/hnsw.zig` | ANN search |
| Editable(T) | `src/reflection/accessor.zig` | Reflection mixin |
| Binary IPC | `src/wasm/wasm.zig` | WASM serialization |
| YAGO types | `src/ontology/yago.zig` | Class registry |
| QueueReactor | `src/coral/cache.zig` | L1-L5 routing |
| HttpTransport | `src/coral/http_transport.zig` | REST + SSE |
| McpServer | `src/coral/mcp.zig` | JSON-RPC STDIO |

---

*Specification v2.1 — March 2026*