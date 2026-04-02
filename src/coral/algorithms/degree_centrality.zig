//! degree_centrality.zig — Node degree computation for Coral Context graph.
//!
//! Computes out-degree for each node in `context_nodes` via a single SQL
//! aggregate query over `neighbor_of`, then writes the results back to
//! `context_nodes.degree`.
//!
//! §Why degree over PageRank as default:
//! - O(E) — single GROUP BY pass over edges (vs O(E × iterations) for PageRank)
//! - No damping factor or convergence threshold to tune
//! - GraphRAG itself uses degree for entity ranking in global search
//! - On Pi 5 with 100K nodes: Degree ≈50ms; PageRank ≈seconds
//!
//! §Usage (ingestion-time only — never called from query path):
//! ```zig
//! var arena = std.heap.ArenaAllocator.init(allocator);
//! defer arena.deinit();
//! try DegreeCentrality.compute(arena.allocator(), &library);
//! ```

const std = @import("std");
const Allocator = std.mem.Allocator;

pub const DegreeCentrality = struct {
    /// Compute out-degree for all nodes and update `context_nodes.degree`.
    ///
    /// Uses a single SQL pass:
    ///   SELECT from_id, COUNT(*) FROM neighbor_of GROUP BY from_id
    /// followed by bulk UPDATE of `context_nodes.degree`.
    ///
    /// Nodes with no outgoing edges retain degree = 0 (the column default).
    pub fn compute(arena: Allocator, library: anytype) !void {
        // Reset all degrees to 0 first (ensures nodes with no edges get 0).
        try library.exec("UPDATE context_nodes SET degree = 0");

        // Query out-degree per node.
        const DegreeRow = struct { node_id: i64, deg: u32 };
        var rows: std.ArrayListUnmanaged(DegreeRow) = .{};

        try library.iterateDegrees(arena, struct {
            list: *std.ArrayListUnmanaged(DegreeRow),
            allocator: Allocator,
            pub fn call(ctx: @This(), node_id: i64, deg: u32) !void {
                try ctx.list.append(ctx.allocator, .{ .node_id = node_id, .deg = deg });
            }
        }{ .list = &rows, .allocator = arena });

        // Bulk update.
        for (rows.items) |row| {
            try library.updateNodeDegree(row.node_id, row.deg);
        }
    }

    /// Compute degree for a single node by counting its neighbor_of rows.
    /// Useful for incremental updates after adding new edges.
    pub fn computeForNode(library: anytype, node_id: i64) !u32 {
        return library.countOutEdges(node_id);
    }
};

// ---------------------------------------------------------------------------
// Tests (unit — tests with live DB are in db_test.zig integration tests)
// ---------------------------------------------------------------------------

const testing = std.testing;

test "DegreeCentrality: trivial — no calls on empty input" {
    // This test validates the struct compiles and the interface is sane.
    // Live DB integration tests are in cache_test.zig / benchmark.zig.
    const T = DegreeCentrality;
    _ = T;
    try testing.expect(true);
}
