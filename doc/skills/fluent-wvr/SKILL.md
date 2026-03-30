---
name: fluent-wvr
description: Crucially important Zig implementation of Fluent Wrapped Vtables with Reflection
---

# Fluent WVR — The Synthesis Pipeline built on Fluent Wrapped Vtables with Reflection

**A design pattern guide for human coders and AI agents working in this codebase.**

---

## Who This Document Is For

**For humans coming from Python or C++**: Zig feels initially restrictive — no `getattr`, no inheritance, no decorators, explicit memory everywhere. This document shows how five composable patterns together give you everything Python's dynamism and C++'s polymorphism provide, with zero runtime overhead and deterministic memory.

**For AI agents**: This document is the authoritative reference for how code in this codebase is structured. When writing new code, check the pattern table in §1, find the applicable pattern, read its rules and examples from implemented source, and follow the anti-pattern section to avoid the most common mistakes.

All code examples in this document are drawn from currently implemented and tested source files. Every pattern is in production use.

---

To transition as a developer from Python and pre-2013 C++ into Zig, it is completely natural to feel the friction of losing dynamic decorators, extensive inheritance trees, and runtime metaprogramming. Zig demands explicit state management and straightforward control flow. There is no `getattr`, no `__init_subclass__`, no virtual keyword, no hidden vtable pointer injected into every class instance.

However, Zig is not just C with better syntax. By combining a small set of distinct paradigms — fluent interfaces, comptime metaprogramming, explicit vtables, and arena allocation — we can recreate the developer ergonomics of Python and the polymorphism of C++, but with zero runtime overhead, complete memory safety, and absolute type clarity. The result is code that reads like configuration, validates like a type system, routes like an interface, and allocates like a custom allocator — all simultaneously, with no magic hidden from the compiler.

This document explains those paradigms as a coherent system called **The Synthesis Pipeline**. Every section answers three questions: what problem this pattern solves in human terms, why this solution and not the obvious alternative, and exactly where in the codebase you can read a working, tested implementation.

---

## 1. The Synthesis Pipeline at a Glance

Five core patterns and three support patterns compose into a single coherent architecture. Each solves a specific problem; together they solve the entire ergonomics problem of systems programming.

| Pattern | Problem solved | Cost | Primary source |
|---------|---------------|------|---------------|
| **Fluent Builder** | Multi-parameter init that callers can't read | Zero — value-typed chain | `src/common/registry.zig` |
| **Comptime Reflection** | Schema defined in multiple places; boundary string parsing | Zero — compile-time expansion | `src/reflection/` |
| **Comptime Wrappers** | Cross-cutting logic duplicated across handlers | Zero — compiler inlines | Pattern only; applied at registration sites |
| **VTables** | Runtime polymorphism with branching in hot loops | Two pointers per handle | `src/common/embeddings.zig`, `src/coral/mcp.zig` |
| **WASM / Binary IPC** | Executing untrusted dynamic code across a boundary | Memcpy + Extism runtime | `src/wasm/wasm.zig` |
| **Arena-Backed Builders** | Repeated malloc/free in batch processing | One arena alloc per batch | `src/coral/batch.zig`, `src/coral/mcp.zig` |
| **Typed Opaque Handles** | ID type confusion across module boundaries | Zero — same representation | `src/coral/db.zig` |
| **Runtime Dynamic Schemas** | Schema known only at runtime (WASM tools, DB rows) | One heap alloc per schema | `src/reflection/accessor.zig` |

The data flow through the full pipeline:

```
Developer writes:   target("build").depends(…).provides(…).register()
                              ↓
Comptime Reflection: generates Accessor array, ConstraintVTable, FieldMeta
                              ↓
Comptime Wrapper:   wraps handler with timing/retry/validation at comptime
                              ↓
VTable:             type-erases wrapped handler to *anyopaque + *const VTable
                              ↓
Runtime:            orchestrator calls vtable.execute() — no branching needed
```

To make this concrete: the developer writes a single declarative registration chain. The compiler, during compilation, inspects the handler's type signature and generates type-safe field accessors, serialization logic, and a JSON Schema description — without any schema file or code generator. Still at compile time, any cross-cutting wrapper (timing, retry, validation) is fused into the function so completely that the binary output is identical to hand-written code. Then, at the `register()` terminal call, the now-fully-instrumented, type-safe handler is cast to a `*anyopaque` and stored behind a vtable. From this point forward, the orchestrator loop sees a uniform interface — it never branches on implementation type. The entire four-stage transformation happens with no runtime cost, no allocations beyond the registration arena, and no loss of type information at any stage where type information is still needed.

This is the core thesis: each pattern in isolation is a local improvement; composed together, they form a system where expressiveness, type safety, observability, and runtime efficiency are simultaneously maximised rather than traded off against each other.

---

## 2. Pattern 1 — Fluent Builder

In Python, we relied heavily on decorators to register targets, inject dependencies, and attach metadata: `@target("build", depends=["init"])`. Zig has no macro arguments that generate code and no compile-time function attributes. The naive translation — a constructor call with positional arguments — collapses readability as the parameter count grows. Named keyword arguments help, but Python raises errors mid-construction when something goes wrong, which means your caller must be prepared to catch from any step.

Instead, we achieve declarative conciseness with the **Fluent Builder pattern combined with explicit terminal registration**. The builder struct chains configuration methods and commits on a single terminal call. This keeps intent explicit at the call site, makes type safety the compiler's problem rather than the programmer's, and concentrates error handling to exactly one location. It is our equivalent to Python's `@decorator` syntax — but you can read every step, trace every allocation, and understand every error path without any magic.

### The problem

```c++
// C++: Can you tell what the 4th argument means?
Target t = Target("build", TargetType::File, {"compile", "link"}, {"artifact"}, true, "zig build");
```

```python
# Python: kwargs help, but errors surface at runtime, mid-construction
target = Target(name="build", depends=["compile"], provides=["artifact"], essential=True)
```

### The Zig solution

A builder struct accumulates configuration through chained `*Self` setters. Errors are stored in `err: ?*BuilderError` — a rich error type that captures **where** and **why** the failure occurred — and surface only at the terminal call. The key insight is that the *accumulation phase* should never fail visibly to the caller. Each setter tests `if (self.hasError()) return self;` and stores any failure silently. The terminal method (`register`, `build`, `sync`) is the only place the caller writes `try` — it either commits cleanly or surfaces the earliest error that occurred during the chain.

**Why `BuilderError` instead of `?anyerror`?**

A naive implementation stores `err: ?anyerror`, which loses all context about *which* setter failed. Zig's error trace shows the terminal `register()` call, not the failing setter. The `BuilderError` pattern solves this by capturing:

- `phase`: which setter failed (depends, provides, command, etc.)
- `field`: the field name being set
- `value`: the user-supplied value (truncated to 128 bytes)
- `constraint`: what validation was violated
- `cause`: the underlying Zig error

The formatted message: `phase=depends field=provides value=compile,link constraint=invalid_reference cause=OutOfMemory`

This is **more useful than a stack trace** — it shows the semantic context that a stack trace cannot.

### Canonical implementation: `TargetBuilder` in `src/common/registry.zig`

```zig
// Usage — entire chain, single try:
try registry.target("build", .file)
    .depends(&.{"compile", "link"})
    .provides(&.{"artifact"})
    .command("zig build -Doptimize=ReleaseFast")
    .essential()
    .register();
```

The builder shape:

```zig
pub const TargetBuilder = struct {
    allocator: std.mem.Allocator,
    /// Owns all strings in BuilderError (error messages, value copies).
    /// Deinited by register() on both success and error paths.
    arena: std.heap.ArenaAllocator,
    registry: *TargetRegistry,
    interner: *StringInterner,
    target: ?*Target,
    /// Rich error with field/value/constraint context (arena-allocated).
    err: ?*BuilderError,
    /// Fallback plain error when BuilderError arena allocation itself fails.
    err_any: ?anyerror,

    fn hasError(self: *const TargetBuilder) bool {
        return self.err != null or self.err_any != null;
    }

    /// Every setter: guard → mutate → return self.
    pub fn depends(self: *TargetBuilder, names: []const []const u8) *TargetBuilder {
        if (self.hasError() or self.target == null) return self;  // short-circuit
        self.target.?.setDepends(self.allocator, self.interner, names) catch |cause| {
            const value = joinStringSlice(self.arena.allocator(), names) catch null;
            self.setError(.depends, "depends", value, "invalid_reference", cause);
        };
        return self;
    }

    /// Terminal: surface accumulated error, transfer ownership to registry.
    /// Always deinits the arena — do not call any setter after register().
    pub fn register(self: *TargetBuilder) !void {
        defer self.arena.deinit();
        if (self.err) |e| {
            if (self.target) |t| { t.deinit(self.allocator); self.allocator.destroy(t); }
            // Caller can log e.message for diagnostics:
            // std.log.err("Registration failed: {s}", .{e.message});
            return e.cause;
        }
        if (self.err_any) |e| {
            if (self.target) |t| { t.deinit(self.allocator); self.allocator.destroy(t); }
            return e;
        }
        if (self.target) |t| {
            try self.registry.add(t);
            self.target = null;   // registry now owns
        }
    }
};
```

