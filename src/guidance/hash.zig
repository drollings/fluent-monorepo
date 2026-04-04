const std = @import("std");
const types = @import("types.zig");
const common = @import("common");

/// SHA-256 hex digest — delegates to src/common/hash.zig.
pub const sha256Hex = common.sha256Hex;

/// Computes a hash from allocator, name, and parameters, returning the resulting hash value.
pub fn apiHash(allocator: std.mem.Allocator, name: []const u8, params: []const types.Param, returns: ?[]const u8) ![]const u8 {
    var sig_buf: std.ArrayList(u8) = .{};
    defer sig_buf.deinit(allocator);
    const writer = sig_buf.writer(allocator);

    try writer.writeAll(name);
    try writer.writeByte('(');
    for (params, 0..) |param, i| {
        if (i > 0) try writer.writeByte(',');
        try writer.writeAll(param.name);
        try writer.writeByte(':');
        try writer.writeAll(param.type orelse "anytype");
    }
    try writer.writeAll(")->");
    try writer.writeAll(normalizeType(returns orelse "void"));

    return sha256Hex(allocator, sig_buf.items);
}

/// Generates a hash for a given Zig struct using its allocator and base data.
pub fn structHash(allocator: std.mem.Allocator, name: []const u8, bases: []const []const u8) ![]const u8 {
    var sig_buf: std.ArrayList(u8) = .{};
    defer sig_buf.deinit(allocator);
    const writer = sig_buf.writer(allocator);

    try writer.writeAll(name);
    try writer.writeByte('(');
    for (bases, 0..) |base, i| {
        if (i > 0) try writer.writeByte(',');
        try writer.writeAll(base);
    }
    try writer.writeByte(')');

    return sha256Hex(allocator, sig_buf.items);
}

/// Compute match_hash for a member: includes signature and comment.
/// When comment changes, match_hash changes, triggering regeneration check.
/// Format: SHA-256(signature || "|||COMMENT|||" || comment) if comment present,
///         SHA-256(signature) otherwise.
pub fn computeMemberHash(allocator: std.mem.Allocator, member: types.Member) ![]const u8 {
    const sig = member.signature orelse member.name;

    if (member.comment) |c| {
        if (c.len > 0) {
            var buf: std.ArrayList(u8) = .{};
            defer buf.deinit(allocator);
            const writer = buf.writer(allocator);

            try writer.writeAll(sig);
            try writer.writeAll("|||COMMENT|||");
            try writer.writeAll(c);

            return sha256Hex(allocator, buf.items);
        }
    }

    return sha256Hex(allocator, sig);
}

/// Converts a null-terminated C string into a normalized Zig type slice.
pub fn normalizeType(type_str: []const u8) []const u8 {
    const trimmed = std.mem.trim(u8, type_str, " \t\n\r");
    if (trimmed.len == 0) return "void";
    return trimmed;
}

test "sha256Hex produces correct length" {
    const result = try sha256Hex(std.testing.allocator, "test");
    defer std.testing.allocator.free(result);
    try std.testing.expectEqual(@as(usize, 64), result.len);
}

test "apiHash is deterministic" {
    const params1 = [_]types.Param{
        .{ .name = "x", .type = "f64" },
        .{ .name = "y", .type = "f64" },
    };
    const hash1 = try apiHash(std.testing.allocator, "add", &params1, "f64");
    defer std.testing.allocator.free(hash1);

    const hash2 = try apiHash(std.testing.allocator, "add", &params1, "f64");
    defer std.testing.allocator.free(hash2);

    try std.testing.expectEqualSlices(u8, hash1, hash2);
}

test "apiHash differs for different names" {
    const params = [_]types.Param{};
    const hash1 = try apiHash(std.testing.allocator, "foo", &params, null);
    defer std.testing.allocator.free(hash1);

    const hash2 = try apiHash(std.testing.allocator, "bar", &params, null);
    defer std.testing.allocator.free(hash2);

    try std.testing.expect(!std.mem.eql(u8, hash1, hash2));
}

