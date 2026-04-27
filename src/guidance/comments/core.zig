//! comments/core.zig — Merged doc comment processing for guidance.
//!
//! Consolidates parser, checker, and cache into a single module for
//! clarity and to enforce the max_comment_len constant in one place.
const std = @import("std");
const types = @import("../types.zig");

pub const max_comment_len: usize = 400;

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

pub const DocComment = struct {
    text: []const u8,
    summary: []const u8,
    is_well_formed: bool,
    code_hash: ?[]const u8 = null,

    pub fn deinit(self: DocComment, allocator: std.mem.Allocator) void {
        allocator.free(self.text);
        allocator.free(self.summary);
        if (self.code_hash) |h| allocator.free(h);
    }
};

pub fn parseDocComment(allocator: std.mem.Allocator, raw: []const u8) !DocComment {
    const text = try allocator.dupe(u8, raw);
    errdefer allocator.free(text);

    const first_line = blk: {
        const idx = std.mem.indexOfScalar(u8, raw, '\n') orelse raw.len;
        break :blk raw[0..idx];
    };
    const trimmed = std.mem.trim(u8, first_line, " \t");
    const summary_len = @min(trimmed.len, 200);
    const summary = try allocator.dupe(u8, trimmed[0..summary_len]);
    errdefer allocator.free(summary);

    const well_formed = isWellFormedComment(raw, "");

    return .{
        .text = text,
        .summary = summary,
        .is_well_formed = well_formed,
    };
}

pub fn isWellFormedComment(comment: []const u8, code_context: []const u8) bool {
    _ = code_context;
    if (comment.len == 0 or comment.len > max_comment_len) return false;

    const preambles = [_][]const u8{
        "we need to write",
        "we need to look",
        "i need to write",
        "let's write",
        "let me write",
        "write a one-sentence",
    };
    const check_len = @min(comment.len, 30);
    var buf: [30]u8 = undefined;
    const lower = std.ascii.lowerString(buf[0..check_len], comment[0..check_len]);
    for (preambles) |p| {
        if (p.len <= lower.len and std.mem.startsWith(u8, lower, p)) return false;
    }

    if (std.mem.indexOf(u8, comment, "TODO") != null) return false;
    if (std.mem.indexOf(u8, comment, "FIXME") != null) return false;

    return true;
}

// ---------------------------------------------------------------------------
// Checker
// ---------------------------------------------------------------------------

pub const CheckResult = struct {
    needs_regeneration: bool,
    reason: ?[]const u8 = null,
};

pub fn checkCommentStaleness(
    existing_comment: []const u8,
    member: types.Member,
    source_context: []const u8,
) CheckResult {
    if (!isWellFormedComment(existing_comment, source_context)) {
        return .{ .needs_regeneration = true, .reason = "comment is not well-formed" };
    }

    if (existing_comment.len > max_comment_len) {
        return .{ .needs_regeneration = true, .reason = "comment exceeds 400 characters" };
    }

    _ = member;
    return .{ .needs_regeneration = false };
}

pub fn isHashStale(stored_hash: ?[]const u8, current_hash: ?[]const u8) bool {
    const stored = stored_hash orelse return false;
    const current = current_hash orelse return false;
    return !std.mem.eql(u8, stored, current);
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

const Key = struct {
    file_path: []const u8,
    member_name: []const u8,
    match_hash: []const u8,
};

const Entry = struct {
    comment: []const u8,
    match_hash: []const u8,
};

pub const CommentCache = struct {
    allocator: std.mem.Allocator,
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

    pub fn put(
        self: *CommentCache,
        file_path: []const u8,
        member_name: []const u8,
        match_hash: []const u8,
        comment: []const u8,
    ) !void {
        const k = try std.fmt.allocPrint(self.allocator, "{s}\x00{s}", .{ file_path, member_name });
        errdefer self.allocator.free(k);

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

    pub fn invalidateFile(self: *CommentCache, file_path: []const u8) void {
        var to_remove: std.ArrayList([]const u8) = .empty;
        defer to_remove.deinit(self.allocator);

        var iter = self.map.keyIterator();
        while (iter.next()) |k| {
            if (std.mem.startsWith(u8, k.*, file_path)) {
                to_remove.append(self.allocator, k.*) catch {
                    std.log.debug("failed to queue cache entry for removal (OOM); entry may be stale", .{});
                };
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
