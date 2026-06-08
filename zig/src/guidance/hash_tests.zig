//! Tests for hash.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const hash_mod = @import("hash.zig");

test "signatureHash is deterministic" {
    const hash1 = try hash_mod.signatureHash(std.testing.allocator, "fn add(x: f64, y: f64) -> f64");
    defer std.testing.allocator.free(hash1);

    const hash2 = try hash_mod.signatureHash(std.testing.allocator, "fn add(x: f64, y: f64) -> f64");
    defer std.testing.allocator.free(hash2);

    try std.testing.expectEqualSlices(u8, hash1, hash2);
}
test "signatureHash differs for different signatures" {
    const hash1 = try hash_mod.signatureHash(std.testing.allocator, "fn foo()");
    defer std.testing.allocator.free(hash1);

    const hash2 = try hash_mod.signatureHash(std.testing.allocator, "fn bar()");
    defer std.testing.allocator.free(hash2);

    try std.testing.expect(!std.mem.eql(u8, hash1, hash2));
}
test "structHash differs when field names change" {
    const fields1 = [_][]const u8{ "x", "y" };
    const hash1 = try hash_mod.structHash(std.testing.allocator, "Vec2", &fields1);
    defer std.testing.allocator.free(hash1);

    // Adding a field changes the hash.
    const fields2 = [_][]const u8{ "x", "y", "z" };
    const hash2 = try hash_mod.structHash(std.testing.allocator, "Vec2", &fields2);
    defer std.testing.allocator.free(hash2);

    try std.testing.expect(!std.mem.eql(u8, hash1, hash2));

    // Renaming a field changes the hash.
    const fields3 = [_][]const u8{ "a", "y" };
    const hash3 = try hash_mod.structHash(std.testing.allocator, "Vec2", &fields3);
    defer std.testing.allocator.free(hash3);

    try std.testing.expect(!std.mem.eql(u8, hash1, hash3));
}
test "structHash same fields produce same hash" {
    const fields = [_][]const u8{ "x", "y" };
    const hash1 = try hash_mod.structHash(std.testing.allocator, "Vec2", &fields);
    defer std.testing.allocator.free(hash1);

    const hash2 = try hash_mod.structHash(std.testing.allocator, "Vec2", &fields);
    defer std.testing.allocator.free(hash2);

    try std.testing.expectEqualSlices(u8, hash1, hash2);
}
