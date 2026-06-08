# Fluent WVR in Rust — The Synthesis Pipeline

**A design pattern guide for human coders and AI agents working in `rust-src/`.**

**Supersedes:** `FLUENT_WVR_RUST.md` (the raw-pointer variant is deprecated; this document uses idiomatic Rust throughout).

**Companion:** `doc/skills/fluent-wvr/SKILL.md` (the Zig original — authoritative for `src/`).

---

## Who This Document Is For

**For humans coming from Python or C++**: Rust's trait system replaces inheritance, its ownership model replaces garbage collection, and its derive macros replace runtime reflection. This document shows how twelve composable patterns together give you Python's runtime ergonomics and C++'s polymorphic flexibility, with absolute memory safety and zero hidden cost.

**For AI agents**: This document is the authoritative reference for how code in `rust-src/` is structured. When writing new code, check the pattern table in §1, find the applicable pattern, and follow the rules and anti-pattern sections. All patterns are in production use in the codebase.

---

## The Core Thesis

Each pattern in this document is a local improvement in isolation. Composed together, they form a system where **expressiveness, type safety, observability, and runtime efficiency are simultaneously maximised rather than traded off against each other.**

The data flow through the full pipeline demonstrates this:

```
Developer writes:   Target::builder().name("build").depends(bits).build()
                           ↓
bon::Builder:      Generates chained setter methods at compile time (zero cost)
                           ↓
Validation:        build() returns Result; ? propagation at call site (type safety)
                           ↓
FieldAccess:       component.set_field("port", "9000")?  (runtime configuration)
                           ↓
Trait Object:      Arc<dyn Component> stored in registry (uniform interface)
                           ↓
Runtime:           Orchestrator calls unit.execute() — no branching needed (efficiency)
```

The developer writes a single declarative construction chain. The compiler generates builder methods, validates types, and produces a trait object. The component can be configured by name at runtime, wrapped with middleware, and executed uniformly. The orchestrator sees a uniform interface — it never branches on implementation type.

---

## The Core Concept: The Unified Process Boundary

In C++, polymorphism hides behind virtual dispatch tables injected by the compiler. In Python, duck typing lets anything with the right methods work — until it doesn't. Both approaches obscure the boundary between components.

Fluent WVR in Rust makes the boundary **explicit and uniform**. Every unit of work — a DAG target, a WASM plugin, a query strategy, an embedding provider — presents the same interface to the orchestrator. The orchestrator never branches on implementation type. It iterates over uniform handles and calls trait methods. The compiler enforces the interface at every implementation site.

The result: **construction reads like configuration, validation surfaces at exactly one point, dispatch is uniform, serialization is automatic, runtime configuration is type-safe, and cross-cutting concerns compose without modifying business logic.**

### The Component Supertrait

At the heart of the system is the `Component` supertrait — the unified interface for anything that can be **built**, **configured by name**, **described**, and **executed**:

```rust
pub trait Component: FieldAccess + Describable + WorkUnit + Send + Sync {}
```

Every orchestratable task in the system implements `Component`. Whether it's a compiled Rust struct, a WASM plugin, or a database-driven configuration, the orchestrator sees the same interface. This is the "unified process boundary" made concrete.

---

## 1. The Synthesis Pipeline at a Glance

Twelve core patterns compose into a single coherent architecture.

| Pattern | Problem solved | Cost | Primary source |
|---------|---------------|------|---------------|
| **Fluent Builder** | Multi-parameter init that callers can't read | Zero — `#[derive(bon::Builder)]` | `common/src/registry.rs` |
| **Trait-Based Reflection** | Schema defined in multiple places; runtime config by name | Zero — `serde` + `FieldAccess` derive | `common/src/types.rs` |
| **Trait Composition** | Cross-cutting logic duplicated across handlers | Zero — newtype wrappers | `common/src/wrapper.rs` |
| **Trait Objects** | Runtime polymorphism with branching in hot loops | One vtable pointer per `dyn Trait` | `common/src/embeddings.rs` |
| **Binary IPC** | Executing untrusted code across a WASM boundary | Memcpy + `#[repr(C, packed)]` | `wasm_ipc/src/lib.rs` |
| **Scoped Ownership** | Repeated malloc/free in batch processing | Zero — RAII scope drop | Throughout |
| **Newtype Handles** | ID type confusion across module boundaries | Zero — same representation | `common/src/types.rs` |
| **Unit of Work** | Uniform orchestration of heterogeneous tasks | One trait impl per task | `dag/src/work_unit.rs` |
| **Middleware Chain** | Composable cross-cutting on trait objects | One allocation per layer | `dag/src/middleware.rs` |
| **Component Adapter** | Runtime type adaptation without losing uniform interface | One `Arc` per adapter | `dag/src/adapter.rs` |
| **Structured Logging Context** | Request-scoped observability without manual context passing | Thread-local storage | `common/src/logging.rs` |
| **Runtime Composition** | Full lifecycle: build → configure → wrap → execute → inspect | Zero — uses above patterns | Throughout |

The data flow through the full pipeline:

```
Developer writes:   Target::builder().name("build").depends(bits).build()
                           ↓
bon::Builder:      Generates chained setter methods at compile time
                           ↓
Validation:        build() returns Result; ? propagation at call site
                           ↓
FieldAccess:       component.set_field("port", "9000")?  // configure by name
                           ↓
Trait Object:      Arc<dyn Component> stored in registry
                           ↓
Runtime:           Orchestrator calls unit.execute() — no branching needed
```

The developer writes a single declarative construction chain. The compiler generates builder methods, validates types, and produces a trait object. The component can be configured by name at runtime, wrapped with middleware, and executed uniformly. The orchestrator sees a uniform interface — it never branches on implementation type.

---

## 2. Pattern 1 — Fluent Builder

### The problem

```cpp
// C++: Can you tell what the 4th argument means?
Target t("build", TargetType::File, {"compile", "link"}, {"artifact"}, true, "zig build");
```

```python
# Python: kwargs help, but errors surface at runtime, mid-construction
target = Target(name="build", depends=["compile"], provides=["artifact"], essential=True)
```

### The Rust solution

`#[derive(bon::Builder)]` generates zero-boilerplate fluent builders. Validation moves to `build()` or a separate `register()` step. The `?` operator replaces error accumulation.

### Canonical implementation: `Target` in `common/src/registry.rs`

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
    pub executor: ExecutorKind,
    pub depends: BitVec,
    pub provides: BitVec,
    #[builder(default)]
    pub command: String,
    #[builder(default = false)]
    pub essential: bool,
}
```

**Call site:**

```rust
let target = Target::new()
    .id(1)
    .name("build".into())
    .target_type(TargetType::File)
    .executor(ExecutorKind::Native)
    .depends(registry.capabilities().to_bitvec(&["compile", "link"]))
    .provides(registry.capabilities().to_bitvec(&["artifact"]))
    .command("cargo build --release".into())
    .essential(true)
    .build();

registry.register(target)?;
```

### Fallible two-step construction

When construction involves validation that can fail (e.g., interning strings into bitset indices), separate infallible argument collection from fallible registration:

```rust
// Step 1: infallible bon builder collects arguments
let args = TargetCreateArgs::builder()
    .name("build".into())
    .target_type(TargetType::File)
    .build();

// Step 2: fallible registry validation
let target = registry.validate_and_allocate(args)?;
```

### Rules

- **Always derive `bon::Builder`** for structs with 4+ fields. Never write manual builder structs.
- **Use `#[builder(default)]`** for optional fields. Use `#[builder(default = value)]` for non-trivial defaults.
- **Use `#[builder(start_fn = new)]`** to generate `Type::new()` as the entry point.
- **Validation belongs in `build()` or `register()`, not in setters.** Bon builders are infallible by default; add fallibility at the boundary.
- **Do NOT apply to structs with 2–3 parameters.** The benefit is readability; three params are already readable.
- **Decouple construction from registration.** `Target::builder().build()` produces an owned `Target`; `registry.register(target)` commits it. This clarifies ownership.

### When to Avoid

- **2–3 parameters with no validation:** A struct literal or `fn new()` is already readable. The builder adds boilerplate with no benefit.
- **No allocation or interning:** If construction cannot fail and involves no heap allocation, a simple constructor is clearer.
- **Single-use internal structs:** If the struct is only constructed in one place and never exposed, a builder is over-engineering.
- **Performance-critical construction:** If you're constructing millions of instances in a hot loop, the builder overhead (even if small) may matter. Use direct construction.

### Why not manual builders?

A manual builder in Rust requires: a separate struct, `impl` blocks for each setter, error accumulation logic, and a terminal method. That's ~50 lines per type. `bon` generates all of this from the struct definition in zero lines of additional code.

### Python analogy

```python
# Python: @dataclass with builder pattern
@dataclass
class Target:
    name: str
    depends: list[str]
    provides: list[str]
    essential: bool = False

# Rust equivalent with bon:
# Target::builder().name("build").depends(bits).essential(true).build()
# Same ergonomics, but type-checked at compile time.
```

---

## 3. Pattern 2 — Trait-Based Reflection

### The Boundary Rule

**Data that arrives from outside the process is always a string at the boundary. Data moving inside the process is not a string and should never be treated as one.**