The `BuilderError` type (see `src/common/builder_error.zig`):

```zig
pub const BuilderError = struct {
    phase: Phase,           // which setter failed: depends, provides, command, etc.
    field: ?[]const u8,     // the field name
    value: ?[]const u8,     // user-supplied value (truncated to 128 bytes)
    constraint: ?[]const u8, // what was violated
    cause: anyerror,        // underlying Zig error
    message: []const u8,    // formatted: "phase=X field=Y value=Z cause=W"
};

pub const Phase = enum {
    depends, provides, command, registration, validation, initialization,
};
```

The factory on the owning struct encodes allocation errors into the builder, not the caller:

```zig
pub fn target(self: *TargetRegistry, name: []const u8, tt: TargetType) TargetBuilder {
    var arena = std.heap.ArenaAllocator.init(self.allocator);
    const t = self.allocator.create(Target) catch |e| {
        return .{ .allocator = self.allocator, .arena = arena,
                  .registry = self, .interner = self.interner,
                  .target = null, .err = null, .err_any = e };
    };_ = t.init(self.allocator, self.interner, name, tt) catch |e| {
        self.allocator.destroy(t);
        return .{ .allocator = self.allocator, .arena = arena,
                  .registry = self, .interner = self.interner,
                  .target = null, .err = null, .err_any = e };
    };
    return .{ .allocator = self.allocator, .arena = arena,
              .registry = self, .interner = self.interner,
              .target = t, .err = null, .err_any = null };
}
```

### Value-copy variant: `DbSyncBuilder` in `src/vector/vector_db.zig`

When the builder holds only scalars (no mid-chain heap allocation), return `Self` by value from each setter. This avoids aliasing issues with stack-allocated builders:

```zig
pub const DbSyncBuilder = struct {
    allocator:        std.mem.Allocator,
    guidance_dir:     []const u8,
    db_path:          []const u8,
    embedder:         vector.EmbeddingProvider,
    capabilities_dir: ?[]const u8 = null,

    pub fn withCapabilities(self: DbSyncBuilder, dir: []const u8) DbSyncBuilder {
        var b = self; b.capabilities_dir = dir; return b;  // copy-modify-return
    }

    pub fn withAliases(self: DbSyncBuilder, aliases: SemanticAliases) DbSyncBuilder {
        var b = self; b.aliases = aliases; return b;
    }

    pub fn sync(self: DbSyncBuilder) !void {
        return syncDatabase(self.allocator, self.guidance_dir, self.db_path,
                            self.embedder, self.capabilities_dir, self.aliases);
    }
};
```

### Also in use: `QueueReactorBuilder` in `src/coral/cache.zig`

```zig
const reactor = try QueueReactorBuilder.init(allocator)
    .library(&lib)
    .embedder(embedding_provider)
    .knnK(10)
    .l4Threshold(0.7)
    .l3MaxDepth(4)
    .decomposerConfig(decomp_cfg)
    .build();
```

### Rules

- `err: ?*BuilderError` — use the rich error type, not bare `?anyerror`. Capture phase, field, value, and constraint.
- `arena: std.heap.ArenaAllocator` — the builder owns an arena for error message strings. Always deinit in the terminal method.
- Every setter calls `hasError()` before any allocation. If already errored, short-circuit.
- Terminal (`register`, `sync`, `build`) is always the only `try` in the call, andalways deinits the arena.
- If the terminal is never called, heap-allocated state leaks — document this.
- Use `*Self` return when the builder is heap-managed or holds mid-chain allocations. Use value-copy (`Self`) return when the builder holds only slice references.
- Do **not** apply to `init()` functions with 2–3 parameters. The benefit is readability; three params are already readable.
- On error, log `e.message` for human-readable diagnostics: `std.log.err("Failed: {s}", .{e.message})`
- Chain errors with `BuilderError.chain(arena, child, parent)` when one error causes another.

### Why not just use a config struct literal?

```zig
// This also works for simple cases:
const target = Target{ .name = "build", .depends = &.{"compile"}, .essential = true };
```

A struct literal is right for 3–4 known-at-compile-time fields with no allocation. The builder earns its place when: (a) construction involves allocation or interning that can fail, (b) the parameters have natural human-readable names that improve call-site clarity, or (c) the object must register itself into a shared data structure. `TargetBuilder` does all three — `depends` and `provides` intern string names into bitset indices, which can fail, and `register()` commits to the `TargetRegistry`. Encoding that failure into a struct literal's initializer would require either a multi-step `try` sequence or swallowing errors, both of which are worse.

### Python analogy

```python
# Python: method chaining with exception-like error accumulation
class TargetBuilder:
    def __init__(self, registry):
        self._error = None
        self._error_context = None# (phase, field, value)
    
    def depends(self, names):
        if self._error: return self
        try:
            self._names = self._interner.intern(names)
        except Exception as e:
            self._error = e
            self._error_context = ("depends", "depends", names)
        return self
    
    def register(self):
        if self._error:
            # Rich error message with context
            phase, field, value = self._error_context
            raise RuntimeError(f"phase={phase} field={field} value={value}") from self._error
        self._registry.add(...)# Zig difference: errors accumulate with semantic context; ONE try at the end
```

**Why BuilderError beats bare `?anyerror`:**

The criticism that "Zig's error traces point to `register()`, not the failing setter" is valid for `?anyerror`. But `BuilderError` solves this differently:

| Approach | Stack Trace | Error Message |
|----------|-------------|---------------|
| `?anyerror` | Shows `register()` only | `Error.OutOfMemory` |
| `?*BuilderError` | Shows `register()` but | `phase=depends field=provides value=compile,link cause=OutOfMemory` |

The debugger sees `register()`, but the **log message** tells you exactly which setter failed, with what value, violating which constraint. This is more actionable than a stack trace through middleware boilerplate.

---

## 3. Pattern 2 — Comptime Reflection

Once the fluent builder captures a developer's handler or configuration, we face the "bridge problem": the rest of the system is generic and knows nothing about the specific struct type, yet it needs to serialize fields to strings, validate ranges, enforce permissions, and emit JSON Schemas. In C++, this requires heavy template metaprogramming or external schema definition. In Python, you'd use `kwargs`, `__dict__`, and runtime inspection — convenient, but unable to validate anything until data is already flowing.

In Zig, we use **comptime reflection**. The `@typeInfo` builtin and `inline for` let the compiler generate field-level serialisation, validation, permission checking, and schema description code from ordinary Zig structs — without any external schema file, code generator, or annotation processor. You write a normal struct. The compiler writes all the boilerplate.

The key insight is about *where* data enters as a string and *where* it doesn't. Data that arrives from outside the process — user input, JSON files, HTTP parameters, database rows, RPC calls — is always a string at the boundary. Data moving *inside* the process between modules that both compiled against the same types is not a string and should never be treated as one. Comptime reflection solves the boundary case efficiently and makes the internal case nearly free.

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

### The Zig solution

`@typeInfo`, `inline for`, and `@Type` let the compiler generate field-level access, validation, and binding code from ordinary Zig structs — without an external schema or code generation step. The result is zero runtime overhead on the hot path and complete type safety.

### `Editable(T)` — the zero-size mixin

**Source: `src/reflection/accessor.zig`**

`Editable(T)` is the primary interface for boundary access. It is a zero-size mixin — you embed it in your struct and it adds zero bytes to the struct's memory footprint while giving you named field access with type coercion, range validation, and role-based permission checking. The implementation is generated entirely at comptime from `@typeInfo(T).@"struct".fields`; no macro, no code generator, no annotation file. Adding a new field to the struct automatically adds it to the reflection layer.

```zig
const Config = struct {
    port:     u16 = 8080,
    host:     []const u8 = "localhost",
    enabled:  bool = false,
    editable: Editable(Config) = .{},   // zero bytes at runtime
};

var config: Config = .{};

// String path — for boundaries: JSON input, user input, database rows
try config.editable.set(allocator, "port", "9000", .coder);   // validated, role-checked
const s = try config.editable.get(allocator, "port", .coder); // returns "9000"
defer allocator.free(s);

// Fast path — for hot code: zero vtable call, zero allocation
config.editable.setFast("port", @as(u16, 9001));
const v = config.editable.getFast("port");  // returns u16 directly
```

