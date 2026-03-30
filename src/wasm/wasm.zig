/// wasm.zig — Milestone 4: WebAssembly Sandboxing (Extism)
///
/// Implements secure, sandboxed execution of dynamically loaded WASM modules.
/// This replaces hardcoded, statically linked Zig function pointers with
/// dynamically loadable, securely sandboxed WebAssembly modules.
///
/// Architecture (§4.1-4.5):
///   - Extism Host Integration (Zig C-API bindings)
///   - Zero-Copy Binary IPC (FlatBuffers-like schema)
///   - Host Functions & Controlled I/O
///   - Dynamic Tool Lifecycle (LLM-to-WASM pipeline)
///   - Execution Pipeline inside the DAG
///
/// Dependencies:
///   - Extism runtime (libextism) - linked via build.zig
///   - Library from coral/db.zig (SQLite backend)
const std = @import("std");
const builtin = @import("builtin");
const options = @import("options");
const have_extism = options.have_extism;
const coral_db = @import("coral_db");
const ContextNode = coral_db.ContextNode;
const Library = coral_db.Library;
const common = @import("common");
const reflection = common.reflection;
const hash_mod = common.hash;
const target_mod = common.target;
const ExecutorKind = target_mod.ExecutorKind;
const context_node_schema = @import("coral_schema");

// Re-export binary IPC types from context_node_schema for backward compatibility
pub const BINARY_SCHEMA_VERSION = context_node_schema.BINARY_SCHEMA_VERSION;
pub const BINARY_MAGIC = context_node_schema.BINARY_MAGIC;
pub const PayloadType = context_node_schema.PayloadType;
pub const BinaryHeader = context_node_schema.BinaryHeader;
pub const BinaryContextNode = context_node_schema.BinaryContextNode;

// §4.2 Execution Request/Result — canonical definitions now live in context_node_schema.zig.
pub const BinaryExecutionRequest = context_node_schema.BinaryExecutionRequest;
pub const BinaryExecutionResult = context_node_schema.BinaryExecutionResult;

// M1.1 ExecutionRequestBuilder / ExecutionResultReader / WasmExecution.
const execution_request_mod = @import("execution_request.zig");
pub const ExecutionRequestBuilder = execution_request_mod.ExecutionRequestBuilder;
pub const ExecutionResultReader = execution_request_mod.ExecutionResultReader;
pub const WasmExecution = execution_request_mod.WasmExecution;

// ---------------------------------------------------------------------------
// §4.1 Extism Host Integration (C-API Bindings)
// ---------------------------------------------------------------------------

/// Extism types from C-API.
pub const ExtismPlugin = ?*anyopaque;
pub const ExtismFunction = ?*anyopaque;
pub const ExtismCurrentPlugin = ?*anyopaque;
pub const ExtismSize = u64;

/// Extism value type for host functions.
pub const ExtismValType = enum(u8) {
    void = 0,
    i32 = 1,
    i64 = 2,
    f32 = 3,
    f64 = 4,
    v128 = 5,
    funcref = 6,
    externref = 7,
    ptr = 8,
};

/// Extism value for host function I/O.
pub const ExtismVal = extern struct {
    t: ExtismValType,
    v: u64,
};

/// Host function signature.
pub const HostFn = *const fn (
    plugin: ?*anyopaque,
    inputs: [*]const ExtismVal,
    n_inputs: u64,
    outputs: [*]ExtismVal,
    n_outputs: u64,
    user_data: ?*anyopaque,
) callconv(std.builtin.CallingConvention.c) void;

/// Extism C-API function declarations.
/// In test builds or when `have_extism` is false (no libextism installed), all
/// symbols are stubbed so the binary links without libextism.  Build with
/// `-Dextism=true` to enable real Extism WASM execution.
pub const extism = if (builtin.is_test or !have_extism) struct {
    pub fn extism_plugin_new(_: [*]const u8, _: ExtismSize, _: ?[*]const ?*anyopaque, _: u64, _: u32) ExtismPlugin {
        return null;
    }
    pub fn extism_plugin_free(_: ExtismPlugin) void {}
    pub fn extism_plugin_call(_: ExtismPlugin, _: [*:0]const u8, _: [*]const u8, _: ExtismSize) i32 {
        return -1;
    }
    pub fn extism_plugin_output(_: ExtismPlugin, _: *ExtismSize) ?[*]const u8 {
        return null;
    }
    pub fn extism_plugin_error(_: ExtismPlugin, _: *ExtismSize) ?[*]const u8 {
        return null;
    }
    pub fn extism_plugin_memory(_: ExtismPlugin) ?[*]u8 {
        return null;
    }
    pub fn extism_plugin_memory_length(_: ExtismPlugin, _: u64) u64 {
        return 0;
    }
    pub fn extism_alloc(_: ExtismPlugin, _: u64) u64 {
        return 0;
    }
    pub fn extism_store(_: ExtismPlugin, _: u64, _: [*]const u8, _: u64) void {}
    pub fn extism_load(_: ExtismPlugin, _: u64, _: [*]u8, _: u64) void {}
    pub fn extism_free(_: ExtismPlugin, _: u64) void {}
    pub fn extism_length(_: ExtismPlugin, _: u64) u64 {
        return 0;
    }
    pub fn extism_function_new(_: [*:0]const u8, _: [*]const ExtismValType, _: u64, _: [*]const ExtismValType, _: u64, _: HostFn, _: ?*anyopaque, _: ?*const fn (?*anyopaque) callconv(std.builtin.CallingConvention.c) void) ExtismFunction {
        return null;
    }
    pub fn extism_function_free(_: ExtismFunction) void {}
} else struct {
    pub extern "extism" fn extism_plugin_new(
        wasm: [*]const u8,
        wasm_size: ExtismSize,
        functions: ?[*]const ?*anyopaque,
        n_functions: u64,
        with_wasi: u32,
    ) ExtismPlugin;

    pub extern "extism" fn extism_plugin_free(plugin: ExtismPlugin) void;

    pub extern "extism" fn extism_plugin_call(
        plugin: ExtismPlugin,
        func_name: [*:0]const u8,
        data: [*]const u8,
        data_len: ExtismSize,
    ) i32;

    pub extern "extism" fn extism_plugin_output(
        plugin: ExtismPlugin,
        output_len: *ExtismSize,
    ) ?[*]const u8;

    pub extern "extism" fn extism_plugin_error(
        plugin: ExtismPlugin,
        error_len: *ExtismSize,
    ) ?[*]const u8;

    pub extern "extism" fn extism_plugin_memory(
        plugin: ExtismPlugin,
    ) ?[*]u8;

    pub extern "extism" fn extism_plugin_memory_length(
        plugin: ExtismPlugin,
        ptr: u64,
    ) u64;

    pub extern "extism" fn extism_alloc(
        plugin: ExtismPlugin,
        size: u64,
    ) u64;

    pub extern "extism" fn extism_store(
        plugin: ExtismPlugin,
        ptr: u64,
        data: [*]const u8,
        data_len: u64,
    ) void;

    pub extern "extism" fn extism_load(
        plugin: ExtismPlugin,
        ptr: u64,
        data: [*]u8,
        data_len: u64,
    ) void;

    pub extern "extism" fn extism_free(
        plugin: ExtismPlugin,
        ptr: u64,
    ) void;

    pub extern "extism" fn extism_length(
        plugin: ExtismPlugin,
        ptr: u64,
    ) u64;

    pub extern "extism" fn extism_function_new(
        name: [*:0]const u8,
        inputs: [*]const ExtismValType,
        n_inputs: u64,
        outputs: [*]const ExtismValType,
        n_outputs: u64,
        func: HostFn,
        user_data: ?*anyopaque,
        free_user_data: ?*const fn (?*anyopaque) callconv(std.builtin.CallingConvention.c) void,
    ) ExtismFunction;

    pub extern "extism" fn extism_function_free(func: ExtismFunction) void;
};