test "structHash differs when field names change" {
    const fields1 = [_][]const u8{ "x", "y" };
    const hash1 = try structHash(std.testing.allocator, "Vec2", &fields1);
    defer std.testing.allocator.free(hash1);

    // Adding a field changes the hash.
    const fields2 = [_][]const u8{ "x", "y", "z" };
    const hash2 = try structHash(std.testing.allocator, "Vec2", &fields2);
    defer std.testing.allocator.free(hash2);

    try std.testing.expect(!std.mem.eql(u8, hash1, hash2));

    // Renaming a field changes the hash.
    const fields3 = [_][]const u8{ "a", "y" };
    const hash3 = try structHash(std.testing.allocator, "Vec2", &fields3);
    defer std.testing.allocator.free(hash3);

    try std.testing.expect(!std.mem.eql(u8, hash1, hash3));
}

test "structHash same fields produce same hash" {
    const fields = [_][]const u8{ "x", "y" };
    const hash1 = try structHash(std.testing.allocator, "Vec2", &fields);
    defer std.testing.allocator.free(hash1);

    const hash2 = try structHash(std.testing.allocator, "Vec2", &fields);
    defer std.testing.allocator.free(hash2);

    try std.testing.expectEqualSlices(u8, hash1, hash2);
}

test "computeMemberHash without comment produces hash of signature" {
    const member = types.Member{
        .type = .fn_decl,
        .name = "myFunc",
        .signature = "fn myFunc(x: i32) i32",
        .comment = null,
    };
    const hash = try computeMemberHash(std.testing.allocator, member);
    defer std.testing.allocator.free(hash);

    // Should be equal to hash of signature alone
    const sig_hash = try sha256Hex(std.testing.allocator, member.signature.?);
    defer std.testing.allocator.free(sig_hash);

    try std.testing.expectEqualSlices(u8, hash, sig_hash);
}

test "computeMemberHash with comment differs from signature-only hash" {
    const member_no_comment = types.Member{
        .type = .fn_decl,
        .name = "myFunc",
        .signature = "fn myFunc(x: i32) i32",
        .comment = null,
    };
    const member_with_comment = types.Member{
        .type = .fn_decl,
        .name = "myFunc",
        .signature = "fn myFunc(x: i32) i32",
        .comment = "Computes the result.",
    };

    const hash1 = try computeMemberHash(std.testing.allocator, member_no_comment);
    defer std.testing.allocator.free(hash1);

    const hash2 = try computeMemberHash(std.testing.allocator, member_with_comment);
    defer std.testing.allocator.free(hash2);

    // Hashes should differ because one has a comment
    try std.testing.expect(!std.mem.eql(u8, hash1, hash2));
}

test "computeMemberHash same comment produces same hash" {
    const member1 = types.Member{
        .type = .fn_decl,
        .name = "myFunc",
        .signature = "fn myFunc(x: i32) i32",
        .comment = "Does something.",
    };
    const member2 = types.Member{
        .type = .fn_decl,
        .name = "myFunc",
        .signature = "fn myFunc(x: i32) i32",
        .comment = "Does something.",
    };

    const hash1 = try computeMemberHash(std.testing.allocator, member1);
    defer std.testing.allocator.free(hash1);

    const hash2 = try computeMemberHash(std.testing.allocator, member2);
    defer std.testing.allocator.free(hash2);

    try std.testing.expectEqualSlices(u8, hash1, hash2);
}

test "computeMemberHash different comments produce different hashes" {
    const member1 = types.Member{
        .type = .fn_decl,
        .name = "myFunc",
        .signature = "fn myFunc(x: i32) i32",
        .comment = "First comment.",
    };
    const member2 = types.Member{
        .type = .fn_decl,
        .name = "myFunc",
        .signature = "fn myFunc(x: i32) i32",
        .comment = "Second comment.",
    };

    const hash1 = try computeMemberHash(std.testing.allocator, member1);
    defer std.testing.allocator.free(hash1);

    const hash2 = try computeMemberHash(std.testing.allocator, member2);
    defer std.testing.allocator.free(hash2);

    try std.testing.expect(!std.mem.eql(u8, hash1, hash2));
}
