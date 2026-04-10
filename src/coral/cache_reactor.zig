//! cache_reactor.zig — QueueReactorBuilder and QueueReactor.
//!
//! Implements the 5-tier cache routing hierarchy:
//!   L1: Memory Cache      — <10ms
//!   L2: Workflow Cache    — <50ms (WASM tools)
//!   L3: Graph Traversal   — <200ms (SQLite recursive CTE)
//!   L4: Semantic Search   — <500ms (KNN embeddings)
//!   L5: LLM Fallback      — >1s
const std = @import("std");
const coral_db = @import("coral_db");
const hashutil = @import("common");
const common_registry = @import("dag").registry;
const llm_mod = @import("llm");
const frontier = @import("frontier.zig");
const wasm_mod = @import("wasm");
const cache_l1 = @import("cache_l1.zig");

const Library = coral_db.Library;
const ContextNode = coral_db.ContextNode;
const WasmTool = coral_db.WasmTool;
const EmbeddingProvider = hashutil.EmbeddingProvider;
const BuilderError = common_registry.BuilderError;
const BuilderPhase = common_registry.BuilderPhase;
const logIfError = common_registry.logIfError;
const LocalDecomposer = llm_mod.LocalDecomposer;
const DecomposerConfig = llm_mod.DecomposerConfig;
const LlmConfig = llm_mod.LlmConfig;
const LlmClient = llm_mod.LlmClient;
const L1Cache = cache_l1.L1Cache;
pub const RoutingResult = cache_l1.RoutingResult;

