//! Hash utilities for guidance — computes stable hashes for API signatures and struct members.
const std = @import("std");
const common = @import("common");

const sha256Hex = common.sha256Hex;

/// Computes a stable match_hash from the member's signature string.
/// Using the signature directly avoids redundant structured params in JSON —
/// signature is the canonical human-readable form and is already stored.
pub fn signatureHash(allocator: std.mem.Allocator, signature: []const u8) ![]const u8 {
    return sha256Hex(allocator, signature);
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
