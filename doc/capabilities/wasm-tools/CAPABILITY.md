---
name: wasm-tools
description: Binary IPC schema for Extism WASM boundary with #[repr(C, packed)] structs. WasmRuntime trait abstracts plugin loading. ExtismWasmRuntime implements it using the extism Rust crate.
anchors:
  - BinaryExecutionRequest
  - BinaryExecutionResult
  - BinaryHeader
  - WasmRuntime
  - ExtismWasmRuntime
  - WasmPlugin
  - PayloadType
  - IpcError
---

# WASM Tools

`wasm_ipc/src/lib.rs` defines the binary IPC schema and `coral/src/wasm_runtime.rs` defines the wasm runtime abstraction for the L2 cache tier.

## Binary IPC schema

All messages use a fixed-size header (`BinaryHeader`) followed by a payload, encoded with `#[repr(C, packed)]`:

```rust
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct BinaryHeader {
    pub magic: [u8; 4],       // "GRPH"
    pub version: u32,         // BINARY_SCHEMA_VERSION (1)
    pub payload_type: u32,    // PayloadType enum
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

## Byte-order encoding

All fields are manually encoded with `to_le_bytes()` / `from_le_bytes()`:

```rust
pub fn encode_request(req: &BinaryExecutionRequest, input: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(size_of::<BinaryExecutionRequest>() + input.len());
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

## WasmRuntime trait

```rust
pub trait WasmRuntime: Send + Sync {
    fn load_plugin(&self, wasm_bytes: &[u8]) -> Result<Box<dyn WasmPlugin>, WasmError>;
    fn load_plugin_from_file(&self, path: &Path) -> Result<Box<dyn WasmPlugin>, WasmError>;
}

pub trait WasmPlugin: Send {
    fn call(&mut self, payload: &[u8]) -> Result<Vec<u8>, WasmError>;
}
```

## ExtismWasmRuntime

```rust
use coral::wasm_runtime::{ExtismWasmRuntime, WasmRuntime};

let runtime = ExtismWasmRuntime::new();
let mut plugin = runtime.load_plugin_from_file(Path::new("plugin.wasm"))?;
let output = plugin.call(b"input data")?;
```

The `ExtismWasmRuntime` wraps the `extism` Rust crate internally:

```rust
impl WasmRuntime for ExtismWasmRuntime {
    fn load_plugin(&self, wasm_bytes: &[u8]) -> Result<Box<dyn WasmPlugin>, WasmError> {
        let plugin = extism::Plugin::new(wasm_bytes, [], true)
            .map_err(|e| WasmError::PluginLoad(e.to_string()))?;
        Ok(Box::new(ExtismPlugin { plugin }))
    }
}

impl WasmPlugin for ExtismPlugin {
    fn call(&mut self, payload: &[u8]) -> Result<Vec<u8>, WasmError> {
        let result = self.plugin.call("execute", payload)
            .map_err(|e| WasmError::PluginCall(e.to_string()))?;
        Ok(result)
    }
}
```

## Dynamic provides bitset decoding

`get_provides_bitset()` reconstructs a `BitVec` from the dynamic words array appended to the payload:

```rust
use wasm_ipc::{decode_result, get_provides_bitset};

let (result, output) = decode_result(&buf)?;
let provides = get_provides_bitset(&result, &buf)?;
```

## Key files

- `wasm_ipc/src/lib.rs` — `BinaryHeader`, `BinaryExecutionRequest`, `BinaryExecutionResult`, `BinaryContextNode`, `PayloadType`, `IpcError`, `encode_request`, `decode_result`, `get_provides_bitset`
- `coral/src/wasm_runtime.rs` — `WasmRuntime` trait, `WasmPlugin` trait, `ExtismWasmRuntime`

## Semantic Deviations

| Aspect | Zig | Rust |
|--------|-----|------|
| Struct layout | `extern struct` with `align(1)` | `#[repr(C, packed)]` |
| Byte encoding | `@bitCast` / `mem.readIntSliceLittle` | `to_le_bytes()` / `from_le_bytes()` |
| WASM runtime | Extism C-API (`libextism`) | `extism` Rust crate |
| Magic bytes | `"CRAL"` (`0x43, 0x52, 0x41, 0x4C`) | `"GRPH"` (`0x47, 0x52, 0x50, 0x48`) |
| Provides bitset | `DynamicBitSetUnmanaged` → `getProvidesBitSet()` | `BitVec` → `get_provides_bitset()` |
| WasmTool registration | `Library.insertWasmTool()` with `wasm_bytes` | Not yet ported — `WasmTool` type exists in `types/src/lib.rs` (`guidance-types`) |
| Execution path | `QueueReactor.route()` TODO stub | `ExtismPlugin::call("execute", payload)` implemented |

## Zig reference

See `doc/capabilities/wasm-tools/CAPABILITY.md` in the Zig project for the original module design (Binary schema, WasmTool registration, ExecutionRequestBuilder).