/// Manifest for WASM plugin configuration.
pub const WasmManifest = struct {
    wasm_bytes: []const u8,
    memory_max: u32 = 16 * 1024 * 1024, // 16 MB default
    timeout_ms: u32 = 30_000, // 30 second default
    allowed_paths: []const []const u8 = &[_][]const u8{},
    allowed_hosts: []const []const u8 = &[_][]const u8{},
    with_wasi: bool = false,

    pub fn init(wasm_bytes: []const u8) WasmManifest {
        return .{ .wasm_bytes = wasm_bytes };
    }
};

/// WASM Plugin wrapper with lifecycle management.
pub const WasmPlugin = struct {
    const Self = @This();

    plugin: ExtismPlugin,
    allocator: std.mem.Allocator,
    manifest: WasmManifest,
    arena: std.heap.ArenaAllocator,

    pub fn init(allocator: std.mem.Allocator, manifest: WasmManifest) !Self {
        var arena = std.heap.ArenaAllocator.init(allocator);

        const plugin = extism.extism_plugin_new(
            manifest.wasm_bytes.ptr,
            manifest.wasm_bytes.len,
            null,
            0,
            if (manifest.with_wasi) 1 else 0,
        );

        if (plugin == null) {
            arena.deinit();
            return error.PluginInitFailed;
        }

        return .{
            .plugin = plugin.?,
            .allocator = allocator,
            .manifest = manifest,
            .arena = arena,
        };
    }

    pub fn deinit(self: *Self) void {
        extism.extism_plugin_free(self.plugin);
        self.arena.deinit();
    }

    pub fn call(self: *Self, func_name: [:0]const u8, input: []const u8) ![]const u8 {
        const result = extism.extism_plugin_call(
            self.plugin,
            func_name.ptr,
            input.ptr,
            input.len,
        );

        if (result != 0) {
            var error_len: ExtismSize = 0;
            const error_msg = extism.extism_plugin_error(self.plugin, &error_len);
            if (error_msg) |_| {
                return error.PluginCallFailed;
            }
            return error.PluginCallFailed;
        }

        var output_len: ExtismSize = 0;
        const output = extism.extism_plugin_output(self.plugin, &output_len);
        if (output) |out| {
            return out[0..output_len];
        }
        return "";
    }

    pub fn alloc(self: *Self, size: u64) u64 {
        return extism.extism_alloc(self.plugin, size);
    }

    pub fn store(self: *Self, ptr: u64, data: []const u8) void {
        extism.extism_store(self.plugin, ptr, data.ptr, data.len);
    }

    pub fn load(self: *Self, ptr: u64, buf: []u8) void {
        extism.extism_load(self.plugin, ptr, buf.ptr, buf.len);
    }

    pub fn freePtr(self: *Self, ptr: u64) void {
        extism.extism_free(self.plugin, ptr);
    }
};

// ---------------------------------------------------------------------------
// §4.3 Host Functions & Controlled I/O
// ---------------------------------------------------------------------------

/// Host function context - passed to host functions for controlled I/O.
///
/// `call_count` is incremented on every host-function invocation and compared
/// against `max_calls` to prevent tight-loop DoS from malicious WASM modules.
/// The counter is reset to zero each time a new plugin invocation begins via
/// `HostFunctionContext.reset()`.
pub const HostFunctionContext = struct {
    library: *Library,
    allocator: std.mem.Allocator,
    /// Total host-function calls made in the current plugin invocation.
    call_count: u32 = 0,
    /// Maximum calls allowed per invocation (see limits.zig MAX_WASM_HOST_CALLS).
    max_calls: u32 = 10_000,

    /// Reset the call counter at the start of each plugin invocation.
    pub fn reset(self: *HostFunctionContext) void {
        self.call_count = 0;
    }
};

/// Check whether the per-invocation host-function rate limit has been reached.
/// Increments `ctx.call_count` and returns `error.RateLimited` when the cap
/// is exceeded.  All host functions call this as their first action.
fn checkRateLimit(ctx: *HostFunctionContext) error{RateLimited}!void {
    ctx.call_count += 1;
    if (ctx.call_count > ctx.max_calls) return error.RateLimited;
}

