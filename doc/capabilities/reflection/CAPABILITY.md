---
name: reflection
description: REPLACED by Rust's native serde serialization, bon builders, FieldAccess/Describable derive macros, and HashMap-based dynamic access. Zig's Editable(T)/DynamicEditable/ConstraintVTable compile-time reflection is replaced by trait-based reflection via fluent-wvr.
anchors:
  - FieldAccess
  - Describable
  - Component
  - serde
  - bon
  - serde_json
  - #[derive(FieldAccess)]
  - #[derive(Describable)]
  - #[builder(start_fn = new)]
---

# Reflection

**This capability is replaced, not ported.** Rust does not have Zig's comptime reflection, nor does it need it. The `fluent-wvr` crate provides trait-based reflection via `FieldAccess` and `Describable` derive macros.

## Zig's approach — replaced

Zig's `src/reflection/` module provides:

| Feature | Zig type | Rust replacement |
|---------|----------|------------------|
| Per-field parse/format | `ConstraintVTable`, `Constraint(T)` | `#[derive(FieldAccess)]` with `#[field(min, max, options)]` attributes |
| Field access by name | `Editable(T)`, `DynamicEditable` | `FieldAccess` trait (`get_field`, `set_field`, `field_names`) |
| Schema description | `describeSchema()` | `#[derive(Describable)]` generating `describe()`, `describe_short()`, `describe_detailed()` |
| Permission-based access | `Role`, `RolePermissions` | Application-layer `match role { ... }` on access |
| Binary encoding | `BinaryFieldCodec` (LE bytes) | `to_le_bytes()` / `from_le_bytes()` on primitives |
| Enum registry | `EnumRegistry` (runtime name↔value) | `serde_json::Value` + match/FromStr |
| JSON Schema generation | `Editable(T).describeSchema()` | `#[derive(Describable)]` with `#[describable(capabilities=...)]` |
| Struct builders | Manual fluent `*Self` chain | `#[derive(bon::Builder)]` via `bon` |

## FieldAccess derive macro

`#[derive(FieldAccess)]` on a struct generates `get_field()`, `set_field()`, and `field_names()` methods that enable runtime field access by name:

```rust
use fluent_wvr::FieldAccess;

#[derive(FieldAccess)]
pub struct ToolConfig {
    #[field(name = "port", description = "TCP listen port", min = 1, max = 65535)]
    pub port: u16,
    #[field(name = "host", description = "Host address")]
    pub host: String,
    #[field(name = "verbose", description = "Enable verbose logging")]
    pub verbose: bool,
}

let mut config = ToolConfig { port: 8080, host: "localhost".into(), verbose: false };

// Runtime field access by name
config.set_field("port", "9000")?;
assert_eq!(config.get_field("port")?, "9000");
assert_eq!(config.field_names(), &["port", "host", "verbose"]);
```

### Field attributes

| Attribute | Purpose |
|-----------|---------|
| `name = "..."` | Override field name in reflection |
| `description = "..."` | Human-readable description for schemas |
| `min = N` | Minimum value constraint (numeric types) |
| `max = N` | Maximum value constraint (numeric types) |
| `step = N` | Step size for numeric fields |
| `options = [...]` | Allowed values (enum-like) |
| `read_only` | Field cannot be set via `set_field` |
| `write_only` | Field cannot be read via `get_field` |

## Describable derive macro

`#[derive(Describable)]` generates schema description methods for MCP tool parameter validation and TUI editors:

```rust
use fluent_wvr::Describable;

#[derive(Describable)]
#[describable(name = "ToolConfig", description = "Configuration for the tool")]
pub struct ToolConfig {
    #[describable(description = "TCP listen port")]
    pub port: u16,
    pub host: String,
}

let config = ToolConfig { port: 8080, host: "localhost".into() };
let schema = config.describe();        // Full JSON schema
let brief = config.describe_short();   // One-line summary
let detailed = config.describe_detailed(); // Expanded description
let caps = config.describe_capabilities(); // Capability tags
```

## Component supertrait

Any type implementing `FieldAccess + Describable + WorkUnit + Send + Sync` automatically becomes a `Component` via the blanket impl:

```rust
use fluent_wvr::Component;

// Blanket impl: no manual Component impl needed
// impl<T: FieldAccess + Describable + WorkUnit + Send + Sync> Component for T {}
```

This means the same struct can be used as a native Rust component, a WASM plugin bridge, or a database-driven dynamic component — all through the same `Arc<dyn Component>` handle.

## `bon` Builder pattern

```rust
use bon::Builder;

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

let t = Target::new()
    .id(1)
    .name("build".into())
    .target_type(TargetType::File)
    .executor(ExecutorKind::Native)
    .depends(bitvec![0, 1])
    .provides(bitvec![1, 0])
    .build();
```

## `serde` serialization

```rust
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Member {
    pub type_name: MemberType,
    pub name: SmolStr,
    pub signature: Option<SmolStr>,
    pub is_pub: bool,
    pub line: Option<u32>,
}

let json = serde_json::to_string_pretty(&member)?;
let restored: Member = serde_json::from_str(&json)?;
```

## Dynamic access

Where Zig uses `DynamicEditable` with runtime accessor slices, Rust uses:

```rust
let mut dynamic: HashMap<ArcIntern<str>, serde_json::Value> = HashMap::new();
dynamic.insert(ArcIntern::from("port"), serde_json::json!(9000));
dynamic.insert(ArcIntern::from("host"), serde_json::json!("localhost"));

// Read
if let Some(serde_json::Value::Number(n)) = dynamic.get(&ArcIntern::from("port")) {
    let port: u16 = n.as_u64().unwrap() as u16;
}

// Write
dynamic.insert(ArcIntern::from("port"), serde_json::json!(8080));
```

## Key files

- `fluent-wvr/src/lib.rs` — `FieldAccess`, `Describable`, `Component` traits, `Capability`, `WorkUnit`
- `fluent-wvr-macros/src/lib.rs` — `#[derive(FieldAccess)]`, `#[derive(Describable)]` proc macros

## Semantic Deviations

| Aspect | Zig | Rust |
|--------|-----|------|
| Field introspection | `@typeInfo`, `FieldMeta`, compile-time | `#[derive(FieldAccess)]` with `#[field(...)]` attributes |
| Schema description | `describeSchema()` built-in | `#[derive(Describable)]` with `#[describable(...)]` attributes |
| Builder pattern | Manual `*Self` fluent chain (60+ lines) | `#[derive(Builder)]` via `bon` (1 annotation) |
| Dynamic get/set | `Editable(T)` zero-size mixin | `HashMap<ArcIntern<str>, Value>` |
| Permissions | `Role`, `RolePermissions`, `perm_*` bitfields | Application-layer match |
| Binary encoding | `BinaryFieldCodec` generic | `to_le_bytes()` per-type |
| Enum registry | `EnumRegistry` with comptime init | `serde_json::Value` or `FromStr` |
| Component | Manual trait dispatch | Blanket impl: `impl<T: FieldAccess + Describable + WorkUnit + Send + Sync> Component for T {}` |

## Zig reference

See `doc/capabilities/reflection/CAPABILITY.md` in the Zig project for the original module design (Editable(T), DynamicEditable, ConstraintVTable, Role permissions).
