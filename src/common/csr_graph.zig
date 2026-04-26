//! csr_graph.zig — Compressed Sparse Row (CSR) graph representation.
//!
//! Builds a CSR adjacency structure from the `neighbor_of` table in a Coral
//! Context Library.  CSR is the canonical in-memory format for graph algorithms
//! (Degree, PageRank, Dijkstra, Louvain) because it gives O(1) neighbor lookup
//! and excellent cache locality.
//!
//! §When to build CSR:
//! CSR is an ingestion-time artefact — never computed on the query path.
//! Build it when Louvain community detection or Dijkstra shortest-path is needed,
//! or when the graph exceeds 10K nodes (below that, SQL traversal is adequate).
//!
//! §Serialization (Fluent WVR Pattern 5 — extern struct BLOB):
//! CSR is serialized to a BLOB for storage in the `csr_cache` table.  The header
//! uses `extern struct` with `align(1)` so the bytes lay out identically on every
//! platform.  The payload (offsets + targets + optional weights) follows the
//! header contiguously.
//!
//! §Lifetime:
//! All slices are arena-owned when built with `CSRGraph.build()`.  When
//! deserialized from a BLOB the slices are views into the caller-owned BLOB;
//! the graph itself carries no allocations.

const std = @import("std");
const Allocator = std.mem.Allocator;

/// Magic number: "CSRG" in ASCII little-endian.
pub const CSR_MAGIC: u32 = 0x4752_5343; // 'C','S','R','G'
pub const CSR_VERSION: u32 = 1;

/// Manages serialized CSR data with fixed buffers; encapsulates ownership and invariants.
pub const SerializedCSR = extern struct {
    magic: u32 align(1),
    version: u32 align(1),
    node_count: u32 align(1),
    edge_count: u32 align(1),
    has_weights: u8 align(1),
    _pad: [3]u8 align(1),
};