/// Host function: Get node LOD1 summary by ID.
/// Input: (id: i64)
/// Returns pointer to string in WASM memory.
pub fn hostGetNodeLod1(
    plugin: ?*anyopaque,
    inputs: [*]const ExtismVal,
    n_inputs: u64,
    outputs: [*]ExtismVal,
    n_outputs: u64,
    user_data: ?*anyopaque,
) callconv(std.builtin.CallingConvention.c) void {
    _ = n_outputs;
    const ctx = @as(?*HostFunctionContext, @ptrCast(@alignCast(user_data))) orelse return;

    checkRateLimit(ctx) catch {
        outputs[0] = .{ .t = .ptr, .v = 0 };
        return;
    };

    if (n_inputs < 1) {
        outputs[0] = .{ .t = .ptr, .v = 0 };
        return;
    }

    const node_id: i64 = @bitCast(inputs[0].v);

    // Fetch node from SQLite via Library
    const node_opt = ctx.library.fetchNode(node_id) catch {
        outputs[0] = .{ .t = .ptr, .v = 0 };
        return;
    };
    const node = node_opt orelse {
        outputs[0] = .{ .t = .ptr, .v = 0 };
        return;
    };

    // Allocate in WASM memory and copy
    const wasm_plugin = @as(ExtismPlugin, @ptrCast(plugin));
    const ptr = extism.extism_alloc(wasm_plugin, node.lod[1].len);
    extism.extism_store(wasm_plugin, ptr, node.lod[1].ptr, node.lod[1].len);

    outputs[0] = .{ .t = .ptr, .v = ptr };
}

/// Host function: Get neighbor node IDs for a given node.
/// Input: (id: i64)
/// Output: ptr to [count: u32][id0: i64, ...] in WASM memory.
pub fn hostGetNeighbors(
    plugin: ?*anyopaque,
    inputs: [*]const ExtismVal,
    n_inputs: u64,
    outputs: [*]ExtismVal,
    n_outputs: u64,
    user_data: ?*anyopaque,
) callconv(std.builtin.CallingConvention.c) void {
    _ = n_outputs;
    const ctx = @as(?*HostFunctionContext, @ptrCast(@alignCast(user_data))) orelse return;

    checkRateLimit(ctx) catch {
        outputs[0] = .{ .t = .ptr, .v = 0 };
        return;
    };

    if (n_inputs < 1) {
        outputs[0] = .{ .t = .ptr, .v = 0 };
        return;
    }

    const node_id: i64 = @bitCast(inputs[0].v);

    // Query neighbor_of via SQLite for outgoing neighbors.
    const neighbor_ids = ctx.library.getNeighborIds(ctx.allocator, node_id) catch {
        outputs[0] = .{ .t = .ptr, .v = 0 };
        return;
    };
    defer ctx.allocator.free(neighbor_ids);

    // Serialise: [count: u32 LE][id0: i64 LE, ...]
    const buf_size = @sizeOf(u32) + neighbor_ids.len * @sizeOf(i64);
    const buf = ctx.allocator.alloc(u8, buf_size) catch {
        outputs[0] = .{ .t = .ptr, .v = 0 };
        return;
    };
    defer ctx.allocator.free(buf);

    std.mem.writeInt(u32, buf[0..4], @intCast(neighbor_ids.len), .little);
    var off: usize = 4;
    for (neighbor_ids) |rid| {
        std.mem.writeInt(i64, buf[off..][0..8], rid, .little);
        off += 8;
    }

    const wasm_plugin = @as(ExtismPlugin, @ptrCast(plugin));
    const ptr = extism.extism_alloc(wasm_plugin, buf_size);
    extism.extism_store(wasm_plugin, ptr, buf.ptr, buf_size);

    outputs[0] = .{ .t = .ptr, .v = ptr };
}

/// Host function: Insert a directed edge between two nodes.
/// Input: (from_id: i64, to_id: i64)
/// Output: (status: i64) — 0 on success, 1 on error.
pub fn hostInsertEdge(
    plugin: ?*anyopaque,
    inputs: [*]const ExtismVal,
    n_inputs: u64,
    outputs: [*]ExtismVal,
    n_outputs: u64,
    user_data: ?*anyopaque,
) callconv(std.builtin.CallingConvention.c) void {
    _ = plugin;
    _ = n_outputs;
    const ctx = @as(?*HostFunctionContext, @ptrCast(@alignCast(user_data))) orelse {
        outputs[0] = .{ .t = .i64, .v = @bitCast(@as(i64, 1)) };
        return;
    };

    checkRateLimit(ctx) catch {
        outputs[0] = .{ .t = .i64, .v = @bitCast(@as(i64, 1)) };
        return;
    };

    if (n_inputs < 2) {
        outputs[0] = .{ .t = .i64, .v = @bitCast(@as(i64, 1)) };
        return;
    }

    const from_id: i64 = @bitCast(inputs[0].v);
    const to_id: i64 = @bitCast(inputs[1].v);

    // Validate both nodes exist before inserting the edge.
    // Prevents referential integrity violations and orphan-edge graph corruption.
    var from_opt = ctx.library.fetchNode(from_id) catch null;
    if (from_opt) |*n| n.free(ctx.allocator) else {
        outputs[0] = .{ .t = .i64, .v = @bitCast(@as(i64, 2)) }; // 2 = node not found
        return;
    }
    var to_opt = ctx.library.fetchNode(to_id) catch null;
    if (to_opt) |*n| n.free(ctx.allocator) else {
        outputs[0] = .{ .t = .i64, .v = @bitCast(@as(i64, 2)) }; // 2 = node not found
        return;
    }

    ctx.library.insertNeighborOf(from_id, to_id, 0.0, .neighbor_of) catch {
        outputs[0] = .{ .t = .i64, .v = @bitCast(@as(i64, 1)) };
        return;
    };

    outputs[0] = .{ .t = .i64, .v = @bitCast(@as(i64, 0)) };
}

