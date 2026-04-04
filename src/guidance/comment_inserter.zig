/// comment_inserter.zig — Insert and replace doc comments in Zig source files.
///
/// Provides utilities to:
///   - Insert a `///` doc comment block above a declaration at a given line
///   - Replace an existing doc comment block above a declaration
///   - Compute line number adjustments after an insertion
const std = @import("std");
const types = @import("types.zig");

/// Tracks how a single declaration's line number changed after an insertion.
pub const LineAdjustment = struct {
    /// Original line number before the insertion.
    old_line: u32,
    /// New line number after the insertion.
    new_line: u32,
};

/// Result of inserting or replacing a comment in source.
pub const InsertResult = struct {
    /// Whether any changes were made to the source.
    changed: bool,
    /// Modified source content (owned; caller must free).
    new_source: []const u8,
    /// Per-declaration line number adjustments (owned; caller must free).
    line_adjustments: []LineAdjustment,

    pub fn deinit(self: InsertResult, allocator: std.mem.Allocator) void {
        if (self.changed) allocator.free(self.new_source);
        allocator.free(self.line_adjustments);
    }
};

/// Insert a `///` doc comment block above the declaration at 1-based `line`.
///
/// `comment` is plain text; each line will be prefixed with `/// `.
/// Returns `InsertResult` with `changed = false` when:
///   - `line` is 0 or beyond the end of `source`
///   - the comment block is empty after formatting
///
/// Caller owns all allocations in the returned struct.
pub fn insertComment(
    allocator: std.mem.Allocator,
    source: []const u8,
    line: u32,
    comment: []const u8,
) !InsertResult {
    if (line == 0) return emptyResult(allocator);

    const formatted = try formatDocComment(allocator, comment);
    defer allocator.free(formatted);
    if (formatted.len == 0) return emptyResult(allocator);

    // Split source into lines; insertion happens *before* `line` (1-based).
    var lines: std.ArrayList([]const u8) = .{};
    defer lines.deinit(allocator);
    var iter = std.mem.splitScalar(u8, source, '\n');
    while (iter.next()) |ln| try lines.append(allocator, ln);

    if (line > lines.items.len) return emptyResult(allocator);

    // Build output: everything before the target line, then comment, then rest.
    var out: std.ArrayList(u8) = .{};
    errdefer out.deinit(allocator);

    const insert_at = line - 1; // 0-based index
    for (lines.items[0..insert_at]) |ln| {
        try out.appendSlice(allocator, ln);
        try out.append(allocator, '\n');
    }
    try out.appendSlice(allocator, formatted);
    for (lines.items[insert_at..]) |ln| {
        try out.appendSlice(allocator, ln);
        try out.append(allocator, '\n');
    }
    // Remove trailing newline added by the loop if source didn't end with one.
    if (!std.mem.endsWith(u8, source, "\n") and std.mem.endsWith(u8, out.items, "\n")) {
        _ = out.pop();
    }

    const comment_lines = countLines(formatted);
    const adj = try allocator.alloc(LineAdjustment, 1);
    adj[0] = .{ .old_line = line, .new_line = line + comment_lines };

    return .{
        .changed = true,
        .new_source = try out.toOwnedSlice(allocator),
        .line_adjustments = adj,
    };
}

/// Replace an existing `///` doc comment block above the declaration at `line`.
///
/// Scans backwards from `line - 1` to find consecutive `///` lines and
/// replaces them with the new `comment`.  If no existing comment is found,
/// falls back to `insertComment`.
pub fn replaceComment(
    allocator: std.mem.Allocator,
    source: []const u8,
    line: u32,
    comment: []const u8,
) !InsertResult {
    if (line == 0) return emptyResult(allocator);

    var lines: std.ArrayList([]const u8) = .{};
    defer lines.deinit(allocator);
    var iter = std.mem.splitScalar(u8, source, '\n');
    while (iter.next()) |ln| try lines.append(allocator, ln);

    if (line > lines.items.len) return emptyResult(allocator);

    // Find the extent of the existing comment block above `line` (0-based idx).
    const decl_idx = line - 1; // 0-based index of the declaration line
    var comment_start = decl_idx;
    while (comment_start > 0) {
        const prev = std.mem.trimLeft(u8, lines.items[comment_start - 1], " \t");
        if (std.mem.startsWith(u8, prev, "///")) {
            comment_start -= 1;
        } else {
            break;
        }
    }

    const had_comment = comment_start < decl_idx;
    if (!had_comment) {
        // No existing comment — insert instead.
        return insertComment(allocator, source, line, comment);
    }

    // Rebuild source: lines before comment_start + new comment + lines from decl_idx onward.
    const formatted = try formatDocComment(allocator, comment);
    defer allocator.free(formatted);

    var out: std.ArrayList(u8) = .{};
    errdefer out.deinit(allocator);

    for (lines.items[0..comment_start]) |ln| {
        try out.appendSlice(allocator, ln);
        try out.append(allocator, '\n');
    }
    try out.appendSlice(allocator, formatted);
    for (lines.items[decl_idx..]) |ln| {
        try out.appendSlice(allocator, ln);
        try out.append(allocator, '\n');
    }
    if (!std.mem.endsWith(u8, source, "\n") and std.mem.endsWith(u8, out.items, "\n")) {
        _ = out.pop();
    }

    const old_comment_lines: u32 = @intCast(decl_idx - comment_start);
    const new_comment_lines: u32 = countLines(formatted);
    const delta: i32 = @as(i32, @intCast(new_comment_lines)) - @as(i32, @intCast(old_comment_lines));

    const adj = try allocator.alloc(LineAdjustment, 1);
    adj[0] = .{
        .old_line = line,
        .new_line = if (delta >= 0)
            line + @as(u32, @intCast(delta))
        else
            line - @as(u32, @intCast(-delta)),
    };

    return .{
        .changed = true,
        .new_source = try out.toOwnedSlice(allocator),
        .line_adjustments = adj,
    };
}