`Editable(T)` is a zero-size struct — it adds **no memory** to the containing struct. `@sizeOf(Editable(Config)) == 0`. The entire implementation is generated at comptime from `@typeInfo(T).@"struct".fields`.

### `Constraint(T)` — the vtable generator

**Source: `src/reflection/constraint.zig`**

```zig
pub fn Constraint(comptime T: type) ConstraintVTable {
    return .{
        .setFn = struct {
            fn set(a: std.mem.Allocator, ptr: *anyopaque, input: []const u8) anyerror!void {
                const p: *align(@alignOf(T)) T = @ptrCast(@alignCast(ptr));
                try constraintSet(T, a, p, input);
            }
        }.set,
        .getFn = struct {
            fn get(a: std.mem.Allocator, ptr: *const anyopaque) anyerror![]const u8 {
                const p: *align(@alignOf(T)) const T = @ptrCast(@alignCast(ptr));
                return constraintGet(T, a, p);
            }
        }.get,
        // releaseFn for []const u8 fields that own their allocation
        .releaseFn = ...,
    };
}
```

The generated vtable is a comptime constant. Adding the same field to 1000 dynamic schemas adds **zero extra binary**.

`constraintSet` dispatch is recursive and comptime-resolved:

```zig
fn constraintSet(comptime T: type, a: std.mem.Allocator, ptr: *T, input: []const u8) !void {
    switch (@typeInfo(T)) {
        .int    => ptr.* = try std.fmt.parseInt(T, input, 10),
        .float  => ptr.* = try std.fmt.parseFloat(T, input),
        .bool   => ptr.* = parseBool(input),
        .pointer => |p| if (p.size == .slice and p.child == u8)
            ptr.* = try a.dupe(u8, input),
        .@"enum"  => ptr.* = std.meta.stringToEnum(T, input) orelse return error.InvalidEnum,
        .optional => |o| if (std.mem.eql(u8, input, "null")) {
            ptr.* = null;
        } else {
            var tmp: o.child = undefined;
            try constraintSet(o.child, a, &tmp, input);
            ptr.* = tmp;
        },
        .array => |arr| { /* iterate csv, recurse per element */ },
        else   => return error.UnsupportedType,
    }
}
```

### `DynamicEditable` — runtime-defined schemas

**Source: `src/reflection/accessor.zig`**

`Editable(T)` requires knowing the struct type at compile time. When you don't — when the schema comes from a database column list, a WASM tool's exported config definition, or a JSON Schema loaded at startup — you need `DynamicEditable`. It operates on the same `Accessor` + `ConstraintVTable` machinery but accepts the field list as a runtime-constructed slice. The type vtables themselves must still exist at compile time (the `Constraint(T)` generator covers all primitive types and the common standard library types), but field names, offsets, and permissions are fully dynamic.

When the struct layout is known only at runtime (WASM tool configs, SQLite rows with dynamic columns), build an `Accessor` slice and use `DynamicEditable`:

```zig
const u16_vtable = reflection.Constraint(u16);  // const global — never stack-allocate
const f32_vtable = reflection.Constraint(f32);

var buffer align(4) = [_]u8{0} ** 8;
const accessors = [_]Accessor{
    .{ .name = "port",  .offset = 0, .permissions = perm_coder, .constraint = &u16_vtable },
    .{ .name = "value", .offset = 4, .permissions = perm_coder, .constraint = &f32_vtable },
};

var dyn = try DynamicEditable.init(allocator, &buffer, &accessors);
defer dyn.deinit();

try dyn.set("port", "9000", .coder);
const val = try dyn.get("port", .coder);
defer allocator.free(val);
```

`TargetSchema` in `src/common/target.zig` is the production example: it provides a `DynamicEditable` interface over `Target` structs and must be heap-allocated (via `TargetSchema.create()`) because its bitset vtables are stored inside the schema struct — if moved on the stack, the `Accessor.constraint` pointers would dangle.

### `FieldMeta` — AI-aware schema annotations

One consequence of generating schemas from struct definitions is that you can also generate *documentation* from them. `FieldMeta` lets you annotate fields with human-readable descriptions, units, valid ranges, and example values. These annotations cost nothing at runtime — they are embedded in the comptime-generated `Accessor` array as compile-time constants. The payoff is `Editable(T).describeSchema(allocator)`, which emits a complete JSON Schema document that MCP tools and AI agents can consume directly. A struct becomes its own documentation.

Attach rich metadata to fields with a comptime `describeField` decl. The reflection layer reads this at comptime, embedding it in the generated `Accessor` array:

```zig
pub fn describeField(comptime name: []const u8) FieldMeta {
    if (comptime std.mem.eql(u8, name, "port")) return .{
        .description = "TCP listen port",
        .min = 1, .max = 65535,
        .identity = false,
    };
    return .{};
}
```

`Editable(T).describeSchema(allocator)` emits a JSON Schema document from these annotations — used to generate MCP tool parameter schemas automatically.

### `TypedAccessorTable(T)` — compile-time typed access for internal code

**Source: `src/reflection/typed.zig`**

When code **knows** field types at compile time and does not need string parsing, use `TypedAccessorTable` for zero-cost type-safe access:

```zig
const Table = TypedAccessorTable(Config);
Table.setField(&config, "port", @as(u16, 9000));  // compile-time type-checked, no alloc
const port = Table.getField(&config, "port");      // returns u16 directly
```

`TypedEditable` adds range validation on top:

```zig
var editable = try TypedEditable.init(allocator, @ptrCast(&config), &accessors);
defer editable.deinit();
try editable.setTyped("port", @as(u16, 8080), .coder);   // range-checked
const port = try editable.getTyped("port", u16, .coder); // type-checked get
```

### Performance tiers

| Access method | Cost | When to use |
|---------------|------|-------------|
| `config.port = 9001` | 1× (baseline) | Hot inner loops, trusted internal code |
| `editable.setFast("port", 9001)` | ~1× | Named access in hot-ish code, no alloc |
| `TypedAccessorTable.setField(…)` | ~1× | Compile-time-known fields, type checking wanted |
| `editable.set(a, "port", "9001", .coder)` | ~10–50× | Boundaries: JSON, user input, DB rows, RPC |

**The boundary rule**: use the string path (`set`/`get`) whenever data arrives as strings from outside the process. Use the fast paths everywhere else.

### Compile-time and binary size considerations

**The critique is valid**: generating `Constraint(T)` vtables and `Accessor` arrays for every struct that touches a boundary is zero runtime cost, but not zero compile-time cost or zero binary size. LLVM must monomorphize generic code for each struct type, which increases debug build times and final binary size.

**Why this trade-off is acceptable:**

1. **The alternative is worse.** Hand-writing serialization, validation, and schema code for every struct would produce similar or greater binary size — plus the maintenance burden of keeping N files in sync.

2. **Boundaries are few.** Schema-driven access is used at process boundaries (JSON-RPC, config loading, DB hydration), not in hot loops. The number of schema-enabled structs is bounded by the number of boundary types.

3. **Debug builds don't go to production.** The binary size impact is most visible in debug builds. Release builds with `-OReleaseFast` benefit from LLVM's dead-code elimination — unused schema fields don't appear in the binary.

**When to avoid reflection:**

If a struct never crosses a boundary (never parsed from JSON, never stored to DB, never accessed by name), don't add `editable: Editable(Self)`. Use direct field access instead. The reflection layer is opt-in — you pay only for what you use.

### `BinaryFieldCodec` — binary serialization without string round-trips

**Source: `src/reflection/binary.zig`**

For WASM IPC and binary protocols where string encoding would be wasteful:

```zig
// Encode
try BinaryFieldCodec.encodeField(u32, 0x12345678, writer);  // little-endian
try BinaryFieldCodec.encodeField(bool, true, writer);        // 0x01
try BinaryFieldCodec.encodeField(Priority, .high, writer);   // enum ordinal

// Decode
const n = try BinaryFieldCodec.decodeField(u32, reader, allocator);
const b = try BinaryFieldCodec.decodeField(bool, reader, allocator);

// Wire size
const sz = BinaryFieldCodec.fieldWireSize(u16);  // returns ?usize = 2
```

### `EnumRegistry` — runtime enum dispatch

**Source: `src/reflection/enum_registry.zig`**

For dynamic dispatch based on string enum names (command parsing, state machines):

```zig
var registry = EnumRegistry.init(allocator);
defer registry.deinit(allocator);
try registry.registerEnum(allocator, Status);

const value = registry.nameToValue("active");  // returns ?i64
const name = registry.valueToName(1);          // returns ?[]const u8
```

### Rules