This is the architectural foundation for choosing between the four tiers of reflection:
- **Tier 1 (Direct access):** Internal code, hot loops, trusted types
- **Tier 2 (serde):** Boundary crossing, type known at compile time
- **Tier 3 (FieldAccess):** Boundary crossing, field names from runtime data
- **Tier 4 (HashMap):** Schema unknown until runtime

The boundary rule prevents the most common performance mistake: treating internal data as strings when it doesn't need to be.

### The problem

```python
# Python: getattr/setattr at runtime — convenient, but:
# - No compile-time checking
# - No permission model
# - No schema description
# - String hashing at every access
setattr(config, "port", 9000)
getattr(config, "port")
```

### The Rust solution: Four Tiers of Reflection

Rust provides a spectrum of reflection mechanisms, each with different costs and capabilities. The key insight is choosing the right tier for the access pattern.

#### Tier 1: Direct Field Access (1x cost)

```rust
// Hot inner loops, trusted internal code
config.port = 9001;
let port = config.port;
```

**When:** Field names known at compile time, access in hot loops, trusted code.

#### Tier 2: `serde` Boundary Serialization (~10x cost)

```rust
use serde::{Serialize, Deserialize};
use internment::ArcIntern;

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

**When:** Data arrives as strings from outside the process (JSON, database, RPC), type is known at compile time.

#### Tier 3: `FieldAccess` Trait (~20x cost)

For runtime-dynamic field access by name — WASM tool configs, TUI editors, MCP tool parameter validation — use the `FieldAccess` trait with a derive macro.

```rust
/// Runtime field access by name with validation.
/// Generated by #[derive(FieldAccess)] for compile-time-known structs.
/// Implemented manually for WASM plugins and DB-driven schemas.
pub trait FieldAccess {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError>;
    fn get_field(&self, name: &str) -> Result<String, FieldError>;
    fn field_names() -> &'static [&'static str];
}

/// Schema description for MCP tool parameter validation and TUI editors.
pub trait Describable {
    fn json_schema() -> serde_json::Value;
}
```

**Generated by derive macro:**

```rust
// Planned derive macro — generates FieldAccess impl from struct definition
#[derive(FieldAccess, Describable)]
pub struct ToolConfig {
    #[field(desc = "TCP listen port", min = 1, max = 65535)]
    pub port: u16,
    #[field(desc = "Host address")]
    pub host: String,
    #[field(desc = "Enable verbose logging")]
    pub verbose: bool,
}

// The derive macro generates:
impl FieldAccess for ToolConfig {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        match name {
            "port" => {
                let v: u16 = value.parse().map_err(|_| FieldError::InvalidType {
                    field: name, expected: "u16", got: value
                })?;
                if v < 1 || v > 65535 {
                    return Err(FieldError::ConstraintViolation {
                        field: name, constraint: "1..=65535"
                    });
                }
                self.port = v;
                Ok(())
            }
            "host" => { self.host = value.to_string(); Ok(()) }
            "verbose" => {
                self.verbose = value.parse().map_err(|_| FieldError::InvalidType {
                    field: name, expected: "bool", got: value
                })?;
                Ok(())
            }
            _ => Err(FieldError::UnknownField(name.to_string()))
        }
    }

    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        match name {
            "port" => Ok(self.port.to_string()),
            "host" => Ok(self.host.clone()),
            "verbose" => Ok(self.verbose.to_string()),
            _ => Err(FieldError::UnknownField(name.to_string()))
        }
    }

    fn field_names() -> &'static [&'static str] {
        &["port", "host", "verbose"]
    }
}

impl Describable for ToolConfig {
    fn json_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "port": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 65535,
                    "description": "TCP listen port"
                },
                "host": {
                    "type": "string",
                    "description": "Host address"
                },
                "verbose": {
                    "type": "boolean",
                    "description": "Enable verbose logging"
                }
            },
            "required": ["port", "host", "verbose"]
        })
    }
}
```

**Usage at the boundary:**

```rust
// Set by name (string boundary — WASM config, DB row, RPC)
config.set_field("port", "9000")?;

// Get by name
let port_str = config.get_field("port")?;

// Generate JSON Schema for MCP tool parameter validation
let schema = ToolConfig::json_schema();
```

**When:** Field names are known at compile time but access pattern is driven by runtime data (e.g., iterating over a list of field names from a config file).

#### Tier 4: `HashMap` Fallback (~15x cost)

When a derive macro is not available or the schema is fully dynamic:

```rust
use std::collections::HashMap;
use internment::ArcIntern;

pub type DynamicConfig = HashMap<ArcIntern<str>, serde_json::Value>;

// Set by name
config.insert("port".into(), serde_json::json!(9000));

// Get by name with type checking
let port: u16 = config.get("port")
    .and_then(|v| v.as_u64())
    .ok_or(ConfigError::MissingField("port"))? as u16;
```

**When:** Schema is not known until runtime (e.g., WASM plugin exports its schema, database-driven schemas).

### The `Component` Supertrait

The `Component` supertrait unifies field access, schema description, and execution:

```rust
/// A component that can be built, configured by name, described, and executed.
/// This is the unified interface for the orchestrator.
pub trait Component: FieldAccess + Describable + WorkUnit + Send + Sync {}

// Blanket implementation: anything that implements all three is a Component
impl<T: FieldAccess + Describable + WorkUnit + Send + Sync> Component for T {}
```

Every orchestratable task in the system implements `Component`. Whether it's a compiled Rust struct, a WASM plugin, or a database-driven configuration, the orchestrator sees the same interface.

### Performance tiers

| Access method | Cost | When to use |
|---------------|------|-------------|
| `config.port = 9001` | 1x (baseline) | Hot inner loops, trusted internal code |
| `serde_json::from_str` | ~10x | Boundary: JSON input, DB hydration |
| `config.set_field("port", "9001")` | ~20x | Runtime-dynamic: WASM configs, TUI editors |
| `HashMap::insert` | ~15x | Fully dynamic schemas |

**The boundary rule**: use Tier 1 (direct access) in hot code. Use Tier 2 (`serde`) for known-at-compile-time types crossing a boundary. Use Tier 3 (`FieldAccess`) when field names are known at compile time but access is driven by runtime data. Use Tier 4 (`HashMap`) only when the schema is genuinely unknown until runtime.

### Decision tree for reflection

```
Is the type known at compile time AND the access pattern is by field name?
  YES → #[derive(FieldAccess)] — generates set_field/get_field from struct def
  NO  → Is the schema known at runtime but the values are typed?
    YES → Implement FieldAccess manually (WASM bridge, DB bridge)
    NO  → HashMap<ArcIntern<str>, serde_json::Value>

Is the access in a hot loop?
  YES → Direct field access (Tier 1)
  NO  → Is the data arriving from outside the process?
    YES → Is the type known at compile time?
      YES → serde (Tier 2)
      NO  → FieldAccess (Tier 3) or HashMap (Tier 4)
    NO  → Direct field access (Tier 1)
```

### Rules

- **Use `serde` for all boundary serialization.** JSON, database rows, RPC — `serde` handles it.
- **Use direct field access in hot code.** Never call `set_field` in a loop.
- **Do NOT try to recreate Zig's `Editable(T)` with procedural macros that use pointer arithmetic.** Rust's type system makes this unnecessary and unsafe.
- **Use `#[serde(default)]`** for fields with defaults. Use `#[serde(skip_serializing_if = "Option::is_none")]` for optional fields.
- **The `FieldAccess` derive macro is planned** for P3. Until then, use `HashMap<ArcIntern<str>, serde_json::Value>` for dynamic schemas, or implement `FieldAccess` manually for critical types.
- **Every `FieldAccess` implementation must also implement `Describable`.** The schema is the single source of truth.

### When to Avoid

- **Hot loops:** Never use `set_field` or `serde` in a loop. Use direct field access (Tier 1).
- **Internal structs that never cross a boundary:** If a struct is only used internally and never configured by name, don't add `FieldAccess`. The derive macro adds code for no benefit.
- **Single-implementation types:** If there's only one way to configure a type and it's known at compile time, use direct field access or a builder. `FieldAccess` is for runtime-dynamic configuration.
- **Performance-critical boundaries:** If you're parsing millions of records per second, `serde` may be too slow. Consider a custom parser or binary format.
- **Types with no schema:** If you can't describe the type with JSON Schema, `FieldAccess` is the wrong tool. Use `HashMap` or a custom representation.

---

## 4. Pattern 3 — Trait Composition (Cross-Cutting Concerns)

### The problem

```python
# Python: decorators for cross-cutting concerns
@timing
@retry(max=3)
def ingest_yago(path: str) -> None:
    # business logic
```

Python decorators execute at import time and wrap functions transparently. The downside: they're runtime closures, they add overhead, and they can be hard to type correctly.

### The Rust solution

Newtype wrappers around trait implementations. Each wrapper implements the same trait, delegating to the inner type while adding its cross-cutting concern. The compiler monomorphizes or dispatches through `dyn Trait` — no runtime closure, no extra overhead beyond the vtable call.

### Canonical shape: `Instrumented<U>`

