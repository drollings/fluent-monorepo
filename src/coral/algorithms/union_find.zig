//! union_find.zig — Union-Find with path compression and union by size.
//!
//! Used by:
//! - Connected component detection (before Louvain community detection)
//! - Kruskal's MST (future)
//! - Deduplication during graph construction
//!
//! §Implementation:
//! - Path compression: `find()` flattens the tree in one traversal.
//! - Union by size: attaches smaller tree under larger tree root.
//! - Together these give amortised O(α(n)) per operation (near-constant).
//!
//! All memory is caller-managed via the provided allocator (typically an arena).

const std = @import("std");
const Allocator = std.mem.Allocator;

/// Manages disjoint-set operations with path compression and union by rank; owns data; ensures efficient merging and finding.
pub const UnionFind = struct {
    parent: []u32,
    size: []u32,
    /// Number of distinct components.
    components: u32,

    const Self = @This();

    /// Initialise `n` singleton components.
    pub fn init(allocator: Allocator, n: u32) !Self {
        const parent = try allocator.alloc(u32, n);
        const size = try allocator.alloc(u32, n);
        for (parent, 0..) |*p, i| p.* = @intCast(i);
        @memset(size, 1);
        return Self{ .parent = parent, .size = size, .components = n };
    }

    /// Free allocations.
    pub fn deinit(self: *Self, allocator: Allocator) void {
        allocator.free(self.parent);
        allocator.free(self.size);
    }

    /// Find the representative of the component containing `x`.
    /// Applies path compression.
    pub fn find(self: *Self, x: u32) u32 {
        var cur = x;
        // Path halving: faster in practice than full compression.
        while (self.parent[cur] != cur) {
            self.parent[cur] = self.parent[self.parent[cur]]; // path halving
            cur = self.parent[cur];
        }
        return cur;
    }

    /// Merge the components containing `a` and `b`.
    /// Returns true if they were in different components (a merge happened).
    pub fn @"union"(self: *Self, a: u32, b: u32) bool {
        const ra = self.find(a);
        const rb = self.find(b);
        if (ra == rb) return false;

        // Union by size: attach smaller to larger.
        if (self.size[ra] < self.size[rb]) {
            self.parent[ra] = rb;
            self.size[rb] += self.size[ra];
        } else {
            self.parent[rb] = ra;
            self.size[ra] += self.size[rb];
        }
        self.components -= 1;
        return true;
    }

    /// Return true if `a` and `b` are in the same component.
    pub fn connected(self: *Self, a: u32, b: u32) bool {
        return self.find(a) == self.find(b);
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "UnionFind: singleton init" {
    var uf = try UnionFind.init(testing.allocator, 5);
    defer uf.deinit(testing.allocator);

    try testing.expectEqual(@as(u32, 5), uf.components);
    for (0..5) |i| {
        try testing.expectEqual(@as(u32, @intCast(i)), uf.find(@intCast(i)));
    }
}

test "UnionFind: union merges components" {
    var uf = try UnionFind.init(testing.allocator, 4);
    defer uf.deinit(testing.allocator);

    try testing.expect(uf.@"union"(0, 1));
    try testing.expectEqual(@as(u32, 3), uf.components);
    try testing.expect(uf.connected(0, 1));
    try testing.expect(!uf.connected(0, 2));

    try testing.expect(uf.@"union"(2, 3));
    try testing.expectEqual(@as(u32, 2), uf.components);

    try testing.expect(uf.@"union"(1, 2));
    try testing.expectEqual(@as(u32, 1), uf.components);
    try testing.expect(uf.connected(0, 3));
}

test "UnionFind: duplicate union is idempotent" {
    var uf = try UnionFind.init(testing.allocator, 3);
    defer uf.deinit(testing.allocator);

    try testing.expect(uf.@"union"(0, 1));
    try testing.expect(!uf.@"union"(0, 1)); // already merged
    try testing.expectEqual(@as(u32, 2), uf.components);
}

test "UnionFind: path compression maintains correctness" {
    var uf = try UnionFind.init(testing.allocator, 6);
    defer uf.deinit(testing.allocator);

    // Build a chain: 0-1-2-3-4-5
    _ = uf.@"union"(0, 1);
    _ = uf.@"union"(1, 2);
    _ = uf.@"union"(2, 3);
    _ = uf.@"union"(3, 4);
    _ = uf.@"union"(4, 5);

    try testing.expectEqual(@as(u32, 1), uf.components);
    // After multiple finds, path should compress.
    const r0 = uf.find(0);
    const r5 = uf.find(5);
    try testing.expectEqual(r0, r5);
}

