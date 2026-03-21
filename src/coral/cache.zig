/// cache.zig — 5-Tier Cache Hierarchy for Query Routing
///
/// Implements a tiered routing system that tries fastest caches first,
/// falling back to slower but more comprehensive methods:
///   L1: Memory Cache      — <10ms  (exact query hash → pre-rendered ContextNodes)
///   L2: Workflow Cache    — <50ms  (pre-compiled WASM tools via Extism)
///   L3: Graph Traversal    — <200ms (CozoDB Datalog queries)
///   L4: Semantic Search    — <500ms (KNN via embeddings)
///   L5: LLM Fallback       — >1s    (external HTTP MCP call)
const std = @import("std");
const db_mod = @import("db.zig");
const wasm_mod = @import("wasm.zig");
const schema = @import("schema.zig");
const hashutil = @import("common");

const Library = db_mod.Library;
const ContextNode = db_mod.ContextNode;
const ContextPacker = db_mod.ContextPacker;
const HydrationPipeline = db_mod.HydrationPipeline;

pub const CacheTier = enum(u8) {
    l1_memory = 1,
    l2_workflow = 2,
    l3_graph = 3,
    l4_semantic = 4,
    l5_llm = 5,
};

pub const RoutingResult = struct {
    nodes: []const ContextNode,
    tool_result: []const u8,
    llm_response: []const u8,
    tier_used: CacheTier,
    latency_ms: u64,
};

pub const L1Cache = struct {
    allocator: std.mem.Allocator,
    entries: std.StringHashMap(RoutingResult),

    pub fn init(allocator: std.mem.Allocator) L1Cache {
        return .{
            .allocator = allocator,
            .entries = std.StringHashMap(RoutingResult).init(allocator),
        };
    }

    pub fn deinit(self: *L1Cache) void {
        var it = self.entries.iterator();
        while (it.next()) |entry| {
            self.allocator.free(entry.key_ptr.*);
            freeRoutingResult(self.allocator, &entry.value_ptr.*);
        }
        self.entries.deinit();
    }

    pub fn get(self: *L1Cache, query_hash: []const u8) ?RoutingResult {
        return self.entries.get(query_hash);
    }

    pub fn put(self: *L1Cache, query_hash: []const u8, result: RoutingResult) !void {
        const owned_key = try self.allocator.dupe(u8, query_hash);
        const owned_result = try dupeRoutingResult(self.allocator, result);
        try self.entries.put(owned_key, owned_result);
    }

    fn freeRoutingResult(allocator: std.mem.Allocator, result: *RoutingResult) void {
        if (result.nodes.len > 0) {
            for (result.nodes) |n| {
                for (n.lod) |l| allocator.free(l);
            }
            allocator.free(result.nodes);
        }
        if (result.tool_result.len > 0) {
            allocator.free(result.tool_result);
        }
        if (result.llm_response.len > 0) {
            allocator.free(result.llm_response);
        }
    }

    fn dupeRoutingResult(allocator: std.mem.Allocator, result: RoutingResult) !RoutingResult {
        var nodes: []const ContextNode = &[_]ContextNode{};
        if (result.nodes.len > 0) {
            const nodes_copy = try allocator.alloc(ContextNode, result.nodes.len);
            for (result.nodes, 0..) |n, i| {
                var copy = n;
                for (n.lod, 0..) |l, j| {
                    copy.lod[j] = try allocator.dupe(u8, l);
                }
                nodes_copy[i] = copy;
            }
            nodes = nodes_copy;
        }
        var tool_result: []const u8 = &[_]u8{};
        if (result.tool_result.len > 0) {
            tool_result = try allocator.dupe(u8, result.tool_result);
        }
        var llm_response: []const u8 = &[_]u8{};
        if (result.llm_response.len > 0) {
            llm_response = try allocator.dupe(u8, result.llm_response);
        }
        return .{
            .nodes = nodes,
            .tool_result = tool_result,
            .llm_response = llm_response,
            .tier_used = result.tier_used,
            .latency_ms = result.latency_ms,
        };
    }
};

