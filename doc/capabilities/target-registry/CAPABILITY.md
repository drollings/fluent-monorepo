---
name: target-registry
description: DAG-based target registry for build pipelines. Target (bon::Builder) stores node metadata with dependency/provides bitsets. TargetRegistry indexes by name and bit index. CapabilityRegistry provides thread-safe string↔bitset-index interning via RwLock.
anchors:
  - Target
  - TargetBuilder
  - TargetRegistry
  - CapabilityRegistry
  - ArcIntern
---

# Target Registry

`common/src/registry.rs` + `common/src/interner.rs` + `dag/src/registry.rs` implement the build target DAG. The `dag/` crate re-exports `Target` from `common/` and adds a richer `TargetRegistry` with provider queries.

## Target

A `Target` represents one node in the capability DAG, built via `bon::Builder`:

| Field | Type | Purpose |
|-------|------|---------|
| `id` | `i64` | Stable numeric ID |
| `name` | `ArcIntern<str>` | Interned name string |
| `target_type` | `TargetType` | `File`, `Phony`, `Abstract` |
| `executor` | `ExecutorKind` | `Native`, `Docker`, `Wasm` |
| `depends` | `BitVec` | Bitset of required capability indices |
| `provides` | `BitVec` | Bitset of provided capability indices |
| `command` | `String` | Shell command (defaults to empty) |
| `essential` | `bool` | Must run in every plan (defaults to false) |

## TargetBuilder — bon::Builder

```rust
use guidance_common::registry::Target;

let t = Target::new()
    .id(1)
    .name("build".into())
    .target_type(TargetType::File)
    .executor(ExecutorKind::Native)
    .depends(bitvec![0, 1])
    .provides(bitvec![1, 0])
    .command("cargo build".into())
    .essential(true)
    .build();
```

## CapabilityRegistry — RwLock interned string mapping

`CapabilityRegistry` in `common/src/interner.rs` provides thread-safe name↔index interning with double-checked locking:

```rust
use guidance_common::interner::CapabilityRegistry;

let reg = CapabilityRegistry::new();
let idx = reg.intern("compile");   // returns 0
let idx2 = reg.intern("compile");  // returns 0 (same)
let named = reg.get_name(0);       // Some("compile")

// Convert to/from bitsets
let bits = reg.to_bitvec(&["compile", "link"]);
let names: Vec<ArcIntern<str>> = reg.bitvec_to_names(&bits);
```

## TargetRegistry — dag-level registry

The `dag` crate's `TargetRegistry` (`dag/src/registry.rs`) wraps `Vec<Target>` with provider query logic:

```rust
use dag::registry::TargetRegistry;

let mut reg = TargetRegistry::new();
reg.register(target)?;

// Lookup
let t = reg.get("build");
let t = reg.get_by_bit_index(1);

// Provider queries
let providers = reg.find_providers(&required_bits);
let providers = reg.get_providers(3);

// Enumeration
for name in reg.list_names() { /* ... */ }
for essential in reg.essential_targets() { /* ... */ }
```

## Key files

- `registry/src/lib.rs` — `Target` (bon::Builder), `TargetRegistry`, `CapabilityRegistry`

## Semantic Deviations

| Aspect | Zig | Rust |
|--------|-----|------|
| Builder | `TargetBuilder` manual `*Self` fluent chain | `bon::Builder` derive macro (`#[builder(start_fn = new)]`) |
| Interning | `StringInterner` with `std.Thread.RwLock` | `CapabilityRegistry` with `std::sync::RwLock` |
| String type | `[]const u8` slices | `ArcIntern<str>` via `internment` crate |
| Bitset type | `DynamicBitSetUnmanaged` | `bitvec::vec::BitVec` |
| Error handling | Deferred error in builder, `register()` surfaces it | Immediate `Result` from `register()` |
| Schema | `TargetSchema` with `DynamicEditable` | Not replicated — fields accessed directly |

## Zig reference

See `doc/capabilities/target-registry/CAPABILITY.md` in the Zig project for the original module design (TargetBuilder fluent chain, StringInterner, TargetSchema, IngestTargetDefs).