- Generate `ConstraintVTable` via `Constraint(T)` — never hand-write vtable functions.
- Store generated vtables as `const` globals; never stack-allocate a vtable and take a pointer.
- Use `@offsetOf(T, field_name)` for `Accessor.offset` — portable and compiler-verified.
- `setFast` / `getFast` bypass vtable dispatch for trusted, hot-path code.
- Do not use `Editable.set` in hot loops — the string path allocates.
- `TargetSchema` and similar schemas with runtime vtables (e.g., bitset vtables) must be heap-allocated via a `create()` function; document this requirement prominently.

### Python analogy

```python
# Python @dataclass + __setattr__ validation:
@dataclass
class Config:
    port: int = 8080
    def __setattr__(self, name, value):
        if name == "port" and not (1 <= value <= 65535):
            raise ValueError("Invalid port")
        super().__setattr__(name, value)

# Zig equivalent:
# - Zero bytes added to Config
# - Validation at the string boundary only
# - Hot path (setFast) is direct field write — no overhead at all
# - JSON Schema generated automatically from FieldMeta
```

---

## 4. Pattern 3 — Comptime Wrappers

Python decorators are used for cross-cutting concerns: logging, timing, retry logic, rate limiting, and validation. They are elegant because they separate the concern from the business logic. The problem is that they operate at runtime — they create closure objects, they add call frames, and they complicate stack traces. More critically, Python decorators can change a function's signature in ways that break type checkers unless carefully written.

In Zig, we replace decorators with **comptime wrapper functions**. Because Zig allows functions to return types (and therefore functions) at compile time, we can create a generic wrapper that takes the user's handler as a comptime argument, injects our logic around it, and returns a new function of *exactly the same type*. The compiler then inlines both layers completely. The binary output is identical to hand-written code — there is no closure, no extra call frame, and no performance overhead. The cross-cutting concern is genuinely invisible at runtime.

### The problem

```python
# Python: decorators for cross-cutting concerns
@timing
@retry(max=3)
def ingest_yago(path: str) -> None:
    # business logic
```

Python decorators execute at import time and wrap functions transparently. The downside: they're runtime closures, they add overhead, and they can be hard to type correctly.

### The Zig solution

A function that accepts a comptime function type and returns a new function of the same type, wrapping it with cross-cutting logic. The compiler inlines both layers.

### Canonical shape

```zig
/// Wrap `func` with execution timing. T must be a function type.
fn measure(comptime T: type, comptime func: T) T {
    return struct {
        fn wrapped(args: anytype) @typeInfo(T).@"fn".return_type.? {
            const start = std.time.nanoTimestamp();
            defer {
                const ns = std.time.nanoTimestamp() - start;
                std.log.debug("{s} took {d}µs", .{ @typeName(T), ns / 1000 });
            }
            return @call(.auto, func, args);
        }
    }.wrapped;
}
```

### Retry wrapper

```zig
fn withRetry(comptime T: type, comptime func: T, comptime max_attempts: usize) T {
    return struct {
        fn wrapped(args: anytype) @typeInfo(T).@"fn".return_type.? {
            var attempt: usize = 0;
            while (attempt < max_attempts) : (attempt += 1) {
                return @call(.auto, func, args) catch |e| {
                    if (attempt + 1 == max_attempts) return e;
                    std.time.sleep(10 * std.time.ns_per_ms * (attempt + 1));
                    continue;
                };
            }
            unreachable;
        }
    }.wrapped;
}
```

### Registration with a wrapper — the correct integration point

The wrapper must be applied **at the registration site**, before type erasure into a vtable. After the `register()` terminal, the system sees only `*anyopaque + *const VTable` — you can't retroactively wrap something you've already type-erased. The registration chain is therefore the natural moment: the developer's handler is still fully typed, the wrapper can be applied in one line, and the vtable never knows the wrapper exists.

Apply the wrapper **at the registration site**, before type erasure. The VTable sees only the opaque handle:

```zig
try registry.target("ingest", .command)
    .handler(measure(@TypeOf(ingestYago), ingestYago))
    .depends(&.{"download"})
    .register();
```

The rest of the system never sees the wrapper type.

### Rules

- Wrappers must preserve the exact function signature (`T` in, `T` out).
- Use `@call(.auto, func, args)` — never hardcode argument count.
- Keep wrapper bodies minimal; the compiler inlines them at every call site.
- **Do not wrap functions that are already behind a vtable** — wrap before type erasure, not after.
- Use `comptime_int` / `comptime` parameters for configuration baked in at compile time (retry count, log label). Use struct fields for runtime configuration.
- Wrappers have no persistent state — if you need state (e.g., a running total), use a capturing struct instead.

### Limitation: Zig cannot perfectly wrap arbitrary generic functions

**The critique is correct**: Zig's type system cannot create a wrapper function that preserves exact parameter types for an arbitrary generic function. The original Python decorator pattern `@retry(max=3)` wraps a function and returns one with the same signature — but Zig cannot express "a function that takes any arguments and returns any type."

**The workaround:** §14 introduces **call-site helpers** instead of true decorators:

```zig
// Instead of decorating a function reference:
const wrapped = retry(myHandler);  // Cannot preserve parameter types// Apply the wrapper at the call site:
const result = try retryCall(3, myHandler, .{arg1, arg2});
```

The `retryCall` helper wraps the *call*, not the *function reference*. This achieves the same effect — retry logic applied transparently — but the syntax differs. See `src/common/wrapper.zig` for`retryCall`, `wrapIf`, and `Pipeline.call`.

---

## 5. Pattern 4 — VTables

Finally, the orchestrator — the routing loop, the MCP server, the ingestion pipeline — needs to execute these wrapped, type-safe handlers uniformly. It cannot know at compile time which embedding provider was configured, which memory engine is active, or which WASM tool will satisfy a given query. This is where we cross the dynamic boundary using the **VTable pattern**.

VTables provide runtime polymorphism without C++ inheritance hierarchies and without Python's duck-typed implicit dispatch. A VTable interface in Zig is simply a struct holding a `*anyopaque` pointer to the data and a `*const VTable` pointer to a table of function pointers. Two pointers per handle, period. There is no object header, no type tag embedded in the allocation, no `dynamic_cast`, no RTTI. The interface is an explicit, auditable struct in your source code.

This explicitness is the point. When you read a `ConstraintVTable`, you see every operation the interface supports, including which ones are optional (`null` = not applicable). When you hold an `EmbeddingProvider`, you know exactly what it can do and exactly what it costs. The orchestrator loop becomes a straightforward iteration over uniform handles rather than a cascading `if isinstance` chain.

### The problem

```c++
// C++: inheritance hierarchies for polymorphism
class Engine { virtual std::vector<Row> query(std::string sql) = 0; };
class SqliteEngine : public Engine { ... };
class RedisEngine  : public Engine { ... };
// Cost: vtable per class, hidden in the ABI, can't control layout
```

```python
# Python: duck typing — convenient but zero static safety
def run_query(engine, sql):
    return engine.query(sql)  # Any object with .query() works, crashes at runtime if wrong
```

### The Zig solution

Runtime polymorphism via `ptr: *anyopaque` + `vtable: *const VTable`. Two pointers per handle; no heap allocation; no inheritance hierarchy. The Zig compiler enforces the interface at every implementation site.

### Canonical shape — `EmbeddingProvider` in `src/common/embeddings.zig`

```zig
pub const EmbeddingProvider = struct {
    ptr:    *anyopaque,
    vtable: *const VTable,

    pub const VTable = struct {
        name:       *const fn (ptr: *anyopaque) []const u8,
        dimensions: *const fn (ptr: *anyopaque) u32,
        embed:      *const fn (ptr: *anyopaque, allocator: std.mem.Allocator,
                               text: []const u8) anyerror![]f32,
        deinit:     *const fn (ptr: *anyopaque) void,
    };

    pub fn embed(self: EmbeddingProvider, allocator: std.mem.Allocator,
                 text: []const u8) ![]f32 {
        return self.vtable.embed(self.ptr, allocator, text);
    }
};
```

Three implementations share this interface (`OllamaEmbedding`, `OpenAiEmbedding`, `NoopEmbedding`) and are swapped via `createEmbeddingProvider()` based on config. The calling code never branches on implementation type.

### Parameterised VTable — runtime context pointer

When vtable behaviour depends on runtime state, store it in the vtable's `context` field:

```zig
pub fn bitSetConstraint(interner: *StringInterner) ConstraintVTable {
    return .{
        .context = @ptrCast(interner),
        .setCtxFn = struct {
            fn setCtx(vtable: *const ConstraintVTable, a: std.mem.Allocator,
                      ptr: *anyopaque, input: []const u8) anyerror!void {
                const i: *StringInterner = @ptrCast(@alignCast(@constCast(vtable.context.?)));
                const bs: *std.bit_set.DynamicBitSetUnmanaged = @ptrCast(@alignCast(ptr));
                try bitSetFromString(i, a, bs, input);
            }
        }.setCtx,
        // ... getBinaryFn, releaseFn ...
    };
}
```