```rust
use std::time::Instant;
use tracing::info;

pub struct Instrumented<U> {
    inner: U,
    name: &'static str,
}

impl<U: WorkUnit> WorkUnit for Instrumented<U> {
    fn name(&self) -> &str { self.name }
    fn depends(&self) -> &[ArcIntern<str>] { self.inner.depends() }
    fn provides(&self) -> &[ArcIntern<str>] { self.inner.provides() }

    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let start = Instant::now();
        let result = self.inner.execute(ctx);
        let elapsed = start.elapsed();
        info!(unit = self.name, elapsed_us = elapsed.as_micros() as u64);
        result
    }
}
```

### Retry wrapper

```rust
pub struct WithRetry<U> {
    inner: U,
    max_attempts: usize,
}

impl<U: WorkUnit> WorkUnit for WithRetry<U> {
    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let mut last_err = None;
        for attempt in 0..self.max_attempts {
            match self.inner.execute(ctx) {
                Ok(output) => return Ok(output),
                Err(e) if attempt + 1 < self.max_attempts => {
                    std::thread::sleep(std::time::Duration::from_millis(10 * (attempt + 1) as u64));
                    last_err = Some(e);
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap())
    }
}
```

### Application at the registration site

Wrappers compose by nesting. Apply them **before** storing in the registry:

```rust
let unit = Instrumented {
    inner: WithRetry {
        inner: MyWorkUnit::new(),
        max_attempts: 3,
    },
    name: "ingest_yago",
};

registry.register(Arc::new(unit));
```

### Composition order

When combining multiple wrappers, apply outer-to-inner:

| Layer | Purpose |
|-------|---------|
| 1 | Rate limiting — reject early if overloaded |
| 2 | Auth — reject early if unauthorized |
| 3 | Tracing — start span |
| 4 | Timing — measure full duration |
| 5 | Retry — retry on transient failure |
| 6 | Validation — validate input |
| 7 | Core handler |

### Rules

- **Wrappers must implement the same trait as the inner type.** This preserves the uniform interface.
- **Apply wrappers at the registration site, before type erasure.** The registry stores `Arc<dyn WorkUnit>`; it never sees the wrapper layers.
- **Keep wrapper bodies minimal.** The compiler cannot inline through `dyn Trait` dispatch.
- **Do NOT wrap when there is only one implementation.** Use the concrete type directly.
- **Use `impl Trait` or generics for hot paths** to avoid vtable dispatch entirely.

### When to Avoid

- **Single implementation:** If there's only one implementation and no plan for a second, don't wrap. Use the concrete type directly.
- **Hot paths:** Wrappers add a vtable call layer. For performance-critical code, use generics (`impl Trait` or `<T: Trait>`) to enable inlining.
- **Stateless wrappers with no cross-cutting concern:** If the wrapper doesn't add logging, retry, timing, or another cross-cutting concern, it's unnecessary indirection.
- **After type erasure:** You cannot wrap an `Arc<dyn Trait>` with a newtype wrapper and expect the compiler to inline. Use middleware (Pattern 9) for post-erasure composition.
- **When the wrapper changes the interface:** Wrappers must preserve the trait interface. If you need to change the interface, use a different pattern (e.g., adapter).

### Python analogy

```python
# Python decorator:
@timing
@retry(max=3)
def handler(): ...

# Rust equivalent:
let handler = Instrumented {
    inner: WithRetry { inner: Handler, max_attempts: 3 },
    name: "handler",
};
// Same composition, but type-safe and zero closure overhead.
```

---

## 5. Pattern 4 — Trait Objects (Runtime Polymorphism)

### The problem

```cpp
// C++: inheritance hierarchies for polymorphism
class Engine { virtual std::vector<Row> query(std::string sql) = 0; };
class SqliteEngine : public Engine { ... };
class RedisEngine  : public Engine { ... };
// Cost: vtable per class, hidden in the ABI, can't control layout
```

```python
# Python: duck typing — convenient but zero static safety
def run_query(engine, sql):
    return engine.query(sql)  # crashes at runtime if wrong method signature
```

### The Rust solution

`dyn Trait` + `Arc<dyn Trait>` for shared ownership. The compiler generates the vtable. `Send + Sync` bounds replace Zig's debug-build thread assertions with compile-time enforcement.

### Canonical implementation: `EmbeddingProvider` in `common/src/embeddings.rs`

```rust
pub trait EmbeddingProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn dimensions(&self) -> u32;
    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError>;
    fn embed_batch(&self, texts: &[&str]) -> Result<BatchEmbedding, EmbeddingError>;
}

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
let vec = provider.embed("hello world")?;
```

### Rules for trait objects

1. **Always add `Send + Sync`** to traits stored in registries or shared across threads.
2. **Use `Arc<dyn Trait>` for shared ownership.** Use `Box<dyn Trait>` only for exclusive ownership.
3. **Avoid `dyn Trait` when there is only one implementation.** Use generics (`impl Trait` or `<T: Trait>`) for hot loops to avoid vtable dispatch.
4. **Do NOT use `typetag` or similar crates** for trait object serialization. Use an enum wrapper instead:

```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProviderConfig {
    Ollama { model: String, base_url: String },
    OpenAi { model: String, api_key: String },
}
```

### When to use trait objects vs. generics

| Scenario | Use | Why |
|----------|-----|-----|
| 2+ implementations, stored in registry | `dyn Trait` + `Arc` | Uniform handles, shared ownership |
| Single implementation, hot loop | `<T: Trait>` or `impl Trait` | Zero-cost monomorphization |
| Single implementation, not hot | Concrete type | No indirection needed |
| Plugin system (WASM, dynamic load) | `dyn Trait` + `Arc` | Runtime-discovered implementations |

### When to Avoid

- **Single implementation:** If there's only one implementation and no concrete plan for a second, use the concrete type. Trait objects add indirection for no benefit.
- **Speculative polymorphism:** Don't create a trait "just in case" you might add more implementations later. Start with a concrete type; add the trait when the second implementation arrives.
- **Hot loops:** `dyn Trait` dispatch prevents inlining. For performance-critical code, use generics or `impl Trait`.
- **When you need to serialize the trait object:** Rust doesn't support serializing `dyn Trait` directly. Use an enum wrapper with `#[serde(tag = "type")]` instead.
- **When the type is known at compile time:** If you know the concrete type at the call site, use it directly. Trait objects are for runtime polymorphism.

---

## 6. Pattern 5 — Binary IPC (`#[repr(C, packed)]`)

### The problem

When executing untrusted or dynamically loaded code (WASM tools), you need a safe, portable, zero-copy message format that works across the host/guest boundary. Strings are too expensive; native structs have alignment and padding problems across compilers.

### The Rust solution

`#[repr(C, packed)]` removes padding and uses C-compatible layout. Encode/decode explicitly with `to_le_bytes()` / `from_le_bytes()`. Validate magic and version before reading any other field.

### Struct definitions in `wasm_ipc/src/lib.rs`

