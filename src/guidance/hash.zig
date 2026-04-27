//! Hash utilities for guidance — computes stable hashes for API signatures and struct members.
const std = @import("std");
const types = @import("types.zig");
const common = @import("common");

const sha256Hex = common.sha256Hex;

/// Computes a hash from allocator, name, and parameters, returning a slice of hash values.
pub fn apiHash(allocator: std.mem.Allocator, name: []const u8, params: []const types.Param, returns: ?[]const u8) ![]const u8 {
    var sig_buf: std.ArrayList(u8) = .empty;
    defer sig_buf.deinit(allocator);

    try sig_buf.appendSlice(allocator, name);
    try sig_buf.append(allocator, '(');
    for (params, 0..) |param, i| {
        if (i > 0) try sig_buf.append(allocator, ',');
        try sig_buf.appendSlice(allocator, param.name);
        try sig_buf.append(allocator, ':');
        try sig_buf.appendSlice(allocator, param.type orelse "anytype");
    }
    try sig_buf.appendSlice(allocator, ")->");
    try sig_buf.appendSlice(allocator, normalizeType(returns orelse "void"));

    return sha256Hex(allocator, sig_buf.items);
}

/// Generates a hash for a given Zig struct using its allocator and base data.
pub fn structHash(allocator: std.mem.Allocator, name: []const u8, bases: []const []const u8) ![]const u8 {
    var sig_buf: std.ArrayList(u8) = .empty;
    defer sig_buf.deinit(allocator);

    try sig_buf.appendSlice(allocator, name);
    try sig_buf.append(allocator, '(');
    for (bases, 0..) |base, i| {
        if (i > 0) try sig_buf.append(allocator, ',');
        try sig_buf.appendSlice(allocator, base);
    }
    try sig_buf.append(allocator, ')');

    return sha256Hex(allocator, sig_buf.items);
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