This is used in `TargetSchema.create()`: each Target's `depends` and `provides` bitset fields need a `StringInterner` at parse time. The vtable carries the pointer; the `Accessor` carries a pointer to the vtable. Both are stable because `TargetSchema` is heap-allocated.

### `ConstraintVTable` — the full shape

```zig
pub const ConstraintVTable = struct {
    setFn:       *const fn (std.mem.Allocator, *anyopaque, []const u8) anyerror!void,
    getFn:       *const fn (std.mem.Allocator, *const anyopaque) anyerror![]const u8,

    // Optional extended paths — null = not applicable
    context:     ?*const anyopaque = null,
    releaseFn:   ?*const fn (std.mem.Allocator, *anyopaque) void = null,
    convertFn:   ?*const fn (*const ConstraintVTable, std.mem.Allocator,
                              *anyopaque, *const anyopaque, *const anyopaque) anyerror!void = null,
    setCtxFn:    ?*const fn (*const ConstraintVTable, std.mem.Allocator,
                              *anyopaque, []const u8) anyerror!void = null,
    getCtxFn:    ?*const fn (*const ConstraintVTable, std.mem.Allocator,
                              *const anyopaque) anyerror![]const u8 = null,
    setBinaryFn: ?*const fn (*const ConstraintVTable, *const anyopaque, []u8) anyerror!usize = null,
    getBinaryFn: ?*const fn (*const ConstraintVTable, std.mem.Allocator,
                              *anyopaque, []const u8) anyerror!void = null,
};
```

Optional fields use `null` to mean "not applicable" — callers null-check before calling.

### Calling through a vtable

```zig
fn callSet(accessor: *const Accessor, base: *anyopaque,
           allocator: std.mem.Allocator, input: []const u8, role: Role) !void {
    if (!accessor.permissions.canWrite(role)) return error.AccessDenied;
    const ptr: *anyopaque = @as([*]u8, @ptrCast(base))[accessor.offset..].ptr;
    if (accessor.constraint.setCtxFn) |f| {
        try f(accessor.constraint, allocator, ptr, input);
    } else {
        try accessor.constraint.setFn(allocator, ptr, input);
    }
}
```

### Rules

- VTables must be `const` globals or comptime-computed values. Never stack-allocate a vtable and take a pointer to it.
- The implementing struct must outlive every handle that references it. Callers must **own** the implementing struct (local var or heap-alloc). Never return a vtable handle pointing to a temporary.
- Optional vtable fields use `null`; callers must null-check before calling.
- Always pair `@ptrCast(@alignCast(...))` — never cast without alignment.
- Prefer the two-field `{ptr, vtable}` handle. The binary size delta is two pointers; the code clarity benefit is large.
- Do **not** use vtables when you only have one implementation — use generics.
- Do **not** use vtables when the type is known at compile time — use generics.

### Thread Safety Contract

VTable handles (`EmbeddingProvider`, `ConstraintVTable`) are two pointers: `ptr` (to the implementation struct) and `vtable` (to static const functions). Thread safety depends on the underlying implementation, not the handle shape.

**Rules:**

1. **Handle creation** is NOT thread-safe. Create handles on a single thread during initialization.
2. **Handle storage** in shared registries requires synchronization (mutex or read-write lock).
3. **Vtable function calls** ARE thread-safe IF the underlying implementation does not mutate shared state. Stateless implementations (`NoopEmbedding`, `Constraint(T)`) are always safe.
4. **Handle destruction** requires all concurrent calls to complete first. Join all threads before calling `deinit`.

**Pattern:**

```zig
// ✅ Thread-safe usage pattern
//
// 1. Create on init thread (single-threaded):
var impl = try OllamaEmbedding.init(allocator, null, null, null);
const provider = impl.provider();                          // creation: single-threaded
//
// 2. Pass to workers (read-only — vtable pointer never changes):
const worker = struct {
    fn run(p: EmbeddingProvider) !void {
        const vec = try p.embed(allocator, "hello");       // call: thread-safe if impl is
        defer allocator.free(vec);
    }
};
//
// 3. Destroy after all workers join:
thread.join();
provider.deinit();                                         // destruction: after join
```

**Debug-build thread assertions:**

`EmbeddingProvider` carries a `thread_id` field in debug/ReleaseSafe builds that is checked on every `embed()` call. This catches accidental cross-thread use of handles that were created for single-threaded use (e.g., HTTP clients that are not thread-safe). The assertion is a zero-cost no-op in ReleaseFast/ReleaseSmall.

```zig
// Cross-thread misuse is caught immediately in debug builds:
const provider = impl.provider();           // creation on thread A
// ... pass to thread B ...
try provider.embed(allocator, "text");      // assert fires: wrong thread
```

Remove the assertion (set `thread_safe = true` when constructing) for implementations that are documented as thread-safe.

### Python / C++ analogy

```python
# Python Protocol — same semantics, different safety model
from typing import Protocol
class EmbeddingProvider(Protocol):
    def embed(self, text: str) -> list[float]: ...
# Python: duck typing, crashes at runtime if wrong method signature
# Zig:    vtable enforced at every implementation site by the compiler
```

```c++
// C++ virtual — same cost, less control
// Zig vtable advantage: you control the struct layout, no hidden vtable pointer per instance
```

---

## 6. Pattern 5 — WASM / Binary IPC

At some point a system must execute code it cannot fully trust — dynamically loaded tool plugins, third-party WASM modules, or code generated by an LLM at runtime. Zig's type system provides safety within a compiled binary, but it cannot reach across the host/guest boundary into a sandboxed WASM process. At that boundary, all you have is a byte buffer.

The naive solution — serialize everything to JSON strings — is too slow and too large for the tight latency budget of the routing loop. The equally naive solution — just pass a Zig struct pointer — fails immediately across WASM boundaries because WebAssembly is a 32-bit address space and has its own memory, disjoint from the host's. Any native struct with default alignment will also have compiler-inserted padding that makes it non-portable across different compilers, languages, or ISAs.

The correct solution is `extern struct` with explicit `align(1)` on every field. This creates a fully portable, padding-free layout that can be `@memcpy`'d across any boundary, parsed by any language that respects the documented offsets, and validated by a magic number in the header. Variable-length data follows the fixed header using offset fields measured from the start of the buffer — no pointers, no relative addressing, just absolute offsets into a known-length byte slice.

### The problem

When executing untrusted or dynamically loaded code (WASM tools), you need a safe, portable, zero-copy message format that works across the host/guest boundary. Strings are too expensive; native structs have alignment and padding problems across compilers.

### The Zig solution

`extern struct` with `align(1)` on every field. Layout is deterministic, padding-free, and safe to `@memcpy` across any language boundary.

### Binary message layout — `src/wasm/wasm.zig`

```zig
pub const BinaryHeader = extern struct {
    magic:        u32 align(1),  // 0xC04A_C0DE — integrity guard
    version:      u8  align(1),  // BINARY_SCHEMA_VERSION (currently 1)
    payload_type: PayloadType align(1),  // enum(u8)
    _pad:         [2]u8 align(1) = .{0, 0},
};

pub const BinaryExecutionRequest = extern struct {
    header:       BinaryHeader align(1),
    target_id:    i64 align(1),
    input_offset: u32 align(1),  // offset from start of buffer to input bytes
    input_len:    u32 align(1),
    flags:        u32 align(1),  // VERBOSE | DRY_RUN | FORCE
};

pub const BinaryExecutionResult = extern struct {
    header:               BinaryHeader align(1),
    success:              u32 align(1),
    error_code:           u32 align(1),
    output_offset:        u32 align(1),
    output_len:           u32 align(1),
    provides_words_offset: u32 align(1),  // DynamicBitSetUnmanaged word array
    provides_words_count:  u32 align(1),

    pub fn getProvidesBitSet(self: *const BinaryExecutionResult,
                              allocator: std.mem.Allocator,
                              payload: []const u8) !std.bit_set.DynamicBitSetUnmanaged {
        const wc = self.provides_words_count;
        var bs = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(
            allocator, @as(usize, wc) * @bitSizeOf(usize));
        errdefer bs.deinit(allocator);
        const off = self.provides_words_offset;
        for (0..wc) |i| {
            const w = std.mem.readInt(u64, payload[off + i * 8 ..][0..8], .little);
            bs.masks[i] = @intCast(w);
        }
        return bs;
    }
};
```

### Sending a request (current implementation in `src/coral/cache.zig`)