```rust
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct BinaryHeader {
    pub magic: [u8; 4],
    pub version: u32,
    pub payload_type: u32,
    pub payload_size: u32,
    pub checksum: u32,
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct BinaryExecutionRequest {
    pub header: BinaryHeader,
    pub target_id: i64,
    pub input_offset: u32,
    pub input_len: u32,
    pub flags: u32,
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
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

### Safe encoding (explicit byte order)

```rust
pub fn encode_request(req: &BinaryExecutionRequest, input: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(
        std::mem::size_of::<BinaryExecutionRequest>() + input.len()
    );

    buf.extend_from_slice(&req.header.magic);
    buf.extend_from_slice(&req.header.version.to_le_bytes());
    buf.extend_from_slice(&req.header.payload_type.to_le_bytes());
    buf.extend_from_slice(&req.header.payload_size.to_le_bytes());
    buf.extend_from_slice(&req.header.checksum.to_le_bytes());

    buf.extend_from_slice(&req.target_id.to_le_bytes());
    buf.extend_from_slice(&req.input_offset.to_le_bytes());
    buf.extend_from_slice(&req.input_len.to_le_bytes());
    buf.extend_from_slice(&req.flags.to_le_bytes());

    buf.extend_from_slice(input);
    buf
}
```

### Safe decoding (packed struct field access)

```rust
pub fn get_provides_bitset(
    result: &BinaryExecutionResult,
    payload: &[u8],
) -> Result<BitVec, IpcError> {
    // SAFETY: packed struct fields must be read with read_unaligned
    let count = unsafe {
        std::ptr::addr_of!(result.provides_words_count).read_unaligned()
    } as usize;
    let offset = unsafe {
        std::ptr::addr_of!(result.provides_words_offset).read_unaligned()
    } as usize;

    let mut bits = BitVec::with_capacity(count * 64);
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

### Rules for binary IPC

1. **Always use `#[repr(C, packed)]`** for structs that cross the Extism boundary.
2. **Always encode/decode explicitly** with `to_le_bytes()` / `from_le_bytes()`. Do not use `transmute`.
3. **Validate magic and version** before reading any other field.
4. **Offsets are absolute from buffer start**, never relative to struct end.
5. **Variable-length data** is appended after the fixed header. The header contains `offset` and `len` fields pointing to it.
6. **Use `read_unaligned`** for packed struct field access. This is the only `unsafe` required.

### When to Avoid

- **Internal structs that never cross a boundary:** If a struct is only used within Rust code, don't add `#[repr(C, packed)]`. It prevents optimizations and requires `unsafe` for field access.
- **Human-readable protocols:** If the protocol needs to be debuggable or human-readable, use JSON or a text format. Binary IPC is for performance-critical, machine-to-machine communication.
- **When the schema changes frequently:** Binary IPC requires coordinated updates on both sides. If the schema is volatile, use a self-describing format like JSON or Protocol Buffers.
- **When performance is not critical:** If you're not in a hot loop or dealing with large payloads, the complexity of binary IPC is not worth it. Use `serde` with JSON.
- **Cross-language boundaries without a shared spec:** If you're communicating with a language that doesn't respect C layout, use a standard format like Protocol Buffers or FlatBuffers.

---

## 7. Pattern 6 — Scoped Ownership (Replacing Arenas)

### The problem

Zig's `ArenaAllocator` scopes all intermediate allocations to a logical unit of work. Rust has no arena allocator (and `bumpalo` is forbidden by project policy).

### The Rust solution

**Trust the stack and `Drop`.** Ownership scopes naturally replicate the arena pattern. All intermediate `Vec`s and `String`s are owned by local variables and dropped automatically at scope exit.

### Per-request scoping (MCP server)

```rust
// Zig: arena per request
// var req_arena = std.heap.ArenaAllocator.init(self.allocator);
// defer req_arena.deinit();

// Rust: no arena needed
async fn handle_request(&self, raw_json: &str) -> Result<String, McpError> {
    let req: JsonRpcRequest = serde_json::from_str(raw_json)?;
    let result = self.reactor.route(&req.query).await?;
    // `req` and intermediates are dropped at end of scope.
    Ok(serde_json::to_string(&result)?)
}
```

### Batch processing

```rust
// Zig: arena per batch
// var batch_arena = std.heap.ArenaAllocator.init(self.allocator);
// defer batch_arena.deinit();

// Rust: owned struct, dropped at scope exit
{
    let mut mapper = TripleMapper::new(&library, &config);
    mapper.process_triples(triples)?;
    mapper.flush()?; // escaped data is cloned into `library`
} // <-- all locals dropped automatically
```

### Rules

- **Do not pre-allocate reusable buffers** unless profiling proves allocation is a bottleneck.
- **If you want an arena, use a scoped function** with local `Vec`s. They are dropped at scope exit.
- **Builder ownership transfer:** `build()` consumes the builder and returns an owned struct. The registry takes ownership. No manual deinit.

### When to Avoid

- **Long-lived data structures:** If data must outlive the function that created it, don't scope it to a local block. Use a persistent allocator or clone the data to a longer-lived owner.
- **Single-allocation functions:** If a function only allocates once, scoping doesn't help. The allocation is already managed by the return value.
- **When you need fine-grained control:** If you need to free individual allocations before scope exit, scoped ownership is too coarse. Use explicit `drop()` or a different ownership model.
- **Cross-thread sharing:** Scoped locals cannot be shared across threads. For shared data, use `Arc` or other thread-safe primitives.
- **When profiling shows allocation is not a bottleneck:** Don't optimize prematurely. If allocation is not a performance issue, use the simplest ownership model.

---

## 8. Pattern 7 — Newtype Handles (Typed Opaque IDs)

### The problem

```python
# Python: type aliases are just hints — no enforcement
NodeId = int
SessionId = int
process_node(session_id)  # Accepted silently — crashes later
```

### The Rust solution

Newtype wrappers create distinct types that share the integer representation. The compiler rejects mixing `NodeId` with `SessionId` at every call site.

### Implementation in `common/src/types.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TargetId(pub i64);

impl NodeId {
    pub fn from_int(i: i64) -> Self { Self(i) }
    pub fn as_int(self) -> i64 { self.0 }
}
// ... same for SessionId, TargetId
```

**Usage:**

```rust
fn process_node(id: NodeId) { ... }

let node: NodeId = NodeId::from_int(42);
let sess: SessionId = SessionId::from_int(42);
process_node(node);  // OK
process_node(sess);  // Compile error: expected NodeId, found SessionId
```

### Rules

- **Use for any integer ID that crosses module boundaries** and must not be mixed with other IDs.
- **Do NOT use when you need arithmetic on the ID** (`id + 1`). Use a raw integer for counters.
- **Derive `Serialize, Deserialize`** so the newtype is transparent in JSON/DB.

### When to Avoid

- **Internal counters or indices:** If you're using the integer as a loop counter, array index, or for arithmetic, use a raw integer. Newtypes prevent arithmetic operations.
- **Single-module IDs:** If an ID never crosses module boundaries and there's no risk of confusion, a raw integer is simpler.
- **When you need multiple representations:** If the same ID needs to be represented as `i64`, `u64`, and `String` in different contexts, newtypes add conversion overhead. Use a single representation with explicit conversion functions.
- **Performance-critical code:** Newtypes add a wrapper layer. In hot loops, the overhead may matter. Use raw integers and document the intended type.
- **When the ID is not an integer:** Newtypes are most useful for integer IDs. For string IDs or composite keys, use a different pattern (e.g., a struct with named fields).

---

## 9. Pattern 8 — Unit of Work (Orchestration Interface)

### The problem

An orchestrator (DAG executor, MCP server, WASM plugin host) must execute heterogeneous tasks uniformly. Without a common interface, the orchestrator branches on implementation type — a maintenance burden and a performance hazard.

### The Rust solution

A `WorkUnit` trait that every orchestratable task implements. The orchestrator stores `Arc<dyn WorkUnit>` and calls `.execute()` without branching.

### Definition in `dag/src/work_unit.rs`

```rust
use internment::ArcIntern;
use std::sync::Arc;

pub struct WorkContext {
    pub library: Arc<Library>,
    pub embedder: Arc<dyn EmbeddingProvider>,
    pub config: WorkConfig,
}

pub struct WorkOutput {
    pub provides: BitVec,
    pub output: Vec<u8>,
    pub success: bool,
}

pub trait WorkUnit: Send + Sync {
    fn name(&self) -> &str;
    fn depends(&self) -> &[ArcIntern<str>];
    fn provides(&self) -> &[ArcIntern<str>];
    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError>;
    fn schema(&self) -> serde_json::Value { serde_json::json!({}) }
}
```

### Implementation for a native command

```rust
pub struct CommandUnit {
    name: ArcIntern<str>,
    command: String,
    depends: Vec<ArcIntern<str>>,
    provides: Vec<ArcIntern<str>>,
}

impl WorkUnit for CommandUnit {
    fn name(&self) -> &str { &self.name }
    fn depends(&self) -> &[ArcIntern<str>] { &self.depends }
    fn provides(&self) -> &[ArcIntern<str>] { &self.provides }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&self.command)
            .output()
            .map_err(|e| WorkError::ExecutionFailed(e.to_string()))?;

        Ok(WorkOutput {
            provides: BitVec::new(), // resolved by registry
            output: if output.status.success() { output.stdout } else { output.stderr },
            success: output.status.success(),
        })
    }
}
```

### Implementation for a WASM plugin

```rust
pub struct WasmUnit {
    name: ArcIntern<str>,
    plugin: Mutex<extism::Plugin>,
    depends: Vec<ArcIntern<str>>,
    provides: Vec<ArcIntern<str>>,
}

impl WorkUnit for WasmUnit {
    fn name(&self) -> &str { &self.name }
    fn depends(&self) -> &[ArcIntern<str>] { &self.depends }
    fn provides(&self) -> &[ArcIntern<str>] { &self.provides }

    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let mut plugin = self.plugin.lock().unwrap();
        let result = plugin.call("execute", &ctx.input)
            .map_err(|e| WorkError::WasmFailed(e.to_string()))?;
        // decode BinaryExecutionResult from result bytes
        Ok(decode_output(&result)?)
    }

    fn schema(&self) -> serde_json::Value {
        // WASM plugin exports its config schema
        serde_json::json!({
            "type": "object",
            "properties": { ... }
        })
    }
}
```

### Registration and orchestration

```rust
let mut registry = WorkRegistry::new();

registry.register(Arc::new(Instrumented {
    inner: CommandUnit { ... },
    name: "build",
}));

registry.register(Arc::new(WasmUnit { ... }));

// Orchestrator never branches on type:
for unit in registry.resolve(&["build"])? {
    let output = unit.execute(&ctx)?;
}
```

### Rules

- **Every orchestratable task implements `WorkUnit`.** No exceptions.
- **Store as `Arc<dyn WorkUnit>`** in registries. Clone the `Arc` for shared ownership.
- **The `schema()` method enables MCP tool parameter validation** and WASM config hydration.
- **Compose with wrappers** (Pattern 3) for cross-cutting concerns: `Instrumented { inner: WithRetry { inner: unit } }`.
- **Do NOT add methods to `WorkUnit` speculatively.** Start with `name`, `depends`, `provides`, `execute`, `schema`. Add more only when a second implementation requires it.

### When to Avoid

- **Non-orchestratable tasks:** If a task is never executed by an orchestrator (DAG executor, MCP server, WASM host), don't implement `WorkUnit`. Use a simpler interface.
- **Single-use tasks:** If a task is only executed once and never stored in a registry, `WorkUnit` is over-engineering. Use a function or a simple struct.
- **When the task has no dependencies or provides:** If a task doesn't participate in a dependency graph, the `depends()` and `provides()` methods are unnecessary. Consider a simpler trait.
- **Hot loops:** `WorkUnit` is for orchestration, not for tight loops. If you're executing millions of tasks per second, use a more direct approach.
- **When you need fine-grained control over execution:** If the orchestrator's execution model doesn't fit your needs (e.g., you need streaming output, partial results, or cancellation), `WorkUnit` may be too restrictive.

---

## 10. Pattern 9 — Middleware Chain (Composable Cross-Cutting on Trait Objects)

### The problem

When you have `Arc<dyn WorkUnit>` and need to add logging, retry, or rate limiting, you cannot use the newtype wrapper pattern directly because the type is already erased. You need a way to compose middleware around a trait object.

### The Rust solution

A `Middleware` trait that wraps an `Arc<dyn WorkUnit>` and returns a new `Arc<dyn WorkUnit>`. Middleware can be stacked.

### Definition in `dag/src/middleware.rs`

```rust
pub trait Middleware: Send + Sync {
    fn wrap(&self, inner: Arc<dyn WorkUnit>) -> Arc<dyn WorkUnit>;
}

