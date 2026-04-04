//! shortest_path.zig — Dijkstra's shortest-path algorithm on CSRGraph.
//!
//! Finds minimum-cost paths through the graph using a binary min-heap as the
//! priority queue.  Supports:
//! - Weighted edges (via `CSRGraph.weights`)
//! - Unweighted graphs (treated as unit-weight)
//! - Optional `max_distance` cutoff for reachability queries
//!
//! §Integration with context packing:
//! Graph distance from Dijkstra feeds into `selectLodByDistance()` so that
//! nodes close to the query anchor (low distance) get more detail (lower LOD
//! index = more text) while distant nodes get summaries only.
//!
//! §Performance:
//! Standard O((V + E) log V) Dijkstra.  For unweighted graphs, BFS is faster
//! in theory but Dijkstra handles the weighted case without a separate path,
//! keeping the code simpler.
//!
//! All allocations use the provided arena; no cleanup beyond `arena.deinit()`.

const std = @import("std");
const Allocator = std.mem.Allocator;
const CSRGraph = @import("csr_graph").CSRGraph;

pub const INFINITY_DIST: f32 = std.math.floatMax(f32);

pub const PathResult = struct {
    node_idx: u32,
    distance: f32,
};

/// Tracks shortest paths in a graph; owns path data; ensures optimal updates.
pub const ShortestPath = struct {
    const Self = @This();

    /// Find the shortest path from `source` to `target`.
    /// Returns null if no path exists.
    /// Returns an arena-owned slice of node indices (inclusive of source and target).
    pub fn findPath(
        arena: Allocator,
        graph: *const CSRGraph,
        source: u32,
        target: u32,
    ) !?[]u32 {
        if (source >= graph.node_count or target >= graph.node_count) return null;
        if (source == target) {
            const path = try arena.alloc(u32, 1);
            path[0] = source;
            return path;
        }

        const dist = try runDijkstra(arena, graph, source, target);
        if (dist.distances[target] == INFINITY_DIST) return null;

        // Reconstruct path by following predecessors from target to source.
        var path: std.ArrayList(u32) = .empty;
        var cur = target;
        while (cur != source) {
            try path.append(arena, cur);
            cur = dist.predecessors[cur];
        }
        try path.append(arena, source);

        // Reverse to get source → target order.
        std.mem.reverse(u32, path.items);
        return @as(?[]u32, try path.toOwnedSlice(arena));
    }

    /// Find all nodes reachable from `source` within `max_distance`.
    /// Returns an arena-owned slice of PathResult, sorted by distance ascending.
    pub fn findAllReachable(
        arena: Allocator,
        graph: *const CSRGraph,
        source: u32,
        max_distance: f32,
    ) ![]PathResult {
        if (source >= graph.node_count) return arena.alloc(PathResult, 0);

        const dist = try runDijkstra(arena, graph, source, null);

        var results: std.ArrayList(PathResult) = .empty;
        for (dist.distances, 0..) |d, i| {
            if (d <= max_distance and i != source) {
                try results.append(arena, .{ .node_idx = @intCast(i), .distance = d });
            }
        }

        // Sort by distance ascending.
        std.mem.sort(PathResult, results.items, {}, struct {
            pub fn lessThan(_: void, a: PathResult, b: PathResult) bool {
                return a.distance < b.distance;
            }
        }.lessThan);

        return results.toOwnedSlice(arena);
    }
};

// ---------------------------------------------------------------------------
// Internal: Dijkstra core
// ---------------------------------------------------------------------------

const DijkstraResult = struct {
    distances: []f32,
    predecessors: []u32,
};

/// Manages heap memory allocations; owns a pool of fixed-size buffers; not thread-safe.
const HeapEntry = struct {
    node: u32,
    dist: f32,

    fn lessThan(_: void, a: HeapEntry, b: HeapEntry) std.math.Order {
        return std.math.order(a.dist, b.dist);
    }
};