/// Host function registry for controlled I/O.
pub const HostFunctionRegistry = struct {
    const Self = @This();

    allocator: std.mem.Allocator,
    functions: std.ArrayListUnmanaged(ExtismFunction),
    context: HostFunctionContext,

    pub fn init(allocator: std.mem.Allocator, library: *Library) Self {
        return .{
            .allocator = allocator,
            .functions = .{},
            .context = .{
                .library = library,
                .allocator = allocator,
            },
        };
    }

    pub fn deinit(self: *Self) void {
        for (self.functions.items) |func| {
            extism.extism_function_free(func);
        }
        self.functions.deinit(self.allocator);
    }

    /// Register the standard host functions for Coral Context.
    pub fn registerStandard(self: *Self) !void {
        // get_node_lod1(id: i64) -> ptr
        const fn_lod1 = extism.extism_function_new(
            "get_node_lod1",
            &[_]ExtismValType{.i64},
            1,
            &[_]ExtismValType{.ptr},
            1,
            hostGetNodeLod1,
            @ptrCast(&self.context),
            null,
        );
        try self.functions.append(self.allocator, fn_lod1);

        // get_neighbors(id: i64) -> ptr
        const fn_neighbors = extism.extism_function_new(
            "get_neighbors",
            &[_]ExtismValType{.i64},
            1,
            &[_]ExtismValType{.ptr},
            1,
            hostGetNeighbors,
            @ptrCast(&self.context),
            null,
        );
        try self.functions.append(self.allocator, fn_neighbors);

        // insert_edge(from_id: i64, to_id: i64) -> i64 (0=ok, 1=err)
        const fn_insert_edge = extism.extism_function_new(
            "insert_edge",
            &[_]ExtismValType{ .i64, .i64 },
            2,
            &[_]ExtismValType{.i64},
            1,
            hostInsertEdge,
            @ptrCast(&self.context),
            null,
        );
        try self.functions.append(self.allocator, fn_insert_edge);
    }

    /// Get function array for plugin creation.
    pub fn getFunctionArray(self: *Self) []ExtismFunction {
        return self.functions.items;
    }
};

// ---------------------------------------------------------------------------
// §4.5 Execution Pipeline: runWasmTarget
// ---------------------------------------------------------------------------
//
// WasmTarget has been merged into Target (via ExecutorKind.wasm).
// This function takes a *Target whose executor tag is .wasm and executes it.

/// Result of executing a WASM-backed Target.
/// `provides` is caller-owned; free with provides.deinit(allocator).
pub const WasmExecutionResult = struct {
    success: bool,
    output: []const u8, // slice into `payload` buffer — valid until payload is freed
    provides: std.bit_set.DynamicBitSetUnmanaged,
    error_code: u32,
    /// Raw output payload from Extism — must be freed by the caller.
    payload: []const u8,

    pub fn deinit(self: *WasmExecutionResult, allocator: std.mem.Allocator) void {
        self.provides.deinit(allocator);
        allocator.free(self.payload);
    }
};

/// Execute a WASM-backed Target against a ContextNode input.
/// The target's executor must be .wasm; asserts otherwise.
pub fn runWasmTarget(
    allocator: std.mem.Allocator,
    target: *target_mod.Target,
    node: *const ContextNode,
    host_registry: ?*HostFunctionRegistry,
) !WasmExecutionResult {
    const wasm_exec = switch (target.executor) {
        .wasm => |*w| w,
        .native => unreachable, // caller must ensure executor == .wasm
    };

    // 1. Serialise ContextNode to binary IPC buffer.
    var ipc_buf: [4096]u8 = undefined;
    const bin_node = BinaryContextNode.init(node, &ipc_buf);
    @memcpy(ipc_buf[0..@sizeOf(BinaryContextNode)], std.mem.asBytes(&bin_node));
    const input = ipc_buf[0..bin_node.header.payload_size];

    // 2. Create Extism plugin.
    var manifest = WasmManifest.init(wasm_exec.wasm_bytes);
    manifest.with_wasi = false;
    var plugin = if (host_registry) |reg|
        try createPluginWithHostFunctions(allocator, manifest, reg)
    else
        try WasmPlugin.init(allocator, manifest);
    defer plugin.deinit();

    // 3. Call the entry point.
    const raw_output = try plugin.call(wasm_exec.entry_point, input);
    // Extism output is valid only until the next call; dupe it so we own it.
    const payload = try allocator.dupe(u8, raw_output);
    errdefer allocator.free(payload);

    // 4. Parse result header.
    if (payload.len < @sizeOf(BinaryExecutionResult)) {
        const empty_bs = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(allocator, 0);
        return .{
            .success = false,
            .output = "Invalid result size",
            .provides = empty_bs,
            .error_code = 1,
            .payload = payload,
        };
    }
    const res: *const BinaryExecutionResult = @ptrCast(@alignCast(payload.ptr));
    if (!res.header.validate()) {
        const empty_bs = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(allocator, 0);
        return .{
            .success = false,
            .output = "Invalid result header",
            .provides = empty_bs,
            .error_code = 2,
            .payload = payload,
        };
    }

    const output_text = if (res.output_len > 0)
        payload[res.output_offset..][0..res.output_len]
    else
        "";

    const provides = try res.getProvidesBitSet(allocator, payload);
    return .{
        .success = res.success == 1,
        .output = output_text,
        .provides = provides,
        .error_code = res.error_code,
        .payload = payload,
    };
}

/// Create plugin with host functions from the registry wired in.
fn createPluginWithHostFunctions(
    allocator: std.mem.Allocator,
    manifest: WasmManifest,
    registry: *HostFunctionRegistry,
) !WasmPlugin {
    var arena = std.heap.ArenaAllocator.init(allocator);

    const funcs = registry.getFunctionArray();
    const plugin = extism.extism_plugin_new(
        manifest.wasm_bytes.ptr,
        manifest.wasm_bytes.len,
        if (funcs.len > 0) @ptrCast(funcs.ptr) else null,
        funcs.len,
        if (manifest.with_wasi) 1 else 0,
    );

    if (plugin == null) {
        arena.deinit();
        return error.PluginInitFailed;
    }

    return .{
        .plugin = plugin.?,
        .allocator = allocator,
        .manifest = manifest,
        .arena = arena,
    };
}

