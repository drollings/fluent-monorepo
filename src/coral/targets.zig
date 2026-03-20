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
// Target definition (lightweight, no CozoDB or Registry dependency)
// ---------------------------------------------------------------------------

pub const TargetKind = enum {
    phony,
    command,
};

pub const TargetDef = struct {
    name: []const u8,
    kind: TargetKind,
    essential: bool,
    /// Names of targets this one depends on.
    depends: []const []const u8,
    description: []const u8,
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

/// Look up a target definition by name.
pub fn lookupTargetDef(name: []const u8) ?*const TargetDef {
    for (&INGEST_TARGET_DEFS) |*def| {
        if (std.mem.eql(u8, def.name, name)) return def;
    }
    return null;
}

/// Verify that all dependency names exist in the definitions.
pub fn validateDependencies() bool {
    for (INGEST_TARGET_DEFS) |def| {
        for (def.depends) |dep| {
            if (lookupTargetDef(dep) == null) return false;
        }
    }
    return true;
}

/// Topological sort result
pub const TopoOrder = struct {
    names: []const []const u8,
};

/// Simple topological order for the known targets (hardcoded order).
pub const TOPO_ORDER = [_][]const u8{
    TARGET_DOWNLOAD,
    TARGET_PARSE,
    TARGET_MAP,
    TARGET_EMBED,
    TARGET_INDEX,
    TARGET_VERIFY,
    TARGET_INGEST,
};

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

test "topological order has all targets" {
    try testing.expectEqual(INGEST_TARGET_DEFS.len, TOPO_ORDER.len);
}
