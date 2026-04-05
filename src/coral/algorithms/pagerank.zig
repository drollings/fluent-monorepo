//! pagerank.zig — PageRank via power iteration (optional, CLI-only).
//!
//! Adapted from the classic power-iteration algorithm.
//!
//! §When to use:
//! PageRank is an optional ingestion-time algorithm.  It is NOT the default
//! importance metric (degree centrality is).  Use PageRank for datasets where
//! link topology matters more than raw connectivity:
//! - Citation networks (authority detection)
//! - Web-like graphs with strong hub/authority structure
//!
//! §Usage (CLI-only — never on query path):
//! ```
//! coral compute-pagerank [--damping 0.85] [--tolerance 0.0001] [--max-iter 20]
//! ```
//!
//! §Implementation:
//! Power iteration: `r' = (1-d)/n + d * A^T r`
//! where A is the column-normalised adjacency matrix and d is the damping factor.
//! We iterate until |r' - r|₁ < tolerance or max_iterations is reached.
//!
//! All intermediates are arena-allocated.

const std = @import("std");
const Allocator = std.mem.Allocator;
const CSRGraph = @import("csr_graph").CSRGraph;

/// Defines PageRank configuration parameters, manages configuration state, and enforces ownership model.
pub const PageRankConfig = struct {
    damping: f32 = 0.85,
    tolerance: f32 = 0.0001,
    max_iterations: u32 = 20,
};

/// Manages PageRank data structures, owns a fixed-size buffer, and ensures consistent state across operations.
pub const PageRank = struct {
    config: PageRankConfig,

    const Self = @This();

    pub fn init(config: PageRankConfig) Self {
        return Self{ .config = config };
    }

    /// Run PageRank on `graph` and return per-node scores.
    ///
    /// Returns an arena-owned slice of `node_count` f32 values.
    /// Scores are normalised so they sum to 1.0.
    pub fn run(self: Self, arena: Allocator, graph: *const CSRGraph) ![]f32 {
        const n = graph.node_count;
        if (n == 0) return arena.alloc(f32, 0);

        const d = self.config.damping;
        const teleport = (1.0 - d) / @as(f32, @floatFromInt(n));

        // Initial uniform distribution.
        var scores = try arena.alloc(f32, n);
        var next = try arena.alloc(f32, n);
        const init_val = 1.0 / @as(f32, @floatFromInt(n));
        @memset(std.mem.sliceAsBytes(scores), 0);
        @memset(std.mem.sliceAsBytes(next), 0);
        for (scores) |*s| s.* = init_val;

        // Pre-compute out-degree for dangling node handling.
        var out_deg = try arena.alloc(u32, n);
        for (0..n) |i| out_deg[i] = graph.degree(@intCast(i));

        var iter: u32 = 0;
        while (iter < self.config.max_iterations) : (iter += 1) {
            // Reset next iteration scores.
            for (next) |*x| x.* = teleport;

            // Dangling node mass (nodes with out-degree 0 redistribute uniformly).
            var dangling_sum: f32 = 0.0;
            for (0..n) |i| {
                if (out_deg[i] == 0) dangling_sum += scores[i];
            }
            const dangling_contrib = d * dangling_sum / @as(f32, @floatFromInt(n));

            // Distribute rank from each node.
            for (0..n) |i| {
                const od = out_deg[i];
                if (od == 0) continue;
                const contrib = d * scores[i] / @as(f32, @floatFromInt(od));
                for (graph.neighbors(@intCast(i))) |nb| {
                    next[nb] += contrib;
                }
            }

            // Add dangling contribution to all nodes.
            for (next) |*x| x.* += dangling_contrib;

            // Check convergence: L1 norm of delta.
            var delta: f32 = 0.0;
            for (0..n) |i| {
                delta += @abs(next[i] - scores[i]);
            }

            // Swap buffers.
            const tmp = scores;
            scores = next;
            next = tmp;

            if (delta < self.config.tolerance) break;
        }

        return scores;
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "PageRank: single-node graph" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    const offsets = [_]usize{ 0, 0 }; // no edges
    const targets = [_]u32{};
    const g = CSRGraph{
        .node_count = 1,
        .edge_count = 0,
        .offsets = &offsets,
        .targets = &targets,
        .weights = null,
    };

    const pr = PageRank.init(.{});
    const scores = try pr.run(a, &g);
    try testing.expectEqual(@as(usize, 1), scores.len);
    try testing.expectApproxEqAbs(@as(f32, 1.0), scores[0], 0.01);
}

test "PageRank: two-node cycle converges" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    // 0→1, 1→0 (symmetric cycle)
    const offsets = [_]usize{ 0, 1, 2 };
    const targets = [_]u32{ 1, 0 };
    const g = CSRGraph{
        .node_count = 2,
        .edge_count = 2,
        .offsets = &offsets,
        .targets = &targets,
        .weights = null,
    };

    const pr = PageRank.init(.{});
    const scores = try pr.run(a, &g);
    try testing.expectEqual(@as(usize, 2), scores.len);
    // Symmetric cycle → equal scores
    try testing.expectApproxEqAbs(scores[0], scores[1], 0.001);
    // Scores sum to ≈ 1.0
    try testing.expectApproxEqAbs(@as(f32, 1.0), scores[0] + scores[1], 0.01);
}

test "PageRank: star graph — center has highest rank" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    // Node 0 is hub; 1, 2, 3 all point to 0. 0 also points back to 1, 2, 3.
    // offsets: 0→[1,2,3], 1→[0], 2→[0], 3→[0]
    const offsets = [_]usize{ 0, 3, 4, 5, 6 };
    const targets = [_]u32{ 1, 2, 3, 0, 0, 0 };
    const g = CSRGraph{
        .node_count = 4,
        .edge_count = 6,
        .offsets = &offsets,
        .targets = &targets,
        .weights = null,
    };

    const pr = PageRank.init(.{});
    const scores = try pr.run(a, &g);
    try testing.expectEqual(@as(usize, 4), scores.len);
    // Node 0 (hub) should have higher rank than leaves.
    try testing.expect(scores[0] > scores[1]);
}
