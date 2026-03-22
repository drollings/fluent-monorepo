/// comment_cache.zig — In-process cache for generated doc comments.
///
/// Avoids redundant LLM calls during `guidance sync-comments` by caching the
/// generated comment per (file_path, member_name, match_hash) triple.
/// The cache is invalidated when the match_hash changes.
const std = @import("std");

/// Cache key for a generated comment.
const Key = struct {
    file_path: []const u8,
    member_name: []const u8,
    match_hash: []const u8,
};

/// A cached comment entry.
const Entry = struct {
    comment: []const u8,
    match_hash: []const u8,
};

/// In-process comment generation cache.
///
/// All strings stored in the cache are owned by the cache; they are freed when
/// the cache is deinitialized.
pub const CommentCache = struct {
    allocator: std.mem.Allocator,
    /// Key: "$file_path\x00$member_name" — stores Entry.
    map: std.StringHashMapUnmanaged(Entry),

    pub fn init(allocator: std.mem.Allocator) CommentCache {
        return .{ .allocator = allocator, .map = .{} };
    }

    pub fn deinit(self: *CommentCache) void {
        var iter = self.map.iterator();
        while (iter.next()) |kv| {
            self.allocator.free(kv.key_ptr.*);
            self.allocator.free(kv.value_ptr.comment);
            self.allocator.free(kv.value_ptr.match_hash);
        }
        self.map.deinit(self.allocator);
    }

    /// Return true when a cached comment exists and the stored hash matches.
    pub fn isValid(
        self: *const CommentCache,
        file_path: []const u8,
        member_name: []const u8,
        match_hash: []const u8,
    ) bool {
        var buf: [1024]u8 = undefined;
        const k = std.fmt.bufPrint(&buf, "{s}\x00{s}", .{ file_path, member_name }) catch return false;
        const entry = self.map.get(k) orelse return false;
        return std.mem.eql(u8, entry.match_hash, match_hash);
    }

    /// Return the cached comment, or null when absent or stale.
    pub fn get(
        self: *const CommentCache,
        file_path: []const u8,
        member_name: []const u8,
        match_hash: []const u8,
    ) ?[]const u8 {
        var buf: [1024]u8 = undefined;
        const k = std.fmt.bufPrint(&buf, "{s}\x00{s}", .{ file_path, member_name }) catch return null;
        const entry = self.map.get(k) orelse return null;
        if (!std.mem.eql(u8, entry.match_hash, match_hash)) return null;
        return entry.comment;
    }

    /// Store `comment` in the cache for the given (file_path, member_name, match_hash) key.
    pub fn put(
        self: *CommentCache,
        file_path: []const u8,
        member_name: []const u8,
        match_hash: []const u8,
        comment: []const u8,
    ) !void {
        const k = try std.fmt.allocPrint(self.allocator, "{s}\x00{s}", .{ file_path, member_name });
        errdefer self.allocator.free(k);

        // Free old entry if present.
        if (self.map.fetchRemove(k)) |old| {
            self.allocator.free(old.key);
            self.allocator.free(old.value.comment);
            self.allocator.free(old.value.match_hash);
        }

        const entry = Entry{
            .comment = try self.allocator.dupe(u8, comment),
            .match_hash = try self.allocator.dupe(u8, match_hash),
        };
        errdefer {
            self.allocator.free(entry.comment);
            self.allocator.free(entry.match_hash);
        }

        try self.map.put(self.allocator, k, entry);
    }

    /// Remove all entries for a given file path (called when a file is modified).
    pub fn invalidateFile(self: *CommentCache, file_path: []const u8) void {
        var to_remove: std.ArrayList([]const u8) = .{};
        defer to_remove.deinit(self.allocator);

        var iter = self.map.keyIterator();
        while (iter.next()) |k| {
            if (std.mem.startsWith(u8, k.*, file_path)) {
                to_remove.append(self.allocator, k.*) catch {};
            }
        }

        for (to_remove.items) |k| {
            if (self.map.fetchRemove(k)) |old| {
                self.allocator.free(old.key);
                self.allocator.free(old.value.comment);
                self.allocator.free(old.value.match_hash);
            }
        }
    }

};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "CommentCache - put and get" {
    const allocator = std.testing.allocator;
    var cache = CommentCache.init(allocator);
    defer cache.deinit();

    try cache.put("src/foo.zig", "myFn", "abc123", "Does something useful.");
    const got = cache.get("src/foo.zig", "myFn", "abc123");
    try std.testing.expect(got != null);
    try std.testing.expectEqualStrings("Does something useful.", got.?);
}

test "CommentCache - stale hash returns null" {
    const allocator = std.testing.allocator;
    var cache = CommentCache.init(allocator);
    defer cache.deinit();

    try cache.put("src/foo.zig", "myFn", "abc123", "Old comment.");
    const got = cache.get("src/foo.zig", "myFn", "def456");
    try std.testing.expect(got == null);
}

test "CommentCache - missing key returns null" {
    const allocator = std.testing.allocator;
    var cache = CommentCache.init(allocator);
    defer cache.deinit();

    const got = cache.get("src/missing.zig", "nope", "hash");
    try std.testing.expect(got == null);
}

test "CommentCache - isValid" {
    const allocator = std.testing.allocator;
    var cache = CommentCache.init(allocator);
    defer cache.deinit();

    try cache.put("src/bar.zig", "init", "h1", "Initialise the struct.");
    try std.testing.expect(cache.isValid("src/bar.zig", "init", "h1"));
    try std.testing.expect(!cache.isValid("src/bar.zig", "init", "h2"));
}