pub struct TimingMiddleware;

impl Middleware for TimingMiddleware {
    fn wrap(&self, inner: Arc<dyn WorkUnit>) -> Arc<dyn WorkUnit> {
        Arc::new(InstrumentedWorkUnit { inner })
    }
}

struct InstrumentedWorkUnit {
    inner: Arc<dyn WorkUnit>,
}

impl WorkUnit for InstrumentedWorkUnit {
    fn name(&self) -> &str { self.inner.name() }
    fn depends(&self) -> &[ArcIntern<str>] { self.inner.depends() }
    fn provides(&self) -> &[ArcIntern<str>] { self.inner.provides() }

    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let start = Instant::now();
        let result = self.inner.execute(ctx);
        info!(unit = self.inner.name(), elapsed_us = start.elapsed().as_micros() as u64);
        result
    }

    fn schema(&self) -> serde_json::Value { self.inner.schema() }
}
```

### Stacking middleware

```rust
let unit: Arc<dyn WorkUnit> = Arc::new(CommandUnit { ... });

let middleware: Vec<Arc<dyn Middleware>> = vec![
    Arc::new(TimingMiddleware),
    Arc::new(RetryMiddleware { max_attempts: 3 }),
];

let wrapped = middleware.into_iter().fold(unit, |u, m| m.wrap(u));
```

### Rules

- **Middleware implements the same trait it wraps.** This preserves the uniform interface.
- **Use `Arc<dyn WorkUnit>` (not `Box`)** because middleware may be shared across threads.
- **Delegate `schema()` to the inner unit.** Middleware does not change the schema.
- **Apply middleware at registration time**, not at execution time.

### When to Avoid

- **Compile-time-known cross-cutting concerns:** If the cross-cutting concern is known at compile time and the type is not erased, use newtype wrappers (Pattern 3) instead. They enable inlining.
- **Stateless middleware with no configuration:** If the middleware doesn't need configuration (e.g., timing middleware that just logs), consider a simpler approach like a wrapper function.
- **When you need to modify the interface:** Middleware must preserve the trait interface. If you need to change the interface, use an adapter pattern.
- **Hot paths:** Each middleware layer adds a vtable call. For performance-critical code, minimize middleware layers or use generics.
- **When the middleware needs to change the schema:** Middleware should not modify the schema. If you need to change the schema, use a different pattern (e.g., adapter).

---

## 11. Pattern 10 — Component Adapter (Runtime Type Adaptation)

### The problem

When you need to adapt a component at runtime — adding behavior, changing the interface, or bridging between different component types — you cannot use compile-time generics because the adaptation decision is made at runtime. You need a way to wrap any `Arc<dyn Component>` and present it as another `Arc<dyn Component>`.

### The Rust solution

A `ComponentAdapter` struct that wraps an inner component and provides custom behavior through function pointers or closures. The adapter implements `Component` and delegates to the inner component for methods it doesn't override.

### Definition in `dag/src/adapter.rs`

```rust
use std::sync::Arc;

pub struct ComponentAdapter {
    inner: Arc<dyn Component>,
    execute_fn: Option<Arc<dyn Fn(&WorkContext) -> Result<WorkOutput, WorkError> + Send + Sync>>,
    name_override: Option<ArcIntern<str>>,
    schema_override: Option<serde_json::Value>,
}

impl ComponentAdapter {
    pub fn new(inner: Arc<dyn Component>) -> Self {
        Self {
            inner,
            execute_fn: None,
            name_override: None,
            schema_override: None,
        }
    }

    pub fn with_execute<F>(mut self, f: F) -> Self
    where
        F: Fn(&WorkContext) -> Result<WorkOutput, WorkError> + Send + Sync + 'static,
    {
        self.execute_fn = Some(Arc::new(f));
        self
    }

    pub fn with_name(mut self, name: ArcIntern<str>) -> Self {
        self.name_override = Some(name);
        self
    }

    pub fn with_schema(mut self, schema: serde_json::Value) -> Self {
        self.schema_override = Some(schema);
        self
    }
}

impl FieldAccess for ComponentAdapter {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        // Delegate to inner component
        // Note: requires interior mutability or mutable access
        // For simplicity, we'll use a different approach
        Err(FieldError::UnknownField(name.to_string()))
    }

    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        self.inner.get_field(name)
    }

    fn field_names() -> &'static [&'static str] {
        &[] // Dynamic
    }
}

impl Describable for ComponentAdapter {
    fn json_schema() -> serde_json::Value {
        serde_json::json!({}) // Override per-instance
    }
}

impl WorkUnit for ComponentAdapter {
    fn name(&self) -> &str {
        self.name_override.as_ref().map(|n| n.as_ref()).unwrap_or_else(|| self.inner.name())
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        self.inner.depends()
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        self.inner.provides()
    }

    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        if let Some(f) = &self.execute_fn {
            f(ctx)
        } else {
            self.inner.execute(ctx)
        }
    }

    fn schema(&self) -> serde_json::Value {
        self.schema_override.clone().unwrap_or_else(|| self.inner.schema())
    }
}
```

### Usage: Adapting a WASM plugin

```rust
let wasm_unit: Arc<dyn Component> = Arc::new(WasmUnit { ... });

let adapted = ComponentAdapter::new(wasm_unit)
    .with_name("custom_name".into())
    .with_execute(|ctx| {
        // Custom execution logic
        Ok(WorkOutput { ... })
    })
    .with_schema(serde_json::json!({
        "type": "object",
        "properties": { ... }
    }));

registry.register(adapted);
```

### Rules

- **Use `ComponentAdapter` when adaptation decisions are made at runtime.** For compile-time adaptation, use newtype wrappers (Pattern 3).
- **Delegate to the inner component** for methods you don't override.
- **The adapter implements `Component`**, so it can be stored in the same registry as any other component.
- **Use function pointers or closures** for custom behavior. Avoid complex state in the adapter itself.

### When to Avoid

- **Compile-time-known adaptations:** If the adaptation is known at compile time, use newtype wrappers (Pattern 3). They enable inlining and are simpler.
- **When you need to change the interface:** `ComponentAdapter` preserves the `Component` interface. If you need a different interface, use a different pattern.
- **When the adapter has complex state:** Adapters should be thin wrappers. If you need complex state, consider a different pattern (e.g., a new struct that implements `Component` directly).
- **Hot paths:** Adapters add indirection. For performance-critical code, minimize adapter layers.
- **When you can modify the original component:** If you own the component and can add the behavior directly, do that instead of wrapping it.

---

## 12. Runtime Composition: Build, Configure, Wrap, Execute, Inspect

The full lifecycle of a runtime-configurable component demonstrates how all patterns compose:

### The Complete Lifecycle

```rust
// 1. Build: Construct the component with bon builder
let mut component: Arc<dyn Component> = Arc::new(
    ToolConfig::builder()
        .port(8080)
        .host("localhost".into())
        .verbose(false)
        .build()
);

// 2. Configure: Set fields by name at runtime
// Note: requires mutable access or interior mutability
// For demonstration, we'll use a mutable reference
if let Some(comp) = Arc::get_mut(&mut component) {
    comp.set_field("port", "9000")?;
    comp.set_field("verbose", "true")?;
}

// 3. Wrap: Add middleware for cross-cutting concerns
let wrapped: Arc<dyn Component> = TimingMiddleware.wrap(component);
let wrapped: Arc<dyn Component> = RetryMiddleware { max_attempts: 3 }.wrap(wrapped);

// 4. Execute: Run the component uniformly
let ctx = WorkContext { ... };
let output = wrapped.execute(&ctx)?;

// 5. Inspect: Read fields by name
let port = wrapped.get_field("port")?;
println!("Port: {}", port);

// 6. Describe: Generate JSON Schema
let schema = <ToolConfig as Describable>::json_schema();
println!("Schema: {}", serde_json::to_string_pretty(&schema)?);
```

### Key Properties

1. **Uniform handle:** Every step uses `Arc<dyn Component>`. The type never changes.
2. **Runtime configuration:** Fields can be set by name after construction.
3. **Composable middleware:** Wrapping preserves the uniform interface.
4. **Uniform execution:** The orchestrator calls `.execute()` without branching.
5. **Runtime inspection:** Fields can be read by name for debugging or UI.
6. **Schema generation:** The component describes itself for MCP tool validation.

### WASM Plugin Lifecycle

```rust
// 1. Load plugin and extract schema
let plugin = extism::Plugin::new(wasm_bytes)?;
let schema_json = plugin.call("get_schema", &[])?;
let schema: serde_json::Value = serde_json::from_slice(&schema_json)?;

