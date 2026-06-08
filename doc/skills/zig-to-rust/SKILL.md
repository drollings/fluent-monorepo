# Zig to Rust Practices: Master Guideline for the `rust-src/` Rewrite

**Date:** 2026-06-08
**Audience:** Follow-up coding agents implementing the Rust rewrite.
**Goal:** Minimize boilerplate, eliminate cognitive load, and ensure the final Rust code is lean, idiomatic, and powerful.

---

## 1. Fluent WVR in Rust

### 1.1 The Core Thesis

In Zig, Fluent WVR achieved Python-like ergonomics through **five patterns**:
1. Fluent Builder (manual `*Self` structs with arena-backed error accumulation)
2. Comptime Reflection (`Editable(T)`, `DynamicEditable`, `ConstraintVTable`)
3. Comptime Wrappers (zero-cost cross-cutting logic via `comptime` functions)
4. Explicit VTables (`{ptr, vtable}` handles for runtime polymorphism)
5. Arena-Backed Builders (batch allocation scopes)

In Rust, we replace this entire machinery with **four standard tools**:
- **`bon`** — generates zero-boilerplate fluent builders.
- **`dyn Trait`** — replaces explicit vtables with compiler-generated ones.
- **`serde`** — replaces comptime reflection for boundary serialization.
- **`ArcIntern<str>` / `smol_str`** — replaces arena-allocated string interning.

The result is **less code, not more**. A Zig builder that required 200 lines of manual struct definition, arena management, and `BuilderError` accumulation becomes a single `#[derive(bon::Builder)]` annotation.

### 1.2 Builders: From Zig Arena to `bon`

#### Zig Pattern (Manual)

```zig
// 200+ lines in src/common/registry.zig
pub const TargetBuilder = struct {
    allocator: std.mem.Allocator,
    arena: std.heap.ArenaAllocator,
    registry: *TargetRegistry,
    interner: *StringInterner,
    target: ?*Target,
    err: ?*BuilderError,
    err_any: ?anyerror,

    pub fn depends(self: *TargetBuilder, names: []const []const u8) *TargetBuilder {
        if (self.hasError() or self.target == null) return self;
        self.target.?.setDepends(...) catch |cause| {
            self.setError(.depends, "depends", value, "invalid_reference", cause);
        };
        return self;
    }

    pub fn register(self: *TargetBuilder) !void {
        defer self.arena.deinit();
        if (self.err) |e| return e.cause;
        try self.registry.add(self.target.?);
        self.target = null;
    }
};
```

**Call site:**
```zig
try registry.target("build", .file)
    .depends(&.{"compile", "link"})
    .provides(&.{"artifact"})
    .command("zig build -Doptimize=ReleaseFast")
    .essential()
    .register();
```

#### Rust Replacement (`bon`)

```rust
use bon::Builder;
use bitvec::vec::BitVec;
use internment::ArcIntern;

#[derive(Debug, Clone, Builder)]
#[builder(start_fn = new)]
pub struct Target {
    pub id: i64,
    pub name: ArcIntern<str>,
    pub target_type: TargetType,
    pub depends: BitVec,
    pub provides: BitVec,
    #[builder(default)]
    pub command: String,
    #[builder(default = false)]
    pub essential: bool,
}

// TargetBuilder is generated automatically by bon.
// It is named `TargetBuilder` by convention.
```

**Call site:**
```rust
use common::registry::{Target, TargetRegistry};

let target = Target::builder()
    .id(1)
    .name("build".into())
    .target_type(TargetType::File)
    .depends(registry.capabilities().to_bitvec(&["compile", "link"]))
    .provides(registry.capabilities().to_bitvec(&["artifact"]))
    .command("cargo build --release")
    .essential(true)
    .build();

registry.register(target)?;
```

#### Key Differences & Rules

