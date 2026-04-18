---
name: reflection
description: Standalone Zig reflection module providing vtable-driven field access, role-based permission enforcement, typed/binary codecs, and enum registry. Zero-cost mixin Editable(T) enables string-driven get/set on any struct at runtime without heap allocation per-field.
anchors:
  - Editable
  - DynamicEditable
  - FieldMeta
  - Accessor
  - Constraint
---

# Reflection

`src/reflection/` is a standalone peer module (promoted from `src/common/reflection.zig` in P2.4). It provides compile-time and runtime field introspection for Zig structs, used throughout Coral and the guidance engine.

## Sub-modules

| Sub-module | Key types | Purpose |
|------------|-----------|---------|
| `constraint` | `ConstraintVTable`, `Constraint(T)` | Per-field parse/format vtable |
| `accessor` | `Accessor`, `Editable(T)`, `DynamicEditable`, `FieldMeta` | Field access by name with permission check |
| `permissions` | `Role`, `RolePermissions`, `perm_*` | 4-role (player/coder/staff/admin) permission bitfield |
| `typed` | `TypedAccessor`, `TypedAccessorTable(T)`, `TypedEditable` | Compile-time type-safe field access with range validation |
| `binary` | `BinaryFieldCodec` | Encode/decode primitive fields to binary streams (little-endian) |
| `enum_registry` | `EnumRegistry` | Runtime name↔value mapping for dynamically known enums |

## Editable(T) — zero-size mixin

```zig
const Config = struct {
    port: u16 = 8080,
    editable: Editable(Config) = .{},
};
var cfg: Config = .{};
try cfg.editable.set(allocator, "port", "9000", .coder);   // string path
cfg.editable.setFast("port", @as(u16, 9000));               // zero-alloc fast path
```

`Editable(T)` is a zero-size struct — it adds no memory overhead to the containing struct.

## DynamicEditable — runtime field layout

`DynamicEditable` accepts an `[]Accessor` slice at runtime, enabling schema-driven editing of arbitrary memory layouts (e.g., WASM binary buffers).

## describeSchema — AI agent tooling

`Editable(T).describeSchema(allocator)` emits a JSON Schema document describing all fields with types, constraints, and `FieldMeta` descriptions. Used to generate MCP tool parameter schemas.

## Roles

```
player  — end user (narrowest permissions)
coder   — developer / internal tool
staff   — operator / admin tool
admin   — full access (broadest permissions)
```

## Key files

- `src/reflection/root.zig` — umbrella module and all flat re-exports
- `src/reflection/accessor.zig` — `Editable(T)`, `DynamicEditable`, `FieldMeta`, `TypeTag`
- `src/reflection/constraint.zig` — `ConstraintVTable`, `Constraint(T)`
- `src/reflection/permissions.zig` — `Role`, `RolePermissions`
- `src/reflection/typed.zig` — `TypedAccessorTable(T)`, `TypedEditable`, `ValidationError`
- `src/reflection/binary.zig` — `BinaryFieldCodec`
- `src/reflection/enum_registry.zig` — `EnumRegistry`

<!-- AUTO-SOURCES: do not edit below this line. Updated by `guidance gen`. -->
## Sources (10 files, auto-discovered)

| File | Confidence | Reason |
|------|-----------|--------|
| `src/reflection/accessor.zig` | 1.0 | defines_anchor |
| `src/reflection/constraint.zig` | 1.0 | defines_anchor |
| `src/reflection/root.zig` | 0.9 | used_by |
| `src/reflection/sql.zig` | 0.9 | used_by |
| `src/reflection/validate.zig` | 0.9 | used_by |
| `src/reflection/typed.zig` | 0.9 | used_by |
| `src/reflection/binary.zig` | 0.4 | path_heuristic |
| `src/reflection/schema_version.zig` | 0.4 | path_heuristic |
| `src/reflection/enum_registry.zig` | 0.4 | path_heuristic |
| `src/reflection/permissions.zig` | 0.4 | path_heuristic |

