/// targets.zig — Ingestion DAG Target Definitions
///
/// Defines the YAGO ingestion pipeline as a set of named target nodes
/// with explicit dependency edges.
///
/// Target hierarchy:
///   yago_ingest (phony)
///   ├── yago_download  — fetch YAGO 4.5 tiny TTL
///   ├── yago_parse     — Turtle → triples (parse only)
///   ├── yago_map       — triples → ContextNodes + edges
///   ├── yago_embed     — compute embeddings for nodes
///   ├── yago_index     — build ANN index
///   └── yago_verify    — integrity checks
///
/// Usage:
///   var defs = IngestTargetDefs.init(allocator);
///   defer defs.deinit();
///   // check dependency order
const std = @import("std");

// ---------------------------------------------------------------------------
// Target names (string constants)
// ---------------------------------------------------------------------------

pub const TARGET_DOWNLOAD = "yago_download";
pub const TARGET_PARSE = "yago_parse";
pub const TARGET_MAP = "yago_map";
pub const TARGET_EMBED = "yago_embed";
pub const TARGET_INDEX = "yago_index";
pub const TARGET_VERIFY = "yago_verify";
pub const TARGET_INGEST = "yago_ingest";

// ---------------------------------------------------------------------------
// Target definition (lightweight, no external DB or Registry dependency)
// ---------------------------------------------------------------------------

pub const TargetKind = enum {
    phony,
    command,
};

/// Handler function type for ingestion targets.
/// Uses an opaque context pointer to keep targets.zig free of Library/BatchConfig dependencies.
/// The executor casts the context to its concrete ExecutionContext.
pub const HandlerFn = *const fn (allocator: std.mem.Allocator, ctx: *anyopaque) anyerror!void;

/// Defines a target definition with fixed-size buffers; managed via ownership model; ensures correct initialization/deinit state.
pub const TargetDef = struct {
    name: []const u8,
    kind: TargetKind,
    essential: bool,
    /// Names of targets this one depends on.
    depends: []const []const u8,
    description: []const u8,
    /// Handler function — null for phony targets or unimplemented stages.
    handler: ?HandlerFn = null,
};

const download_deps = [_][]const u8{};
const parse_deps = [_][]const u8{TARGET_DOWNLOAD};
const map_deps = [_][]const u8{TARGET_PARSE};
const embed_deps = [_][]const u8{TARGET_MAP};
const index_deps = [_][]const u8{TARGET_EMBED};
const verify_deps = [_][]const u8{TARGET_INDEX};
const ingest_deps = [_][]const u8{
    TARGET_DOWNLOAD,
    TARGET_PARSE,
    TARGET_MAP,
    TARGET_EMBED,
    TARGET_INDEX,
    TARGET_VERIFY,
};

pub const INGEST_TARGET_DEFS = [_]TargetDef{
    .{
        .name = TARGET_DOWNLOAD,
        .kind = .command,
        .essential = true,
        .depends = &download_deps,
        .description = "Download YAGO 4.5 tiny TTL file",
    },
    .{
        .name = TARGET_PARSE,
        .kind = .command,
        .essential = true,
        .depends = &parse_deps,
        .description = "Parse Turtle file into RDF triples",
    },
    .{
        .name = TARGET_MAP,
        .kind = .command,
        .essential = true,
        .depends = &map_deps,
        .description = "Map RDF triples to ContextNodes and edges",
    },
    .{
        .name = TARGET_EMBED,
        .kind = .command,
        .essential = false,
        .depends = &embed_deps,
        .description = "Compute embeddings for ContextNodes",
    },
    .{
        .name = TARGET_INDEX,
        .kind = .command,
        .essential = false,
        .depends = &index_deps,
        .description = "Build ANN index from node embeddings",
    },
    .{
        .name = TARGET_VERIFY,
        .kind = .command,
        .essential = true,
        .depends = &verify_deps,
        .description = "Verify ingestion integrity and completeness",
    },
    .{
        .name = TARGET_INGEST,
        .kind = .phony,
        .essential = true,
        .depends = &ingest_deps,
        .description = "Run full YAGO ingestion pipeline",
    },
};

/// Retrieves a TargetDef instance from a given array of bytes, returning null if not found.
pub fn lookupTargetDef(name: []const u8) ?*const TargetDef {
    for (&INGEST_TARGET_DEFS) |*def| {
        if (std.mem.eql(u8, def.name, name)) return def;
    }
    return null;
}

/// Checks required dependencies for the Zig build process and returns true if all are present.
pub fn validateDependencies() bool {
    for (INGEST_TARGET_DEFS) |def| {
        for (def.depends) |dep| {
            if (lookupTargetDef(dep) == null) return false;
        }
    }
    return true;
}

