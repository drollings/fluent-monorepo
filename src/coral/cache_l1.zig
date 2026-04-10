//! cache_l1.zig — L1/L1Hash Cache Types
//!
//! L1: Memory Cache (<10ms exact query hash → pre-rendered ContextNodes)
//! L1Hash: u64-keyed cache with RwLock for shared reads.
const std = @import("std");
const coral_db = @import("coral_db");
const hashutil = @import("common");

const Library = coral_db.Library;
const ContextNode = coral_db.ContextNode;
const EmbeddingProvider = hashutil.EmbeddingProvider;

pub const CacheTier = enum(u8) {
    l1_memory = 1,
    l2_workflow = 2,
    l3_graph = 3,
    l4_semantic = 4,
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

pub const L1_DEFAULT_MAX_ENTRIES: usize = 1024;

const L1LruNodeData = struct {
    key: []const u8,
    list_node: std.DoublyLinkedList.Node = .{},
};

const L1Entry = struct {
    result: RoutingResult,
    lru: *L1LruNodeData,
};

pub const L1Cache = struct {
    allocator: std.mem.Allocator,
    entries: std.StringHashMap(L1Entry),
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
        self.order = .{};
    }

    pub fn get(self: *L1Cache, query_hash: []const u8) ?RoutingResult {
        self.mu.lock();
        defer self.mu.unlock();
        const entry = self.entries.getPtr(query_hash) orelse return null;
        self.order.remove(&entry.lru.list_node);
        self.order.prepend(&entry.lru.list_node);
        return entry.result;
    }

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

        if (self.entries.getPtr(owned_key)) |existing| {
            freeRoutingResult(self.allocator, &existing.result);
            existing.result = owned_result;
            self.order.remove(&existing.lru.list_node);
            self.order.prepend(&existing.lru.list_node);
            self.allocator.free(owned_key);
            return;
        }

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

const L1HashLruNodeData = struct {
    key: u64,
    list_node: std.DoublyLinkedList.Node = .{ .data = undefined },
};

const L1HashEntry = struct {
    result: RoutingResult,
    lru: *L1HashLruNodeData,
};

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

    pub fn get(self: *Self, query_hash: u64) ?RoutingResult {
        self.mu.lockShared();
        defer self.mu.unlockShared();
        const entry = self.entries.getPtr(query_hash) orelse return null;
        return entry.result;
    }

    pub fn getWithPromotion(self: *Self, query_hash: u64) ?RoutingResult {
        self.mu.lock();
        defer self.mu.unlock();
        const entry = self.entries.getPtr(query_hash) orelse return null;
        self.order.remove(&entry.lru.list_node);
        self.order.prepend(&entry.lru.list_node);
        return entry.result;
    }

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

    pub fn hashQuery(query: []const u8) u64 {
        return hashutil.hash.fnv1a64(query);
    }
};
