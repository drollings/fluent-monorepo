/// fixtures.zig — Test factory functions for coral integration tests
///
/// Provides reusable test fixtures that create pre-configured test libraries,
/// embedding providers, and WASM tools for L1-L5 routing tests.
///
/// Usage:
///   const fixtures = @import("fixtures.zig");
///   var lib = try fixtures.createTestLibrary(testing.allocator);
///   defer lib.deinit();
const std = @import("std");
const coral_db = @import("coral_db");
const embed_mod = @import("common").embeddings;
const common_mod = @import("common");
const Library = coral_db.Library;
const ContextNode = coral_db.ContextNode;
const WasmTool = coral_db.WasmTool;
const EmbeddingProvider = common_mod.EmbeddingProvider;

/// Manages test fixtures with a keyword struct, owns test data, and ensures consistent initialization across runs.
pub const TestLibrary = struct {
    lib: *Library,
    arena: std.heap.ArenaAllocator,
    allocator: std.mem.Allocator,
};

/// Creates a test library instance using the provided allocator.
pub fn createTestLibrary(allocator: std.mem.Allocator) !TestLibrary {
    var arena = std.heap.ArenaAllocator.init(allocator);
    errdefer arena.deinit();
    const aa = arena.allocator();

    const lib = try Library.init(aa, .mem, "");
    try lib.initSchema();

    return TestLibrary{
        .arena = arena,
        .allocator = aa,
        .lib = lib,
    };
}

/// Cleans up resources by deallocating the test library, ensuring no memory leaks.
pub fn deinitTestLibrary(tl: *TestLibrary) void {
    tl.lib.deinit();
    tl.arena.deinit();
    tl.* = undefined;
}

/// Creates an EmbeddingProvider instance from a Zig code snippet.
pub fn createTestEmbedding() EmbeddingProvider {
    var noop = embed_mod.NoopEmbedding{};
    return noop.provider();
}

/// Creates a context node for a test node with specified ID, name, and full text.
pub fn createTestNode(
    tl: *TestLibrary,
    id: i64,
    name: []const u8,
    full_text: []const u8,
) !ContextNode {
    return try ContextNode.init(id, name, full_text, tl.allocator);
}

/// Inserts a test node into the left subtree of a given context node.
pub fn insertTestNode(tl: *TestLibrary, node: ContextNode) !void {
    try tl.lib.insertNode(node);
}

/// Creates a Wasm tool instance using Zig's allocator, target ID, and test data.
pub fn createTestWasmTool(
    allocator: std.mem.Allocator,
    id: i64,
    target_id: i64,
    wasm_b64: []const u8,
    test_passed: bool,
) !WasmTool {
    const b64_copy = try allocator.dupe(u8, wasm_b64);
    errdefer allocator.free(b64_copy);
    return WasmTool{
        .id = id,
        .target_id = target_id,
        .wasm_b64 = b64_copy,
        .schema_hash = "",
        .test_passed = test_passed,
        .created_at = @floatFromInt(std.time.timestamp()),
    };
}

/// Tracks test node configurations; manages state with a single ownership model; ensures invariants during setup and teardown.
pub const TestNodeSpec = struct {
    id: i64,
    name: []const u8,
    full_text: []const u8,
    lod1: ?[]const u8 = null,
    lod2: ?[]const u8 = null,
};

/// Inserts test nodes into the specified test library structure, updating the test suite with new test cases.
pub fn insertTestNodes(tl: *TestLibrary, specs: []const TestNodeSpec) !void {
    for (specs) |spec| {
        var node = try createTestNode(tl, spec.id, spec.name, spec.full_text);
        if (spec.lod1) |l1| node.setLod(1, l1);
        if (spec.lod2) |l2| node.setLod(2, l2);
        try insertTestNode(tl, node);
    }
}