// 2. Create WASM component with dynamic FieldAccess
let wasm_comp = WasmComponent::new(plugin, schema.clone());

// 3. Configure by name (delegates to WASM plugin)
wasm_comp.set_field("timeout_ms", "5000")?;

// 4. Wrap with middleware
let wrapped: Arc<dyn Component> = Arc::new(wasm_comp);
let wrapped = TimingMiddleware.wrap(wrapped);

// 5. Execute
let output = wrapped.execute(&ctx)?;

// 6. Schema is already available
let schema = wrapped.schema();
```

### Database-Driven Configuration Lifecycle

```rust
// 1. Load config from database
let rows = db.query("SELECT key, value FROM tool_config WHERE tool_id = ?", &[tool_id])?;
let mut config = HashMap::new();
for row in rows {
    config.insert(row.get::<_, String>("key").into(), row.get::<_, String>("value").into());
}

// 2. Create dynamic component
let dyn_comp = DynamicComponent::new(config);

// 3. Configure by name
dyn_comp.set_field("retries", "3")?;

// 4. Wrap and execute
let wrapped: Arc<dyn Component> = Arc::new(dyn_comp);
let output = wrapped.execute(&ctx)?;
```

---

## 13. Pattern 11 — Structured Logging Context

### The problem

In a multi-threaded server, log messages from different requests interleave. Without request context, debugging is difficult: you can't correlate log lines with a specific request, user, or trace. Adding context to every log call is verbose and error-prone.

### The Rust solution

Thread-local request context that is automatically included in all log messages within a scope. The context is set at request boundaries and cleared when the request completes. Timing scopes measure function execution and log duration automatically.

### Definition in `common/src/logging.rs`

```rust
use std::cell::RefCell;
use std::time::Instant;
use tracing::info;

#[derive(Debug, Clone, Default)]
pub struct LogContext {
    pub request_id: Option<String>,
    pub user_id: Option<String>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
}

thread_local! {
    static CURRENT_CONTEXT: RefCell<Option<LogContext>> = RefCell::new(None);
}

impl LogContext {
    pub fn set(ctx: LogContext) {
        CURRENT_CONTEXT.with(|c| *c.borrow_mut() = Some(ctx));
    }

    pub fn get() -> Option<LogContext> {
        CURRENT_CONTEXT.with(|c| c.borrow().clone())
    }

    pub fn clear() {
        CURRENT_CONTEXT.with(|c| *c.borrow_mut() = None);
    }
}
```

### Timing scopes

```rust
pub struct Scope {
    name: &'static str,
    start: Instant,
}

impl Scope {
    pub fn begin(name: &'static str) -> Self {
        let start = Instant::now();
        if LogContext::get().is_some() {
            info!(scope = name, event = "start");
        }
        Self { name, start }
    }

    pub fn end(self) {
        if LogContext::get().is_some() {
            let elapsed_us = self.start.elapsed().as_micros() as u64;
            info!(scope = self.name, event = "end", elapsed_us);
        }
    }
}
```

### Single-expression helper

```rust
pub inline fn call_logged<F, T, E>(
    name: &'static str,
    f: F,
) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
{
    let scope = Scope::begin(name);
    let result = f();
    scope.end();
    result
}
```

### Usage at request boundaries

```rust
async fn handle_request(&self, raw_json: &str) -> Result<String, McpError> {
    // Set context for this request
    LogContext::set(LogContext {
        request_id: Some(generate_request_id()),
        user_id: Some(self.user_id.clone()),
        trace_id: Some(generate_trace_id()),
        span_id: None,
    });
    defer LogContext::clear();

    // All log calls in this scope include the context
    let req: JsonRpcRequest = serde_json::from_str(raw_json)?;
    let result = call_logged("route", || self.reactor.route(&req.query))?;
    
    Ok(serde_json::to_string(&result)?)
}
```

### Key properties

1. **Thread-local:** Each thread has its own context slot. No synchronization needed.
2. **Zero-copy:** `LogContext` holds owned strings, not references. Context does NOT propagate across thread boundaries.
3. **Zero overhead when inactive:** When `LogContext::get()` is `None`, `Scope::begin()` and `Scope::end()` are no-ops. No allocation, no log call.
4. **Automatic cleanup:** Use `defer` or `Drop` to ensure context is cleared even on error paths.

### Rules

- **Set context at request boundaries** (HTTP handler, MCP request, WASM plugin call).
- **Clear context when the request completes.** Use `defer` or a guard type.
- **Do NOT propagate context across thread boundaries.** Pass the value explicitly and call `LogContext::set()` on the new thread.
- **Use `call_logged` for single-expression calls.** It wraps the call in a `Scope` and logs duration.
- **Do NOT use `Scope` in hot loops.** Each scope logs two messages (start and end). For tight loops, use direct field access.

### When to Avoid

- **Single-threaded applications:** If your application is single-threaded and doesn't handle concurrent requests, thread-local context is unnecessary. Use a global context or pass context explicitly.
- **Hot loops:** Each scope logs two messages. For performance-critical code, use direct logging or disable context.
- **When you need cross-thread propagation:** Thread-local context does NOT propagate across threads. If you need to propagate context, pass it explicitly.
- **When logging is disabled:** If tracing/logging is disabled, the context is unused. Don't set it.

---

## 14. Why They Work in Synergy

### What Each Pattern Produces and Consumes

The patterns are not independently beneficial — their value multiplies when composed. To understand why, think about what each pattern *produces* and what it *consumes*:

| Pattern | Produces | Consumes |
|---------|----------|----------|
| **Fluent Builder** | Fully-configured, allocation-complete object | Allocator, registry |
| **Trait-Based Reflection** | Vtable-driven field access (zero per-field overhead) | Struct definitions |
| **Trait Composition** | Instrumented functions of the same type | Comptime function values |
| **Trait Objects** | Uniform handles for runtime dispatch | Type-erased pointers, `const` vtable globals |
| **Scoped Ownership** | Single deinit that frees all intermediate allocations | Allocator scope boundary |
| **Newtype Handles** | Distinct types that share integer representation | Integer IDs |
| **Unit of Work** | Uniform orchestration interface | Heterogeneous tasks |
| **Middleware Chain** | Composable cross-cutting on trait objects | `Arc<dyn Trait>` |
| **Component Adapter** | Runtime type adaptation | `Arc<dyn Component>` |
| **Structured Logging Context** | Request-scoped observability | Thread-local context |

These hand-offs are not accidental. The Fluent Builder is a natural scope boundary for scoped ownership precisely because builders have a clear terminal. Trait Composition can only wrap before type erasure, and the registration terminal is exactly the moment of type erasure — so the wrapper goes at that site naturally. Trait Objects require `const` globals for their vtables, and comptime-generated vtables are automatically `const` globals. The patterns fit together because they were each designed around the same philosophy: explicit scope, explicit ownership, explicit dispatch, zero hidden cost.

### Synergy Details

The patterns are not just independently beneficial — their value multiplies when composed.

### Synergy 1: Builder + Trait Object eliminates per-call type branching

A `Target::builder()` call produces an owned `Target`. The registry stores it as `Arc<dyn WorkUnit>`. The orchestrator iterates and calls `.execute()` without branching. Builder accumulation is completely lock-free; only the registry insertion requires synchronization.

### Synergy 2: Trait-Based Reflection + Schema Description makes schema the single source of truth

The `#[derive(FieldAccess, Describable)]` macro generates field access, validation, and JSON Schema from one struct definition. Adding a new field automatically generates its accessor, its validator, its schema entry, and its binary wire size — in one edit, at one place.

Without this synergy, schema changes require updates in: the struct definition, the serializer, the deserializer, the JSON Schema emitter, and the binary codec. Four files instead of one.

### Synergy 3: Trait Composition + Trait Objects gives zero-modification observability

Wrap a `WorkUnit` with `Instrumented` at the registration site. The registry stores `Arc<dyn WorkUnit>`. The entire orchestrator is instrumented without modifying any business logic. The wrapper delegates all trait methods to the inner type.

### Synergy 4: Scoped Ownership + Binary IPC eliminates payload lifetime management

The `encode_request` function produces an owned `Vec<u8>`. The `extism_plugin_call` takes a byte slice. The `Vec` is dropped after the call returns. No individual allocation to track; RAII cleans everything.

### Synergy 5: Newtype Handles + Trait Objects prevent ID confusion in polymorphic registries

A `ProviderRegistry` stores `Arc<dyn EmbeddingProvider>`. A `WorkRegistry` stores `Arc<dyn WorkUnit>`. Both use `NodeId`, `TargetId`, `SessionId` newtypes. The compiler prevents passing a `SessionId` where a `TargetId` is expected, even though both are `i64` underneath.

### Synergy 6: Component Adapter + Middleware enables runtime orchestration policies

A `ComponentAdapter` can change execution behavior at runtime. Middleware can add retry, timing, or rate limiting. The orchestrator can compose these dynamically based on configuration, without recompilation.

---

## 15. Anti-Patterns

### ❌ Manual builder structs