| Zig Concept | Rust Equivalent | Rule |
|-------------|----------------|------|
| `arena: std.heap.ArenaAllocator` | **No arena.** Builder owns its fields via `String`, `Vec`, `BitVec`. | `bon` builders are owned structs; no lifetime trickery needed. |
| `err: ?*BuilderError` | `Result<TargetBuilder, RegistryError>` | Validation moves to `build()` or `register()`. Use `?` propagation. |
| `*Self` return for chaining | `Self` return (bon generates this) | Bon handles chaining automatically. |
| Terminal `register()` frees arena | `build()` consumes the builder; Rust drops it | No manual deinit. |
| Factory on registry (`registry.target(...)`) | Free function or `Target::builder()` + `registry.register(t)` | Decouple construction from registration for clearer ownership. |

**Anti-pattern:** Do NOT write manual builder structs in Rust. If `bon` cannot express a pattern (e.g., conditional fields that affect other fields), write a custom `impl` block on the generated builder or use a two-step pattern:

```rust
// Step 1: infallible bon builder
let args = TargetCreateArgs::builder()
    .name("build".into())
    .target_type(TargetType::File)
    .build();

// Step 2: fallible registry validation
let target = registry.validate_and_allocate(args)?;
```

### 1.3 VTables: From `{ptr, vtable}` to `dyn Trait`

#### Zig Pattern (Explicit VTable)

```zig
pub const EmbeddingProvider = struct {
    ptr: *anyopaque,
    vtable: *const VTable,

    pub const VTable = struct {
        name: *const fn (ptr: *anyopaque) []const u8,
        dimensions: *const fn (ptr: *anyopaque) u32,
        embed: *const fn (ptr: *anyopaque, allocator: std.mem.Allocator, text: []const u8) anyerror![]f32,
        deinit: *const fn (ptr: *anyopaque) void,
    };

    pub fn embed(self: EmbeddingProvider, allocator: std.mem.Allocator, text: []const []const u8) ![]f32 {
        return self.vtable.embed(self.ptr, allocator, text);
    }
};
```

#### Rust Replacement (`dyn Trait`)

```rust
use async_trait::async_trait;
use std::sync::Arc;

#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    fn name(&self) -> &str;
    fn dimensions(&self) -> u32;
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError>;
    async fn embed_batch(&self, texts: &[&str]) -> Result<BatchEmbedding, EmbedError>;
}

// No manual vtable struct. The compiler generates it.
// Storage in a registry:
pub struct ProviderRegistry {
    providers: Vec<Arc<dyn EmbeddingProvider>>,
}

impl ProviderRegistry {
    pub fn register(&mut self, provider: Arc<dyn EmbeddingProvider>) {
        self.providers.push(provider);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn EmbeddingProvider>> {
        self.providers.iter().find(|p| p.name() == name).cloned()
    }
}
```

**Call site:**
```rust
let provider: Arc<dyn EmbeddingProvider> = create_provider("ollama")?;
let vec = provider.embed("hello world").await?;
```

#### Rules for VTable Translation

1. **Always add `Send + Sync`** to traits that will be stored in registries or shared across threads. This replaces Zig's debug-build `thread_id` assertions with compile-time enforcement.
2. **Use `Arc<T>` for shared ownership** of implementations. Zig's `*anyopaque` was a borrowed pointer; in Rust, the registry typically owns the provider.
3. **Do NOT use `Box<dyn Trait>` if the handle will be cloned.** Use `Arc<dyn Trait>`.
4. **Avoid `dyn Trait` when there is only one implementation.** Use generics (`impl Trait` or `<T: EmbeddingProvider>`) for hot loops to avoid vtable dispatch.

### 1.4 Reflection: From `Editable(T)` to `serde`

#### Zig Pattern (Comptime Reflection)

```zig
const Config = struct {
    port: u16 = 8080,
    host: []const u8 = "localhost",
    editable: Editable(Config) = .{},
};

// String boundary (JSON input, DB row, RPC)
try config.editable.set(allocator, "port", "9000", .coder);

// Fast path (hot code, zero alloc)
config.editable.setFast("port", @as(u16, 9001));
```

#### Rust Replacement (`serde` + direct field access)

```rust
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_host")]
    pub host: ArcIntern<str>,
}

fn default_port() -> u16 { 8080 }
fn default_host() -> ArcIntern<str> { "localhost".into() }

// String boundary (JSON input)
let config: Config = serde_json::from_str(json)?;

// Fast path (hot code)
config.port = 9001;
```