/// Execute a WASM binary with a plain string query as input.
/// Calls the exported "run" function and returns the allocator-owned output.
/// Used by QueueReactor L2 routing.  Returns error if Extism runtime is
/// unavailable or the WASM invocation fails.
///
/// In test builds this function returns `error.WasmRuntimeNotAvailable`
/// immediately so that test binaries do not require libextism to be linked.
pub fn executeWasmQuery(
    allocator: std.mem.Allocator,
    wasm_bytes: []const u8,
    query: []const u8,
) ![]const u8 {
    if (comptime builtin.is_test or !have_extism) return error.WasmRuntimeNotAvailable;
    const manifest = WasmManifest.init(wasm_bytes);
    var plugin = try WasmPlugin.init(allocator, manifest);
    defer plugin.deinit();
    const raw = try plugin.call("run", query);
    return allocator.dupe(u8, raw);
}

/// Execute a WASM module with Library host functions wired in via a HostFunctionRegistry.
/// Identical to executeWasmQuery but creates the plugin with host functions so that
/// WASM tools can call get_node_lod1, get_neighbors, and insert_edge.
///
/// Used by QueueReactor L2 routing (R2).
pub fn executeWasmQueryWithHosts(
    allocator: std.mem.Allocator,
    wasm_bytes: []const u8,
    query: []const u8,
    registry: *HostFunctionRegistry,
) ![]const u8 {
    if (comptime builtin.is_test or !have_extism) return error.WasmRuntimeNotAvailable;
    const manifest = WasmManifest.init(wasm_bytes);
    var plugin = try createPluginWithHostFunctions(allocator, manifest, registry);
    defer plugin.deinit();
    const raw = try plugin.call("run", query);
    return allocator.dupe(u8, raw);
}

// ---------------------------------------------------------------------------
// §4.4 Dynamic Tool Lifecycle (LLM-to-WASM Pipeline)
// ---------------------------------------------------------------------------

/// Defines compiler configuration settings for Zig tooling; manages ownership and invariants of tool parameters.
pub const ToolCompilerConfig = struct {
    zig_compiler: []const u8 = "zig",
    assemblyscript_compiler: []const u8 = "asc",
    rust_compiler: []const u8 = "cargo",
    temp_dir: []const u8 = "/tmp/coral-wasm",
    target_wasi: bool = true,
    /// Zero-size reflection mixin.
    editable: reflection.Editable(@This()) = .{},
};

/// Supported WASM source languages.
pub const WasmLanguage = enum {
    zig,
    rust,
    assemblyscript,
    c,
    go,

    pub fn extension(self: WasmLanguage) []const u8 {
        return switch (self) {
            .zig => ".zig",
            .rust => ".rs",
            .assemblyscript => ".ts",
            .c => ".c",
            .go => ".go",
        };
    }
};

/// Dynamic tool generator - compiles source code to WASM.
pub const ToolGenerator = struct {
    const Self = @This();

    allocator: std.mem.Allocator,
    config: ToolCompilerConfig,
    arena: std.heap.ArenaAllocator,

    pub fn init(allocator: std.mem.Allocator, config: ToolCompilerConfig) Self {
        return .{
            .allocator = allocator,
            .config = config,
            .arena = std.heap.ArenaAllocator.init(allocator),
        };
    }

    pub fn deinit(self: *Self) void {
        self.arena.deinit();
    }

    /// Shared implementation: write `source` to a temp file, run `compiler_argv`,
    /// then read and return the produced `tool.wasm`.
    /// Both `temp_path` and `wasm_path` are allocated in the arena allocator and
    /// freed when `self.arena` is deinitialized.
    fn compileSource(
        self: *Self,
        source: []const u8,
        src_filename: []const u8,
        compiler_argv: []const []const u8,
    ) ![]const u8 {
        const arena = self.arena.allocator();

        const temp_path = try std.fs.path.join(arena, &[_][]const u8{ self.config.temp_dir, src_filename });
        const wasm_path = try std.fs.path.join(arena, &[_][]const u8{ self.config.temp_dir, "tool.wasm" });

        try std.fs.cwd().makePath(self.config.temp_dir);
        try std.fs.cwd().writeFile(.{ .sub_path = temp_path, .data = source });

        var child = std.process.Child.init(compiler_argv, self.allocator);
        _ = try child.spawnAndWait();

        const wasm_file = try std.fs.cwd().openFile(wasm_path, .{});
        defer wasm_file.close();
        return wasm_file.readToEndAlloc(self.allocator, 1024 * 1024);
    }

    /// Compile Zig source to WASM (wasm32-freestanding, ReleaseSmal).
    pub fn compileZig(self: *Self, source: []const u8) ![]const u8 {
        const temp_path = try std.fs.path.join(
            self.arena.allocator(),
            &[_][]const u8{ self.config.temp_dir, "tool.zig" },
        );
        const argv = [_][]const u8{
            self.config.zig_compiler, "build-exe",
            "-target",                "wasm32-freestanding",
            "-fno-entry",             "-OReleaseSmall",
            temp_path,
        };
        return self.compileSource(source, "tool.zig", &argv);
    }

    /// Compile AssemblyScript source to WASM.
    pub fn compileAssemblyScript(self: *Self, source: []const u8) ![]const u8 {
        const temp_path = try std.fs.path.join(
            self.arena.allocator(),
            &[_][]const u8{ self.config.temp_dir, "tool.ts" },
        );
        const wasm_path = try std.fs.path.join(
            self.arena.allocator(),
            &[_][]const u8{ self.config.temp_dir, "tool.wasm" },
        );
        const argv = [_][]const u8{
            self.config.assemblyscript_compiler, temp_path,
            "-o",                                wasm_path,
            "--optimize",
        };
        return self.compileSource(source, "tool.ts", &argv);
    }

    /// Verify a WASM tool by running test input.
    pub fn verifyTool(self: *Self, wasm_bytes: []const u8, test_input: []const u8) !bool {
        const manifest = WasmManifest.init(wasm_bytes);
        var plugin = try WasmPlugin.init(self.allocator, manifest);
        defer plugin.deinit();

        const output = plugin.call("execute_target", test_input) catch return false;
        const result_ptr: *const BinaryExecutionResult = @ptrCast(@alignCast(output.ptr));

        return result_ptr.success == 1;
    }

    /// Generate WASM tool from LLM prompt response.
    pub fn generateFromLLM(self: *Self, source: []const u8, language: WasmLanguage) ![]const u8 {
        return switch (language) {
            .zig => self.compileZig(source),
            .assemblyscript => self.compileAssemblyScript(source),
            else => error.UnsupportedLanguage,
        };
    }
};

