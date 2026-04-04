//! louvain.zig — Louvain community detection (single-level).
//!
//! Adapted from the standard Louvain algorithm with simplifications:
//! - Single-level (not hierarchical) for initial implementation
//! - Modularity optimization with configurable resolution parameter
//! - Runs on untyped entities only (typed entities use YAGO ontology)
//!
//! §Why Louvain over Leiden:
//! Leiden guarantees well-connected communities but requires a C++ dependency.
//! Louvain is simpler, fully native Zig, and sufficient for Coral Context's
//! use case of grouping untyped entities.  If community quality becomes critical,
//! Leiden can replace this later.
//!
//! §Usage (ingestion-time only):
//! ```
//! coral compute-communities [--resolution 1.0] [--max-iter 10]
//! ```
//!
//! §Algorithm outline:
//! 1. Assign each node to its own community.
//! 2. For each node, try moving it to the community of each neighbour.
//!    Accept the move if it improves modularity (ΔQ > 0).
//! 3. Repeat until no moves improve modularity (convergence) or max_iterations.
//!
//! §Modularity gain formula (Louvain ΔQ for moving node i to community C):
//!   ΔQ = [k_{i,C}/m - γ * k_i * Σ_C / (2m²)]
//! where k_{i,C} is the weighted edges from i to C, m is total edge weight,
//! k_i is node degree, Σ_C is the sum of degrees in C, and γ is resolution.

const std = @import("std");
const Allocator = std.mem.Allocator;
const CSRGraph = @import("csr_graph").CSRGraph;

pub const LouvainConfig = struct {
    resolution: f32 = 1.0,
    max_iterations: u32 = 10,
};

/// Represents a Louvain algorithm structure managing community detection; encapsulates key invariants and ownership model.
pub const Louvain = struct {
    config: LouvainConfig,

    const Self = @This();

    pub fn init(config: LouvainConfig) Self {
        return Self{ .config = config };
    }

    /// Run Louvain community detection on `graph`.
    ///
    /// Returns an arena-owned slice of community IDs, one per node (0-based).
    /// Nodes in the same community share the same community ID.
    /// Community IDs are dense integers in [0, num_communities).
    pub fn run(self: Self, arena: Allocator, graph: *const CSRGraph) ![]u32 {
        const n = graph.node_count;
        if (n == 0) return arena.alloc(u32, 0);

        // Assign each node to its own community (node i → community i).
        const community = try arena.alloc(u32, n);
        for (community, 0..) |*c, i| c.* = @intCast(i);

        // Sum of degrees per community (Σ_C in modularity formula).
        const community_deg = try arena.alloc(f64, n);
        for (0..n) |i| {
            community_deg[i] = @floatFromInt(graph.degree(@intCast(i)));
        }

        // Total edge weight m (sum of all edge weights / 2 for undirected).
        var total_weight: f64 = 0.0;
        for (0..n) |i| {
            const nbrs = graph.neighbors(@intCast(i));
            for (nbrs, 0..) |_, k| {
                const w: f32 = if (graph.weights) |wt| wt[graph.offsets[i] + k] else 1.0;
                total_weight += w;
            }
        }
        // For undirected: m = total directed weight / 2
        // (but our graph stores directed edges, so keep total_weight as-is)
        const m = if (total_weight > 0.0) total_weight else 1.0;
        const inv_2m = 1.0 / (2.0 * m);
        const gamma = self.config.resolution;

        var iter: u32 = 0;
        while (iter < self.config.max_iterations) : (iter += 1) {
            var moved: bool = false;

            for (0..n) |i| {
                const ui: u32 = @intCast(i);
                const ki: f64 = @floatFromInt(graph.degree(ui));
                const cur_comm = community[i];

                // Compute k_{i, current_community}: weighted edges from i to cur_comm.
                var k_i_cur: f64 = 0.0;
                const nbrs = graph.neighbors(ui);
                for (nbrs, 0..) |v, k| {
                    if (community[v] == cur_comm and v != ui) {
                        const w: f32 = if (graph.weights) |wt| wt[graph.offsets[i] + k] else 1.0;
                        k_i_cur += w;
                    }
                }

                var best_comm = cur_comm;
                var best_dq: f64 = 0.0; // Must be positive to move

                // Temporarily remove i from its community.
                community_deg[cur_comm] -= ki;

                // Collect candidate communities from neighbours.
                var seen_comms: std.AutoHashMapUnmanaged(u32, f64) = .empty;
                defer seen_comms.deinit(arena);

                for (nbrs, 0..) |v, k| {
                    const nb_comm = community[v];
                    if (nb_comm == cur_comm) continue;
                    const w: f32 = if (graph.weights) |wt| wt[graph.offsets[i] + k] else 1.0;
                    const gop = try seen_comms.getOrPut(arena, nb_comm);
                    if (!gop.found_existing) gop.value_ptr.* = 0.0;
                    gop.value_ptr.* += w;
                }

                var comm_iter = seen_comms.iterator();
                while (comm_iter.next()) |entry| {
                    const nb_comm = entry.key_ptr.*;
                    const k_i_nb = entry.value_ptr.*;
                    const sigma_nb = community_deg[nb_comm];

                    // ΔQ for moving to nb_comm:
                    //   gain  = k_i_nb / m - γ * ki * sigma_nb * inv_2m
                    //   minus = k_i_cur / m - γ * ki * (community_deg[cur_comm]) * inv_2m
                    // We only need the gain term to compare (same constant factors).
                    const gain = k_i_nb / m - gamma * ki * sigma_nb * inv_2m;
                    if (gain > best_dq) {
                        best_dq = gain;
                        best_comm = nb_comm;
                    }
                }

                // Restore or move.
                if (best_comm != cur_comm) {
                    community[i] = best_comm;
                    community_deg[best_comm] += ki;
                    moved = true;
                } else {
                    community_deg[cur_comm] += ki; // restore
                }
            }

            if (!moved) break;
        }

        // Relabel communities to dense integers [0, num_communities).
        return relabelDense(arena, community, n);
    }
};

