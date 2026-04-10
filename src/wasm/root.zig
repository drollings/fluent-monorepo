//! wasm — WebAssembly Sandboxing (Extism)
//!
//! Provides:
//!   ExtismPlugin    — Extism C-API bindings
//!   WasmManifest     — WASM module metadata
//!   WasmPlugin      — Loaded plugin handle
//!   HostFunctionContext — Host function execution context
//!   WasmExecutionResult — Execution result
//!   ToolCompilerConfig — Tool compiler configuration
//!   ToolGenerator  — ToolGenerator from WASM
//!   WasmToolCache  — Cache for compiled tools

pub const extism = @import("wasm.zig").extism;
pub const ExtismPlugin = @import("wasm.zig").ExtismPlugin;
pub const WasmManifest = @import("wasm.zig").WasmManifest;
pub const WasmPlugin = @import("wasm.zig").WasmPlugin;
pub const HostFunctionContext = @import("wasm.zig").HostFunctionContext;
pub const WasmExecutionResult = @import("wasm.zig").WasmExecutionResult;
pub const ToolCompilerConfig = @import("wasm.zig").ToolCompilerConfig;
pub const ToolGenerator = @import("wasm.zig").ToolGenerator;
pub const WasmToolCache = @import("wasm.zig").WasmToolCache;
pub const WasmToolCacheEntry = @import("wasm.zig").WasmToolCacheEntry;
pub const BinarySchemaVersion = @import("wasm.zig").BINARY_SCHEMA_VERSION;
pub const BinaryMagic = @import("wasm.zig").BINARY_MAGIC;
pub const PayloadType = @import("wasm.zig").PayloadType;
pub const BinaryHeader = @import("wasm.zig").BinaryHeader;
pub const BinaryContextNode = @import("wasm.zig").BinaryContextNode;
pub const BinaryExecutionRequest = @import("wasm.zig").BinaryExecutionRequest;
pub const BinaryExecutionResult = @import("wasm.zig").BinaryExecutionResult;
pub const ExecutionRequestBuilder = @import("wasm.zig").ExecutionRequestBuilder;
pub const ExecutionResultReader = @import("wasm.zig").ExecutionResultReader;
pub const WasmExecution = @import("wasm.zig").WasmExecution;