/// Extract the doc comment text above the declaration at 1-based `line`,
/// stripping `///` prefixes.  Returns null when no comment is found.
/// Caller owns the returned slice.
pub fn extractCommentAtLine(
    allocator: std.mem.Allocator,
    source: []const u8,
    line: u32,
) !?[]const u8 {
    if (line == 0) return null;

    var lines: std.ArrayList([]const u8) = .{};
    defer lines.deinit(allocator);
    var iter = std.mem.splitScalar(u8, source, '\n');
    while (iter.next()) |ln| try lines.append(allocator, ln);

    if (line > lines.items.len) return null;

    var doc_lines: std.ArrayList([]const u8) = .{};
    defer doc_lines.deinit(allocator);

    var idx = line - 1; // 0-based
    while (idx > 0) {
        idx -= 1;
        const prev = std.mem.trimLeft(u8, lines.items[idx], " \t");
        if (!std.mem.startsWith(u8, prev, "///")) break;
        try doc_lines.append(allocator, prev);
    }

    if (doc_lines.items.len == 0) return null;

    // Tokens were collected closest-first; reverse for correct order.
    std.mem.reverse([]const u8, doc_lines.items);

    var result: std.ArrayList(u8) = .{};
    errdefer result.deinit(allocator);
    for (doc_lines.items, 0..) |dl, i| {
        const after = if (dl.len > 3) dl[3..] else "";
        const text = if (after.len > 0 and after[0] == ' ') after[1..] else after;
        if (i > 0) try result.append(allocator, '\n');
        try result.appendSlice(allocator, text);
    }
    return @as(?[]const u8, try result.toOwnedSlice(allocator));
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Format `comment` as a `///`-prefixed doc comment block, one line per input
/// line.  Returns an owned string ending with a newline.
pub fn formatDocComment(allocator: std.mem.Allocator, comment: []const u8) ![]const u8 {
    var out: std.ArrayList(u8) = .{};
    errdefer out.deinit(allocator);

    var iter = std.mem.splitScalar(u8, comment, '\n');
    while (iter.next()) |ln| {
        const trimmed = std.mem.trimRight(u8, ln, " \t");
        if (trimmed.len == 0) {
            try out.appendSlice(allocator, "///\n");
        } else {
            try out.appendSlice(allocator, "/// ");
            try out.appendSlice(allocator, trimmed);
            try out.append(allocator, '\n');
        }
    }
    return out.toOwnedSlice(allocator);
}

/// Counts the number of lines in a text buffer, returning a u32 value.
fn countLines(text: []const u8) u32 {
    var n: u32 = 0;
    for (text) |c| if (c == '\n') {
        n += 1;
    };
    return n;
}

/// Returns an empty InsertResult with default values when no data is provided.
fn emptyResult(allocator: std.mem.Allocator) !InsertResult {
    return .{
        .changed = false,
        .new_source = try allocator.dupe(u8, ""),
        .line_adjustments = try allocator.alloc(LineAdjustment, 0),
    };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "insertComment - basic" {
    const allocator = std.testing.allocator;
    const source =
        \\const std = @import("std");
        \\
        \\pub fn foo() void {}
    ;
    const result = try insertComment(allocator, source, 3, "Does something.");
    defer result.deinit(allocator);

    try std.testing.expect(result.changed);
    try std.testing.expect(std.mem.indexOf(u8, result.new_source, "/// Does something.") != null);
    try std.testing.expectEqual(@as(u32, 3), result.line_adjustments[0].old_line);
    try std.testing.expectEqual(@as(u32, 4), result.line_adjustments[0].new_line);
}

test "insertComment - line beyond source" {
    const allocator = std.testing.allocator;
    const result = try insertComment(allocator, "x", 99, "comment");
    defer result.deinit(allocator);
    try std.testing.expect(!result.changed);
}

test "replaceComment - existing comment replaced" {
    const allocator = std.testing.allocator;
    const source =
        \\const std = @import("std");
        \\/// Old comment.
        \\pub fn foo() void {}
    ;
    const result = try replaceComment(allocator, source, 3, "New comment.");
    defer result.deinit(allocator);

    try std.testing.expect(result.changed);
    try std.testing.expect(std.mem.indexOf(u8, result.new_source, "/// New comment.") != null);
    try std.testing.expect(std.mem.indexOf(u8, result.new_source, "Old comment") == null);
}

test "extractCommentAtLine - finds comment" {
    const allocator = std.testing.allocator;
    const source =
        \\/// First line.
        \\/// Second line.
        \\pub fn foo() void {}
    ;
    const comment = try extractCommentAtLine(allocator, source, 3);
    defer if (comment) |c| allocator.free(c);

    try std.testing.expect(comment != null);
    try std.testing.expectEqualStrings("First line.\nSecond line.", comment.?);
}

test "extractCommentAtLine - no comment" {
    const allocator = std.testing.allocator;
    const source = "pub fn foo() void {}\n";
    const comment = try extractCommentAtLine(allocator, source, 1);
    defer if (comment) |c| allocator.free(c);
    try std.testing.expect(comment == null);
}

test "formatDocComment - multi-line" {
    const allocator = std.testing.allocator;
    const formatted = try formatDocComment(allocator, "Line one.\nLine two.");
    defer allocator.free(formatted);
    try std.testing.expectEqualStrings("/// Line one.\n/// Line two.\n", formatted);
}