#### Runtime Dynamic Access (WASM tool configs, DB rows)

Zig's `DynamicEditable` used offset-based field access via `Accessor` + `ConstraintVTable`. In Rust, use `serde_json::Value` or a typed map:

```rust
use std::collections::HashMap;
use internment::ArcIntern;

/// Runtime schema for WASM tool configs.
pub type DynamicConfig = HashMap<ArcIntern<str>, serde_json::Value>;

// Set by name (string boundary)
config.insert("port".into(), serde_json::Value::Number(9000.into()));

// Get by name with type checking
let port: u16 = config.get("port")
    .and_then(|v| v.as_u64())
    .ok_or(ConfigError::MissingField("port"))? as u16;
```

**Rule:** Do **not** try to recreate `Editable(T)` with procedural macros or pointer arithmetic. It is unnecessary in Rust. `serde` handles 95% of boundary serialization; `HashMap` handles the remaining 5% of runtime-dynamic access.

### 1.5 String Interning: `ArcIntern<str>` and `smol_str`

#### Zig Pattern

```zig
var interner = StringInterner.init(allocator);
const idx = try interner.intern("hello");
const name = interner.getString(idx).?;
```

#### Rust Replacement

```rust
use internment::ArcIntern;

// Global, thread-safe, deduplicated, copy-on-write.
let s1: ArcIntern<str> = "hello".into();
let s2: ArcIntern<str> = "hello".into();
assert!(ArcIntern::ptr_eq(&s1, &s2)); // Same underlying allocation.

// Cheap to clone: just an Arc bump.
let s3 = s1.clone();
```

For **small, hot-path strings** (AST tokens, identifiers, keywords):

```rust
use smol_str::SmolStr;

let token: SmolStr = "identifier".into();
// Stack-allocated if ≤ 23 bytes; heap otherwise.
```

**Decision matrix:**

| Use Case | Type | Why |
|----------|------|-----|
| Capability names, target names, registry keys | `ArcIntern<str>` | Deduplication across the whole process. |
| AST tokens, parameter names, signatures | `SmolStr` | Fast stack allocation, no refcount overhead for short-lived data. |
| LOD text, file content, HTTP bodies | `String` / `Arc<str>` | Owned, potentially large. |

---

## 2. Memory Translation: From Arena Allocators to Strict Ownership

### 2.1 The Zig Way

Zig's `ArenaAllocator` scopes all intermediate allocations to a logical unit of work:

```zig
var arena = std.heap.ArenaAllocator.init(allocator);
defer arena.deinit();
const a = arena.allocator();

// Hundreds of intermediate allocations from `a`
var mapper = TripleMapper.init(a, ...);
try mapper.flush(); // only escaped data survives
// arena.deinit() frees everything else at once
```

### 2.2 The Rust Way

Rust has no arena allocator (and we are forbidden from using `bumpalo`). Instead, **ownership scopes** naturally replicate the arena pattern:

```rust
{
    // Logical unit of work: a batch ingestion
    let mut mapper = TripleMapper::new(&library, &config);
    // All intermediate Vecs and Strings are owned by `mapper` or local variables
    mapper.process_triples(triples)?;
    mapper.flush()?; // escaped data is cloned into `library`
} // <-- all locals are dropped automatically. No manual deinit.
```

### 2.3 Builder Ownership Transfer

In Zig, the builder's terminal method (`register()`, `build()`, `sync()`) either committed data to a long-lived allocator or discarded the arena. In Rust, the builder **transfers ownership** to the registry:

```rust
// Zig: arena owns intermediate strings; registry owns target after register()
// Rust: builder owns args; build() returns an owned Target; registry takes it

let target = Target::builder()
    .name("build".into())
    .depends(bitvec![0, 1, 1])
    .build(); // consumes builder, returns Target

registry.register(target)?; // registry now owns Target
```

**If the builder needs to allocate intermediate strings** (e.g., joining a list for error messages), those strings are owned by the builder struct and dropped when `build()` consumes it.

