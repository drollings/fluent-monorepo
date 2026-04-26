/// doc_registry.zig — Shared path ↔ u32 doc_id mapping for word and trigram indices.
///
/// Eliminates the identical getOrCreateDocId + path_to_id / id_to_path pair that
/// appeared in both word_index.zig and trigram_index.zig.
///
/// The `own_strings` flag controls memory ownership semantics:
///   false — borrow: the caller dupes strings before inserting; DocRegistry stores
///           the pointer directly and does NOT free keys on deinit.
///   true  — own: DocRegistry dupes the path on insert and frees all keys on deinit.
const std = @import("std");

pub const DocRegistry = struct {
    path_to_id: std.StringHashMap(u32),
    id_to_path: std.ArrayList([]const u8),
    allocator: std.mem.Allocator,
    own_strings: bool,

    pub fn init(allocator: std.mem.Allocator, own_strings: bool) DocRegistry {
        return .{
            .path_to_id = std.StringHashMap(u32).init(allocator),
            .id_to_path = .empty,
            .allocator = allocator,
            .own_strings = own_strings,
        };
    }

    pub fn deinit(self: *DocRegistry) void {
        if (self.own_strings) {
            var iter = self.path_to_id.keyIterator();
            while (iter.next()) |k| self.allocator.free(k.*);
        }
        self.path_to_id.deinit();
        self.id_to_path.deinit(self.allocator);
    }

    /// Returns the existing doc_id for `path`, or inserts a new entry and returns a
    /// freshly assigned id.  When `own_strings = true` the path is duped; callers
    /// must not free the original.  When `own_strings = false` the path slice must
    /// outlive the registry.
    pub fn getOrCreate(self: *DocRegistry, path: []const u8) !u32 {
        if (self.path_to_id.get(path)) |id| return id;
        const id: u32 = @intCast(self.id_to_path.items.len);
        if (self.own_strings) {
            const owned = try self.allocator.dupe(u8, path);
            errdefer self.allocator.free(owned);
            try self.path_to_id.put(owned, id);
            try self.id_to_path.append(self.allocator, owned);
        } else {
            try self.id_to_path.append(self.allocator, path);
            try self.path_to_id.put(path, id);
        }
        return id;
    }

    /// Returns the path string for a given doc_id, or "" if out of range.
    pub fn pathForId(self: *const DocRegistry, id: u32) []const u8 {
        if (id < self.id_to_path.items.len) return self.id_to_path.items[id];
        return "";
    }

    /// Total number of registered documents.
    pub fn count(self: *const DocRegistry) u32 {
        return @intCast(self.id_to_path.items.len);
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "DocRegistry borrow: getOrCreate returns consistent ids" {
    var reg = DocRegistry.init(testing.allocator, false);
    defer reg.deinit();

    const id0 = try reg.getOrCreate("a/b.zig");
    const id1 = try reg.getOrCreate("c/d.zig");
    const id0_again = try reg.getOrCreate("a/b.zig");

    try testing.expectEqual(@as(u32, 0), id0);
    try testing.expectEqual(@as(u32, 1), id1);
    try testing.expectEqual(id0, id0_again);
    try testing.expectEqual(@as(u32, 2), reg.count());
}

test "DocRegistry borrow: pathForId roundtrips" {
    var reg = DocRegistry.init(testing.allocator, false);
    defer reg.deinit();

    _ = try reg.getOrCreate("src/foo.zig");
    _ = try reg.getOrCreate("src/bar.zig");

    try testing.expectEqualStrings("src/foo.zig", reg.pathForId(0));
    try testing.expectEqualStrings("src/bar.zig", reg.pathForId(1));
    try testing.expectEqualStrings("", reg.pathForId(99));
}

test "DocRegistry own: dupes and frees strings" {
    var reg = DocRegistry.init(testing.allocator, true);
    defer reg.deinit();

    var path_buf: [64]u8 = undefined;
    const path = try std.fmt.bufPrint(&path_buf, "src/test.zig", .{});

    const id = try reg.getOrCreate(path);
    try testing.expectEqual(@as(u32, 0), id);
    try testing.expectEqualStrings("src/test.zig", reg.pathForId(0));
}
