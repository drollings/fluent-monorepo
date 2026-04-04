/// comment_parser.zig — Doc comment parsing and quality validation for guidance.
///
/// Provides:
///   parseDocComment()      — parse raw comment text into DocComment struct
///   isWellFormedComment()  — validate comment against style guidelines
const std = @import("std");

/// Represents structured documentation comments; owned by the module; ensures consistent parsing and invariants.
pub const DocComment = struct {
    /// Raw text with `///` prefixes stripped and lines joined by newlines.
    text: []const u8,
    /// First line of the comment (summary, ≤ 200 chars).
    summary: []const u8,
    /// True when the comment passes the well-formed heuristics.
    is_well_formed: bool,
    /// Optional hash of the code this comment describes (for staleness tracking).
    code_hash: ?[]const u8 = null,

    /// Free all owned memory in this DocComment.
    pub fn deinit(self: DocComment, allocator: std.mem.Allocator) void {
        allocator.free(self.text);
        allocator.free(self.summary);
        if (self.code_hash) |h| allocator.free(h);
    }
};

/// Converts a raw C string into a DocComment object using an allocator.
pub fn parseDocComment(allocator: std.mem.Allocator, raw: []const u8) !DocComment {
    const text = try allocator.dupe(u8, raw);
    errdefer allocator.free(text);

    // Summary is the first non-empty line, capped at 200 chars.
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

/// Checks if a Zig comment slice matches expected format and returns true or false.
pub fn isWellFormedComment(comment: []const u8, code_context: []const u8) bool {
    _ = code_context;
    if (comment.len == 0 or comment.len > 400) return false;

    // Reject leaked LLM reasoning preambles.
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

    // Reject placeholder markers.
    if (std.mem.indexOf(u8, comment, "TODO") != null) return false;
    if (std.mem.indexOf(u8, comment, "FIXME") != null) return false;

    return true;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "parseDocComment - simple single line" {
    const allocator = std.testing.allocator;
    const raw = "Parse a JSON document and return the root value.";
    const dc = try parseDocComment(allocator, raw);
    defer dc.deinit(allocator);

    try std.testing.expectEqualStrings(raw, dc.text);
    try std.testing.expectEqualStrings(raw, dc.summary);
    try std.testing.expect(dc.is_well_formed);
}

test "parseDocComment - multi-line" {
    const allocator = std.testing.allocator;
    const raw = "First line.\nSecond line.";
    const dc = try parseDocComment(allocator, raw);
    defer dc.deinit(allocator);

    try std.testing.expectEqualStrings("First line.", dc.summary);
    try std.testing.expect(dc.is_well_formed);
}

test "isWellFormedComment - empty" {
    try std.testing.expect(!isWellFormedComment("", ""));
}

test "isWellFormedComment - too long" {
    var long: [401]u8 = undefined;
    @memset(&long, 'x');
    try std.testing.expect(!isWellFormedComment(&long, ""));
}

test "isWellFormedComment - leaked prompt" {
    try std.testing.expect(!isWellFormedComment("Let me write a comment for this", ""));
    try std.testing.expect(!isWellFormedComment("We need to write something", ""));
}

test "isWellFormedComment - TODO marker" {
    try std.testing.expect(!isWellFormedComment("TODO: implement this", ""));
}

test "isWellFormedComment - valid" {
    try std.testing.expect(isWellFormedComment("Returns the sum of all elements.", ""));
}