### 2.4 Per-Request Scoping (MCP Server)

Zig:
```zig
pub fn handleRequest(self: *McpServer, raw_json: []const u8) ![]const u8 {
    var req_arena = std.heap.ArenaAllocator.init(self.allocator);
    defer req_arena.deinit();
    const req = try parseJsonRpc(req_arena.allocator(), raw_json);
    const result = try self.reactor.route(req.query);
    return try serializeResponse(self.allocator, result);
}
```

Rust:
```rust
async fn handle_request(&self, raw_json: &str) -> Result<String, McpError> {
    // No arena. Parse into owned structs.
    let req: JsonRpcRequest = serde_json::from_str(raw_json)?;
    let result = self.reactor.route(&req.query).await?;
    // `req` and `result` are dropped at the end of this scope.
    Ok(serde_json::to_string(&result)?)
}
```

**Rule:** Trust the stack and `Drop`. Do not pre-allocate reusable buffers unless profiling proves allocation is a bottleneck.

---

## 3. DAG Resolution: Custom Topological Engine + `bitvec`

### 3.1 Why Custom?

`petgraph` is forbidden. The DAG in this codebase is not a generic graph problem; it is a **capability-matched dependency graph** with these properties:
- Nodes have `depends` and `provides` bitsets.
- Edge direction is implied by bitset overlap.
- Graph size is small (hundreds to low-thousands of nodes).
- We need topological sort, cycle detection, and execution planning.

A custom implementation using `HashMap` and `Vec` is ~150 lines and gives us full control over bitset matching.

### 3.2 Data Structures

```rust
use bitvec::vec::BitVec;
use internment::ArcIntern;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Target {
    pub bit_index: usize,
    pub name: ArcIntern<str>,
    pub depends: BitVec,
    pub provides: BitVec,
    pub essential: bool,
    pub command: String,
}

pub struct TargetRegistry {
    targets: Vec<Target>,
    by_name: HashMap<ArcIntern<str>, usize>,
    by_bit_index: HashMap<usize, usize>, // bit_index -> targets index
    providers: HashMap<usize, Vec<usize>>, // capability_idx -> target indices
}
```

### 3.3 Capability Matching with `bitvec`

```rust
impl TargetRegistry {
    /// Returns targets whose `provides` bitset covers all bits in `required`.
    pub fn find_providers(&self, required: &BitVec) -> Vec<&Target> {
        self.targets.iter()
            .filter(|t| {
                let prov = &t.provides;
                // `prov` covers `required` if (required & !prov) is empty
                let missing: BitVec = required & !prov;
                missing.not_any()
            })
            .collect()
    }
}
```

### 3.4 Topological Sort (Kahn's Algorithm)

```rust
use std::collections::{HashMap, VecDeque};

pub struct ExecutionPlan {
    pub order: Vec<usize>, // bit_indices in topological order
}

#[derive(Debug, thiserror::Error)]
pub enum ResolverError {
    #[error("circular dependency detected")]
    CircularDependency,
    #[error("target not found: {0}")]
    TargetNotFound(String),
}

pub fn resolve(registry: &TargetRegistry, goal_names: &[&str]) -> Result<ExecutionPlan, ResolverError> {
    // 1. Collect all transitive dependencies via DFS
    let mut needed: HashMap<usize, &Target> = HashMap::new();
    let mut stack: Vec<usize> = goal_names.iter()
        .map(|n| registry.by_name.get(*n).copied())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| ResolverError::TargetNotFound("...".into()))?;

    while let Some(idx) = stack.pop() {
        let target = &registry.targets[idx];
        if needed.contains_key(&target.bit_index) {
            continue;
        }
        needed.insert(target.bit_index, target);

        for dep_idx in target.depends.iter_ones() {
            if let Some(&dep_target_idx) = registry.by_bit_index.get(&dep_idx) {
                stack.push(dep_target_idx);
            } else if strict {
                return Err(ResolverError::TargetNotFound(
                    registry.get_name(dep_idx).unwrap_or("?").into()
                ));
            }
        }
    }

    // 2. Build adjacency and in-degree maps
    let mut in_degree: HashMap<usize, usize> = needed.keys().map(|&k| (k, 0)).collect();
    let mut adj: HashMap<usize, Vec<usize>> = HashMap::new();

    for (&idx, target) in &needed {
        for dep_idx in target.depends.iter_ones() {
            if needed.contains_key(&dep_idx) {
                adj.entry(dep_idx).or_default().push(idx);
                *in_degree.get_mut(&idx).unwrap() += 1;
            }
        }
    }

    // 3. Kahn's algorithm
    let mut queue: VecDeque<usize> = in_degree.iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&k, _)| k)
        .collect();

    // Deterministic ordering: sort queue by bit_index
    let mut queue: Vec<usize> = queue.into_iter().collect();
    queue.sort_unstable();

    let mut order = Vec::with_capacity(needed.len());
    let mut head = 0;

    while head < queue.len() {
        let current = queue[head];
        head += 1;
        order.push(current);

        if let Some(dependents) = adj.get(&current) {
            for &dep in dependents {
                let deg = in_degree.get_mut(&dep).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    queue.push(dep);
                    queue[head..].sort_unstable(); // maintain determinism
                }
            }
        }
    }

    if order.len() != needed.len() {
        return Err(ResolverError::CircularDependency);
    }

    Ok(ExecutionPlan { order })
}
```

