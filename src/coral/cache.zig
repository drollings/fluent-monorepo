/// cache.zig — 5-Tier Cache Hierarchy for Query Routing
///
/// Implements a tiered routing system that tries fastest caches first,
/// falling back to slower but more comprehensive methods:
///   L1: Memory Cache      — <10ms  (exact query hash → pre-rendered ContextNodes)
///   L2: Workflow Cache    — <50ms  (pre-compiled WASM tools via Extism)
///   L3: Graph Traversal    — <200ms (SQLite recursive CTE graph traversal)
///   L4: Semantic Search    — <500ms (KNN via embeddings)
///   L5: LLM Fallback       — >1s    (external HTTP MCP call)
const std = @import("std");
const coral_db = @import("coral_db");
const wasm_mod = @import("wasm");
const hashutil = @import("common");
const local_model = @import("local_model");
const LocalDecomposer = local_model.LocalDecomposer;
const DecomposerConfig = local_model.DecomposerConfig;

const Library = coral_db.Library;
const ContextNode = coral_db.ContextNode;
const ContextPacker = coral_db.ContextPacker;
const HydrationPipeline = coral_db.HydrationPipeline;
const WasmTool = coral_db.WasmTool;
const EmbeddingProvider = hashutil.EmbeddingProvider;

pub const CacheTier = enum(u8) {
    l1_memory = 1,
    l2_workflow = 2,
    l3_graph = 3,
    l4_semantic = 4,
    /// P6.2 — Local model decomposition: query split into sub-tasks then re-routed.
    l4_5_decompose = 9,
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

// ---------------------------------------------------------------------------
// P3.0 — QueueReactorBuilder (fluent builder)
// ---------------------------------------------------------------------------

pub const QueueReactorBuilder = struct {
    allocator: std.mem.Allocator,
    _library: ?*Library = null,
    _embedder: ?EmbeddingProvider = null,
    _decomposer_cfg: ?DecomposerConfig = null,
    knn_k: usize = 20,
    l4_threshold: f32 = 0.85,
    l3_max_depth: u8 = 4,
    err: ?anyerror = null,

    pub fn init(allocator: std.mem.Allocator) QueueReactorBuilder {
        return .{ .allocator = allocator };
    }

    pub fn library(self: *@This(), lib: *Library) *@This() {
        self._library = lib;
        return self;
    }

    pub fn embedder(self: *@This(), emb: EmbeddingProvider) *@This() {
        self._embedder = emb;
        return self;
    }

    pub fn knnK(self: *@This(), k: usize) *@This() {
        self.knn_k = k;
        return self;
    }

    pub fn l4Threshold(self: *@This(), t: f32) *@This() {
        self.l4_threshold = t;
        return self;
    }

    pub fn l3MaxDepth(self: *@This(), d: u8) *@This() {
        self.l3_max_depth = d;
        return self;
    }

    /// Enable L4.5 local decomposition with the supplied LLM config.
    pub fn decomposerConfig(self: *@This(), cfg: DecomposerConfig) *@This() {
        self._decomposer_cfg = cfg;
        return self;
    }

    pub fn build(self: *@This()) !QueueReactor {
        if (self._library == null) return error.LibraryRequired;
        return QueueReactor{
            .allocator = self.allocator,
            .library = self._library.?,
            .l1_cache = L1Cache.init(self.allocator),
            .max_knn_k = self.knn_k,
            .embedder = self._embedder,
            .l4_threshold = self.l4_threshold,
            .l3_max_depth = self.l3_max_depth,
            .decomposer_cfg = self._decomposer_cfg,
        };
    }
};

// ---------------------------------------------------------------------------
// QueueReactor — 5-tier cache router
// ---------------------------------------------------------------------------

pub const QueueReactor = struct {
    const Self = @This();

    allocator: std.mem.Allocator,
    library: *Library,
    l1_cache: L1Cache,
    max_knn_k: usize,
    // P3.0 — new fields
    embedder: ?EmbeddingProvider = null,
    l4_threshold: f32 = 0.85,
    l3_max_depth: u8 = 4,
    // P3.3 — WASM tool cache (stub)
    wasm_tools: []const WasmTool = &.{},
    // P4.4 — work queue infrastructure (placeholders for Phase 5 worker pool)
    queue_mu: std.Thread.Mutex = .{},
    queue_cond: std.Thread.Condition = .{},
    // P6.2 — L4.5 local decomposition (null = disabled)
    decomposer_cfg: ?DecomposerConfig = null,

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
    pub fn route(self: *Self, query: []const u8) anyerror!RoutingResult {
        const start_time = std.time.nanoTimestamp();

        // L1: Check memory cache (exact query hash)
        const hash = try self.hashQuery(query);
        defer self.allocator.free(hash);
        if (self.l1_cache.get(hash)) |cached| {
            return .{ .nodes = cached.nodes, .tool_result = &[_]u8{}, .llm_response = &[_]u8{}, .tier_used = .l1_memory, .latency_ms = @intCast(@divTrunc(std.time.nanoTimestamp() - start_time, 1_000_000)) };
        }

        // L2: WASM tool cache (requires Extism runtime — Phase 3.3 stub)
        if (self.wasm_tools.len > 0) {
            if (self.findWasmTool(query)) |_tool| {
                // TODO P3.3: ExecutionRequestBuilder + Extism execution
                _ = _tool;
            }
        }

        // L3: Graph traversal
        if (try self.graphTraversal(query)) |nodes| {
            const elapsed = @divTrunc(std.time.nanoTimestamp() - start_time, 1_000_000);
            const result = RoutingResult{
                .nodes = nodes,
                .tool_result = &[_]u8{},
                .llm_response = &[_]u8{},
                .tier_used = .l3_graph,
                .latency_ms = @intCast(elapsed),
            };
            try self.cacheResult(query, result);
            return result;
        }

        // L4: KNN semantic search
        if (try self.semanticSearch(query)) |hits| {
            const elapsed = @divTrunc(std.time.nanoTimestamp() - start_time, 1_000_000);
            const result = RoutingResult{
                .nodes = hits,
                .tool_result = &[_]u8{},
                .llm_response = &[_]u8{},
                .tier_used = .l4_semantic,
                .latency_ms = @intCast(elapsed),
            };
            try self.cacheResult(query, result);
            return result;
        }

        // L4.5: Local model decomposition — split query into sub-tasks and re-route each.
        if (self.decomposer_cfg != null) {
            if (try self.localDecompose(query, 0)) |merged| {
                const elapsed = @divTrunc(std.time.nanoTimestamp() - start_time, 1_000_000);
                const result = RoutingResult{
                    .nodes = merged,
                    .tool_result = &[_]u8{},
                    .llm_response = &[_]u8{},
                    .tier_used = .l4_5_decompose,
                    .latency_ms = @intCast(elapsed),
                };
                try self.cacheResult(query, result);
                // P6.3: persist novel solution so future similar queries hit L4.
                self.persistSolution(query, result) catch {};
                return result;
            }
        }

        // L5: LLM fallback (stub for now)
        const l5_result = self.llmFallback(query);
        if (l5_result.nodes.len > 0) {
            self.persistSolution(query, l5_result) catch {};
        }
        return l5_result;
    }

    // ------------------------------------------------------------------
    // P3.3 — L2: WASM tool lookup (stub)
    // ------------------------------------------------------------------

    fn findWasmTool(self: *Self, query: []const u8) ?WasmTool {
        _ = self;
        _ = query;
        return null;
    }

    // ------------------------------------------------------------------
    // P3.2 — L3: Graph Traversal
    // ------------------------------------------------------------------

    fn graphTraversal(self: *Self, query: []const u8) !?[]ContextNode {
        // Look up a node whose lod4 (name) exactly matches the query
        const maybe_id = try self.library.findNodeByName(query);
        if (maybe_id == null) return null;

        var graph_arena = std.heap.ArenaAllocator.init(self.allocator);
        defer graph_arena.deinit();

        const nodes = try self.library.traverseFrom(graph_arena.allocator(), maybe_id.?, self.l3_max_depth);
        if (nodes.len == 0) return null;

        // Copy nodes out of the arena into self.allocator so caller can own them
        const owned = try self.allocator.alloc(ContextNode, nodes.len);
        errdefer self.allocator.free(owned);
        for (nodes, 0..) |src_node, i| {
            var copy = src_node;
            // Re-dupe each owned LOD string from the arena into self.allocator
            copy.lod_owned = 0;
            for (src_node.lod, 0..) |lod_str, j| {
                if (src_node.lod_owned & (@as(u8, 1) << @intCast(j)) != 0) {
                    copy.lod[j] = try self.allocator.dupe(u8, lod_str);
                    copy.lod_owned |= @as(u8, 1) << @intCast(j);
                }
            }
            owned[i] = copy;
        }
        return owned;
    }

    // ------------------------------------------------------------------
    // P3.1 — L4: KNN Semantic Search
    // ------------------------------------------------------------------

    fn semanticSearch(self: *Self, query: []const u8) !?[]ContextNode {
        if (self.embedder == null) return null;

        var search_arena = std.heap.ArenaAllocator.init(self.allocator);
        defer search_arena.deinit();

        const embedding = try self.embedder.?.embed(search_arena.allocator(), query);
        if (embedding.len == 0) return null;

        const knn_hits = try self.library.knnSearch(search_arena.allocator(), embedding, self.max_knn_k);
        if (knn_hits.len == 0) return null;

        var result_list: std.ArrayListUnmanaged(ContextNode) = .{};
        errdefer {
            for (result_list.items) |*n| n.free(self.allocator);
            result_list.deinit(self.allocator);
        }

        for (knn_hits) |hit| {
            const maybe_node = try self.library.fetchNode(hit.id);
            if (maybe_node) |node| {
                try result_list.append(self.allocator, node);
            }
        }

        if (result_list.items.len == 0) {
            result_list.deinit(self.allocator);
            return null;
        }

        return try result_list.toOwnedSlice(self.allocator);
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

    // ------------------------------------------------------------------
    // P6.2 — L4.5: Local decomposition with recursive sub-task routing
    // ------------------------------------------------------------------

    /// Decompose `query` into sub-tasks via local LLM and route each recursively.
    /// Returns merged nodes slice (owned by self.allocator) or null on failure.
    /// `depth` guards against unbounded recursion; max_depth comes from DecomposerConfig.
    fn localDecompose(self: *Self, query: []const u8, depth: u8) !?[]ContextNode {
        const cfg = self.decomposer_cfg orelse return null;
        if (depth >= cfg.max_depth) return null;

        var decomp = LocalDecomposer.init(self.allocator, cfg);
        var sub_arena = std.heap.ArenaAllocator.init(self.allocator);
        defer sub_arena.deinit();

        const sub_tasks = try decomp.decompose(sub_arena.allocator(), query);

        // If decomposer returned the original query unchanged (fallback), skip.
        if (sub_tasks.len == 1 and std.mem.eql(u8, sub_tasks[0], query)) return null;

        // Route each sub-task and collect nodes.
        var merged = std.ArrayListUnmanaged(ContextNode).empty;
        errdefer merged.deinit(self.allocator);

        for (sub_tasks) |sub| {
            const sub_result = self.route(sub) catch continue;
            for (sub_result.nodes) |node| {
                // Deduplicate by node id.
                var found = false;
                for (merged.items) |existing| {
                    if (existing.id == node.id) {
                        found = true;
                        break;
                    }
                }
                if (!found) try merged.append(self.allocator, node);
            }
            // Cache the individual sub-task result.
            try self.cacheResult(sub, sub_result);
        }

        if (merged.items.len == 0) {
            merged.deinit(self.allocator);
            return null;
        }

        return try merged.toOwnedSlice(self.allocator);
    }

    /// Cache a successful routing result in L1.
    pub fn cacheResult(self: *Self, query: []const u8, result: RoutingResult) !void {
        const h = try self.hashQuery(query);
        defer self.allocator.free(h);
        try self.l1_cache.put(h, result);
    }

    fn hashQuery(self: *Self, query: []const u8) ![]const u8 {
        return hashutil.hashString(self.allocator, query, .sha256);
    }

    // ------------------------------------------------------------------
    // P6.3 — Solution caching: persist novel L4.5/L5 results to Library
    // ------------------------------------------------------------------
    //
    // When a novel query is resolved via decomposition or LLM fallback,
    // we store a ContextNode whose lod4 = query text and lod0 = summary
    // of resolved nodes.  On subsequent semantically-similar queries,
    // L4 KNN search finds this node and returns it as a cached hit.

    fn persistSolution(self: *Self, query: []const u8, result: RoutingResult) !void {
        if (result.nodes.len == 0) return;

        // Build a summary string: list of node names from the result.
        var summary_buf = std.ArrayListUnmanaged(u8).empty;
        defer summary_buf.deinit(self.allocator);
        for (result.nodes, 0..) |node, i| {
            if (i > 0) try summary_buf.appendSlice(self.allocator, ", ");
            try summary_buf.appendSlice(self.allocator, node.lod[4]); // lod4 = name
        }

        // Assign a stable id derived from query hash so re-inserting is idempotent.
        const hash_bytes = try self.hashQuery(query);
        defer self.allocator.free(hash_bytes);
        var id_bytes: [8]u8 = undefined;
        @memcpy(&id_bytes, hash_bytes[0..8]);
        const solution_id: i64 = @bitCast(id_bytes);

        const node = try ContextNode.init(
            solution_id,
            query,
            summary_buf.items,
            self.allocator,
        );
        defer {
            // ContextNode.init allocates lod[0] and lod[4] (lod_owned bitmask).
            const n = node;
            for (0..n.lod.len) |i| {
                if ((n.lod_owned >> @intCast(i)) & 1 != 0) self.allocator.free(n.lod[i]);
            }
        }

        // Best-effort insert — ignore duplicate key errors.
        self.library.insertNode(node) catch |err| {
            if (err != error.AlreadyExists) return err;
        };
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
    // simple struct field access test — confirms fields compile
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    var reactor = QueueReactor.init(allocator, lib, 10);
    defer reactor.deinit();

    // Access the P4.4 fields to confirm they compile and have correct types
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

test "Library.traverseFrom: returns root node" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    // Insert a single node; BFS from it returns at least itself
    var node = try ContextNode.init(7, "root_node", "Root content.", allocator);
    defer node.free(allocator);
    try lib.insertNode(node);

    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();

    const nodes = try lib.traverseFrom(arena.allocator(), 7, 4);
    try testing.expect(nodes.len >= 1);
    try testing.expectEqual(@as(i64, 7), nodes[0].id);
}