/// A test case for WASM tool verification.
pub const ToolTestCase = struct {
    /// Input to pass to the tool's "execute" entry point.
    input: []const u8,
    /// Optional output substring to check (null = accept any output).
    expected_output_contains: ?[]const u8 = null,
};

/// Run a series of test cases against a WASM binary.
/// Returns true if all test cases pass, false if any fail.
pub fn verifyWithTests(
    allocator: std.mem.Allocator,
    wasm_bytes: []const u8,
    test_cases: []const ToolTestCase,
) bool {
    if (test_cases.len == 0) return true;

    for (test_cases) |tc| {
        const output = executeWasmQuery(allocator, wasm_bytes, tc.input) catch return false;
        defer allocator.free(output);
        if (tc.expected_output_contains) |expected| {
            if (std.mem.indexOf(u8, output, expected) == null) return false;
        }
    }
    return true;
}

/// Cache entry for a single WASM tool with TTL/LRU metadata.
pub const WasmToolCacheEntry = struct {
    wasm_bytes: []const u8,
    created_at: i64,
    last_accessed: i64,
    access_count: usize,
};

/// Manages Wasm cache structures, owns buffers, supports initialization/deinit; ensures consistent state across operations.
pub const WasmToolCache = struct {
    const Self = @This();
    const DEFAULT_TTL_SECONDS: u64 = 3600; // 1 hour
    const DEFAULT_MAX_ENTRIES: usize = 256;

    allocator: std.mem.Allocator,
    tools: std.StringHashMapUnmanaged(WasmToolCacheEntry),
    max_entries: usize,
    ttl_seconds: u64,

    pub fn init(allocator: std.mem.Allocator) Self {
        return .{
            .allocator = allocator,
            .tools = .{},
            .max_entries = DEFAULT_MAX_ENTRIES,
            .ttl_seconds = DEFAULT_TTL_SECONDS,
        };
    }

    pub fn deinit(self: *Self) void {
        var it = self.tools.iterator();
        while (it.next()) |entry| {
            self.allocator.free(entry.key_ptr.*);
            self.allocator.free(entry.value_ptr.wasm_bytes);
        }
        self.tools.deinit(self.allocator);
    }

    /// Get WASM bytes by name. Returns null if not found or expired.
    pub fn get(self: *Self, name: []const u8) ?[]const u8 {
        const entry = self.tools.getPtr(name) orelse return null;
        const now = std.time.timestamp();
        if (self.ttl_seconds > 0 and now - entry.created_at > @as(i64, @intCast(self.ttl_seconds))) {
            return null; // expired
        }
        entry.last_accessed = now;
        entry.access_count += 1;
        return entry.wasm_bytes;
    }

    /// Store WASM bytes under `name`. Evicts LRU entry if at capacity.
    pub fn put(self: *Self, name: []const u8, wasm_bytes: []const u8) !void {
        // Evict oldest entry if at capacity
        if (self.tools.count() >= self.max_entries) {
            try self.evictOldest();
        }
        // Evict existing entry with same name if present
        if (self.tools.fetchRemove(name)) |kv| {
            self.allocator.free(kv.key);
            self.allocator.free(kv.value.wasm_bytes);
        }
        const owned_key = try self.allocator.dupe(u8, name);
        errdefer self.allocator.free(owned_key);
        const owned_bytes = try self.allocator.dupe(u8, wasm_bytes);
        errdefer self.allocator.free(owned_bytes);
        const now = std.time.timestamp();
        try self.tools.put(self.allocator, owned_key, .{
            .wasm_bytes = owned_bytes,
            .created_at = now,
            .last_accessed = now,
            .access_count = 0,
        });
    }

    /// Evict all entries older than ttl_seconds.
    pub fn evictExpired(self: *Self) !void {
        const now = std.time.timestamp();
        var to_remove: std.ArrayListUnmanaged([]const u8) = .{};
        defer to_remove.deinit(self.allocator);

        var it = self.tools.iterator();
        while (it.next()) |entry| {
            if (self.ttl_seconds > 0 and now - entry.value_ptr.created_at > @as(i64, @intCast(self.ttl_seconds))) {
                try to_remove.append(self.allocator, entry.key_ptr.*);
            }
        }
        for (to_remove.items) |key| {
            if (self.tools.fetchRemove(key)) |kv| {
                self.allocator.free(kv.key);
                self.allocator.free(kv.value.wasm_bytes);
            }
        }
    }

    pub fn computeHash(_: *const Self, bytes: []const u8) [32]u8 {
        var h: std.crypto.hash.sha2.Sha256 = .init(.{});
        h.update(bytes);
        return h.finalResult();
    }

    fn evictOldest(self: *Self) !void {
        if (self.tools.count() == 0) return;
        var oldest_key: ?[]const u8 = null;
        var oldest_time: i64 = std.math.maxInt(i64);
        var it = self.tools.iterator();
        while (it.next()) |entry| {
            if (entry.value_ptr.last_accessed < oldest_time) {
                oldest_time = entry.value_ptr.last_accessed;
                oldest_key = entry.key_ptr.*;
            }
        }
        if (oldest_key) |key| {
            if (self.tools.fetchRemove(key)) |kv| {
                self.allocator.free(kv.key);
                self.allocator.free(kv.value.wasm_bytes);
            }
        }
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "BinaryHeader validation" {
    const header = BinaryHeader.init(.context_node, 100);
    try testing.expect(header.validate());

    var bad_header = header;
    bad_header.magic = [_]u8{ 'B', 'A', 'D', 'X' };
    try testing.expect(!bad_header.validate());
}

test "BinaryContextNode serialization" {
    var node = try ContextNode.init(0xDEADBEEF, "test_node", "This is full content.", testing.allocator);
    defer node.free(testing.allocator);
    node.setLod(1, "Summary text.");

    var buffer: [4096]u8 = undefined;
    const bin_node = BinaryContextNode.init(&node, &buffer);

    try testing.expectEqual(@as(i64, 0xDEADBEEF), bin_node.getId());
    try testing.expectEqual(@as(u32, 21), bin_node.lod_lengths[0]); // "This is full content."
    try testing.expectEqual(@as(u32, 13), bin_node.lod_lengths[1]); // "Summary text."
    try testing.expect(bin_node.header.validate());
}

test "PayloadType enum values" {
    try testing.expectEqual(@as(u32, 1), @intFromEnum(PayloadType.context_node));
    try testing.expectEqual(@as(u32, 2), @intFromEnum(PayloadType.execution_request));
    try testing.expectEqual(@as(u32, 3), @intFromEnum(PayloadType.execution_result));
}

test "WasmManifest defaults" {
    const manifest = WasmManifest.init(&[_]u8{});
    try testing.expectEqual(@as(u32, 16 * 1024 * 1024), manifest.memory_max);
    try testing.expectEqual(@as(u32, 30_000), manifest.timeout_ms);
    try testing.expect(!manifest.with_wasi);
}

test "WasmLanguage extension" {
    try testing.expectEqualStrings(".zig", WasmLanguage.zig.extension());
    try testing.expectEqualStrings(".rs", WasmLanguage.rust.extension());
    try testing.expectEqualStrings(".ts", WasmLanguage.assemblyscript.extension());
}

test "WasmToolCache put and get" {
    var cache = WasmToolCache.init(testing.allocator);
    defer cache.deinit();

    const wasm_bytes = [_]u8{ 0x00, 0x61, 0x73, 0x6d }; // WASM magic
    try cache.put("test_tool", &wasm_bytes);

    const retrieved = cache.get("test_tool");
    try testing.expect(retrieved != null);
    try testing.expectEqualSlices(u8, &wasm_bytes, retrieved.?);
}

test "WasmToolCache hash computation" {
    var cache = WasmToolCache.init(testing.allocator);
    defer cache.deinit();

    const wasm_bytes = [_]u8{ 0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00 };
    const hash = cache.computeHash(&wasm_bytes);

    try testing.expectEqual(@as(usize, 32), hash.len);
    // Hash should be deterministic
    const hash2 = cache.computeHash(&wasm_bytes);
    try testing.expectEqualSlices(u8, &hash, &hash2);
}

test "ToolCompilerConfig defaults" {
    const config = ToolCompilerConfig{};
    try testing.expectEqualStrings("zig", config.zig_compiler);
    try testing.expectEqualStrings("asc", config.assemblyscript_compiler);
    try testing.expectEqualStrings("/tmp/coral-wasm", config.temp_dir);
}

test "ToolCompilerConfig: editable mixin is zero size" {
    try testing.expectEqual(@as(usize, 0), @sizeOf(reflection.Editable(ToolCompilerConfig)));
}

test "ToolCompilerConfig: reflective set and get for compiler path" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var config = ToolCompilerConfig{};
    // Simulate loading compiler path from a config file or TUI input.
    try config.editable.set(allocator, "zig_compiler", "/usr/local/bin/zig", .coder);
    defer allocator.free(config.zig_compiler); // free the duped string

    try testing.expectEqualStrings("/usr/local/bin/zig", config.zig_compiler);

    const val = try config.editable.get(allocator, "zig_compiler", .coder);
    defer allocator.free(val);
    try testing.expectEqualStrings("/usr/local/bin/zig", val);
}

test "ToolCompilerConfig: reflective set of bool field" {
    var config = ToolCompilerConfig{};
    try config.editable.set(testing.allocator, "target_wasi", "false", .coder);
    try testing.expect(!config.target_wasi);
}

test "ToolCompilerConfig: field not found returns error" {
    var config = ToolCompilerConfig{};
    const result = config.editable.set(testing.allocator, "nonexistent", "value", .coder);
    try testing.expectError(error.FieldNotFound, result);
}

test "BinaryExecutionRequest flag values" {
    try testing.expectEqual(@as(u32, 1), BinaryExecutionRequest.Flag.VERBOSE);
    try testing.expectEqual(@as(u32, 2), BinaryExecutionRequest.Flag.DRY_RUN);
    try testing.expectEqual(@as(u32, 4), BinaryExecutionRequest.Flag.FORCE);
}

test "BinaryExecutionResult provides words layout" {
    // Verify the struct has the new word-array fields (not the old i64 mask).
    var result: BinaryExecutionResult = undefined;
    result.header = BinaryHeader.init(.execution_result, 0);
    result.success = 1;
    result.error_code = 0;
    result.output_offset = 0;
    result.output_len = 0;
    result.provides_words_offset = @sizeOf(BinaryExecutionResult);
    result.provides_words_count = 0;
    // Zero words → empty provides set.
    const bs = try result.getProvidesBitSet(testing.allocator, &[_]u8{});
    defer @constCast(&bs).deinit(testing.allocator);
    try testing.expectEqual(@as(usize, 0), bs.bit_length);
}

test "BinaryContextNode: getLod returns correct slices" {
    var node = try ContextNode.init(0x1234, "myname", "The full content string.", testing.allocator);
    defer node.free(testing.allocator);

    var buffer: [4096]u8 = undefined;
    const bin = BinaryContextNode.init(&node, &buffer);

    try testing.expectEqualStrings("The full content string.", bin.getLod(&buffer, 0));
    try testing.expectEqualStrings("myname", bin.getLod(&buffer, 4));
}

test "BinaryContextNode: id round-trip via getId" {
    var node = try ContextNode.init(0x0EAD_BEEF_0123_4567, "n", "t", testing.allocator);
    defer node.free(testing.allocator);
    var buffer: [4096]u8 = undefined;
    const bin = BinaryContextNode.init(&node, &buffer);
    try testing.expectEqual(@as(i64, 0x0EAD_BEEF_0123_4567), bin.getId());
}

test "BinaryContextNode: empty strings produce zero lengths" {
    // lod1..lod3 are empty by default from ContextNode.init.
    var node = try ContextNode.init(0x01, "n", "full", testing.allocator);
    defer node.free(testing.allocator);
    var buffer: [4096]u8 = undefined;
    const bin = BinaryContextNode.init(&node, &buffer);

    try testing.expectEqual(@as(u32, 0), bin.lod_lengths[1]);
    try testing.expectEqual(@as(u32, 0), bin.lod_lengths[2]);
    try testing.expectEqual(@as(u32, 0), bin.lod_lengths[3]);
}

test "BinaryHeader: wrong magic fails validate" {
    var h = BinaryHeader.init(.context_node, 0);
    h.magic = [_]u8{ 'X', 'X', 'X', 'X' };
    try testing.expect(!h.validate());
}

test "BinaryHeader: wrong version fails validate" {
    var h = BinaryHeader.init(.context_node, 0);
    h.version = BINARY_SCHEMA_VERSION + 1;
    try testing.expect(!h.validate());
}

test "WasmToolCache: put then get retrieves bytes" {
    var cache = WasmToolCache.init(testing.allocator);
    defer cache.deinit();

    const bytes = [_]u8{ 0x00, 0x61, 0x73, 0x6d };
    try cache.put("tool_a", &bytes);

    const got = cache.get("tool_a");
    try testing.expect(got != null);
    try testing.expectEqualSlices(u8, &bytes, got.?);
}

test "WasmToolCache: put collision replaces old bytes" {
    var cache = WasmToolCache.init(testing.allocator);
    defer cache.deinit();

    const v1 = [_]u8{ 0x01, 0x02 };
    const v2 = [_]u8{ 0x03, 0x04, 0x05 };

    try cache.put("tool", &v1);
    try cache.put("tool", &v2); // overwrite

    const got = cache.get("tool");
    try testing.expect(got != null);
    try testing.expectEqualSlices(u8, &v2, got.?);
}

test "WasmToolCache: get for unknown name returns null" {
    var cache = WasmToolCache.init(testing.allocator);
    defer cache.deinit();

    try testing.expect(cache.get("ghost") == null);
}

test "WasmToolCache: hash is 32 bytes and deterministic" {
    var cache = WasmToolCache.init(testing.allocator);
    defer cache.deinit();

    const data = [_]u8{ 0xDE, 0xAD, 0xBE, 0xEF };
    const h1 = cache.computeHash(&data);
    const h2 = cache.computeHash(&data);

    try testing.expectEqual(@as(usize, 32), h1.len);
    try testing.expectEqualSlices(u8, &h1, &h2);
}

test "WasmToolCache: different inputs produce different hashes" {
    var cache = WasmToolCache.init(testing.allocator);
    defer cache.deinit();

    const a = [_]u8{0x01};
    const b = [_]u8{0x02};
    const ha = cache.computeHash(&a);
    const hb = cache.computeHash(&b);

    try testing.expect(!std.mem.eql(u8, &ha, &hb));
}

test "WasmToolCache: GPA no leaks across put and deinit" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    const allocator = gpa.allocator();

    {
        var cache = WasmToolCache.init(allocator);
        defer cache.deinit();

        try cache.put("t1", &[_]u8{ 1, 2, 3 });
        try cache.put("t2", &[_]u8{ 4, 5 });
        try cache.put("t1", &[_]u8{9}); // overwrite t1
    }

    try testing.expectEqual(.ok, gpa.deinit());
}

