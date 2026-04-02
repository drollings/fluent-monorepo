//! edge_weights.zig — Co-occurrence edge weight computation.
//!
//! Computes edge weights for `neighbor_of` edges using a simplified PMI-inspired
//! co-occurrence formula:
//!
//!   weight(a, b) = cooccurrence(a, b) / sqrt(degree(a) * degree(b))
//!
//! This normalises by the geometric mean of node degrees, so edges between
//! high-degree nodes are not artificially inflated.
//!
//! §Usage (ingestion-time only — never called from query path):
//! ```zig
//! try EdgeWeights.compute(arena.allocator(), &library);
//! ```
//!
//! §Notes:
//! - Assumes `context_nodes.degree` has already been populated (run DegreeCentrality first).
//! - Edge weights are stored in `neighbor_of.weight` for use by Dijkstra and Louvain.
//! - Edges with zero weight (degree product is zero) are assigned weight 1.0 (fallback).

const std = @import("std");
const Allocator = std.mem.Allocator;

pub const EdgeWeights = struct {
    /// Compute and store edge weights for all `neighbor_of` edges.
    ///
    /// Requires `context_nodes.degree` to be populated (run DegreeCentrality first).
    /// Updates `neighbor_of.weight` in-place for every edge.
    pub fn compute(arena: Allocator, library: anytype) !void {
        // Iterate all edges; for each, fetch from/to degrees and compute weight.
        try library.iterateNeighborOfWithDegrees(arena, struct {
            lib: @TypeOf(library),
            pub fn call(ctx: @This(), from_id: i64, to_id: i64, from_deg: u32, to_deg: u32) !void {
                const weight = computeWeight(from_deg, to_deg);
                try ctx.lib.updateEdgeWeight(from_id, to_id, weight, null);
            }
        }{ .lib = library });
    }

    /// Compute the PMI-inspired weight for a single edge.
    pub fn computeWeight(from_deg: u32, to_deg: u32) f32 {
        const a = @as(f32, @floatFromInt(from_deg));
        const b = @as(f32, @floatFromInt(to_deg));
        const product = a * b;
        if (product <= 0.0) return 1.0; // Fallback for dangling nodes
        return 1.0 / @sqrt(product);
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "EdgeWeights.computeWeight: symmetric" {
    // weight(a, b) == weight(b, a)
    const w1 = EdgeWeights.computeWeight(4, 9);
    const w2 = EdgeWeights.computeWeight(9, 4);
    try testing.expectApproxEqAbs(w1, w2, 0.0001);
}

test "EdgeWeights.computeWeight: unit degrees give weight 1.0" {
    const w = EdgeWeights.computeWeight(1, 1);
    try testing.expectApproxEqAbs(@as(f32, 1.0), w, 0.0001);
}

test "EdgeWeights.computeWeight: zero degree falls back to 1.0" {
    const w = EdgeWeights.computeWeight(0, 5);
    try testing.expectApproxEqAbs(@as(f32, 1.0), w, 0.0001);
}

test "EdgeWeights.computeWeight: higher degree gives lower weight" {
    const w_low = EdgeWeights.computeWeight(1, 1);
    const w_high = EdgeWeights.computeWeight(100, 100);
    try testing.expect(w_high < w_low);
}

test "EdgeWeights.computeWeight: matches formula" {
    // weight(4, 9) = 1 / sqrt(36) = 1/6 ≈ 0.1667
    const w = EdgeWeights.computeWeight(4, 9);
    try testing.expectApproxEqAbs(@as(f32, 1.0 / 6.0), w, 0.0001);
}