```zig
// Build payload: header struct + input bytes contiguous in one buffer
var req = BinaryExecutionRequest{
    .header    = .{ .magic = BINARY_MAGIC, .version = BINARY_SCHEMA_VERSION,
                    .payload_type = .execution_request },
    .target_id    = target_id,
    .input_offset = @sizeOf(BinaryExecutionRequest),
    .input_len    = @intCast(input.len),
    .flags        = 0,
};
var buf: std.ArrayListUnmanaged(u8) = .{};
defer buf.deinit(allocator);
try buf.appendSlice(allocator, std.mem.asBytes(&req));
try buf.appendSlice(allocator, input);

const rc = extism_plugin_call(plugin, "execute", buf.items.ptr, buf.items.len);
```

### Layout rules for `extern struct`

```zig
// CORRECT: all fields carry align(1) — ABI-portable across languages
pub const Message = extern struct {
    tag:  u8  align(1),
    len:  u32 align(1),
    data: [16]u8 align(1),
};

// WRONG: default alignment inserts padding — breaks cross-language reads
pub const Message = extern struct {
    tag:  u8,   // 1 byte + 3 bytes implicit padding before len
    len:  u32,
};
```

### Variable-length payload pattern

Fixed header → offset fields → payload sections. All offsets are from the start of the message buffer:

```
[BinaryHeader][BinaryExecutionRequest header][input_bytes...]
                            ↑
              input_offset points here (= sizeof(BinaryExecutionRequest))
```

### Rules

- Always use `align(1)` on every field of an `extern struct` used for IPC.
- Offsets are absolute from buffer start, never relative to struct end.
- Include a magic number (`BINARY_MAGIC`) and a version (`BINARY_SCHEMA_VERSION`) in every header — validate on receipt.
- Use `std.mem.readInt(u64, slice[0..8], .little)` for cross-platform integer reads from buffers.
- The `ExecutionRequestBuilder` pattern (arena-backed payload assembly) is planned for P3.3 to formalize the buffer construction shown above.

---

## 7. Pattern 6 — Arena-Backed Builders

Python programmers often reach for the garbage collector without thinking about it — create intermediates, let them go out of scope, trust the GC to collect them later. C++ programmers manage this with RAII and `unique_ptr`. Both approaches have the same underlying structure: you define a logical unit of work, and everything created during that work is cleaned up when the work is done.

Zig has no garbage collector and no RAII destructors, but it has something arguably better for bulk operations: `ArenaAllocator`. An arena is a bump-pointer allocator that hands out memory by simply advancing a pointer and frees everything at once by resetting the pointer to the beginning. Individual frees are impossible, which means every allocation in the arena is effectively free at deinit time. No fragmentation, no list traversal, one `defer arena.deinit()` at the scope entrance to guarantee cleanup.

When this is combined with a fluent builder, you get a particularly clean composition: the builder's accumulation phase corresponds exactly to the arena's allocation phase, and the terminal method corresponds exactly to the arena's commit-or-discard decision point. The builder accumulates into the arena; the terminal either persists the result to a long-lived allocator (escaping the arena) or discards everything at once.

The practical impact for this codebase is significant. Batch YAGO ingestion processes hundreds of thousands of triples. Each triple generates multiple intermediate string allocations during RDF normalisation and ContextNode construction. An arena reset between batches — a single pointer update — replaces hundreds of individual `allocator.free()` calls that would otherwise be required, and eliminates the category of bugs where one of those frees is missed or called twice.

### The problem

Batch processing generates hundreds of intermediate allocations (node struct fields, edge descriptors, temporary strings). Managing them with individual `defer allocator.free(...)` calls is verbose and error-prone. Forgetting one is a memory leak.

### The Zig solution

Scope an `ArenaAllocator` to the logical unit of work (a request, a batch, a pipeline invocation). All intermediate allocations come from the arena. At the end of the scope, one `arena.deinit()` frees everything.

### In `BatchIngestor` — `src/coral/batch.zig`

Each batch cycle uses an arena reset rather than per-node frees:

```zig
pub fn ingestSource(self: *BatchIngestor, source: []const u8) !void {
    var batch_arena = std.heap.ArenaAllocator.init(self.allocator);
    defer batch_arena.deinit();
    const a = batch_arena.allocator();

    // Hundreds of TripleMapper intermediate allocations come from `a`
    var mapper = TripleMapper.init(a, &self.library, self.config);
    // ... parse triples, accumulate nodes ...
    try mapper.flush();       // writes to Library (owned by library, not the arena)
    // batch_arena.deinit() frees everything that didn't escape to Library
}
```

### In `McpServer` request handling — `src/coral/mcp.zig`

Each incoming JSON-RPC request gets its own arena. The response is the only thing that escapes:

```zig
pub fn handleRequest(self: *McpServer, raw_json: []const u8) ![]const u8 {
    var req_arena = std.heap.ArenaAllocator.init(self.allocator);
    defer req_arena.deinit();
    const a = req_arena.allocator();

    const req  = try parseJsonRpc(a, raw_json);           // arena-owned
    const result = try self.reactor.route(a, req.query);  // arena-owned intermediates
    return try serializeResponse(self.allocator, result); // caller-owned response only
}
```

No mutex inside `handleRequest`. Each worker thread gets its own arena; there is no allocator contention.

### Arena + Fluent Builder composition

```
Builder created  →  Arena init
Setters called   →  Arena allocations for intermediate values
Terminal called  →  Long-lived data moves to registry / library; arena deinits
```

The simplification is largest when intermediate allocations are numerous and short-lived relative to the data they produce. This is exactly the profile of batch ingestion and per-request processing.

### Rules

- Scope the arena to the natural unit of work (request, batch, pipeline run).
- Only escape to the caller's allocator what the caller explicitly owns.
- `arena.reset(.retain_capacity)` between loop iterations avoids page allocator pressure for repeated operations (e.g., a long-running worker thread).
- Do **not** wrap a single-allocation function in an arena — it adds overhead for no benefit.
- Do **not** use arena for long-lived data structures (Library, TargetRegistry) — they need their own allocators.
- Config loading (`Config.load()`) already uses an internal arena — don't add another layer.

---

## 8. Pattern 7 — Typed Opaque Handles

This is the simplest pattern in the group and the one most often overlooked. As a system grows, integer IDs proliferate: node IDs, session IDs, bit-index handles, version numbers, timestamps. At some scale, they become indistinguishable to the type system. A function that takes a `NodeId` will happily accept a `SessionId` if both are `i64`. The compiler will not warn. The bug will surface in production when a session ID is used to look up a graph node.

The fix is to make the type system distinguish them at zero cost. Zig's non-exhaustive `enum(i64) { _ }` idiom creates a new type with the same underlying integer representation. Conversion requires an explicit `@enumFromInt` / `@intFromEnum` at the boundary. The compiler rejects accidental mixing at every other call site. There is no runtime overhead — the enum is `i64` in the binary. There is no boilerplate — you just add `enum(i64) { _ }` as the type.

### The problem

```python
# Python: type aliases are just hints — no enforcement
NodeId = int
SessionId = int
process_node(session_id)  # Accepted silently — crashes later
```

### The Zig solution

`enum(i64) { _ }` creates a distinct type that shares the integer representation. The compiler rejects mixing `NodeId` with `SessionId` even though both are `i64` underneath.

### In `src/coral/db.zig`

```zig
pub const NodeId = i64;  // Matches SQLite INTEGER PRIMARY KEY
```

The pattern is also used structurally throughout `db.zig` — functions taking `NodeId` cannot be accidentally called with a raw loop counter or session ID. For stricter enforcement, use the `enum` variant:

```zig
pub const NodeId    = enum(i64) { _ };
pub const SessionId = enum(i64) { _ };

fn processNode(id: NodeId) void { ... }

const node: NodeId    = @enumFromInt(42);
const sess: SessionId = @enumFromInt(42);
processNode(node);  // OK
processNode(sess);  // Compile error: expected NodeId, found SessionId
```

### Rules

- Use for any integer ID that crosses module boundaries and must not be mixed.
- Use `@enumFromInt` / `@intFromEnum` to convert at the boundary.
- Do **not** use when you need arithmetic on the ID (`id + 1`).
- The `enum(i64) { _ }` variant (non-exhaustive enum) is the strictest form. `i64` aliases are less strict but still communicate intent.

---

## 9. Why They Work in Synergy

The patterns are not just independently beneficial — their value multiplies when composed. Each pattern hands off to the next in a way that eliminates costs that the previous pattern alone could not avoid.

To understand why, think about what each pattern *produces* and what it *consumes*:

- The **Fluent Builder** produces a fully-configured, allocation-complete object at its terminal call. It consumes an allocator (often an arena) and a registry.
- **Comptime Reflection** produces vtable-driven field access that requires zero per-field overhead for code that was already compiled together. It consumes struct definitions.
- **Comptime Wrappers** produce instrumented functions of the same type as their input. They consume comptime function values.
- **VTables** produce uniform handles that the orchestrator can call without branching. They consume type-erased pointers and `const` vtable globals.
- **Arenas** produce a single deinit that frees all intermediate allocations. They consume an allocator scope boundary.

