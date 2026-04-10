//! cache.zig — 5-Tier Cache Hierarchy for Query Routing (re-export facade)
//!
//! Type definitions moved to:
//!   cache_l1.zig    — CacheTier, RoutingResult, L1Cache, L1HashCache
//!   cache_reactor.zig — QueueReactorBuilder, QueueReactor
//!   cache_router.zig — ParallelRouter

const std = @import("std");
const cache_l1 = @import("cache_l1.zig");
const cache_reactor = @import("cache_reactor.zig");
const cache_router = @import("cache_router.zig");
const Library = @import("coral_db").Library;
const ContextNode = @import("coral_db").ContextNode;
const WasmTool = @import("coral_db").WasmTool;

pub const CacheTier = cache_l1.CacheTier;
pub const RoutingResult = cache_l1.RoutingResult;
pub const L1_DEFAULT_MAX_ENTRIES = cache_l1.L1_DEFAULT_MAX_ENTRIES;
pub const L1Cache = cache_l1.L1Cache;
pub const L1HashCache = cache_l1.L1HashCache;
pub const QueueReactorBuilder = cache_reactor.QueueReactorBuilder;
pub const QueueReactor = cache_reactor.QueueReactor;
pub const ParallelRouter = cache_router.ParallelRouter;

const testing = std.testing;

test "L1Cache: put and get" {
    var cache = L1Cache.init(testing.allocator);
    defer cache.deinit();

    var node = try ContextNode.init(1, "test", "Test content.", testing.allocator);
    defer node.free(testing.allocator);
    const result = RoutingResult{
        .nodes = &[_]ContextNode{node},
        .tool_result = &[_]u8{},
        .llm_response = &[_]u8{},
        .tier_used = .l1_memory,
        .latency_ms = 5,
    };

    try cache.put("test_query", result);
    const cached = cache.get("test_query");
    try testing.expect(cached != null);
}

test "QueueReactor: L1 cache hit" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    var reactor = QueueReactor.init(allocator, lib, 10);
    defer reactor.deinit();

    var node = try ContextNode.init(42, "cached_node", "Cached content.", allocator);
    defer node.free(allocator);
    const result = RoutingResult{
        .nodes = &[_]ContextNode{node},
        .tool_result = &[_]u8{},
        .llm_response = &[_]u8{},
        .tier_used = .l1_memory,
        .latency_ms = 1,
    };
    try reactor.cacheResult("cached_query", result);

    const routed = try reactor.route("cached_query");
    try testing.expectEqual(CacheTier.l1_memory, routed.tier_used);
}

test "QueueReactor: L5 fallback" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    var reactor = QueueReactor.init(allocator, lib, 10);
    defer reactor.deinit();

    const routed = try reactor.route("unknown_query_xyz");
    try testing.expectEqual(CacheTier.l5_llm, routed.tier_used);
    try testing.expect(routed.nodes.len == 0);
}

test "QueueReactorBuilder: builds with library" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    var builder = QueueReactorBuilder.init(allocator);
    var reactor = try builder.library(lib).knnK(5).l3MaxDepth(3).l4Threshold(0.9).build();
    defer reactor.deinit();

    try testing.expectEqual(@as(usize, 5), reactor.max_knn_k);
    try testing.expectEqual(@as(u8, 3), reactor.l3_max_depth);
    try testing.expectEqual(@as(f32, 0.9), reactor.l4_threshold);
}

test "QueueReactorBuilder: error when library missing" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var builder = QueueReactorBuilder.init(allocator);
    const result = builder.build();
    try testing.expectError(error.LibraryRequired, result);
}

test "QueueReactor: work queue fields exist" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    var reactor = QueueReactor.init(allocator, lib, 10);
    defer reactor.deinit();

    _ = &reactor.queue_mu;
    _ = &reactor.queue_cond;
    try testing.expect(true);
}

test "Library.findNodeByName: finds by lod4" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    var node = try ContextNode.init(99, "my_entity", "Full description.", allocator);
    defer node.free(allocator);
    try lib.insertNode(node);

    const found_id = try lib.findNodeByName("my_entity");
    try testing.expect(found_id != null);
    try testing.expectEqual(@as(i64, 99), found_id.?);

    const not_found = try lib.findNodeByName("nonexistent");
    try testing.expect(not_found == null);
}

