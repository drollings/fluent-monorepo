/// cache_test.zig — Integration tests for L1-L5 routing pipeline
///
/// Tests the full routing hierarchy:
///   L1: Memory Cache      — exact query hash match
///   L2: Workflow Cache   — WASM tool execution
///   L3: Graph Traversal   — name lookup + BFS
///   L4: Semantic Search   — KNN via embeddings
///   L5: LLM Fallback      — external HTTP call (mocked)
const std = @import("std");
const testing = std.testing;
const cache = @import("cache.zig");
const coral_db = @import("coral_db");
const common_mod = @import("common");
const embed_mod = common_mod.embeddings;
const Library = coral_db.Library;
const ContextNode = coral_db.ContextNode;
const QueueReactor = cache.QueueReactor;
const QueueReactorBuilder = cache.QueueReactorBuilder;
const CacheTier = cache.CacheTier;
const WasmTool = coral_db.WasmTool;
const RoutingResult = cache.RoutingResult;

/// Creates a test library instance using the provided allocator.
fn makeTestLib(allocator: std.mem.Allocator) !*Library {
    const lib = try Library.init(allocator, .mem, "");
    try lib.initSchema();
    return lib;
}

test "L1: cache returns pre-populated result" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try makeTestLib(allocator);
    defer lib.deinit();

    var reactor = try QueueReactorBuilder.init(allocator)
        .library(lib)
        .knnK(5)
        .build();
    defer reactor.deinit();

    const query = "test_query_exact";
    const cached_result = RoutingResult{
        .nodes = &[_]ContextNode{},
        .tool_result = &[_]u8{},
        .llm_response = &[_]u8{},
        .tier_used = .l1_memory,
        .latency_ms = 1,
    };
    try reactor.l1_cache.put(query, cached_result);

    const result = try reactor.route(query);
    try testing.expectEqual(CacheTier.l1_memory, result.tier_used);
}

test "L3: graph traversal finds node by exact name match" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try makeTestLib(allocator);
    defer lib.deinit();

    var node = try ContextNode.init(42, "search_target", "Full content here.", allocator);
    defer node.free(allocator);
    try lib.insertNode(node);

    var reactor = try QueueReactorBuilder.init(allocator)
        .library(lib)
        .knnK(5)
        .l3MaxDepth(4)
        .build();
    defer reactor.deinit();

    const result = try reactor.route("search_target");
    try testing.expectEqual(CacheTier.l3_graph, result.tier_used);
    try testing.expect(result.nodes.len >= 1);
}

test "L3: graph traversal returns null for unknown name" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try makeTestLib(allocator);
    defer lib.deinit();

    var reactor = try QueueReactorBuilder.init(allocator)
        .library(lib)
        .knnK(5)
        .l3MaxDepth(4)
        .build();
    defer reactor.deinit();

    const result = try reactor.route("nonexistent_node_name");
    try testing.expect(result.nodes.len == 0);
}

test "L5: fallback when no caches hit and no LLM configured" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try makeTestLib(allocator);
    defer lib.deinit();

    var reactor = try QueueReactorBuilder.init(allocator)
        .library(lib)
        .knnK(5)
        .build();
    defer reactor.deinit();

    const result = try reactor.route("unknown query with no matches");
    try testing.expectEqual(CacheTier.l5_llm, result.tier_used);
    try testing.expectEqual(@as(usize, 0), result.nodes.len);
}

test "L2 skipped when wasm_tools is empty" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try makeTestLib(allocator);
    defer lib.deinit();

    var reactor = try QueueReactorBuilder.init(allocator)
        .library(lib)
        .knnK(5)
        .build();
    defer reactor.deinit();

    try testing.expectEqual(@as(usize, 0), reactor.wasm_tools.len);
    const result = try reactor.route("wasm_query");
    try testing.expect(result.tier_used != .l2_workflow);
}

test "L4 skipped when embedder is null" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try makeTestLib(allocator);
    defer lib.deinit();

    var reactor = try QueueReactorBuilder.init(allocator)
        .library(lib)
        .knnK(5)
        .build();
    defer reactor.deinit();

    try testing.expect(reactor.embedder == null);
    const result = try reactor.route("semantic query");
    try testing.expect(result.tier_used != .l4_semantic);
}

test "routing pipeline falls through all tiers to L5" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try makeTestLib(allocator);
    defer lib.deinit();

    var reactor = try QueueReactorBuilder.init(allocator)
        .library(lib)
        .knnK(5)
        .build();
    defer reactor.deinit();

    const result = try reactor.route("completely unknown query");
    try testing.expectEqual(CacheTier.l5_llm, result.tier_used);
    try testing.expectEqual(@as(usize, 0), result.nodes.len);
}

test "QueueReactor.route returns allocator-owned nodes for L3" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try makeTestLib(allocator);
    defer lib.deinit();

    var node = try ContextNode.init(99, "owned_node_test", "Content for ownership test.", allocator);
    defer node.free(allocator);
    try lib.insertNode(node);

    var reactor = try QueueReactorBuilder.init(allocator)
        .library(lib)
        .knnK(5)
        .l3MaxDepth(2)
        .build();
    defer reactor.deinit();

    const result = try reactor.route("owned_node_test");
    try testing.expectEqual(CacheTier.l3_graph, result.tier_used);
    try testing.expect(result.nodes.len > 0);
    for (result.nodes) |n| {
        n.free(allocator);
    }
    allocator.free(result.nodes);
}

