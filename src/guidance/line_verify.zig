/// line_verify.zig — Declaration-level line number verification for guidance.
///
/// Detects stale line numbers stored in JSON by checking whether a member's
/// declaration actually appears at its recorded position, and searching the
/// file for the current location when it does not.
const std = @import("std");
const types = @import("types.zig");

/// Result of verifying a member's line number against the source file.
pub const VerificationResult = struct {
    /// Line number is correct (declaration found at recorded position).
    verified: bool,
    /// Corrected line number when the recorded position is stale.
    corrected_line: ?u32 = null,
    /// Short snippet of the found declaration (for diagnostics).
    found_snippet: ?[]const u8 = null,

    pub fn deinit(self: VerificationResult, allocator: std.mem.Allocator) void {
        if (self.found_snippet) |s| allocator.free(s);
    }
};

/// Verify that `member.line` points to the member's declaration in `source`.
/// When the recorded line is stale, the file is searched for the correct line.
///
/// `source` must be the full contents of the source file (not null-terminated).
pub fn verifyMemberLine(
    allocator: std.mem.Allocator,
    source: []const u8,
    member: types.Member,
) !VerificationResult {
    const recorded_line = member.line orelse return .{ .verified = false };

    // Build the patterns we look for depending on member type.
    const name = member.name;

    // At the recorded line, check if the declaration matches.
    if (lineMatchesDecl(source, recorded_line, name, member.type)) {
        return .{ .verified = true };
    }

    // Line is stale — search the whole file for the declaration.
    if (try searchForDecl(allocator, source, name, member.type)) |res| {
        return res;
    }

    return .{ .verified = false };
}

/// Return true when the 1-based `line_no` in `source` contains a declaration
/// for `name` of the given `member_type`.
fn lineMatchesDecl(source: []const u8, line_no: u32, name: []const u8, member_type: types.MemberType) bool {
    var line_iter = std.mem.splitScalar(u8, source, '\n');
    var current_line: u32 = 1;
    while (line_iter.next()) |line| : (current_line += 1) {
        if (current_line != line_no) continue;
        return declPatternInLine(line, name, member_type);
    }
    return false;
}

/// Return true when `line` contains a declaration pattern for `name`.
fn declPatternInLine(line: []const u8, name: []const u8, member_type: types.MemberType) bool {
    return switch (member_type) {
        .fn_decl, .fn_private, .method, .method_private => blk: {
            // Look for "fn name(" anywhere in the line.
            var buf: [256]u8 = undefined;
            const pattern = std.fmt.bufPrint(&buf, "fn {s}(", .{name}) catch break :blk false;
            break :blk std.mem.indexOf(u8, line, pattern) != null;
        },
        .@"struct" => blk: {
            var buf: [256]u8 = undefined;
            const pattern = std.fmt.bufPrint(&buf, "{s} =", .{name}) catch break :blk false;
            break :blk std.mem.indexOf(u8, line, pattern) != null and
                std.mem.indexOf(u8, line, "struct") != null;
        },
        .@"enum" => blk: {
            var buf: [256]u8 = undefined;
            const pattern = std.fmt.bufPrint(&buf, "{s} =", .{name}) catch break :blk false;
            break :blk std.mem.indexOf(u8, line, pattern) != null and
                std.mem.indexOf(u8, line, "enum") != null;
        },
        .@"union" => blk: {
            var buf: [256]u8 = undefined;
            const pattern = std.fmt.bufPrint(&buf, "{s} =", .{name}) catch break :blk false;
            break :blk std.mem.indexOf(u8, line, pattern) != null and
                std.mem.indexOf(u8, line, "union") != null;
        },
        .enum_field => blk: {
            // Enum fields appear as bare identifiers; check for exact word match.
            break :blk containsWord(line, name);
        },
        .test_decl => blk: {
            var buf: [256]u8 = undefined;
            const pattern = std.fmt.bufPrint(&buf, "\"{s}\"", .{name}) catch break :blk false;
            break :blk std.mem.indexOf(u8, line, "test") != null and
                std.mem.indexOf(u8, line, pattern) != null;
        },
        .comptime_block => false,
    };
}