These hand-offs are not accidental. The Fluent Builder is a natural scope boundary for an arena precisely because builders have a clear terminal. Comptime Wrappers can only wrap before type erasure, and the registration terminal is exactly the moment of type erasure — so the wrapper goes at that site naturally. VTables require `const` globals for their function pointer tables, and comptime-generated vtables are automatically `const` globals. The patterns fit together because they were each designed around the same philosophy: explicit scope, explicit ownership, explicit dispatch, zero hidden cost.

### Synergy 1: Builder + Arena eliminates per-call allocator contention

A `QueueReactorBuilder` call creates a per-request arena for all intermediate routing allocations. Each worker thread has its own arena. The only mutex-protected objects are `Library` (SQLite writes), `StringInterner` (intern calls), and the MCP work queue. Builder accumulation is completely lock-free.

### Synergy 2: Comptime Reflection + VTable makes schema the single source of truth

`Constraint(T)` generates vtable functions from `@typeInfo(T)`. The `Accessor` array is generated from the struct's field list at comptime. Adding a new field to a struct automatically generates its vtable, its accessor entry, its JSON Schema description, and its binary wire size — in one edit, at one place.

Without this synergy, schema changes require updates in: the struct definition, the serializer, the deserializer, the JSON Schema emitter, and the binary codec. Four files instead of one.

### Synergy 3: Comptime Wrapper + VTable gives zero-overhead observability

Wrap a function with `measure()` at the registration site. The compiler inlines the wrapper. The VTable sees only the type-erased handle. The entire orchestrator is instrumented without modifying any business logic, and the cost is truly zero (inlined timing code is identical to hand-written code).

### Synergy 4: Arena + Binary IPC eliminates payload lifetime management

`BinaryExecutionRequest` payload assembly uses an `ArrayListUnmanaged` backed by an arena. The arena is freed after `extism_plugin_call` returns. The result (`BinaryExecutionResult`) is read directly from the Extism output buffer before it disappears. No individual allocation to track; one `defer arena.deinit()` cleans everything.

---

## 10. Anti-Patterns

The anti-patterns below are not hypothetical. They represent the most common ways these patterns are misapplied, each with a concrete explanation of why the mistake is made and what it actually causes.

### ❌ Applying Fluent Builder to a 3-parameter init

```zig
// Wrong: Library.init() has 3 params and is already readable
Library.init(allocator, db_path, timeout)  // Fine as-is
```

A builder for 3 params adds boilerplate with no readability benefit. The threshold is approximately 5+ parameters, or any configuration that reads naturally as chained declarations.

### ❌ Using Comptime Reflection in a hot loop

```zig
// Wrong: string path allocates on every iteration
for (items) |item| {
    try editable.set(allocator, "count", std.fmt.allocPrint(allocator, "{d}", .{item.count}));
}

// Right: direct field access
for (items) |item| {
    config.count = item.count;
}
```

The string path is for boundaries (data that arrives as strings from outside the process). Inside the process, use direct field access or `setFast`.

### ❌ Stack-allocating a vtable

```zig
// Wrong: vtable will dangle when the function returns
fn makeProvider() EmbeddingProvider {
    const vtable = ConstraintVTable{ .setFn = ..., .getFn = ... };  // STACK
    return .{ .ptr = impl, .vtable = &vtable };  // DANGLING POINTER
}

// Right: vtable is a const global
const my_vtable = ConstraintVTable{ .setFn = ..., .getFn = ... };  // GLOBAL
```

### ❌ Stack-allocating a schema with internal vtable pointers

```zig
// Wrong: TargetSchema stores vtables by value internally
var schema = TargetSchema.init(allocator, interner);  // STACK
var dyn = schema.createEditable(buffer);              // DANGLING: Accessor.constraint points into schema

// Right: heap-allocate schemas that contain vtables
const schema = try TargetSchema.create(allocator, interner);
defer schema.destroy(allocator);
```

This is the subtlest memory safety issue in the pattern. When a vtable is stored inside a struct and accessors hold pointers to it, moving the struct (returning by value, storing in a resizing container) invalidates all the pointers. Always heap-allocate structs that serve as vtable storage.

### ❌ Wrapping a vtable-dispatched function

```zig
// Wrong: wrapping after type erasure adds a call layer with no inlining
const wrapped_vtable = wrap(provider.vtable.embed);  // Can't inline through vtable

// Right: wrap before registration
const wrapped_embed = measure(@TypeOf(embedImpl), embedImpl);
const provider = OllamaEmbedding{ .embed_fn = wrapped_embed, ... };
```

Wrappers must be applied before type erasure, at the registration site.

### ❌ Arena for long-lived data

```zig
// Wrong: Library should outlive all requests
var arena = std.heap.ArenaAllocator.init(page_allocator);
var lib = Library.init(arena.allocator(), ...);  // Library freed when arena deinits
// Later: arena.deinit() destroys Library while requests still reference it
```

Arenas scope to operations, not to data structures with long lifetimes. The Library, TargetRegistry, and StringInterner use their own persistent allocators.

### ❌ Mixing typed and untyped access in DynamicEditable

```zig
// DynamicEditable has no type information — don't try to add it
var dyn = try DynamicEditable.init(allocator, buffer, accessors);
// dyn has no idea what type "port" is — it works through ConstraintVTable only
// The string path is the contract; don't try to bypass it
```

`DynamicEditable` operates on raw byte buffers via vtables. It has no type information at runtime. For typed access over a known struct, use `Editable(T)` or `TypedEditable`.

---

## 11. Pattern Selection Guide

The most common question when reading new code is: "which pattern, if any, should I reach for here?" The guide below is a decision tree. The questions are ordered so that the most frequent case appears first. If you reach the end without matching anything, the right answer is almost certainly plain imperative Zig: a simple `init()`, direct field access, or a free function.

```
Does the construction have 5+ parameters, or reads like configuration?
  YES → Fluent Builder
  NO  → Simple init()

Does data arrive as a string from outside the process?
  YES → Editable(T).set() or DynamicEditable.set()
  NO, but field name known at runtime → Editable(T).setFast()
  NO, field name known at compile time → direct field access

Do you need runtime polymorphism (multiple swappable implementations)?
  YES → VTable
  NO, one implementation → generics

Does a struct cross the WASM host/guest boundary?
  YES → extern struct with align(1) fields + BinaryFieldCodec
  NO  → normal struct

Does a batch operation allocate many short-lived intermediates?
  YES → ArenaAllocator scoped to the batch
  NO  → caller-managed allocator

Is an integer ID passed across module boundaries?
  YES → enum(i64) { _ } typed opaque handle
  NO  → plain integer

Does a vtable need runtime state (e.g., StringInterner context)?
  YES → parameterised vtable with context: ?*const anyopaque
  NO  → plain comptime-generated vtable
```

---

## 12. Thread Safety and the Patterns

These patterns are **weakly positive** for thread safety:

| Pattern | Thread safety story |
|---------|---------------------|
| VTables | `*const VTable` globals are read-only — zero contention |
| Comptime Reflection | Accessor arrays are `const` comptime globals — zero contention |
| Comptime Wrappers | Compile-time constants — zero contention |
| Fluent Builders | Per-request; lock-free until terminal `register()` |
| Arena-Backed Builders | Per-thread arenas eliminate allocator lock contention |
| Typed Opaque Handles | No state; no contention |

The shared mutable objects that **do** need explicit protection in this codebase:

| Object | Mechanism | Reason |
|--------|-----------|--------|
| `Library` | `std.Thread.Mutex` on write paths | SQLite writes must serialize |
| `StringInterner` | `std.Thread.RwLock` + double-checked locking in `intern()` | Read-heavy concurrent intern calls |
| `QueueReactor` work queue | `Mutex` + `Condition` | Producer-consumer coordination |

Arenas eliminate the most common source of allocator contention: a `GeneralPurposeAllocator` under concurrent pressure serializes on its internal free-list mutex. Per-request arenas backed by a thread-local page allocator avoid this entirely.

---

## 13. Schema Evolution and Versioning (M4)

`src/reflection/schema_version.zig` provides `SchemaVersion` and helpers for
forward- and backward-compatible schema evolution.

### SchemaVersion

```zig
pub const SchemaVersion = struct {
    major: u16,
    minor: u16 = 0,

    pub fn compatible(self, other: SchemaVersion) bool  // true if same major
    pub fn isNewerThan(self, other: SchemaVersion) bool
    pub fn eql(self, other: SchemaVersion) bool
    pub fn format(self, writer: anytype) !void   // {f} → "v1.0"
};

pub const SCHEMA_CURRENT: SchemaVersion = .{ .major = 1, .minor = 0 };

pub fn checkCompatible(stored: SchemaVersion, current: SchemaVersion) !void;
```