```rust
// Wrong: 50+ lines of boilerplate
pub struct TargetBuilder {
    name: Option<String>,
    depends: Option<BitVec>,
    // ...
}
impl TargetBuilder {
    pub fn name(mut self, name: String) -> Self { self.name = Some(name); self }
    // ... 10 more setters
    pub fn build(self) -> Target { ... }
}

// Right: zero boilerplate
#[derive(bon::Builder)]
pub struct Target { ... }
```

### ❌ Raw-pointer vtables for compile-time-known types

```rust
// Wrong: bypasses borrow checker for a type known at compile time
pub struct WvrHandle {
    pub ptr: *mut (),
    pub vtable: &'static WvrVTable,
}
let handle = WvrHandle { ptr: Box::into_raw(boxed), ... };
// Requires manual Box::from_raw cleanup — error-prone

// Right: use the concrete type or Arc<dyn Trait>
let unit: Arc<dyn Component> = Arc::new(MyComponent::new());
// Automatic cleanup via Drop
```

### ✅ Manual vtables for runtime-assembled interfaces (legitimate use case)

When the interface is assembled at runtime from multiple sources (e.g., some methods from a compiled struct, some from a WASM plugin), use `Arc<dyn Component>` with a bridge struct, not raw pointers:

```rust
// Right: bridge WASM plugin into Component interface
pub struct WasmComponent {
    plugin: Mutex<extism::Plugin>,
    schema: serde_json::Value,
}

impl FieldAccess for WasmComponent {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        let mut plugin = self.plugin.lock().unwrap();
        plugin.call("set_config", &format!("{}={}", name, value))?;
        Ok(())
    }
    // ...
}

impl WorkUnit for WasmComponent {
    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let mut plugin = self.plugin.lock().unwrap();
        let result = plugin.call("execute", &ctx.input)?;
        Ok(decode_output(&result)?)
    }
    // ...
}
```

### ❌ Using `serde` in hot loops

```rust
// Wrong: serializes/deserializes on every iteration
for item in &items {
    let json = serde_json::to_string(item).unwrap();
    let parsed: Item = serde_json::from_str(&json).unwrap();
}

// Right: direct field access
for item in &items {
    let value = item.port;
}
```

### ❌ `dyn Trait` when there is only one implementation

```rust
// Wrong: vtable dispatch for a single type
let provider: Arc<dyn EmbeddingProvider> = Arc::new(NoopEmbedding::new(768));

// Right: concrete type, zero indirection
let provider = NoopEmbedding::new(768);
```

### ❌ Speculative trait objects

```rust
// Wrong: creating a trait for a single implementation "just in case"
pub trait DataStore: Send + Sync { ... }
pub struct SqliteStore;
impl DataStore for SqliteStore { ... }
// Only one implementation exists and none is planned

// Right: use the concrete type directly
pub struct SqliteStore;
impl SqliteStore { ... }
// Add the trait when the second implementation arrives
```

### ❌ Cosmopolitan Polymorphism (identical execute body)

```rust
// Wrong: three trait implementations with identical execute bodies
impl WorkUnit for IdentifierQuery {
    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        run_staged_query(ctx, &self.config)  // same body
    }
}
impl WorkUnit for CapabilityQuery {
    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        run_staged_query(ctx, &self.config)  // same body
    }
}

// Right: separate the predicate from the execution
pub struct QueryMatch {
    pub matches: fn(&str, &GuidanceDb) -> bool,
    pub intent: QueryIntent,
}

fn execute_query(ctx: &WorkContext, config: &StagedConfig) -> Result<WorkOutput, WorkError> {
    run_staged_query(ctx, config)  // ONE body
}
```

**Why it's wrong:** The trait is routing to the same function through three identical implementations. The only thing that differs is the `matches` predicate. The trait is a vehicle for the `matches` function pointer — but the `execute` indirection is pure waste. This is Cosmopolitan Polymorphism: applying the trait pattern for the routing mechanism while the polymorphism itself is an illusion.

**How to catch it:** If you write three implementations and notice their `execute` bodies are word-for-word identical, stop. The trait is not earning its indirection. Use a function-pointer array (like `QueryMatch`) instead. Keep `matches` as a function pointer; move `execute` to a single shared function.

### ❌ Returning a trait object that wraps a temporary

```rust
// Wrong: the inner value is dropped at the end of the function
fn make_provider() -> Arc<dyn EmbeddingProvider> {
    let provider = OllamaProvider::new("model");  // stack-allocated
    Arc::new(provider)  // Arc wraps a reference to a temporary
}

// Right: heap-allocate the inner value
fn make_provider() -> Arc<dyn EmbeddingProvider> {
    Arc::new(OllamaProvider::new("model"))  // Arc owns the value
}
```

**Why it's wrong:** If you create a value on the stack and then wrap it in an `Arc`, the `Arc` takes ownership and the value is moved to the heap. But if you try to wrap a reference to a stack value, the reference becomes dangling when the function returns. Always ensure the `Arc` owns the value, not a reference to it.

**How to catch it:** If the compiler complains about lifetimes or borrowing, you're probably trying to wrap a reference. Use `Arc::new(value)` to transfer ownership, not `Arc::new(&value)`.

### ❌ Wrapping after type erasure

```rust
// Wrong: cannot inline through dyn Trait
let unit: Arc<dyn WorkUnit> = Arc::new(MyUnit);
let wrapped = Instrumented { inner: unit };  // adds a vtable call layer

// Right: wrap before storing in registry
let unit = Instrumented { inner: MyUnit };
registry.register(Arc::new(unit));  // one vtable call total
```

### ❌ `transmute` for binary IPC

```rust
// Wrong: undefined behavior if layout changes
let result: BinaryExecutionResult = unsafe { std::mem::transmute::<&[u8], _>(buf) };

// Right: explicit byte-order decoding
let version = u32::from_le_bytes(buf[4..8].try_into().unwrap());
```

### ❌ Using `FieldAccess` in hot loops

```rust
// Wrong: string parsing on every iteration
for item in &items {
    item.set_field("count", &item.count.to_string()).unwrap();
}

// Right: direct field access
for item in &items {
    item.count = compute_count();
}
```

### ❌ Implementing `FieldAccess` for types that never cross a boundary

```rust
// Wrong: adding FieldAccess to an internal struct that's never configured by name
#[derive(FieldAccess)]
pub struct InternalCounter {
    pub count: u64,
}

// Right: only add FieldAccess to types that need runtime configuration
#[derive(FieldAccess)]
pub struct ToolConfig {
    pub port: u16,
    pub timeout_ms: u64,
}
```

---

## 16. Pattern Selection Guide

```
Does the construction have 4+ parameters, or reads like configuration?
  YES → #[derive(bon::Builder)]
  NO  → Simple fn new() or struct literal

Does data arrive as a string from outside the process?
  YES → Is the type known at compile time?
    YES → serde (Deserialize)
    NO  → Is the schema known at runtime but values are typed?
      YES → Implement FieldAccess manually
      NO  → HashMap<ArcIntern<str>, serde_json::Value>
  NO  → Is the access in a hot loop?
    YES → Direct field access
    NO  → Is the field name known at runtime?
      YES → FieldAccess trait
      NO  → Direct field access

Do you have multiple concrete implementations today?
  YES (2+) → dyn Trait + Arc<dyn Trait>
  NO  → Does a second implementation exist or is one genuinely planned?
      ONE implementation, no concrete second planned
        → Concrete type. No trait. No indirection.
      ONE implementation, second is actively being coded
        → Still prefer concrete type. Design the interface as methods.
        → Add the trait when the second implementation arrives.
      Speculative ("maybe I'll add more later")
        → Premature generalization. Use the concrete type.

Does a struct cross the WASM host/guest boundary?
  YES → #[repr(C, packed)] + explicit encode/decode
  NO  → normal struct

Does a batch operation allocate many short-lived intermediates?
  YES → Scoped function with local Vecs (no arena needed)
  NO  → normal ownership

Is an integer ID passed across module boundaries and must not mix?
  YES → newtype wrapper: struct NodeId(i64)
  NO  → plain integer

Does the task need uniform orchestration (DAG, MCP, WASM)?
  YES → impl WorkUnit trait
  NO  → concrete type with methods

Do multiple implementations share the same execute body, differing only in a predicate?
  YES → Function-pointer array (Vec<QueryMatch>) — not a full trait
  NO  → Normal trait if multiple implementations apply

Do you need cross-cutting concerns on a trait object?
  YES → Is the adaptation decision made at compile time or runtime?
    Compile time → Newtype wrapper (applied before type erasure)
    Runtime → Middleware trait or ComponentAdapter
  NO  → direct trait method call

Do you need to adapt a component's behavior at runtime?
  YES → ComponentAdapter
  NO  → Use the component directly or wrap with newtype
```

---

## 17. Thread Safety

### Thread Safety Contract

The patterns in this document are **weakly positive** for thread safety:

| Pattern | Thread safety story |
|---------|---------------------|
| `dyn Trait` + `Arc` | `Send + Sync` bounds enforced at compile time |
| `serde` | Serialization is stateless — zero contention |
| `FieldAccess` | Depends on implementation; use `Mutex` for mutable access |
| Newtype wrappers | No state; no contention |
| `bon::Builder` | Per-request; lock-free until `build()` |
| Scoped ownership | Stack-local; no sharing |
| Newtype handles | No state; no contention |
| `WorkUnit` trait | `Send + Sync` required; implementors must be thread-safe |
| `Middleware` | `Send + Sync` required; stateless middleware is zero-contention |
| `ComponentAdapter` | `Send + Sync` required; closures must be thread-safe |
| `LogContext` | Thread-local; no synchronization needed |