pub const QueueReactorBuilder = struct {
    allocator: std.mem.Allocator,
    arena: std.heap.ArenaAllocator,
    _library: ?*Library = null,
    _embedder: ?EmbeddingProvider = null,
    _decomposer_cfg: ?DecomposerConfig = null,
    knn_k: usize = 20,
    l4_threshold: f32 = 0.85,
    l3_max_depth: u8 = 4,
    _thread_count: ?u32 = null,
    _frontier_cfg: ?LlmConfig = null,
    err: ?*BuilderError = null,

    pub fn init(allocator: std.mem.Allocator) QueueReactorBuilder {
        return .{
            .allocator = allocator,
            .arena = std.heap.ArenaAllocator.init(allocator),
        };
    }

    fn setError(self: *@This(), phase: BuilderPhase, field: []const u8, constraint: []const u8, cause: anyerror) void {
        if (self.err != null) return;
        self.err = BuilderError.init(self.arena.allocator(), phase, field, null, constraint, cause) catch null;
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

    pub fn decomposerConfig(self: *@This(), cfg: DecomposerConfig) *@This() {
        self._decomposer_cfg = cfg;
        return self;
    }

    pub fn threadCount(self: *@This(), n: u32) *@This() {
        self._thread_count = n;
        return self;
    }

    pub fn frontierCfg(self: *@This(), cfg: LlmConfig) *@This() {
        self._frontier_cfg = cfg;
        return self;
    }

    pub fn build(self: *@This()) !QueueReactor {
        defer self.arena.deinit();
        if (self._library == null) {
            self.setError(.validation, "library", "required", error.LibraryRequired);
        }
        if (self.err) |e| {
            logIfError(e);
            return e.cause;
        }
        var reactor = QueueReactor{
            .allocator = self.allocator,
            .library = self._library.?,
            .l1_cache = L1Cache.init(self.allocator),
            .max_knn_k = self.knn_k,
            .embedder = self._embedder,
            .l4_threshold = self.l4_threshold,
            .l3_max_depth = self.l3_max_depth,
            .decomposer_cfg = self._decomposer_cfg,
            .frontier_cfg = self._frontier_cfg,
        };
        if (self._thread_count) |n| {
            const pool = try self.allocator.create(std.Thread.Pool);
            errdefer self.allocator.destroy(pool);
            try pool.init(.{ .allocator = self.allocator, .n_jobs = n });
            reactor.thread_pool = pool;
        }
        return reactor;
    }
};

pub const QueueReactor = struct {
    const Self = @This();

    allocator: std.mem.Allocator,
    library: *Library,
    l1_cache: L1Cache,
    max_knn_k: usize,
    embedder: ?EmbeddingProvider = null,
    l4_threshold: f32 = 0.85,
    l3_max_depth: u8 = 4,
    wasm_tools: []const WasmTool = &.{},
    queue_mu: std.Thread.Mutex = .{},
    queue_cond: std.Thread.Condition = .{},
    thread_pool: ?*std.Thread.Pool = null,
    decomposer_cfg: ?DecomposerConfig = null,
    frontier_cfg: ?LlmConfig = null,

    pub fn init(allocator: std.mem.Allocator, library: *Library, max_knn_k: usize) Self {
        return .{
            .allocator = allocator,
            .library = library,
            .l1_cache = L1Cache.init(allocator),
            .max_knn_k = max_knn_k,
        };
    }

    pub fn deinit(self: *Self) void {
        if (self.thread_pool) |pool| {
            pool.deinit();
            self.allocator.destroy(pool);
        }
        self.l1_cache.deinit();
    }

    pub const Task = struct {
        arena: std.heap.ArenaAllocator,
        query: []const u8 = &[_]u8{},
        result: RoutingResult = .{
            .nodes = &[_]ContextNode{},
            .tool_result = &[_]u8{},
            .llm_response = &[_]u8{},
            .tier_used = .l5_llm,
            .latency_ms = 0,
        },
        done: std.atomic.Value(bool) = std.atomic.Value(bool).init(false),
        err: ?anyerror = null,
    };

    pub fn submitAsync(self: *Self, query: []const u8) !*Task {
        const task = try self.allocator.create(Task);
        task.arena = std.heap.ArenaAllocator.init(self.allocator);
        task.result = .{
            .nodes = &[_]ContextNode{},
            .tool_result = &[_]u8{},
            .llm_response = &[_]u8{},
            .tier_used = .l5_llm,
            .latency_ms = 0,
        };
        task.done = std.atomic.Value(bool).init(false);
        task.err = null;
        errdefer {
            task.arena.deinit();
            self.allocator.destroy(task);
        }
        task.query = try task.arena.allocator().dupe(u8, query);

        if (self.thread_pool) |pool| {
            var wg = std.Thread.WaitGroup{};
            pool.spawnWg(&wg, routeTask, .{ self, task });
            pool.waitAndWork(&wg);
        } else {
            routeTask(self, task);
        }
        return task;
    }

    fn routeTask(self: *Self, task: *Task) void {
        task.result = self.route(task.query) catch |err| blk: {
            task.err = err;
            break :blk .{
                .nodes = &[_]ContextNode{},
                .tool_result = &[_]u8{},
                .llm_response = &[_]u8{},
                .tier_used = .l5_llm,
                .latency_ms = 0,
            };
        };
        task.done.store(true, .release);
    }

    pub fn route(self: *Self, query: []const u8) anyerror!RoutingResult {
        const start_time = std.time.nanoTimestamp();

        const hash = try self.hashQuery(query);
        defer self.allocator.free(hash);
        if (self.l1_cache.get(hash)) |cached| {
            return .{ .nodes = cached.nodes, .tool_result = &[_]u8{}, .llm_response = &[_]u8{}, .tier_used = .l1_memory, .latency_ms = @intCast(@divTrunc(std.time.nanoTimestamp() - start_time, 1_000_000)) };
        }

        if (self.wasm_tools.len > 0) {
            if (try self.routeL2Wasm(query)) |wasm_result| {
                const elapsed = @divTrunc(std.time.nanoTimestamp() - start_time, 1_000_000);
                const result = RoutingResult{
                    .nodes = &[_]ContextNode{},
                    .tool_result = wasm_result,
                    .llm_response = &[_]u8{},
                    .tier_used = .l2_workflow,
                    .latency_ms = @intCast(elapsed),
                };
                try self.cacheResult(query, result);
                return result;
            }
        }

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
                self.persistSolution(query, result) catch {};
                return result;
            }
        }

        if (try self.routeL5Frontier(query)) |frontier_result| {
            const elapsed = @divTrunc(std.time.nanoTimestamp() - start_time, 1_000_000);
            const result = RoutingResult{
                .nodes = frontier_result.nodes,
                .tool_result = frontier_result.tool_result,
                .llm_response = frontier_result.llm_response,
                .tier_used = .l5_llm,
                .latency_ms = @intCast(elapsed),
            };
            self.persistSolution(query, result) catch {};
            return result;
        }
        const l5_result = self.llmFallback(query);
        return l5_result;
    }

    pub fn findWasmTool(self: *Self, query: []const u8) ?WasmTool {
        _ = query;
        for (self.wasm_tools) |tool| {
            if (tool.test_passed and tool.wasm_b64.len > 0) return tool;
        }
        return null;
    }

    fn routeL2Wasm(self: *Self, query: []const u8) !?[]const u8 {
        const tool = self.findWasmTool(query) orelse return null;
        if (tool.wasm_b64.len == 0) return null;

        const wasm_size = std.base64.standard.Decoder.calcSizeForSlice(tool.wasm_b64) catch return null;
        const wasm_bytes = try self.allocator.alloc(u8, wasm_size);
        defer self.allocator.free(wasm_bytes);
        std.base64.standard.Decoder.decode(wasm_bytes, tool.wasm_b64) catch return null;

        if (wasm_bytes.len < 4 or
            wasm_bytes[0] != 0x00 or wasm_bytes[1] != 0x61 or
            wasm_bytes[2] != 0x73 or wasm_bytes[3] != 0x6D)
        {
            return null;
        }

        var host_reg = wasm_mod.HostFunctionRegistry.init(self.allocator, self.library);
        defer host_reg.deinit();
        host_reg.registerStandard() catch {};
        const output = wasm_mod.executeWasmQueryWithHosts(self.allocator, wasm_bytes, query, &host_reg) catch return null;
        return output;
    }

    fn graphTraversal(self: *Self, query: []const u8) !?[]ContextNode {
        const maybe_id = try self.library.findNodeByName(query);
        if (maybe_id == null) return null;

        var graph_arena = std.heap.ArenaAllocator.init(self.allocator);
        defer graph_arena.deinit();

        const nodes = try self.library.traverseFrom(graph_arena.allocator(), maybe_id.?, self.l3_max_depth);
        if (nodes.len == 0) return null;

        const owned = try self.allocator.alloc(ContextNode, nodes.len);
        errdefer self.allocator.free(owned);
        for (nodes, 0..) |src_node, i| {
            owned[i] = try src_node.clone(self.allocator);
        }
        return owned;
    }

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
        return .{
            .nodes = &[_]ContextNode{},
            .tool_result = &[_]u8{},
            .llm_response = &[_]u8{},
            .tier_used = .l5_llm,
            .latency_ms = 0,
        };
    }

    fn routeL5Frontier(self: *Self, query: []const u8) !?RoutingResult {
        const cfg = self.frontier_cfg orelse return null;

        const focal_id_opt = try self.library.findNodeByName(query);

        var prompt: []const u8 = undefined;
        var prompt_owned = false;
        if (focal_id_opt) |focal_id| {
            if (frontier.minimizeContext(self.allocator, self.library, query, focal_id, 2048)) |*ctx| {
                var mctx = ctx.*;
                defer mctx.deinit();
                prompt = try frontier.buildPrompt(self.allocator, mctx);
                prompt_owned = true;
            } else |_| {
                prompt = try std.fmt.allocPrint(self.allocator, "Answer this query: {s}", .{query});
                prompt_owned = true;
            }
        } else {
            prompt = try std.fmt.allocPrint(self.allocator, "Answer this query: {s}", .{query});
            prompt_owned = true;
        }
        defer if (prompt_owned) self.allocator.free(prompt);

        const maybe_result = try self.callFrontierLlm(cfg, prompt);
        if (maybe_result) |result| {
            if (result.llm_response.len > 0) {
                frontier.indexSolution(self.allocator, self.library, query, result.llm_response) catch {};
            }
        }
        return maybe_result;
    }

    fn callFrontierLlm(self: *Self, cfg: LlmConfig, prompt: []const u8) !?RoutingResult {
        var client = LlmClient.init(self.allocator, cfg) catch return null;
        defer client.deinit();

        const raw_opt = client.complete(prompt, 1024, 0.2, null) catch return null;
        const raw = raw_opt orelse return null;
        defer self.allocator.free(raw);

        const vr = frontier.validateSolution(raw);
        if (!vr.valid) return null;

        const owned_response = try self.allocator.dupe(u8, raw);
        return RoutingResult{
            .nodes = &[_]ContextNode{},
            .tool_result = &[_]u8{},
            .llm_response = owned_response,
            .tier_used = .l5_llm,
            .latency_ms = 0,
        };
    }

    fn localDecompose(self: *Self, query: []const u8, depth: u8) !?[]ContextNode {
        const cfg = self.decomposer_cfg orelse return null;
        if (depth >= cfg.max_depth) return null;

        var decomp = LocalDecomposer.init(self.allocator, cfg);
        var sub_arena = std.heap.ArenaAllocator.init(self.allocator);
        defer sub_arena.deinit();

        const sub_tasks = try decomp.decompose(sub_arena.allocator(), query);

        if (sub_tasks.len == 1 and std.mem.eql(u8, sub_tasks[0], query)) return null;

        var merged = std.ArrayListUnmanaged(ContextNode).empty;
        errdefer merged.deinit(self.allocator);

        for (sub_tasks) |sub| {
            const sub_result = self.route(sub) catch continue;
            for (sub_result.nodes) |node| {
                var found = false;
                for (merged.items) |existing| {
                    if (existing.id == node.id) {
                        found = true;
                        break;
                    }
                }
                if (!found) try merged.append(self.allocator, node);
            }
            try self.cacheResult(sub, sub_result);
        }

        if (merged.items.len == 0) {
            merged.deinit(self.allocator);
            return null;
        }

        return try merged.toOwnedSlice(self.allocator);
    }

    pub fn cacheResult(self: *Self, query: []const u8, result: RoutingResult) !void {
        const h = try self.hashQuery(query);
        defer self.allocator.free(h);
        try self.l1_cache.put(h, result);
    }

    fn hashQuery(self: *Self, query: []const u8) ![]const u8 {
        return hashutil.hashString(self.allocator, query, .sha256);
    }

    fn persistSolution(self: *Self, query: []const u8, result: RoutingResult) !void {
        if (result.nodes.len == 0 and result.llm_response.len == 0) return;

        var summary_buf = std.ArrayListUnmanaged(u8).empty;
        defer summary_buf.deinit(self.allocator);
        if (result.llm_response.len > 0) {
            const max_len = @min(result.llm_response.len, 800);
            try summary_buf.appendSlice(self.allocator, result.llm_response[0..max_len]);
        } else {
            for (result.nodes, 0..) |node, i| {
                if (i > 0) try summary_buf.appendSlice(self.allocator, ", ");
                try summary_buf.appendSlice(self.allocator, node.content.lod[4]);
            }
        }

        const hash_bytes = try self.hashQuery(query);
        defer self.allocator.free(hash_bytes);
        var id_bytes: [8]u8 = undefined;
        @memcpy(&id_bytes, hash_bytes[0..8]);
        const solution_id: i64 = @bitCast(id_bytes);

        var node = try ContextNode.init(
            solution_id,
            query,
            summary_buf.items,
            self.allocator,
        );
        defer node.free(self.allocator);

        try self.library.exec("BEGIN");
        errdefer self.library.exec("ROLLBACK") catch {};
        self.library.insertNode(node) catch |err| {
            if (err != error.AlreadyExists) {
                self.library.exec("ROLLBACK") catch {};
                return;
            }
        };
        try self.library.exec("COMMIT");
    }
};