test "BinaryContextNode: all-empty LODs produce zero payload size beyond header" {
    // A node with no content in any LOD slot should still serialise without panic.
    var node = try ContextNode.init(0x42, "", "", testing.allocator);
    defer node.free(testing.allocator);

    var buffer: [4096]u8 = undefined;
    const bin = BinaryContextNode.init(&node, &buffer);

    // All four lod_lengths must be zero when no LODs are set.
    for (bin.lod_lengths) |l| {
        try testing.expectEqual(@as(u32, 0), l);
    }
    try testing.expect(bin.header.validate());
}

test "BinaryContextNode: large LOD0 round-trips without truncation" {
    // Simulate a 2 KB embedding/content blob to verify no off-by-one in sizing.
    var content_buf: [2048]u8 = undefined;
    @memset(&content_buf, 'A');

    var node = try ContextNode.init(0x99, "large_node", &content_buf, testing.allocator);
    defer node.free(testing.allocator);

    var buffer: [8192]u8 = undefined;
    const bin = BinaryContextNode.init(&node, &buffer);

    try testing.expectEqual(@as(u32, 2048), bin.lod_lengths[0]);
    try testing.expectEqualSlices(u8, &content_buf, bin.getLod(&buffer, 0));
    try testing.expect(bin.header.validate());
}

test "BinaryContextNode: multiple non-empty LODs round-trip" {
    var node = try ContextNode.init(0x77, "multi_lod", "LOD0 full content", testing.allocator);
    defer node.free(testing.allocator);
    node.setLod(1, "LOD1 summary");
    node.setLod(2, "LOD2 short");

    var buffer: [4096]u8 = undefined;
    const bin = BinaryContextNode.init(&node, &buffer);

    try testing.expectEqualStrings("LOD0 full content", bin.getLod(&buffer, 0));
    try testing.expectEqualStrings("LOD1 summary", bin.getLod(&buffer, 1));
    try testing.expectEqualStrings("LOD2 short", bin.getLod(&buffer, 2));
    try testing.expectEqual(@as(u32, 0), bin.lod_lengths[3]); // LOD3 never set
}

test "verifyWithTests: empty test cases returns true" {
    const result = verifyWithTests(testing.allocator, &[_]u8{}, &[_]ToolTestCase{});
    try testing.expect(result);
}