pub const QueueReactor = struct {
    const Self = @This();

    allocator: std.mem.Allocator,
    library: *Library,
    l1_cache: L1Cache,
    max_knn_k: usize,

    pub fn init(allocator: std.mem.Allocator, library: *Library, max_knn_k: usize) Self {
        return .{
            .allocator = allocator,
            .library = library,
            .l1_cache = L1Cache.init(allocator),
            .max_knn_k = max_knn_k,
        };
    }

    pub fn deinit(self: *Self) void {
        self.l1_cache.deinit();
    }

    /// Route a query through the L1-L5 hierarchy.
    pub fn route(self: *Self, query: []const u8) !RoutingResult {
        const start_time = std.time.nanoTimestamp();

        // L1: Check memory cache (exact query hash)
        const hash = try self.hashQuery(query);
        defer self.allocator.free(hash);
        if (self.l1_cache.get(hash)) |cached| {
            return .{ .nodes = cached.nodes, .tool_result = &[_]u8{}, .llm_response = &[_]u8{}, .tier_used = .l1_memory, .latency_ms = @intCast(@divTrunc(std.time.nanoTimestamp() - start_time, 1_000_000)) };
        }

        // L2: Check WASM tool cache (stub for now)
        if (self.findWasmTool(query)) |tool| {
            // TODO: Execute WASM tool
            _ = tool;
        }

        // L3: CozoDB graph traversal
        if (self.graphTraversal(query)) |nodes| {
            const elapsed = @divTrunc(std.time.nanoTimestamp() - start_time, 1_000_000);
            return .{
                .nodes = nodes,
                .tool_result = &[_]u8{},
                .llm_response = &[_]u8{},
                .tier_used = .l3_graph,
                .latency_ms = @intCast(elapsed),
            };
        }

        // L4: KNN semantic search
        if (self.semanticSearch(query)) |hits| {
            const elapsed = @divTrunc(std.time.nanoTimestamp() - start_time, 1_000_000);
            return .{
                .nodes = hits,
                .tool_result = &[_]u8{},
                .llm_response = &[_]u8{},
                .tier_used = .l4_semantic,
                .latency_ms = @intCast(elapsed),
            };
        }

        // L5: LLM fallback (stub for now)
        return self.llmFallback(query);
    }

    // ------------------------------------------------------------------
    // Stub implementations (to be connected to real systems later)
    // ------------------------------------------------------------------

    fn findWasmTool(self: *Self, query: []const u8) ?[]const u8 {
        _ = self;
        _ = query;
        return null;
    }

    fn graphTraversal(self: *Self, query: []const u8) ?[]ContextNode {
        _ = self;
        _ = query;
        // TODO: Implement actual CozoDB query
        return null;
    }

    fn semanticSearch(self: *Self, query: []const u8) ?[]ContextNode {
        _ = self;
        _ = query;
        // TODO: Implement embedding + KNN search
        return null;
    }

    fn llmFallback(self: *Self, query: []const u8) RoutingResult {
        _ = self;
        _ = query;
        const start = std.time.nanoTimestamp();
        // Stub: return empty result with high latency
        const elapsed = @divTrunc(std.time.nanoTimestamp() - start, 1_500_000);
        return .{
            .nodes = &[_]ContextNode{},
            .tool_result = &[_]u8{},
            .llm_response = &[_]u8{},
            .tier_used = .l5_llm,
            .latency_ms = @intCast(elapsed),
        };
    }

    /// Cache a successful routing result in L1.
    pub fn cacheResult(self: *Self, query: []const u8, result: RoutingResult) !void {
        const hash = try self.hashQuery(query);
        defer self.allocator.free(hash);
        try self.l1_cache.put(hash, result);
    }

    fn hashQuery(self: *Self, query: []const u8) ![]const u8 {
        return hashutil.hashString(self.allocator, query, .sha256);
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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

    // Cache a result first
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

    // Route should hit L1 cache
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

    // Unknown query should fall through to L5
    const routed = try reactor.route("unknown_query_xyz");
    try testing.expectEqual(CacheTier.l5_llm, routed.tier_used);
    try testing.expect(routed.nodes.len == 0);
}
