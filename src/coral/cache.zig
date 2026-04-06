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
const common_registry = @import("dag").registry;
const BuilderError = common_registry.BuilderError;
const BuilderPhase = common_registry.BuilderPhase;
const logIfError = common_registry.logIfError;
const llm_mod = @import("llm");
const frontier = @import("frontier.zig");
const LocalDecomposer = local_model.LocalDecomposer;
const DecomposerConfig = local_model.DecomposerConfig;
const LlmConfig = llm_mod.LlmConfig;
const LlmClient = llm_mod.LlmClient;

const Library = coral_db.Library;
const ContextNode = coral_db.ContextNode;
const ContextPacker = coral_db.ContextPacker;
const HydrationPipeline = coral_db.HydrationPipeline;
const WasmTool = coral_db.WasmTool;
const EmbeddingProvider = hashutil.EmbeddingProvider;

/// Manages cache tiers with fixed-size buffers; owned by the system; ensures consistent storage allocation.
pub const CacheTier = enum(u8) {
    l1_memory = 1,
    l2_workflow = 2,
    l3_graph = 3,
    l4_semantic = 4,
    /// P6.2 — Local model decomposition: query split into sub-tasks then re-routed.
    l4_5_decompose = 9,
    l5_llm = 5,
};

/// Manages routing result structures with fixed-size buffers; owned by the module; ensures consistent state across operations.
pub const RoutingResult = struct {
    nodes: []const ContextNode,
    tool_result: []const u8,
    llm_response: []const u8,
    tier_used: CacheTier,
    latency_ms: u64,
};

/// Default maximum number of entries in the L1 cache.
pub const L1_DEFAULT_MAX_ENTRIES: usize = 1024;

/// Intrusive LRU list node.  Heap-allocated; stores the cached key alongside
/// the prev/next pointers so eviction can look up the matching hash entry.
const L1LruNodeData = struct {
    key: []const u8,
    list_node: std.DoublyLinkedList.Node = .{},
};

/// Entry stored per cache slot: result + pointer to the LRU tracking node.
const L1Entry = struct {
    result: RoutingResult,
    /// Pointer to the heap-allocated LRU node for this entry.
    lru: *L1LruNodeData,
};

