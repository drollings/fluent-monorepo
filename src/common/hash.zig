/// hash.zig — Generic cryptographic hashing utilities
///
/// Provides allocator-friendly SHA-256 helpers and a content+model hash
/// suitable for embedding cache keys.  All functions are free of
/// guidance-domain types so they can be reused by any Zig tool.
const std = @import("std");

/// Compute SHA-256 over `data` and return the digest as a 64-char lowercase
/// hex string.  Caller must free the returned slice.
pub fn sha256Hex(allocator: std.mem.Allocator, data: []const u8) ![]const u8 {
    var hash_out: [32]u8 = undefined;
    std.crypto.hash.sha2.Sha256.hash(data, &hash_out, .{});
    const hex = std.fmt.bytesToHex(hash_out, .lower);
    const result = try allocator.alloc(u8, hex.len);
    @memcpy(result, &hex);
    return result;
}

/// SHA-256-based 16-hex-char content+model hash.
///
/// Combines `model` and `content` so that the same content hashed under
/// different embedding models produces distinct keys — preventing stale
/// cache hits after a model swap.  Returns a fixed-size [16]u8 so the
/// caller needs no allocator.
pub fn contentHashWithModel(content: []const u8, model: []const u8) [16]u8 {
    var hasher = std.crypto.hash.sha2.Sha256.init(.{});
    hasher.update(model);
    hasher.update("\x00");
    hasher.update(content);
    var digest: [32]u8 = undefined;
    hasher.final(&digest);

    var result: [16]u8 = undefined;
    const hex_chars = "0123456789abcdef";
    for (0..8) |i| {
        result[i * 2] = hex_chars[digest[i] >> 4];
        result[i * 2 + 1] = hex_chars[digest[i] & 0x0f];
    }
    return result;
}

// =============================================================================
// Tests
// =============================================================================

test "sha256Hex produces correct length" {
    const result = try sha256Hex(std.testing.allocator, "test");
    defer std.testing.allocator.free(result);
    try std.testing.expectEqual(@as(usize, 64), result.len);
}

test "sha256Hex is deterministic" {
    const h1 = try sha256Hex(std.testing.allocator, "hello");
    defer std.testing.allocator.free(h1);
    const h2 = try sha256Hex(std.testing.allocator, "hello");
    defer std.testing.allocator.free(h2);
    try std.testing.expectEqualSlices(u8, h1, h2);
}

test "sha256Hex differs for different inputs" {
    const h1 = try sha256Hex(std.testing.allocator, "foo");
    defer std.testing.allocator.free(h1);
    const h2 = try sha256Hex(std.testing.allocator, "bar");
    defer std.testing.allocator.free(h2);
    try std.testing.expect(!std.mem.eql(u8, h1, h2));
}

test "contentHashWithModel is deterministic and model-sensitive" {
    const h1 = contentHashWithModel("text", "model-a");
    const h2 = contentHashWithModel("text", "model-a");
    const h3 = contentHashWithModel("text", "model-b");
    try std.testing.expectEqualSlices(u8, &h1, &h2);
    try std.testing.expect(!std.mem.eql(u8, &h1, &h3));
}