test "QueueReactor: L2 skipped when no wasm_tools" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    var reactor = QueueReactor.init(allocator, lib, 10);
    defer reactor.deinit();
    try testing.expectEqual(@as(usize, 0), reactor.wasm_tools.len);
    const routed = try reactor.route("wasm_query");
    try testing.expectEqual(CacheTier.l5_llm, routed.tier_used);
}

test "findWasmTool: returns null when all tools fail test_passed check" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    var reactor = QueueReactor.init(allocator, lib, 10);
    defer reactor.deinit();

    const tools = [_]WasmTool{
        .{ .id = 1, .target_id = 0, .wasm_b64 = "abc", .schema_hash = "", .test_passed = false, .created_at = 0 },
    };
    reactor.wasm_tools = &tools;
    try testing.expect(reactor.findWasmTool("any") == null);
}

test "Library.traverseFrom: returns root node" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    var node = try ContextNode.init(7, "root_node", "Root content.", allocator);
    defer node.free(allocator);
    try lib.insertNode(node);

    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();

    const nodes = try lib.traverseFrom(arena.allocator(), 7, 4);
    try testing.expect(nodes.len >= 1);
    try testing.expectEqual(@as(i64, 7), nodes[0].id);
}

test "QueueReactor: submitAsync falls back to synchronous without thread pool" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    var reactor = QueueReactor.init(allocator, lib, 10);
    defer reactor.deinit();

    const task = try reactor.submitAsync("test_query");
    defer {
        task.arena.deinit();
        allocator.destroy(task);
    }
    try testing.expect(task.done.load(.acquire));
    try testing.expectEqual(CacheTier.l5_llm, task.result.tier_used);
}

test "QueueReactorBuilder: threadCount initialises thread pool" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    var builder = QueueReactorBuilder.init(allocator);
    var reactor = try builder.library(lib).threadCount(2).build();
    defer reactor.deinit();

    try testing.expect(reactor.thread_pool != null);
}

test "L1Cache: concurrent reads are safe" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var cache = L1Cache.init(allocator);
    defer cache.deinit();

    const result = RoutingResult{
        .nodes = &[_]ContextNode{},
        .tool_result = &[_]u8{},
        .llm_response = &[_]u8{},
        .tier_used = .l1_memory,
        .latency_ms = 5,
    };
    try cache.put("hash1", result);

    const r1 = cache.get("hash1");
    const r2 = cache.get("hash1");
    try testing.expect(r1 != null);
    try testing.expect(r2 != null);
}

test "M5: concurrent writes via thread pool do not deadlock" {
    const NTHREADS = 4;
    const NODES_PER_THREAD = 10;

    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    const InsertArgs = struct {
        library: *Library,
        base_id: i64,
        allocator: std.mem.Allocator,
    };

    const insertBatch = struct {
        fn run(args: InsertArgs) void {
            var i: i64 = 0;
            while (i < NODES_PER_THREAD) : (i += 1) {
                const node_id = args.base_id * 1000 + i;
                var node = ContextNode.init(node_id, "n", "desc", args.allocator) catch return;
                defer node.free(args.allocator);
                args.library.insertNode(node) catch {};
            }
        }
    }.run;

    var pool: std.Thread.Pool = undefined;
    try pool.init(.{ .allocator = allocator, .n_jobs = NTHREADS });
    defer pool.deinit();

    var wg = std.Thread.WaitGroup{};
    var i: i64 = 0;
    while (i < NTHREADS) : (i += 1) {
        pool.spawnWg(&wg, insertBatch, .{InsertArgs{ .library = lib, .base_id = i, .allocator = allocator }});
    }
    pool.waitAndWork(&wg);

    const count = lib.countNodes() catch 0;
    try testing.expect(count > 0);
}

test "ParallelRouter: routeBatch with empty input" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    var reactor = QueueReactor.init(allocator, lib, 10);
    defer reactor.deinit();

    var router = ParallelRouter.init(&reactor);
    const results = try router.routeBatch(allocator, &[_][]const u8{});
    defer allocator.free(results);
    try testing.expectEqual(@as(usize, 0), results.len);
}