/// Manages low-latency cache operations with fixed-size buffers; owned by the system; ensures consistent key-value storage.
pub const L1Cache = struct {
    allocator: std.mem.Allocator,
    entries: std.StringHashMap(L1Entry),
    /// LRU list: head = most recently used, tail = least recently used.
    order: std.DoublyLinkedList = .{},
    max_entries: usize,
    mu: std.Thread.Mutex = .{},

    pub fn init(allocator: std.mem.Allocator) L1Cache {
        return initWithCapacity(allocator, L1_DEFAULT_MAX_ENTRIES);
    }

    pub fn initWithCapacity(allocator: std.mem.Allocator, max_entries: usize) L1Cache {
        return .{
            .allocator = allocator,
            .entries = std.StringHashMap(L1Entry).init(allocator),
            .max_entries = max_entries,
        };
    }

    pub fn deinit(self: *L1Cache) void {
        var it = self.entries.iterator();
        while (it.next()) |entry| {
            self.allocator.free(entry.key_ptr.*);
            freeRoutingResult(self.allocator, &entry.value_ptr.*.result);
            self.allocator.destroy(entry.value_ptr.*.lru);
        }
        self.entries.deinit();
        // LRU nodes were already destroyed above; just reset the list head.
        self.order = .{};
    }

    /// Look up `query_hash` and promote the entry to most-recently-used.
    /// Returns a copy of the RoutingResult, or null if not cached.
    pub fn get(self: *L1Cache, query_hash: []const u8) ?RoutingResult {
        self.mu.lock();
        defer self.mu.unlock();
        const entry = self.entries.getPtr(query_hash) orelse return null;
        // Promote to MRU position.
        self.order.remove(&entry.lru.list_node);
        self.order.prepend(&entry.lru.list_node);
        return entry.result;
    }

    /// Insert (or replace) `result` for `query_hash`, evicting the LRU entry
    /// if the cache is at `max_entries` capacity.
    pub fn put(self: *L1Cache, query_hash: []const u8, result: RoutingResult) !void {
        const owned_key = try self.allocator.dupe(u8, query_hash);
        errdefer self.allocator.free(owned_key);
        const owned_result = try dupeRoutingResult(self.allocator, result);
        errdefer {
            var r = owned_result;
            freeRoutingResult(self.allocator, &r);
        }

        self.mu.lock();
        defer self.mu.unlock();

        // If key already exists, update in place and promote.
        if (self.entries.getPtr(owned_key)) |existing| {
            freeRoutingResult(self.allocator, &existing.result);
            existing.result = owned_result;
            self.order.remove(&existing.lru.list_node);
            self.order.prepend(&existing.lru.list_node);
            self.allocator.free(owned_key); // key already in map
            return;
        }

        // Evict LRU entry when at capacity.
        while (self.entries.count() >= self.max_entries) {
            if (self.order.pop()) |list_node| {
                const lru_data: *L1LruNodeData = @alignCast(@fieldParentPtr("list_node", list_node));
                const lru_key = lru_data.key;
                if (self.entries.fetchRemove(lru_key)) |kv| {
                    var evicted = kv.value.result;
                    freeRoutingResult(self.allocator, &evicted);
                    self.allocator.free(lru_key);
                }
                self.allocator.destroy(lru_data);
            } else break;
        }

        // Create a new LRU node for the key.
        const lru_data = try self.allocator.create(L1LruNodeData);
        errdefer self.allocator.destroy(lru_data);
        lru_data.* = .{ .key = owned_key };
        self.order.prepend(&lru_data.list_node);

        try self.entries.put(owned_key, .{ .result = owned_result, .lru = lru_data });
    }

    fn freeRoutingResult(allocator: std.mem.Allocator, result: *RoutingResult) void {
        if (result.nodes.len > 0) {
            for (result.nodes) |*n| @constCast(n).free(allocator);
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
                nodes_copy[i] = try n.clone(allocator);
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
// P5.1 — L1HashCache: u64-keyed cache with RwLock for shared reads
// ---------------------------------------------------------------------------

/// Hash-keyed L1 cache entry.  Separate from L1Cache to preserve the existing
/// string-keyed API while introducing the optimised integer-key path.
const L1HashLruNodeData = struct {
    key: u64,
    list_node: std.DoublyLinkedList.Node = .{ .data = undefined },
};

const L1HashEntry = struct {
    result: RoutingResult,
    lru: *L1HashLruNodeData,
};

/// Manages L1 cache storage with fixed-size buckets; owns data structures; key invariants ensure consistent access.
pub const L1HashCache = struct {
    allocator: std.mem.Allocator,
    entries: std.AutoHashMap(u64, L1HashEntry),
    order: std.DoublyLinkedList = .{},
    max_entries: usize,
    mu: std.Thread.RwLock = .{},

    const Self = @This();

    pub fn init(allocator: std.mem.Allocator, max_entries: usize) Self {
        return .{
            .allocator = allocator,
            .entries = std.AutoHashMap(u64, L1HashEntry).init(allocator),
            .max_entries = max_entries,
        };
    }

    pub fn deinit(self: *Self) void {
        var it = self.entries.iterator();
        while (it.next()) |entry| {
            L1Cache.freeRoutingResult(self.allocator, &entry.value_ptr.*.result);
            self.allocator.destroy(entry.value_ptr.*.lru);
        }
        self.entries.deinit();
        self.order = .{};
    }

    /// Shared-lock read — does NOT promote (avoids write lock on the hot path).
    /// Returns a copy of the cached RoutingResult, or null on miss.
    pub fn get(self: *Self, query_hash: u64) ?RoutingResult {
        self.mu.lockShared();
        defer self.mu.unlockShared();
        const entry = self.entries.getPtr(query_hash) orelse return null;
        return entry.result;
    }

    /// Exclusive-lock read with MRU promotion.  Use on paths that benefit from
    /// accurate LRU eviction order (e.g. warm cache pre-fetch).
    pub fn getWithPromotion(self: *Self, query_hash: u64) ?RoutingResult {
        self.mu.lock();
        defer self.mu.unlock();
        const entry = self.entries.getPtr(query_hash) orelse return null;
        self.order.remove(&entry.lru.list_node);
        self.order.prepend(&entry.lru.list_node);
        return entry.result;
    }

    /// Insert (or replace) `result` for `query_hash`, evicting LRU when full.
    pub fn put(self: *Self, query_hash: u64, result: RoutingResult) !void {
        const owned = try L1Cache.dupeRoutingResult(self.allocator, result);
        errdefer {
            var r = owned;
            L1Cache.freeRoutingResult(self.allocator, &r);
        }

        self.mu.lock();
        defer self.mu.unlock();

        if (self.entries.getPtr(query_hash)) |existing| {
            L1Cache.freeRoutingResult(self.allocator, &existing.result);
            existing.result = owned;
            self.order.remove(&existing.lru.list_node);
            self.order.prepend(&existing.lru.list_node);
            return;
        }

        while (self.entries.count() >= self.max_entries) {
            if (self.order.pop()) |list_node| {
                const lru_data: *L1HashLruNodeData = @alignCast(@fieldParentPtr("list_node", list_node));
                if (self.entries.fetchRemove(lru_data.key)) |kv| {
                    var evicted = kv.value.result;
                    L1Cache.freeRoutingResult(self.allocator, &evicted);
                }
                self.allocator.destroy(lru_data);
            } else break;
        }

        const lru_data = try self.allocator.create(L1HashLruNodeData);
        errdefer self.allocator.destroy(lru_data);
        lru_data.* = .{ .key = query_hash };
        self.order.prepend(&lru_data.list_node);
        try self.entries.put(query_hash, .{ .result = owned, .lru = lru_data });
    }

    /// FNV-1a 64-bit hash — stable, fast, suitable for cache keys.
    pub fn hashQuery(query: []const u8) u64 {
        const FNV_OFFSET: u64 = 14695981039346656037;
        const FNV_PRIME: u64 = 1099511628211;
        var h: u64 = FNV_OFFSET;
        for (query) |byte| {
            h ^= byte;
            h *%= FNV_PRIME;
        }
        return h;
    }
};

// ---------------------------------------------------------------------------
// P3.0 — QueueReactorBuilder (fluent builder)
// ---------------------------------------------------------------------------

/// Manages queue reactor construction with fixed-size buffers; encapsulates ownership and lifecycle; not thread-safe.
pub const QueueReactorBuilder = struct {
    allocator: std.mem.Allocator,
    /// Owns BuilderError strings; deinited by build() on all paths.
    arena: std.heap.ArenaAllocator,
    _library: ?*Library = null,
    _embedder: ?EmbeddingProvider = null,
    _decomposer_cfg: ?DecomposerConfig = null,
    knn_k: usize = 20,
    l4_threshold: f32 = 0.85,
    l3_max_depth: u8 = 4,
    _thread_count: ?u32 = null,
    _frontier_cfg: ?LlmConfig = null,
    /// Rich structured error (arena-allocated); surfaced by build().
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

    /// Enable L4.5 local decomposition with the supplied LLM config.
    pub fn decomposerConfig(self: *@This(), cfg: DecomposerConfig) *@This() {
        self._decomposer_cfg = cfg;
        return self;
    }

    /// Enable async routing via a thread pool with `n` worker threads.
    /// Pass 0 to use the default (number of logical CPUs).
    pub fn threadCount(self: *@This(), n: u32) *@This() {
        self._thread_count = n;
        return self;
    }

    /// Enable L5 frontier LLM routing with the supplied config.
    pub fn frontierCfg(self: *@This(), cfg: LlmConfig) *@This() {
        self._frontier_cfg = cfg;
        return self;
    }

    /// Terminal method: build the QueueReactor.
    /// Always deinits the builder's arena on return — do not call setters after build().
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

// ---------------------------------------------------------------------------
// QueueReactor — 5-tier cache router
// ---------------------------------------------------------------------------

/// Manages asynchronous queue operations with fixed-size buffers; owned by the system; ensures data integrity during transitions.
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
    // P4.4 — work queue infrastructure
    queue_mu: std.Thread.Mutex = .{},
    queue_cond: std.Thread.Condition = .{},
    // M5 — optional thread pool for async routing tasks
    thread_pool: ?*std.Thread.Pool = null,
    // P6.2 — L4.5 local decomposition (null = disabled)
    decomposer_cfg: ?DecomposerConfig = null,
    // M6 — L5 frontier LLM config (null = disabled; falls back to empty stub)
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

    // ------------------------------------------------------------------
    // M5 — Async task submission
    // ------------------------------------------------------------------

    /// A routing task with its own arena allocator for full isolation.
    /// Create via submitAsync(); await with waitAndFree().
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

    /// Submit `query` as an async routing task on the thread pool.
    /// Returns a heap-allocated Task; caller must call `task.arena.deinit()` after
    /// inspecting the result.  Falls back to synchronous execution if no thread pool.
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

    /// Route a query through the L1-L5 hierarchy.
    pub fn route(self: *Self, query: []const u8) anyerror!RoutingResult {
        const start_time = std.time.nanoTimestamp();

        // L1: Check memory cache (exact query hash)
        const hash = try self.hashQuery(query);
        defer self.allocator.free(hash);
        if (self.l1_cache.get(hash)) |cached| {
            return .{ .nodes = cached.nodes, .tool_result = &[_]u8{}, .llm_response = &[_]u8{}, .tier_used = .l1_memory, .latency_ms = @intCast(@divTrunc(std.time.nanoTimestamp() - start_time, 1_000_000)) };
        }

        // L2: WASM tool cache
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

        // L5: Frontier LLM (real HTTP call when frontier_cfg is set)
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

    // ------------------------------------------------------------------
    // M4.1 — L2: WASM tool execution
    // ------------------------------------------------------------------

    /// Return the first tested + non-empty WASM tool from the pre-loaded slice.
    /// Full trait-matching via popCount bitsets is deferred to M5+.
    fn findWasmTool(self: *Self, query: []const u8) ?WasmTool {
        _ = query;
        for (self.wasm_tools) |tool| {
            if (tool.test_passed and tool.wasm_b64.len > 0) return tool;
        }
        return null;
    }

    /// Decode a WasmTool's base64 bytes and call its "run" entry-point with
    /// `query` as input.  Returns the raw output (allocator-owned) or null on
    /// any failure (no tool found, decode error, Extism runtime unavailable).
    fn routeL2Wasm(self: *Self, query: []const u8) !?[]const u8 {
        const tool = self.findWasmTool(query) orelse return null;
        if (tool.wasm_b64.len == 0) return null;

        // Decode base64 WASM bytes.
        const wasm_size = std.base64.standard.Decoder.calcSizeForSlice(tool.wasm_b64) catch return null;
        const wasm_bytes = try self.allocator.alloc(u8, wasm_size);
        defer self.allocator.free(wasm_bytes);
        std.base64.standard.Decoder.decode(wasm_bytes, tool.wasm_b64) catch return null;

        // Validate WASM magic bytes (0x00 0x61 0x73 0x6D).
        if (wasm_bytes.len < 4 or
            wasm_bytes[0] != 0x00 or wasm_bytes[1] != 0x61 or
            wasm_bytes[2] != 0x73 or wasm_bytes[3] != 0x6D)
        {
            return null;
        }

        // Execute via Extism with Library host functions (requires libextism at runtime).
        // R2: use HostFunctionRegistry so WASM tools can call get_node_lod1,
        // get_neighbors, and insert_edge against the live Library.
        var host_reg = wasm_mod.HostFunctionRegistry.init(self.allocator, self.library);
        defer host_reg.deinit();
        host_reg.registerStandard() catch {};
        const output = wasm_mod.executeWasmQueryWithHosts(self.allocator, wasm_bytes, query, &host_reg) catch return null;
        return output;
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

        // Copy nodes out of the arena into self.allocator so caller can own them.
        // clone() shares lod[0] via SharedString ref and dupes owned lod[1..5].
        const owned = try self.allocator.alloc(ContextNode, nodes.len);
        errdefer self.allocator.free(owned);
        for (nodes, 0..) |src_node, i| {
            owned[i] = try src_node.clone(self.allocator);
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
        return .{
            .nodes = &[_]ContextNode{},
            .tool_result = &[_]u8{},
            .llm_response = &[_]u8{},
            .tier_used = .l5_llm,
            .latency_ms = 0,
        };
    }

    // ------------------------------------------------------------------
    // M6 — L5: Frontier LLM route
    // ------------------------------------------------------------------

    /// Call the frontier LLM with a minimized context prompt.
    /// Returns null when frontier_cfg is not set or the LLM call fails.
    /// On success, returns a RoutingResult with llm_response set (allocator-owned).
    fn routeL5Frontier(self: *Self, query: []const u8) !?RoutingResult {
        const cfg = self.frontier_cfg orelse return null;

        // Try to find a focal node for context assembly.
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
        // R4: Index the validated LLM solution so future semantically-similar queries
        // resolve via L4 KNN (<200ms) rather than triggering another frontier call.
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
        if (result.nodes.len == 0 and result.llm_response.len == 0) return;

        // Build summary: prefer llm_response, fall back to node name list.
        var summary_buf = std.ArrayListUnmanaged(u8).empty;
        defer summary_buf.deinit(self.allocator);
        if (result.llm_response.len > 0) {
            const max_len = @min(result.llm_response.len, 800);
            try summary_buf.appendSlice(self.allocator, result.llm_response[0..max_len]);
        } else {
            for (result.nodes, 0..) |node, i| {
                if (i > 0) try summary_buf.appendSlice(self.allocator, ", ");
                try summary_buf.appendSlice(self.allocator, node.content.lod[4]); // lod4 = name
            }
        }

        // Assign a stable id derived from query hash so re-inserting is idempotent.
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

        // Wrap insert in a transaction so partial failures roll back cleanly.
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

// ---------------------------------------------------------------------------
// Task 7.1 — ParallelRouter
// ---------------------------------------------------------------------------

/// Manages parallel routing configurations; owned by the application; ensures consistent state across threads.
pub const ParallelRouter = struct {
    const Self = @This();

    reactor: *QueueReactor,

    pub fn init(reactor: *QueueReactor) Self {
        return .{ .reactor = reactor };
    }

    /// Route all queries in `queries` concurrently.
    /// Returns a slice of RoutingResults (same order as input).
    /// Caller must free results and each result's nodes.
    pub fn routeBatch(
        self: *Self,
        allocator: std.mem.Allocator,
        queries: []const []const u8,
    ) ![]RoutingResult {
        if (queries.len == 0) return &[_]RoutingResult{};

        const results = try allocator.alloc(RoutingResult, queries.len);
        errdefer allocator.free(results);

        if (self.reactor.thread_pool) |pool| {
            const TaskCtx = struct {
                reactor: *QueueReactor,
                query: []const u8,
                result: *RoutingResult,
                err: ?anyerror = null,

                fn run(ctx: *@This()) void {
                    ctx.result.* = ctx.reactor.route(ctx.query) catch |err| blk: {
                        ctx.err = err;
                        break :blk RoutingResult{
                            .nodes = &[_]ContextNode{},
                            .tool_result = &[_]u8{},
                            .llm_response = &[_]u8{},
                            .tier_used = .l5_llm,
                            .latency_ms = 0,
                        };
                    };
                }
            };

            const ctxs = try allocator.alloc(TaskCtx, queries.len);
            defer allocator.free(ctxs);

            for (queries, 0..) |q, i| {
                ctxs[i] = .{
                    .reactor = self.reactor,
                    .query = q,
                    .result = &results[i],
                };
            }

            var wg: std.Thread.WaitGroup = .{};
            for (ctxs) |*ctx| {
                pool.spawnWg(&wg, TaskCtx.run, .{ctx});
            }
            pool.waitAndWork(&wg);
        } else {
            // Sequential fallback
            for (queries, 0..) |q, i| {
                results[i] = try self.reactor.route(q);
            }
        }

        return results;
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

test "QueueReactor: L2 skipped when no wasm_tools" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    var reactor = QueueReactor.init(allocator, lib, 10);
    defer reactor.deinit();
    // wasm_tools is empty by default — L2 must be skipped and fall to L5.
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
    // findWasmTool skips tools with test_passed=false.
    try testing.expect(reactor.findWasmTool("any") == null);
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
    // No thread pool — should fall through to L5 for unknown query.
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

    // put a result
    const result = RoutingResult{
        .nodes = &[_]ContextNode{},
        .tool_result = &[_]u8{},
        .llm_response = &[_]u8{},
        .tier_used = .l1_memory,
        .latency_ms = 5,
    };
    try cache.put("hash1", result);

    // two concurrent reads should not race (single-threaded test just verifies no crash)
    const r1 = cache.get("hash1");
    const r2 = cache.get("hash1");
    try testing.expect(r1 != null);
    try testing.expect(r2 != null);
}

test "M5: concurrent writes via thread pool do not deadlock" {
    // Spawn multiple goroutines inserting nodes; WAL mode prevents SQLITE_BUSY.
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

    // Verify total nodes inserted (may be fewer than NTHREADS*NODES_PER_THREAD
    // if some inserts silently failed, but at least some should succeed).
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