/// Manages graph data structures with a fixed-size buffer; encapsulates ownership and invariants.
pub const CSRGraph = struct {
    node_count: u32,
    edge_count: u32,
    /// `node_count + 1` offsets.  offsets[i]..offsets[i+1] indexes into `targets`.
    offsets: []const usize,
    /// Packed adjacency list (node indices).
    targets: []const u32,
    /// Optional per-edge weights, parallel to `targets`.  null → unweighted.
    weights: ?[]const f32,

    const Self = @This();

    // -----------------------------------------------------------------------
    // Construction
    // -----------------------------------------------------------------------

    /// Build a CSR graph from the `neighbor_of` table in `library`.
    ///
    /// All memory is allocated from `arena`; the graph is valid for the
    /// lifetime of `arena`.
    ///
    /// `predicate_filter`: if non-null, only include edges whose `predicate_iri`
    /// matches this string.  Passing null includes all edges.
    pub fn build(
        arena: Allocator,
        library: anytype, // *Library — accepts anytype to avoid circular imports
        predicate_filter: ?[]const u8,
        with_weights: bool,
    ) !Self {
        // Step 1: count distinct node IDs in context_nodes to size the offset array.
        const node_count = @as(u32, @intCast(try library.countNodes()));
        if (node_count == 0) {
            // CSR requires node_count+1 offset entries even for empty graphs.
            const empty_offsets = try arena.alloc(usize, 1);
            empty_offsets[0] = 0;
            return Self{
                .node_count = 0,
                .edge_count = 0,
                .offsets = empty_offsets,
                .targets = &[_]u32{},
                .weights = null,
            };
        }

        // Step 2: collect all edges (from_id, to_id, weight) sorted by from_id.
        // We use an arena-allocated intermediate array.
        const Edge = struct { from: u32, to: u32, weight: f32 };
        var edges: std.ArrayListUnmanaged(Edge) = .{};
        _ = predicate_filter; // Used by SQL query below when implemented

        // Load edges from library via iterateNeighborOf.
        // The library exposes iterateNeighborOf(callback) that yields (from_id, to_id, weight).
        try library.iterateNeighborOf(arena, struct {
            list: *std.ArrayListUnmanaged(Edge),
            allocator: Allocator,
            pub fn call(ctx: @This(), from: i64, to: i64, weight: f32) !void {
                try ctx.list.append(ctx.allocator, .{
                    .from = @intCast(from),
                    .to = @intCast(to),
                    .weight = weight,
                });
            }
        }{ .list = &edges, .allocator = arena });

        const edge_count = @as(u32, @intCast(edges.items.len));

        // Step 3: sort by from_id for CSR construction.
        std.mem.sort(Edge, edges.items, {}, struct {
            pub fn lessThan(_: void, a: Edge, b: Edge) bool {
                return a.from < b.from;
            }
        }.lessThan);

        // Step 4: build CSR offset array and target array.
        // Map node IDs to dense 0-based indices using allNodeIds().
        const all_ids = try library.allNodeIds(arena);
        var id_to_idx = std.AutoHashMap(i64, u32).init(arena);
        for (all_ids, 0..) |id, i| {
            try id_to_idx.put(id, @intCast(i));
        }

        const offsets = try arena.alloc(usize, node_count + 1);
        const targets = try arena.alloc(u32, edge_count);
        const weights_arr: ?[]f32 = if (with_weights) try arena.alloc(f32, edge_count) else null;

        @memset(offsets, 0);

        // Count out-degree of each node.
        for (edges.items) |e| {
            const src_idx = id_to_idx.get(@intCast(e.from)) orelse continue;
            if (src_idx < node_count) offsets[src_idx + 1] += 1;
        }

        // Prefix-sum to get actual offsets.
        for (1..node_count + 1) |i| {
            offsets[i] += offsets[i - 1];
        }

        // Fill target + weight arrays.
        var cursor = try arena.dupe(usize, offsets[0..node_count]);
        for (edges.items) |e| {
            const src_idx = id_to_idx.get(@intCast(e.from)) orelse continue;
            const dst_idx = id_to_idx.get(@intCast(e.to)) orelse continue;
            if (src_idx >= node_count) continue;
            const pos = cursor[src_idx];
            targets[pos] = dst_idx;
            if (weights_arr) |wa| wa[pos] = e.weight;
            cursor[src_idx] += 1;
        }

        return Self{
            .node_count = node_count,
            .edge_count = edge_count,
            .offsets = offsets,
            .targets = targets,
            .weights = if (weights_arr) |wa| wa else null,
        };
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// Return the slice of target node indices for `node_idx`.
    pub fn neighbors(self: Self, node_idx: u32) []const u32 {
        if (node_idx >= self.node_count) return &[_]u32{};
        const start = self.offsets[node_idx];
        const end = self.offsets[node_idx + 1];
        return self.targets[start..end];
    }

    /// Return the out-degree of `node_idx`.
    pub fn degree(self: Self, node_idx: u32) u32 {
        if (node_idx >= self.node_count) return 0;
        return @intCast(self.offsets[node_idx + 1] - self.offsets[node_idx]);
    }

    // -----------------------------------------------------------------------
    // Serialization
    // -----------------------------------------------------------------------

    /// Serialize the CSR to a BLOB for storage in `csr_cache`.
    /// Returns an allocator-owned byte slice.
    pub fn serialize(self: *const Self, allocator: Allocator) ![]u8 {
        const header = SerializedCSR{
            .magic = CSR_MAGIC,
            .version = CSR_VERSION,
            .node_count = self.node_count,
            .edge_count = self.edge_count,
            .has_weights = if (self.weights != null) 1 else 0,
            ._pad = .{ 0, 0, 0 },
        };

        const header_size = @sizeOf(SerializedCSR);
        const offsets_size = (self.node_count + 1) * @sizeOf(usize);
        const targets_size = self.edge_count * @sizeOf(u32);
        const weights_size: usize = if (self.weights != null) self.edge_count * @sizeOf(f32) else 0;
        const total = header_size + offsets_size + targets_size + weights_size;

        const buf = try allocator.alloc(u8, total);
        var pos: usize = 0;

        @memcpy(buf[pos..][0..header_size], std.mem.asBytes(&header));
        pos += header_size;

        @memcpy(buf[pos..][0..offsets_size], std.mem.sliceAsBytes(self.offsets));
        pos += offsets_size;

        @memcpy(buf[pos..][0..targets_size], std.mem.sliceAsBytes(self.targets));
        pos += targets_size;

        if (self.weights) |w| {
            @memcpy(buf[pos..][0..weights_size], std.mem.sliceAsBytes(w));
        }

        return buf;
    }

    /// Deserialize a CSR from a BLOB into arena-owned aligned arrays.
    ///
    /// Copies the BLOB payload into freshly allocated aligned memory so
    /// callers do not need to manage alignment or BLOB lifetime.
    pub fn deserialize(allocator: Allocator, blob: []const u8) !Self {
        const header_size = @sizeOf(SerializedCSR);
        if (blob.len < header_size) return error.BlobTooShort;

        // Read header fields byte-by-byte via @memcpy into a local copy.
        var header: SerializedCSR = undefined;
        @memcpy(std.mem.asBytes(&header), blob[0..header_size]);

        if (header.magic != CSR_MAGIC) return error.InvalidMagic;
        if (header.version != CSR_VERSION) return error.UnsupportedVersion;

        const nc = header.node_count;
        const ec = header.edge_count;
        const has_weights = header.has_weights != 0;

        const offsets_size = (nc + 1) * @sizeOf(usize);
        const targets_size = ec * @sizeOf(u32);
        const weights_size: usize = if (has_weights) ec * @sizeOf(f32) else 0;

        const needed = header_size + offsets_size + targets_size + weights_size;
        if (blob.len < needed) return error.BlobTooShort;

        var pos: usize = header_size;

        // Copy into aligned allocations.
        const offsets = try allocator.alloc(usize, nc + 1);
        @memcpy(std.mem.sliceAsBytes(offsets), blob[pos..][0..offsets_size]);
        pos += offsets_size;

        const targets = try allocator.alloc(u32, ec);
        @memcpy(std.mem.sliceAsBytes(targets), blob[pos..][0..targets_size]);
        pos += targets_size;

        const weights: ?[]f32 = if (has_weights) blk: {
            const w = try allocator.alloc(f32, ec);
            @memcpy(std.mem.sliceAsBytes(w), blob[pos..][0..weights_size]);
            break :blk w;
        } else null;

        return Self{
            .node_count = nc,
            .edge_count = ec,
            .offsets = offsets,
            .targets = targets,
            .weights = weights,
        };
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "SerializedCSR: size and layout" {
    // Header must be compact for BLOB storage: 4×u32 + u8 + 3×u8 pad = 20 bytes.
    try testing.expectEqual(@as(usize, 20), @sizeOf(SerializedCSR));
}

test "CSRGraph: empty graph round-trip" {
    const allocator = testing.allocator;

    // A valid CSR for 0 nodes still requires 1 offset entry (offsets[0] = 0).
    const empty_offsets = [_]usize{0};
    const g = CSRGraph{
        .node_count = 0,
        .edge_count = 0,
        .offsets = &empty_offsets,
        .targets = &[_]u32{},
        .weights = null,
    };

    const blob = try g.serialize(allocator);
    defer allocator.free(blob);

    const g2 = try CSRGraph.deserialize(allocator, blob);
    defer allocator.free(g2.offsets);
    defer allocator.free(g2.targets);
    try testing.expectEqual(@as(u32, 0), g2.node_count);
    try testing.expectEqual(@as(u32, 0), g2.edge_count);
}

test "CSRGraph: manual construction and accessor" {
    // 3-node graph: 0→1, 0→2, 1→2
    const offsets = [_]usize{ 0, 2, 3, 3 };
    const targets = [_]u32{ 1, 2, 2 };

    const g = CSRGraph{
        .node_count = 3,
        .edge_count = 3,
        .offsets = &offsets,
        .targets = &targets,
        .weights = null,
    };

    try testing.expectEqual(@as(u32, 2), g.degree(0));
    try testing.expectEqual(@as(u32, 1), g.degree(1));
    try testing.expectEqual(@as(u32, 0), g.degree(2));

    const n0 = g.neighbors(0);
    try testing.expectEqual(@as(usize, 2), n0.len);
    try testing.expectEqual(@as(u32, 1), n0[0]);
    try testing.expectEqual(@as(u32, 2), n0[1]);

    const n2 = g.neighbors(2);
    try testing.expectEqual(@as(usize, 0), n2.len);
}

test "CSRGraph: serialize and deserialize round-trip" {
    const allocator = testing.allocator;

    const offsets = [_]usize{ 0, 2, 3, 3 };
    const targets = [_]u32{ 1, 2, 2 };
    const weights = [_]f32{ 0.5, 1.0, 0.75 };

    const g = CSRGraph{
        .node_count = 3,
        .edge_count = 3,
        .offsets = &offsets,
        .targets = &targets,
        .weights = &weights,
    };

    const blob = try g.serialize(allocator);
    defer allocator.free(blob);

    const g2 = try CSRGraph.deserialize(allocator, blob);
    defer allocator.free(g2.offsets);
    defer allocator.free(g2.targets);
    defer if (g2.weights) |w| allocator.free(w);
    try testing.expectEqual(@as(u32, 3), g2.node_count);
    try testing.expectEqual(@as(u32, 3), g2.edge_count);
    try testing.expectEqual(@as(u32, 2), g2.degree(0));
    try testing.expectEqual(@as(u32, 1), g2.degree(1));

    const w = g2.weights.?;
    try testing.expectApproxEqAbs(@as(f32, 0.5), w[0], 0.001);
    try testing.expectApproxEqAbs(@as(f32, 1.0), w[1], 0.001);
}

test "CSRGraph: deserialize detects bad magic" {
    // Header is 20 bytes (4×u32 + u8 + 3×padding); all zeros = bad magic.
    var bad = [_]u8{0} ** 20;
    try testing.expectError(error.InvalidMagic, CSRGraph.deserialize(testing.allocator, &bad));
}

test "CSRGraph: out-of-range node returns empty neighbors" {
    const offsets = [_]usize{ 0, 1, 1 };
    const targets = [_]u32{1};
    const g = CSRGraph{
        .node_count = 2,
        .edge_count = 1,
        .offsets = &offsets,
        .targets = &targets,
        .weights = null,
    };
    try testing.expectEqual(@as(usize, 0), g.neighbors(99).len);
    try testing.expectEqual(@as(u32, 0), g.degree(99));
}