### 3.5 Rules for DAG Code

1. **Use `BitVec` for all capability sets.** Do not use `HashSet<String>` or `Vec<bool>`.
2. **Determinism is mandatory.** Always sort queues by `bit_index` before processing. This ensures identical outputs across runs—a core requirement of the deterministic-first philosophy.
3. **Cycle detection is not optional.** Kahn's algorithm naturally detects cycles (if `order.len() != node_count`). Always check.
4. **Abstract dependency resolution** (Zig's `resolveAbstractDependencies`) is a thin wrapper: collect provided capabilities into a `BitVec`, subtract them from requirements, then resolve the remainder.

---

## 4. Binary IPC: `#[repr(C, packed)]` for Extism

### 4.1 The Zig Pattern

Zig used `extern struct align(1)` to guarantee zero-copy, padding-free binary layouts:

```zig
pub const BinaryExecutionRequest = extern struct {
    header:       BinaryHeader align(1),
    target_id:    i64 align(1),
    input_offset: u32 align(1),
    input_len:    u32 align(1),
    flags:        u32 align(1),
};
```

### 4.2 The Rust Replacement

Rust provides `#[repr(C, packed)]` which removes padding and uses C-compatible layout. However, **packed structs have unaligned fields**, so you must use `read_unaligned` / `write_unaligned` or byte-slice mapping.

#### Struct Definition

```rust
#[repr(C, packed)]
pub struct BinaryHeader {
    pub magic: [u8; 4],
    pub version: u32,
    pub payload_type: u32, // enum stored as u32
    pub payload_size: u32,
    pub checksum: u32,
}

#[repr(C, packed)]
pub struct BinaryExecutionRequest {
    pub header: BinaryHeader,
    pub target_id: i64,
    pub input_offset: u32,
    pub input_len: u32,
    pub flags: u32,
}

#[repr(C, packed)]
pub struct BinaryExecutionResult {
    pub header: BinaryHeader,
    pub success: u32,
    pub error_code: u32,
    pub output_offset: u32,
    pub output_len: u32,
    pub provides_words_offset: u32,
    pub provides_words_count: u32,
}
```

#### Safe Encoding (Manual Byte Slice)

**Do not** directly transmute packed structs into byte slices across versions or platforms. Use explicit byte-order writers:

```rust
use std::io::Write;

pub fn encode_request(req: &BinaryExecutionRequest, input: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(
        std::mem::size_of::<BinaryExecutionRequest>() + input.len()
    );

    // Write header fields explicitly in little-endian
    buf.extend_from_slice(&req.header.magic);
    buf.extend_from_slice(&req.header.version.to_le_bytes());
    buf.extend_from_slice(&req.header.payload_type.to_le_bytes());
    buf.extend_from_slice(&req.header.payload_size.to_le_bytes());
    buf.extend_from_slice(&req.header.checksum.to_le_bytes());

    buf.extend_from_slice(&req.target_id.to_le_bytes());
    buf.extend_from_slice(&req.input_offset.to_le_bytes());
    buf.extend_from_slice(&req.input_len.to_le_bytes());
    buf.extend_from_slice(&req.flags.to_le_bytes());

    // Append variable-length input
    buf.extend_from_slice(input);
    buf
}
```

#### Safe Decoding

```rust
use std::convert::TryInto;

pub fn decode_result(buf: &[u8]) -> Result<(BinaryExecutionResult, Vec<u8>), IpcError> {
    if buf.len() < std::mem::size_of::<BinaryExecutionResult>() {
        return Err(IpcError::BufferTooSmall);
    }

    let mut offset = 0;
    let read_u32 = |o: &mut usize| -> u32 {
        let val = u32::from_le_bytes(buf[*o..*o + 4].try_into().unwrap());
        *o += 4;
        val
    };
    let read_u64 = |o: &mut usize| -> u64 {
        let val = u64::from_le_bytes(buf[*o..*o + 8].try_into().unwrap());
        *o += 8;
        val
    };

    let magic = [buf[0], buf[1], buf[2], buf[3]];
    if magic != BINARY_MAGIC {
        return Err(IpcError::InvalidMagic);
    }
    offset = 4;

    let version = read_u32(&mut offset);
    if version != BINARY_SCHEMA_VERSION {
        return Err(IpcError::UnsupportedVersion);
    }
    let payload_type = read_u32(&mut offset);
    let payload_size = read_u32(&mut offset);
    let checksum = read_u32(&mut offset);

    let target_id = read_u64(&mut offset) as i64;
    let success = read_u32(&mut offset);
    // ... etc

    // Extract output slice
    let output = buf[output_offset as usize..][..output_len as usize].to_vec();

    Ok((result, output))
}
```

### 4.3 Bitset Reconstruction from Trailing Words

Zig:
```zig
const w = std.mem.readInt(u64, payload[off + i * 8 ..][0..8], .little);
bs.masks[i] = @intCast(w);
```

Rust:
```rust
pub fn get_provides_bitset(
    result: &BinaryExecutionResult,
    payload: &[u8],
) -> Result<bitvec::vec::BitVec, IpcError> {
    let count = result.provides_words_count as usize;
    let offset = result.provides_words_offset as usize;
    let mut bits = bitvec::vec::BitVec::with_capacity(count * 64);

    for i in 0..count {
        let start = offset + i * 8;
        let word = u64::from_le_bytes(
            payload[start..start + 8].try_into().map_err(|_| IpcError::BufferTooSmall)?
        );
        for bit in 0..64 {
            bits.push((word >> bit) & 1 == 1);
        }
    }
    Ok(bits)
}
```

### 4.4 Rules for Binary IPC

1. **Always use `#[repr(C, packed)]`** for structs that cross the Extism boundary. Never use default Rust layout.
2. **Always encode/decode explicitly** with `to_le_bytes()` / `from_le_bytes()`. Do not rely on `std::mem::transmute` or `bytemuck` (not in the mandated crate list).
3. **Validate magic and version** before reading any other field.
4. **Offsets are absolute from buffer start**, never relative to struct end.
5. **Variable-length data** (input bytes, output bytes, LOD strings, bitset words) is appended after the fixed header. The header contains `offset` and `len` fields pointing to it.

---

## 5. Cross-Cutting Concerns

### 5.1 Error Handling

Zig used error unions (`anyerror!T`) and `BuilderError` for rich context. Rust uses `thiserror` (or manual `std::error::Error`) and the `?` operator:

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum RegistryError {
    #[error("target '{name}' already exists")]
    DuplicateTarget { name: String },
    #[error("invalid capability reference: {0}")]
    InvalidCapability(String),
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
}
```

**Rule:** Every public fallible function returns a specific `Result<T, E>`, not `anyhow::Result` (unless at the CLI boundary). Specific errors enable down-stream `match` logic.

### 5.2 Logging & Tracing

Zig used `std.log` and custom `LogContext` / `Scope` for structured logging. Rust uses `tracing`:

```rust
use tracing::{info, span, Level};

#[tracing::instrument(skip_all, fields(query = %query))]
pub async fn route(&self, query: &str) -> Result<RoutingResult, CacheError> {
    let span = span!(Level::INFO, "cache_route", query);
    let _enter = span.enter();

    if let Some(cached) = self.l1_cache.get(query) {
        info!(tier = "L1", "cache hit");
        return Ok(cached);
    }
    // ...
}
```

**Rule:** Use `#[tracing::instrument]` on all public entry points. This replaces Zig's `callLogged` and `Scope.begin/end` with zero boilerplate.

### 5.3 Concurrency

Zig used `std.Thread` and per-thread arenas. Rust uses `tokio` for async I/O and `rayon` (if needed) for CPU-bound parallelism:

- **SQLite writes:** Use a `tokio::sync::Mutex<rusqlite::Connection>` or a dedicated SQLite actor thread.
- **String interning:** `ArcIntern` is thread-safe by design (global atomic refcount).
- **Embedding providers:** `Arc<dyn EmbeddingProvider + Send + Sync>` can be cloned into tasks.

```rust
let provider = Arc::clone(&self.embedder);
tokio::spawn(async move {
    let vec = provider.embed("text").await?;
    Ok(vec)
});
```

---

## 6. Summary: Translation Cheat Sheet

| Zig Pattern | Rust Equivalent | When to Use |
|-------------|----------------|-------------|
| `TargetBuilder` (manual) | `#[derive(bon::Builder)]` | Always. Never write manual builders. |
| `ArenaAllocator` | Owned `Vec`/`String` + scope drop | Always. No arenas. |
| `{ptr, vtable}` | `dyn Trait` + `Arc<dyn Trait>` | 2+ implementations; registry storage. |
| `comptime` generic | `impl Trait` or `<T: Trait>` | Single impl or hot loops (zero-cost). |
| `Editable(T)` / `DynamicEditable` | `serde` + `HashMap` | Boundary serialization / runtime dynamic access. |
| `StringInterner` | `ArcIntern<str>` | Deduplicated global strings. |
| `DynamicBitSetUnmanaged` | `bitvec::BitVec` | Capability bitsets, adjacency masks. |
| `extern struct align(1)` | `#[repr(C, packed)]` | Extism binary IPC. |
| `std.log` + `Scope` | `tracing` + `#[instrument]` | Structured logging. |
| `std.Thread` | `tokio` (async I/O) / `std::thread` (blocking) | Concurrency. |
| `anyerror!T` | `Result<T, SpecificError>` | Error handling. |
| `try` everywhere | `?` operator | Error propagation. |
| `defer` | `Drop` impl / RAII | Cleanup. |

---

## 7. Agent Directives

When writing implementation code:

1. **Check `bon` first.** If a struct needs multi-parameter construction, derive `Builder`. Do not write `impl FooBuilder { ... }` by hand.
2. **Check the checklist.** Tick boxes in `ROADMAP_20260608_ZIG_TO_RUST_CHECKLIST.md` as you complete modules.
3. **Verify against capability docs.** Every module must have a corresponding `rust-src/doc/capabilities/<name>/CAPABILITY.md` that references the original Zig doc.
4. **No arenas. No `bumpalo`.** If you find yourself wanting an arena, use a scoped function with local `Vec`s.
5. **No `petgraph`.** If you need graph algorithms, write them using `HashMap` + `Vec`.
6. **No `typetag`.** If you need trait object serialization, use an enum wrapper (`enum Provider { Ollama(...), OpenAi(...) }`) or manual dispatch.
7. **Prefer `ArcIntern<str>` over `String`** for keys, names, and registry entries.
8. **Prefer `smol_str` over `String`** for AST tokens and small identifiers.
9. **Write tests with the module.** Every `.rs` file should have a `#[cfg(test)] mod tests { ... }` block.
10. **Run `cargo clippy` before finishing.** The project enforces `#![deny(warnings)]`.

---

*End of Practices Document*
