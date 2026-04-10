//! Tests for hash.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const hash_mod = @import("hash.zig");

test "sha256Hex produces correct length" {
    const result = try hash_mod.sha256Hex(std.testing.allocator, "test");
    defer std.testing.allocator.free(result);
    try std.testing.expectEqual(@as(usize, 64), result.len);
}
test "sha256Hex is deterministic" {
    const h1 = try hash_mod.sha256Hex(std.testing.allocator, "hello");
    defer std.testing.allocator.free(h1);
    const h2 = try hash_mod.sha256Hex(std.testing.allocator, "hello");
    defer std.testing.allocator.free(h2);
    try std.testing.expectEqualSlices(u8, h1, h2);
}
test "sha256Hex differs for different inputs" {
    const h1 = try hash_mod.sha256Hex(std.testing.allocator, "foo");
    defer std.testing.allocator.free(h1);
    const h2 = try hash_mod.sha256Hex(std.testing.allocator, "bar");
    defer std.testing.allocator.free(h2);
    try std.testing.expect(!std.mem.eql(u8, h1, h2));
}
test "contentHashWithModel is deterministic and model-sensitive" {
    const h1 = hash_mod.contentHashWithModel("text", "model-a");
    const h2 = hash_mod.contentHashWithModel("text", "model-a");
    const h3 = hash_mod.contentHashWithModel("text", "model-b");
    try std.testing.expectEqualSlices(u8, &h1, &h2);
    try std.testing.expect(!std.mem.eql(u8, &h1, &h3));
}
test "hashString: sha256" {
    const result = try hash_mod.hashString(std.testing.allocator, "hello", .sha256);
    defer std.testing.allocator.free(result);
    try std.testing.expectEqual(@as(usize, 64), result.len);
}
test "hashString: blake3" {
    const result = try hash_mod.hashString(std.testing.allocator, "hello", .blake3);
    defer std.testing.allocator.free(result);
    try std.testing.expectEqual(@as(usize, 64), result.len);
}
test "blake3Hash returns 32 bytes" {
    const h = hash_mod.blake3Hash("hello");
    try std.testing.expectEqual(@as(usize, 32), h.len);
}
test "blake3Hex produces 64-char hex" {
    const h = try hash_mod.blake3Hex(std.testing.allocator, "hello");
    defer std.testing.allocator.free(h);
    try std.testing.expectEqual(@as(usize, 64), h.len);
}
test "HashState incremental matches single-pass sha256" {
    var state = hash_mod.HashState.init(.sha256);
    state.update("hello");
    state.update(" ");
    state.update("world");
    const inc = try state.final(std.testing.allocator);
    defer std.testing.allocator.free(inc);

    var single = hash_mod.HashState.init(.sha256);
    single.update("hello world");
    const one = try single.final(std.testing.allocator);
    defer std.testing.allocator.free(one);

    try std.testing.expectEqualStrings(one, inc);
}
test "HashAlgorithm digestLength" {
    try std.testing.expectEqual(@as(usize, 32), hash_mod.HashAlgorithm.sha256.digestLength());
    try std.testing.expectEqual(@as(usize, 64), hash_mod.HashAlgorithm.sha512.digestLength());
    try std.testing.expectEqual(@as(usize, 32), hash_mod.HashAlgorithm.blake3.digestLength());
}