/// Implements Dijkstra's algorithm to find shortest paths from source to target in a graph.
fn runDijkstra(
    arena: Allocator,
    graph: *const CSRGraph,
    source: u32,
    target: ?u32,
) !DijkstraResult {
    const n = graph.node_count;
    const distances = try arena.alloc(f32, n);
    const predecessors = try arena.alloc(u32, n);
    const visited = try arena.alloc(bool, n);

    for (distances) |*d| d.* = INFINITY_DIST;
    for (predecessors, 0..) |*p, i| p.* = @intCast(i);
    @memset(visited, false);
    distances[source] = 0.0;

    var pq = std.PriorityQueue(HeapEntry, void, HeapEntry.lessThan).init(arena, {});
    try pq.add(.{ .node = source, .dist = 0.0 });

    while (pq.removeOrNull()) |entry| {
        const u = entry.node;
        if (visited[u]) continue;
        visited[u] = true;

        // Early exit if we reached the target.
        if (target) |t| {
            if (u == t) break;
        }

        const base_dist = distances[u];
        const nbrs = graph.neighbors(u);
        for (nbrs, 0..) |v, k| {
            if (visited[v]) continue;
            const edge_w: f32 = if (graph.weights) |w| w[graph.offsets[u] + k] else 1.0;
            const new_dist = base_dist + edge_w;
            if (new_dist < distances[v]) {
                distances[v] = new_dist;
                predecessors[v] = u;
                try pq.add(.{ .node = v, .dist = new_dist });
            }
        }
    }

    return DijkstraResult{ .distances = distances, .predecessors = predecessors };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "ShortestPath.findPath: same node returns single-element path" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    const offsets = [_]usize{ 0, 0 };
    const g = CSRGraph{ .node_count = 1, .edge_count = 0, .offsets = &offsets, .targets = &[_]u32{}, .weights = null };
    const path = (try ShortestPath.findPath(a, &g, 0, 0)).?;
    try testing.expectEqual(@as(usize, 1), path.len);
    try testing.expectEqual(@as(u32, 0), path[0]);
}

test "ShortestPath.findPath: simple chain" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    // 0→1→2→3
    const offsets = [_]usize{ 0, 1, 2, 3, 3 };
    const targets = [_]u32{ 1, 2, 3 };
    const g = CSRGraph{ .node_count = 4, .edge_count = 3, .offsets = &offsets, .targets = &targets, .weights = null };

    const path = (try ShortestPath.findPath(a, &g, 0, 3)).?;
    try testing.expectEqual(@as(usize, 4), path.len);
    try testing.expectEqual(@as(u32, 0), path[0]);
    try testing.expectEqual(@as(u32, 3), path[3]);
}

test "ShortestPath.findPath: no path returns null" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    // Two disconnected nodes.
    const offsets = [_]usize{ 0, 0, 0 };
    const g = CSRGraph{ .node_count = 2, .edge_count = 0, .offsets = &offsets, .targets = &[_]u32{}, .weights = null };

    try testing.expect((try ShortestPath.findPath(a, &g, 0, 1)) == null);
}

test "ShortestPath.findAllReachable: within max_distance" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    // 0→1 (w=1), 0→2 (w=5), 1→3 (w=1)
    const offsets = [_]usize{ 0, 2, 3, 3, 3 };
    const targets = [_]u32{ 1, 2, 3 };
    const weights = [_]f32{ 1.0, 5.0, 1.0 };
    const g = CSRGraph{ .node_count = 4, .edge_count = 3, .offsets = &offsets, .targets = &targets, .weights = &weights };

    // Within distance 2.5: can reach nodes 1 (dist=1) and 3 (dist=2).
    const results = try ShortestPath.findAllReachable(a, &g, 0, 2.5);
    try testing.expectEqual(@as(usize, 2), results.len);
    try testing.expectApproxEqAbs(@as(f32, 1.0), results[0].distance, 0.001);
    try testing.expectApproxEqAbs(@as(f32, 2.0), results[1].distance, 0.001);
}

test "ShortestPath.findPath: weighted shortest path" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    // 0→1 (w=10), 0→2 (w=1), 2→1 (w=1) → shortest 0→2→1 (cost 2)
    const offsets = [_]usize{ 0, 2, 2, 3 };
    const targets = [_]u32{ 1, 2, 1 };
    const weights = [_]f32{ 10.0, 1.0, 1.0 };
    const g = CSRGraph{ .node_count = 3, .edge_count = 3, .offsets = &offsets, .targets = &targets, .weights = &weights };

    const path = (try ShortestPath.findPath(a, &g, 0, 1)).?;
    // Should take 0→2→1 (cheaper path)
    try testing.expectEqual(@as(usize, 3), path.len);
    try testing.expectEqual(@as(u32, 0), path[0]);
    try testing.expectEqual(@as(u32, 2), path[1]);
    try testing.expectEqual(@as(u32, 1), path[2]);
}
