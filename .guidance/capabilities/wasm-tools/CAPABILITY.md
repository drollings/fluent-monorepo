---
name: wasm-tools
description: L2 Workflow Cache tier using Extism (libextism) for sandboxed WASM tool execution. Binary IPC schema (BinaryExecutionRequest/BinaryExecutionResult) is fully defined. WasmTool registration and findWasmTool() matching are implemented. Extism execution path is a TODO stub (P3.3).
---

# WASM Tools

`src/wasm/wasm.zig` defines the binary IPC schema and Extism C-API bindings for the L2 cache tier. The binary types and tool registration are fully implemented; the Extism execution path inside `QueueReactor.route()` is a TODO stub pending P3.3.

## Implementation status

| Component | Status |
|-----------|--------|
| `BinaryExecutionRequest` / `BinaryExecutionResult` extern structs | Implemented |
| `BinaryHeader`, `PayloadType`, `BinaryContextNode` schema | Implemented |
| `Library.insertWasmTool()` + `findWasmTool()` in QueueReactor | Implemented |
| Extism C-API bindings (`ExtismPlugin`, `HostFn`, etc.) | Defined |
| Actual Extism `plugin.call("execute", payload)` in `route()` | TODO P3.3 stub |
| `ExecutionRequestBuilder` fluent builder (Arena #4) | Planned — not yet implemented |

## Planned architecture (P3.3)

```
QueueReactor.route()
  → findWasmTool(query)                       ← match by name or provides bitset
  → ExecutionRequestBuilder.build()           ← arena-backed binary payload (Arena #4)
  → extism_plugin_call("execute", payload)    ← sandboxed WASM execution
  → BinaryExecutionResult.getProvidesBitSet() ← decode dynamic provides words
  → cacheResult() → L1
```

## Binary IPC schema

All messages use a fixed-size header (`BinaryHeader`) followed by a payload:

```
BinaryHeader {
    magic: [4]u8  = "CRAL"
    version: u8   = BINARY_SCHEMA_VERSION (1)
    payload_type: PayloadType  (enum: execution_request | execution_result | context_node)
    payload_len: u32
}
```

`BinaryExecutionRequest` and `BinaryExecutionResult` are `extern struct` types (no padding, `align(1)` fields) for direct BLOB I/O. The result includes a dynamic `provides_words` array (DynamicBitSetUnmanaged serialized as u64 LE words) appended after the fixed header; `getProvidesBitSet()` reconstructs it.

## WasmTool registration

Tools are registered in `Library` with name and `provides` bitset:

```zig
try library.insertWasmTool(.{
    .name = "summarize",
    .wasm_bytes = module_bytes,
    .provides = capabilities_bitset,
});
```

`findWasmTool()` in `QueueReactor` matches by name or by checking if the tool's `provides` bitset covers the query's required capabilities.

## ExecutionRequestBuilder (planned)

An arena-backed fluent builder for assembling the binary payload will be added in P3.3. The payload buffer (`ArrayListUnmanaged(u8)`) is already arena-like in structure — P3.3 formalizes it. Only the serialized bytes escape to the Extism call; the arena is freed after the call returns.

## Key types (re-exported from `context_node_schema`)

- `BINARY_SCHEMA_VERSION` — current wire format version (1)
- `BINARY_MAGIC` — `"CRAL"` magic bytes
- `PayloadType` — `execution_request | execution_result | context_node`
- `BinaryHeader` — fixed 8-byte header
- `BinaryContextNode` — binary encoding of a ContextNode for WASM IPC

## Key files

- `src/wasm/wasm.zig` — `BinaryExecutionRequest`, `BinaryExecutionResult`, Extism C-API types
- `src/coral/context_node_schema.zig` — `BinaryHeader`, `BinaryContextNode`, `PayloadType`, schema constants
- `src/coral/db.zig` — `Library.insertWasmTool`, `WasmTool` struct
- `src/coral/cache.zig` — L2 `route()` stub, `findWasmTool()`
