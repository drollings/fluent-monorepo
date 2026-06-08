---
name: reflection
description: REPLACED by Rust's native serde serialization, bon builders, and HashMap-based dynamic access. Zig's Editable(T)/DynamicEditable/ConstraintVTable compile-time reflection is not replicated — Rust's trait system and serde derive macros provide equivalent capabilities with less complexity.
anchors:
  - serde
  - bon
  - serde_json
  - HashMap<ArcIntern<str>, serde_json::Value>
  - #[derive(Serialize, Deserialize)]
  - #[builder(start_fn = new)]
---

# Reflection

**This capability is replaced, not ported.** Rust does not have Zig's comptime reflection, nor does it need it.

## Zig's approach — replaced

Zig's `src/reflection/` module provides:

| Feature | Zig type | Rust replacement |
|---------|----------|------------------|
| Per-field parse/format | `ConstraintVTable`, `Constraint(T)` | `serde` with custom `Deserialize`/`Serialize` impls |
| Field access by name | `Editable(T)`, `DynamicEditable` | `HashMap<ArcIntern<str>, serde_json::Value>` or `serde_json::Value` |
| Permission-based access | `Role`, `RolePermissions` | Application-layer `match role { ... }` on access |
| Binary encoding | `BinaryFieldCodec` (LE bytes) | `to_le_bytes()` / `from_le_bytes()` on primitives |
| Enum registry | `EnumRegistry` (runtime name↔value) | `serde_json::Value` + match/FromStr |
| JSON Schema generation | `Editable(T).describeSchema()` | `schemars` crate (if needed) |
| Struct builders | Manual fluent `*Self` chain | `#[derive(Builder)]` via `bon` |

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

## Semantic Deviations

| Aspect | Zig | Rust |
|--------|-----|------|
| Field introspection | `@typeInfo`, `FieldMeta`, compile-time | `serde` derive + `serde_json::Value` |
| Builder pattern | Manual `*Self` fluent chain (60+ lines) | `#[derive(Builder)]` via `bon` (1 annotation) |
| Dynamic get/set | `Editable(T)` zero-size mixin | `HashMap<ArcIntern<str>, Value>` |
| Permissions | `Role`, `RolePermissions`, `perm_*` bitfields | Application-layer match |
| Binary encoding | `BinaryFieldCodec` generic | `to_le_bytes()` per-type |
| Enum registry | `EnumRegistry` with comptime init | `serde_json::Value` or `FromStr` |
| JSON Schema | `describeSchema()` built-in | `schemars` external crate |

## Zig reference

See `doc/capabilities/reflection/CAPABILITY.md` in the Zig project for the original module design (Editable(T), DynamicEditable, ConstraintVTable, Role permissions).