### Detailed Thread Safety Rules

1. **Handle creation is NOT thread-safe.** Create trait objects (`Arc<dyn Trait>`) on a single thread during initialization. Do not create handles concurrently.

2. **Handle storage in shared registries requires synchronization.** Use `Mutex`, `RwLock`, or concurrent collections (`DashMap`) for registries that are accessed from multiple threads.

3. **Trait object method calls ARE thread-safe IF the implementation is thread-safe.** Stateless implementations (`NoopEmbedding`, simple structs) are always safe. Stateful implementations (HTTP clients, database connections) must use interior mutability (`Mutex`, `RwLock`) for mutable state.

4. **Handle destruction requires all concurrent calls to complete first.** Before dropping an `Arc<dyn Trait>`, ensure all threads that hold a reference have completed their work. Use `Arc::strong_count()` to check for remaining references, or use a barrier/join mechanism.

5. **`Send + Sync` bounds are your friend.** Always add `Send + Sync` to traits stored in registries or shared across threads. The compiler will enforce thread safety at every implementation site.

### Example: Thread-safe usage pattern

```rust
// 1. Create on init thread (single-threaded):
let provider = Arc::new(OllamaProvider::new("model"));

// 2. Pass to workers (read-only — Arc is immutable):
let worker = move || {
    let vec = provider.embed("hello").unwrap();  // thread-safe if impl is
};

// 3. Destroy after all workers join:
let handles: Vec<_> = (0..4).map(|_| std::thread::spawn(worker)).collect();
for h in handles {
    h.join().unwrap();
}
drop(provider);  // safe: all workers have completed
```

### Shared mutable objects that need explicit protection

| Object | Mechanism | Reason |
|--------|-----------|--------|
| `Library` | `Mutex<rusqlite::Connection>` | SQLite writes must serialize |
| `CapabilityRegistry` | `RwLock<HashMap<...>>` | Read-heavy concurrent intern calls |
| `L1Cache` | `DashMap<String, RoutingResult>` | Concurrent cache access |
| `WasmUnit.plugin` | `Mutex<extism::Plugin>` | Plugin state is not thread-safe |
| `LogContext` | Thread-local | Each thread has its own context |

---

## 18. Schema Evolution

### Versioning with `serde`

```rust
#[derive(Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_field: Option<String>,  // added in v1.1
}
```

### Field-level versioning annotations

For more complex schema evolution, annotate fields with version metadata:

```rust
#[derive(Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_port")]
    pub port: u16,
    
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[field(version_added = "1.1")]
    pub new_field: Option<String>,
    
    #[serde(skip)]  // removed in v2.0
    #[field(version_removed = "2.0")]
    pub old_field: Option<String>,
    
    #[serde(alias = "old_name")]  // renamed in v1.2
    #[field(version_added = "1.2", version_removed = "1.2")]
    pub renamed_field: String,
}
```

### Migration functions

For type changes or complex migrations, provide a migration function:

```rust
#[derive(Serialize, Deserialize)]
pub struct Config {
    #[serde(default, deserialize_with = "migrate_timeout")]
    pub timeout_ms: u64,  // changed from String to u64 in v2.0
}

fn migrate_timeout<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::String(s) => {
            // Old format: "5s" or "5000ms"
            parse_duration(&s).map_err(serde::de::Error::custom)
        }
        serde_json::Value::Number(n) => {
            // New format: 5000 (milliseconds)
            n.as_u64().ok_or_else(|| serde::de::Error::custom("invalid timeout"))
        }
        _ => Err(serde::de::Error::custom("invalid timeout format")),
    }
}
```

### Versioning for binary IPC

```rust
pub const BINARY_SCHEMA_VERSION: u32 = 1;

// On decode:
let version = u32::from_le_bytes(buf[4..8].try_into().unwrap());
if version != BINARY_SCHEMA_VERSION {
    return Err(IpcError::UnsupportedVersion);
}
```

### Upgrade path

| Change | Bump | Action |
|--------|------|--------|
| Add field with default | Minor | Add `#[serde(default)]` — backward compatible |
| Remove field | Major | Add `#[serde(skip)]` — forward compatible |
| Rename field | Major | Old field with `#[serde(skip)]`; new field with `#[serde(alias = "old_name")]` |
| Type change | Major | Old field + `deserialize_with` migration function |
| Binary IPC breaking change | Major | Bump `BINARY_SCHEMA_VERSION` |

### Schema compatibility rules

- **Backward compatible:** New code can read old data. Adding fields with defaults is backward compatible.
- **Forward compatible:** Old code can read new data. Removing fields with `#[serde(skip)]` is forward compatible.
- **Breaking change:** Requires migration. Type changes and field removals without defaults are breaking.

---

## 19. Summary for AI Agents

When writing new code in `rust-src/`:

1. **Check the source first.** Run `guidance explain "<topic>"` before writing. The pattern you need is probably already implemented and tested. Check `common/src/registry.rs` for builders, `common/src/embeddings.rs` for trait objects, `wasm_ipc/src/lib.rs` for binary IPC.

2. **New multi-parameter construction** → `#[derive(bon::Builder)]` on the struct. Use `#[builder(default)]` for optional fields. Use `#[builder(start_fn = new)]` for `Type::new()` entry point. Never write manual builders — they add 50+ lines of boilerplate.

3. **New boundary serialization** → `#[derive(Serialize, Deserialize)]` with `#[serde(default)]` and `#[serde(skip_serializing_if = "Option::is_none")]`. For schema evolution, use `#[serde(alias = "old_name")]` for renames and `deserialize_with` for type migrations.

4. **New runtime-configurable component** → `#[derive(FieldAccess, Describable)]` or implement manually for WASM/DB bridges. The component can be configured by name, described with JSON Schema, and executed uniformly. Every `FieldAccess` implementation must also implement `Describable`.

5. **New cross-cutting logic** → Newtype wrapper implementing the same trait. Apply at registration site, before type erasure. For runtime decisions, use `Middleware` or `ComponentAdapter`. Never wrap after type erasure — it adds a vtable call layer.

6. **New subsystem with multiple implementations** → Define a trait with `Send + Sync`. Store as `Arc<dyn Trait>`. See `EmbeddingProvider` in `common/src/embeddings.rs`. Never use `dyn Trait` when there is only one implementation — use the concrete type.

7. **New WASM/binary IPC type** → `#[repr(C, packed)]` with explicit `to_le_bytes()` / `from_le_bytes()` encode/decode. Validate magic + version. Never use `transmute` — it's undefined behavior if layout changes.

8. **New batch-processing loop** → Scoped function with local `Vec`s. No arena needed. RAII drops everything at scope exit.

9. **New orchestratable task** → Implement `WorkUnit` trait. If it needs runtime configuration, also implement `FieldAccess` and `Describable` to make it a `Component`. Store as `Arc<dyn Component>`.

10. **New request-scoped observability** → Use `LogContext::set()` at request boundaries. Use `Scope::begin()` / `Scope::end()` for timing. Use `call_logged()` for single-expression calls. Clear context with `defer` or a guard type.

### Never do these

11. **Never use raw pointers for vtables.** Use `dyn Trait` + `Arc`. The compiler generates the vtable. Raw pointers bypass the borrow checker and require manual `Box::from_raw` cleanup.

12. **Never use `transmute` for binary IPC.** Use explicit byte-order encoding. `transmute` is undefined behavior if the layout changes.

13. **Never use `dyn Trait` when there is only one implementation.** Use the concrete type. Trait objects add indirection for no benefit.

14. **Never use `serde` or `FieldAccess` in hot loops.** Use direct field access. Serialization and string parsing add 10-50x overhead.

15. **Never stack-allocate a vtable or a struct that stores vtables internally.** This is the most common memory bug in this pattern family. Heap-allocate schemas that contain vtables.

16. **Never wrap a function that is already behind a vtable.** Wrap before type erasure, at the registration site. Wrapping after adds a vtable call layer with no inlining.

17. **Never use the string reflection path in hot loops.** It allocates. Use direct field access or `setFast`/`getFast`.

18. **Never apply Fluent Builder to a 3-parameter init.** The threshold is 4+ parameters. Three params are already readable.

19. **Never add `FieldAccess` to a type that never crosses a boundary.** If a struct is only used internally and never configured by name, don't add `FieldAccess`.

20. **Never create a trait speculatively.** Start with a concrete type. Add the trait when the second implementation arrives.

### Verification

21. **Run `cargo clippy` before finishing.** The project enforces `#![deny(warnings)]`.

22. **Run `cargo test` before finishing.** All 448 tests must pass.

23. **Check for `unsafe` blocks.** The project has only 7 `unsafe` blocks (3 packed struct reads in `wasm_ipc`, 4 libc ioctl in `terminal.rs`). If you add a new `unsafe` block, it must be justified and documented.

---

*This document is the authoritative Rust reference for Fluent WVR patterns. The Zig original (`doc/skills/fluent-wvr/SKILL.md`) remains authoritative for `src/`. The deprecated `FLUENT_WVR_RUST.md` (raw-pointer variant) should not be followed.*