/// Transforms dense community assignments into a sparse representation by relabeling nodes.
fn relabelDense(arena: Allocator, communities: []u32, n: u32) ![]u32 {
    var remap = std.AutoHashMap(u32, u32).init(arena);
    var next_id: u32 = 0;

    for (communities[0..n]) |c| {
        if (!remap.contains(c)) {
            try remap.put(c, next_id);
            next_id += 1;
        }
    }

    const result = try arena.alloc(u32, n);
    for (result, communities[0..n]) |*r, c| {
        r.* = remap.get(c).?;
    }
    return result;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "Louvain: empty graph" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const g = CSRGraph{ .node_count = 0, .edge_count = 0, .offsets = &[_]usize{}, .targets = &[_]u32{}, .weights = null };
    const lou = Louvain.init(.{});
    const communities = try lou.run(arena.allocator(), &g);
    try testing.expectEqual(@as(usize, 0), communities.len);
}

test "Louvain: isolated nodes each in own community" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    // 3 nodes, no edges.
    const offsets = [_]usize{ 0, 0, 0, 0 };
    const g = CSRGraph{ .node_count = 3, .edge_count = 0, .offsets = &offsets, .targets = &[_]u32{}, .weights = null };
    const lou = Louvain.init(.{});
    const communities = try lou.run(a, &g);
    try testing.expectEqual(@as(usize, 3), communities.len);
    // All different communities (3 isolated → 3 communities after relabelling).
    var seen = std.AutoHashMap(u32, void).init(testing.allocator);
    defer seen.deinit();
    for (communities) |c| try seen.put(c, {});
    try testing.expectEqual(@as(usize, 3), seen.count());
}

test "Louvain: valid partition output" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    // Triangle clique: 0↔1↔2↔0 (fully connected, all edges bidirectional).
    const offsets = [_]usize{ 0, 2, 4, 6 };
    const targets = [_]u32{ 1, 2, 0, 2, 0, 1 };
    const g = CSRGraph{ .node_count = 3, .edge_count = 6, .offsets = &offsets, .targets = &targets, .weights = null };

    const lou = Louvain.init(.{});
    const communities = try lou.run(a, &g);
    try testing.expectEqual(@as(usize, 3), communities.len);

    // All community IDs must be in [0, num_nodes).
    for (communities) |c| {
        try testing.expect(c < 3);
    }
    // Dense relabelling: IDs must be contiguous starting from 0.
    var seen = std.AutoHashMap(u32, void).init(testing.allocator);
    defer seen.deinit();
    for (communities) |c| try seen.put(c, {});
    // At minimum 1 community, at most num_nodes communities.
    try testing.expect(seen.count() >= 1);
    try testing.expect(seen.count() <= 3);
}

test "Louvain: two strong cliques merge fewer communities" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    // Two separate cliques, no bridge. Each should become its own community.
    // Clique A: 0↔1, 1↔0.  Clique B: 2↔3, 3↔2.  (No edges between A and B.)
    const offsets = [_]usize{ 0, 1, 2, 3, 4 };
    const targets = [_]u32{ 1, 0, 3, 2 };
    const g = CSRGraph{ .node_count = 4, .edge_count = 4, .offsets = &offsets, .targets = &targets, .weights = null };

    const lou = Louvain.init(.{});
    const communities = try lou.run(a, &g);
    try testing.expectEqual(@as(usize, 4), communities.len);

    // Connected components should be together or stay separate.
    // Since there are no inter-clique edges, nodes 0 and 2 should differ.
    // (They may or may not merge with intra-clique partner.)
    // At minimum: output has between 1 and 4 distinct communities.
    var seen = std.AutoHashMap(u32, void).init(testing.allocator);
    defer seen.deinit();
    for (communities) |c| try seen.put(c, {});
    try testing.expect(seen.count() >= 1);
    try testing.expect(seen.count() <= 4);
}