/// Returns true when `line` contains `word` as a whole word (surrounded by
/// non-alphanumeric/underscore characters or at the line boundaries).
fn containsWord(line: []const u8, word: []const u8) bool {
    var start: usize = 0;
    while (std.mem.indexOf(u8, line[start..], word)) |rel| {
        const abs = start + rel;
        const before_ok = abs == 0 or !isIdentChar(line[abs - 1]);
        const end = abs + word.len;
        const after_ok = end >= line.len or !isIdentChar(line[end]);
        if (before_ok and after_ok) return true;
        start = abs + 1;
        if (start >= line.len) break;
    }
    return false;
}

/// Checks if a byte is a valid identifier character in Zig.
fn isIdentChar(c: u8) bool {
    return std.ascii.isAlphanumeric(c) or c == '_';
}

/// Search `source` line-by-line for a declaration matching `name`/`member_type`.
/// Returns a `VerificationResult` with `corrected_line` set, or null if not found.
fn searchForDecl(
    allocator: std.mem.Allocator,
    source: []const u8,
    name: []const u8,
    member_type: types.MemberType,
) !?VerificationResult {
    var line_iter = std.mem.splitScalar(u8, source, '\n');
    var current_line: u32 = 1;
    while (line_iter.next()) |line| : (current_line += 1) {
        if (!declPatternInLine(line, name, member_type)) continue;

        const trimmed = std.mem.trim(u8, line, " \t");
        const snippet = try allocator.dupe(u8, trimmed[0..@min(trimmed.len, 120)]);

        return .{
            .verified = false,
            .corrected_line = current_line,
            .found_snippet = snippet,
        };
    }
    return null;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "verifyMemberLine - correct line" {
    const allocator = std.testing.allocator;
    const source =
        \\const std = @import("std");
        \\
        \\pub fn foo(x: u32) u32 {
        \\    return x;
        \\}
    ;
    const member = types.Member{
        .type = .fn_decl,
        .name = "foo",
        .line = 3,
    };
    const result = try verifyMemberLine(allocator, source, member);
    defer result.deinit(allocator);
    try std.testing.expect(result.verified);
}

test "verifyMemberLine - stale line number" {
    const allocator = std.testing.allocator;
    const source =
        \\const std = @import("std");
        \\
        \\// some comment added above
        \\pub fn foo(x: u32) u32 {
        \\    return x;
        \\}
    ;
    // Member thinks it's at line 3, but it's actually at line 4.
    const member = types.Member{
        .type = .fn_decl,
        .name = "foo",
        .line = 3,
    };
    const result = try verifyMemberLine(allocator, source, member);
    defer result.deinit(allocator);
    try std.testing.expect(!result.verified);
    try std.testing.expectEqual(@as(?u32, 4), result.corrected_line);
}

test "verifyMemberLine - member not found" {
    const allocator = std.testing.allocator;
    const source =
        \\const std = @import("std");
    ;
    const member = types.Member{
        .type = .fn_decl,
        .name = "missingFn",
        .line = 1,
    };
    const result = try verifyMemberLine(allocator, source, member);
    defer result.deinit(allocator);
    try std.testing.expect(!result.verified);
    try std.testing.expect(result.corrected_line == null);
}

test "verifyMemberLine - struct detection" {
    const allocator = std.testing.allocator;
    const source =
        \\const std = @import("std");
        \\
        \\pub const MyStruct = struct {
        \\    x: u32,
        \\};
    ;
    const member = types.Member{
        .type = .@"struct",
        .name = "MyStruct",
        .line = 3,
    };
    const result = try verifyMemberLine(allocator, source, member);
    defer result.deinit(allocator);
    try std.testing.expect(result.verified);
}

test "verifyMemberLine - no line recorded" {
    const allocator = std.testing.allocator;
    const source = "pub fn foo() void {}";
    const member = types.Member{
        .type = .fn_decl,
        .name = "foo",
        .line = null,
    };
    const result = try verifyMemberLine(allocator, source, member);
    defer result.deinit(allocator);
    try std.testing.expect(!result.verified);
}