/// Converts a topologically sorted list into a flat array of slices.
pub fn topoSort(allocator: std.mem.Allocator) ![]const []const u8 {
    const N = INGEST_TARGET_DEFS.len;

    // Helper: name → slot index in INGEST_TARGET_DEFS.
    const indexOf = struct {
        fn call(name: []const u8) ?usize {
            for (INGEST_TARGET_DEFS, 0..) |def, i| {
                if (std.mem.eql(u8, def.name, name)) return i;
            }
            return null;
        }
    }.call;

    // Build per-node lists of dependents and track in-degrees.
    var in_degree = [_]usize{0} ** N;
    var adj: [N]std.ArrayListUnmanaged(usize) = [_]std.ArrayListUnmanaged(usize){.{}} ** N;
    defer for (&adj) |*list| list.deinit(allocator);

    for (INGEST_TARGET_DEFS, 0..) |def, j| {
        for (def.depends) |dep| {
            const i = indexOf(dep) orelse continue;
            try adj[i].append(allocator, j);
            in_degree[j] += 1;
        }
    }

    // Seed queue with zero-in-degree nodes.
    var queue: std.ArrayListUnmanaged(usize) = .{};
    defer queue.deinit(allocator);
    for (in_degree, 0..) |deg, i| {
        if (deg == 0) try queue.append(allocator, i);
    }

    var result: std.ArrayListUnmanaged([]const u8) = .{};
    errdefer result.deinit(allocator);

    while (queue.items.len > 0) {
        const idx = queue.orderedRemove(0);
        try result.append(allocator, INGEST_TARGET_DEFS[idx].name);
        for (adj[idx].items) |next| {
            in_degree[next] -= 1;
            if (in_degree[next] == 0) try queue.append(allocator, next);
        }
    }

    if (result.items.len != N) return error.CyclicDependency;
    return result.toOwnedSlice(allocator);
}

// =============================================================================
// Tests — Milestone 3.1
// =============================================================================

const testing = std.testing;

test "all target names look up" {
    for (INGEST_TARGET_DEFS) |def| {
        const found = lookupTargetDef(def.name);
        try testing.expect(found != null);
    }
}

test "dependency graph is valid" {
    try testing.expect(validateDependencies());
}

test "parse depends on download" {
    const parse = lookupTargetDef(TARGET_PARSE).?;
    var found = false;
    for (parse.depends) |dep| {
        if (std.mem.eql(u8, dep, TARGET_DOWNLOAD)) found = true;
    }
    try testing.expect(found);
}

test "map depends on parse" {
    const map = lookupTargetDef(TARGET_MAP).?;
    var found = false;
    for (map.depends) |dep| {
        if (std.mem.eql(u8, dep, TARGET_PARSE)) found = true;
    }
    try testing.expect(found);
}

test "yago_ingest is phony" {
    const ingest = lookupTargetDef(TARGET_INGEST).?;
    try testing.expectEqual(TargetKind.phony, ingest.kind);
}

test "essential targets marked correctly" {
    const download = lookupTargetDef(TARGET_DOWNLOAD).?;
    try testing.expect(download.essential);
    const embed = lookupTargetDef(TARGET_EMBED).?;
    try testing.expect(!embed.essential); // embed is optional
}

test "topoSort returns all targets" {
    const order = try topoSort(testing.allocator);
    defer testing.allocator.free(order);
    try testing.expectEqual(INGEST_TARGET_DEFS.len, order.len);
}

test "topoSort: each dependency appears before its dependent" {
    const order = try topoSort(testing.allocator);
    defer testing.allocator.free(order);

    // Build position map: name → index in order.
    var pos: [INGEST_TARGET_DEFS.len]usize = undefined;
    for (order, 0..) |name, i| {
        for (INGEST_TARGET_DEFS, 0..) |def, j| {
            if (std.mem.eql(u8, def.name, name)) {
                pos[j] = i;
                break;
            }
        }
    }

    // For every edge dep → def, pos[dep] < pos[def].
    for (INGEST_TARGET_DEFS, 0..) |def, j| {
        for (def.depends) |dep_name| {
            for (INGEST_TARGET_DEFS, 0..) |dep_def, i| {
                if (std.mem.eql(u8, dep_def.name, dep_name)) {
                    try testing.expect(pos[i] < pos[j]);
                    break;
                }
            }
        }
    }
}

test "topoSort: download is first, yago_ingest is last" {
    const order = try topoSort(testing.allocator);
    defer testing.allocator.free(order);
    try testing.expectEqualStrings(TARGET_DOWNLOAD, order[0]);
    try testing.expectEqualStrings(TARGET_INGEST, order[order.len - 1]);
}
