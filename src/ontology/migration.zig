/// migration.zig — Ontology Versioning and Migration
///
/// Tracks which ontology version was loaded and provides stubs for
/// schema migrations between YAGO versions.
const std = @import("std");
const yago = @import("yago.zig");

// ---------------------------------------------------------------------------
// Version record
// ---------------------------------------------------------------------------

/// Tracks ontology version changes with fixed-size buffers; managed by owner; invariant version history.
pub const OntologyVersion = struct {
    version: []const u8, // e.g. "4.5"
    loaded_at: f64, // Unix timestamp
    source_url: []const u8, // e.g. file path or HTTP URL
    triple_count: u64,
};

// ---------------------------------------------------------------------------
// Migration stubs
// ---------------------------------------------------------------------------

/// A migration function transforms triples from one schema version to another.
/// Stub type — implementations TBD.
pub const MigrateFn = *const fn (allocator: std.mem.Allocator) anyerror!void;

/// Defines migration struct for ontology changes, manages invariants and lifecycle; owned by the system, not thread-safe.
pub const OntologyMigration = struct {
    from_version: []const u8,
    to_version: []const u8,
    /// Migration function (no-op stub by default).
    migrate_fn: MigrateFn,
};

fn noopMigration(allocator: std.mem.Allocator) anyerror!void {
    _ = allocator;
}

/// Built-in stub migrations.
pub const MIGRATIONS = [_]OntologyMigration{
    .{
        .from_version = "4.5",
        .to_version = "4.6",
        .migrate_fn = noopMigration,
    },
};

// ---------------------------------------------------------------------------
// Version registry — in-memory tracking of loaded ontologies
// ---------------------------------------------------------------------------

/// Manages version registry entries with strict ownership and invariants; ensures consistent state across operations.
pub const VersionRegistry = struct {
    allocator: std.mem.Allocator,
    versions: std.ArrayList(OntologyVersion),

    pub fn init(allocator: std.mem.Allocator) VersionRegistry {
        return .{
            .allocator = allocator,
            .versions = .{},
        };
    }

    pub fn deinit(self: *VersionRegistry) void {
        for (self.versions.items) |v| {
            self.allocator.free(v.version);
            self.allocator.free(v.source_url);
        }
        self.versions.deinit(self.allocator);
    }

    pub fn record(self: *VersionRegistry, ver: OntologyVersion) !void {
        const owned_version = try self.allocator.dupe(u8, ver.version);
        errdefer self.allocator.free(owned_version);
        const owned_url = try self.allocator.dupe(u8, ver.source_url);
        errdefer self.allocator.free(owned_url);
        try self.versions.append(self.allocator, .{
            .version = owned_version,
            .loaded_at = ver.loaded_at,
            .source_url = owned_url,
            .triple_count = ver.triple_count,
        });
    }

    pub fn latest(self: *const VersionRegistry) ?OntologyVersion {
        if (self.versions.items.len == 0) return null;
        return self.versions.items[self.versions.items.len - 1];
    }
};

// =============================================================================
// Tests — Milestone 2.4
// =============================================================================

const testing = std.testing;

test "version recorded and retrieved" {
    var reg = VersionRegistry.init(testing.allocator);
    defer reg.deinit();

    try reg.record(.{
        .version = yago.YAGO_VERSION,
        .loaded_at = 1_700_000_000.0,
        .source_url = "data/yago-4.5.0.2-tiny/yago-tiny.ttl",
        .triple_count = 1234,
    });

    const latest = reg.latest().?;
    try testing.expectEqualStrings(yago.YAGO_VERSION, latest.version);
    try testing.expectEqual(@as(u64, 1234), latest.triple_count);
}

test "migration stub is no-op" {
    try MIGRATIONS[0].migrate_fn(testing.allocator);
}

test "latest returns null when empty" {
    var reg = VersionRegistry.init(testing.allocator);
    defer reg.deinit();
    try testing.expect(reg.latest() == null);
}