### Versioning Fields on Accessor

Each `Accessor` carries three optional versioning fields (all default to
v1.0 / null — backward-compatible with pre-versioning code):

```zig
pub const Accessor = struct {
    // ... existing fields ...

    version_added:   SchemaVersion = SCHEMA_CURRENT,
    version_removed: ?SchemaVersion = null,
    migrate_from:    ?*const fn (old: []const u8, allocator: std.mem.Allocator)
                         anyerror![]const u8 = null,

    pub fn isPresentIn(self: *const Accessor, stored: SchemaVersion) bool
};
```

**`isPresentIn(stored)`** returns true when:
1. `stored >= version_added` (field had been introduced)
2. `version_removed` is null OR `stored < version_removed` (field not yet removed)

**`migrate_from`** transforms the serialized string before passing it to
`constraint.setFn`:

```zig
if (accessor.migrate_from) |migrate| {
    const new_val = try migrate(raw_value, allocator);
    defer allocator.free(new_val);
    try accessor.constraint.setFn(allocator, field_ptr, new_val);
} else {
    try accessor.constraint.setFn(allocator, field_ptr, raw_value);
}
```

### Versioning Fields on ConstraintVTable

```zig
pub const ConstraintVTable = struct {
    // ... existing vtable entries ...

    version:   SchemaVersion = SCHEMA_CURRENT,
    migrateFn: ?*const fn (from: SchemaVersion, to: SchemaVersion,
                            allocator: std.mem.Allocator, ptr: *anyopaque)
                   anyerror!void = null,
};
```

### Upgrade Path for Schema Changes

| Change | Bump | Action |
|--------|------|--------|
| Add field with default | `minor` | Set `version_added = .{ .major=1, .minor=N }` |
| Remove field | `major` | Set `version_removed = .{ .major=N }` on old accessor |
| Rename field | `major` | Old accessor with `version_removed`; new accessor with `version_added` + `migrate_from` |
| Type change | `major` | Old accessor + `migrate_from` transforms old type string to new |

### Re-exports from `reflection`

```zig
const reflection = @import("reflection");
reflection.SchemaVersion    // the struct
reflection.SCHEMA_CURRENT   // v1.0 constant
reflection.checkCompatible  // returns error.SchemaMismatch on major mismatch
```

---

## 14. Conditional Wrapper Application (M9)

`src/common/wrapper.zig` provides build-mode-conditional wrappers, a retry
call helper, and a composable `Pipeline` for call sites.

### Why call helpers instead of type-preserving wrappers

Zig's type system cannot create a wrapper function that perfectly preserves an
arbitrary function's parameter types (generic functions cannot be cast to
concrete function types).  The practical solution is **call helpers** that wrap
a *call site* rather than a *function reference*:

```zig
// Instead of:
const result = try func(arg1, arg2);

// Write:
const result = try retryCall(3, func, .{arg1, arg2});
```

### wrapIf

Selects between two functions of the *same* type at comptime:

```zig
const handler = wrapIf(builtin.mode == .Debug, debugHandler, releaseHandler);
```

In release builds, `releaseHandler` is selected with zero overhead.

### retryCall

```zig
const result = try retryCall(3, fetchData, .{url, allocator});
```

Calls `func(args...)` up to `max_attempts` times.  Returns the first success
or the last error on exhaustion.

### Pipeline

Composes wrapper kinds around a call site:

```zig
const result = try Pipeline.call(&.{.retry, .none}, fetchData, .{url, alloc});
```

Currently defined `WrapperKind` values: `.none` (identity), `.retry` (3 attempts).

### Composition Order

When combining multiple wrappers, apply outer-to-inner:

| Layer | Purpose |
|-------|---------|
| 1 | Rate limiting — reject early if overloaded |
| 2 | Auth — reject early if unauthorized |
| 3 | Tracing — start span (→ `Scope.begin` in logging.zig) |
| 4 | Timing — measure full duration (→ `callLogged`) |
| 5 | Retry — retry on transient failure (→ `retryCall`) |
| 6 | Validation — validate input (→ `validateValue`) |
| 7 | Core handler |

### Re-exports from `common`

```zig
common.wrapIf       // conditional function selector
common.retryCall    // retry call helper
common.WrapperKind  // enum for Pipeline
common.Pipeline     // call pipeline factory
```

---

## 15. Structured Logging Context (M8)

`src/common/logging.zig` provides thread-local request context and a timing scope.

### LogContext

```zig
const LogContext = struct {
    request_id: ?[]const u8 = null,
    user_id:    ?[]const u8 = null,
    trace_id:   ?[]const u8 = null,
    span_id:    ?[]const u8 = null,

    threadlocal var current: ?LogContext = null;

    pub fn set(ctx: LogContext) void   { current = ctx; }
    pub fn get() ?LogContext           { return current; }
    pub fn clear() void                { current = null; }

    // {f} format: [req=<id> user=<id> trace=<id> span=<id>]
    pub fn format(self: LogContext, writer: anytype) !void { ... }
};
```

**Key properties:**
- Thread-local: each OS thread has its own slot — no synchronization needed.
- Zero-copy: `LogContext` holds slices into caller-owned memory, not copies.
- Context does NOT propagate across thread boundaries. Pass the value explicitly
  and call `LogContext.set()` on the new thread.

### Scope

```zig
pub const Scope = struct {
    pub fn begin(name: []const u8) Scope { ... }  // logs start if context active
    pub fn end(self: Scope) void { ... }           // logs duration in µs
};
```

When `LogContext.get()` is null (no context active), `Scope.begin/end` are
no-ops — no allocation, no log call. Zero overhead in production.

Usage:

```zig
// Manual scope:
const scope = Scope.begin("embed");
defer scope.end();
const vec = try provider.embed(allocator, text);

// Single-expression callsite:
const vec = try callLogged("embed", provider.embed, .{ allocator, text });
```

### callLogged

```zig
pub inline fn callLogged(
    comptime name: []const u8,
    func: anytype,
    args: anytype,
) @typeInfo(@TypeOf(func)).@"fn".return_type.? { ... }
```

Wraps a single function call in a `Scope`. Error unions propagate normally —
callers use `try` as needed.

### Setting context at request boundaries

```zig
pub fn handleRequest(self: *McpServer, raw_json: []const u8) ![]const u8 {
    LogContext.set(.{ .request_id = generateRequestId() });
    defer LogContext.clear();

    // All Scope/callLogged calls in this stack now log with context.
    return try self.reactor.route(raw_json);
}
```

### Re-exports from `common`

```zig
const common = @import("common");
common.LogContext    // the struct
common.LogScope      // alias for logging.Scope
common.callLogged    // the inline wrapper
```

---

## 16. Summary for AI Agents

The patterns in this document are the architectural vocabulary of this codebase. Code that departs from them without a clear local reason will be inconsistent, harder to test, and harder to extend. Code that applies them correctly will compose cleanly with the existing infrastructure, pass the test suite without leaks, and be readable to any contributor who has read this document.

The following directives distill everything above into concrete action items for writing new code:

When writing new code in this codebase:

1. **Check the source first.** Run `guidance explain "<topic>"` before writing. The pattern you need is probably already implemented and tested.

2. **New multi-parameter construction** → `TargetBuilder` in `src/common/registry.zig` is the template. Use `err: ?*BuilderError` with an `arena: std.heap.ArenaAllocator` for error strings. Capture phase, field, value, and constraint. Log `e.message` at the terminal for diagnostics.

3. **New field-level access** → Add `editable: Editable(Self) = .{}` to the struct. Use `set`/`get` at boundaries; `setFast`/`getFast` inside the process.

4. **New cross-cutting logic** → Use call-site helpers (`retryCall`, `callLogged`, `Pipeline.call`). Do not try to create true comptime decorators — Zig cannot preserve exact parameter types for wrapped generic functions.

5. **New subsystem with multiple implementations** → `EmbeddingProvider` in `src/common/embeddings.zig` is the template. Two-field `{ptr, vtable}` handle, `const` vtable globals.

6. **New WASM/binary IPC type** → `extern struct` with `align(1)` on all fields, magic + version in header, offsets from buffer start.

7. **New batch-processing loop** → Arena scoped to the batch. Only escape what the caller owns.

8. **Vtable with runtime state** → `bitSetConstraint` in `src/common/interner.zig` is the template. `context: ?*const anyopaque` in the vtable, retrieved by `setCtxFn`/`getCtxFn`.

9. **Never stack-allocate a vtable or a struct that stores vtables internally.** This is the most common memory bug in this pattern family.

10. **Never use the string reflection path in hot loops.** It allocates. Use direct field access.
