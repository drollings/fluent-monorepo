//! Tests for core.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const types = @import("../types.zig");
const core_mod = @import("core.zig");

test "parseDocComment - simple single line" {
    const allocator = std.testing.allocator;
    const raw = "Parse a JSON document and return the root value.";
    const dc = try core_mod.parseDocComment(allocator, raw);
    defer dc.deinit(allocator);

    try std.testing.expectEqualStrings(raw, dc.text);
    try std.testing.expectEqualStrings(raw, dc.summary);
    try std.testing.expect(dc.is_well_formed);
}
test "parseDocComment - multi-line" {
    const allocator = std.testing.allocator;
    const raw = "First line.\nSecond line.";
    const dc = try core_mod.parseDocComment(allocator, raw);
    defer dc.deinit(allocator);

    try std.testing.expectEqualStrings("First line.", dc.summary);
    try std.testing.expect(dc.is_well_formed);
}
test "isWellFormedComment - empty" {
    try std.testing.expect(!core_mod.isWellFormedComment("", ""));
}
test "isWellFormedComment - too long" {
    var long: [401]u8 = undefined;
    @memset(&long, 'x');
    try std.testing.expect(!core_mod.isWellFormedComment(&long, ""));
}
test "isWellFormedComment - leaked prompt" {
    try std.testing.expect(!core_mod.isWellFormedComment("Let me write a comment for this", ""));
    try std.testing.expect(!core_mod.isWellFormedComment("We need to write something", ""));
}
test "isWellFormedComment - TODO marker" {
    try std.testing.expect(!core_mod.isWellFormedComment("TODO: implement this", ""));
}
test "isWellFormedComment - valid" {
    try std.testing.expect(core_mod.isWellFormedComment("Returns the sum of all elements.", ""));
}
test "checkCommentStaleness - valid comment" {
    const member = types.Member{ .type = .fn_decl, .name = "foo" };
    const result = core_mod.checkCommentStaleness("Parse the input and return a result.", member, "");
    try std.testing.expect(!result.needs_regeneration);
}
test "checkCommentStaleness - empty comment" {
    const member = types.Member{ .type = .fn_decl, .name = "foo" };
    const result = core_mod.checkCommentStaleness("", member, "");
    try std.testing.expect(result.needs_regeneration);
}
test "checkCommentStaleness - leaked prompt" {
    const member = types.Member{ .type = .fn_decl, .name = "foo" };
    const result = core_mod.checkCommentStaleness("Let me write a function summary.", member, "");
    try std.testing.expect(result.needs_regeneration);
}
test "checkCommentStaleness - too long" {
    var long: [401]u8 = undefined;
    @memset(&long, 'a');
    const member = types.Member{ .type = .fn_decl, .name = "foo" };
    const result = core_mod.checkCommentStaleness(&long, member, "");
    try std.testing.expect(result.needs_regeneration);
}
test "isHashStale - different hashes" {
    try std.testing.expect(core_mod.isHashStale("abc", "def"));
}
test "isHashStale - same hash" {
    try std.testing.expect(!core_mod.isHashStale("abc", "abc"));
}
test "isHashStale - null hash" {
    try std.testing.expect(!core_mod.isHashStale(null, "abc"));
    try std.testing.expect(!core_mod.isHashStale("abc", null));
}
test "isHashStale triggers regeneration path" {
    const stored_hash: ?[]const u8 = "oldhash_abc123";
    const current_hash: ?[]const u8 = "newhash_xyz789";
    try std.testing.expect(core_mod.isHashStale(stored_hash, current_hash));
    try std.testing.expect(!core_mod.isHashStale("samehash", "samehash"));
}
test "CommentCache - put and get" {
    const allocator = std.testing.allocator;
    var cache = core_mod.CommentCache.init(allocator);
    defer cache.deinit();

    try cache.put("src/foo.zig", "myFn", "abc123", "Does something useful.");
    const got = cache.get("src/foo.zig", "myFn", "abc123");
    try std.testing.expect(got != null);
    try std.testing.expectEqualStrings("Does something useful.", got.?);
}
test "CommentCache - stale hash returns null" {
    const allocator = std.testing.allocator;
    var cache = core_mod.CommentCache.init(allocator);
    defer cache.deinit();

    try cache.put("src/foo.zig", "myFn", "abc123", "Old comment.");
    const got = cache.get("src/foo.zig", "myFn", "def456");
    try std.testing.expect(got == null);
}
test "CommentCache - missing key returns null" {
    const allocator = std.testing.allocator;
    var cache = core_mod.CommentCache.init(allocator);
    defer cache.deinit();

    const got = cache.get("src/missing.zig", "nope", "hash");
    try std.testing.expect(got == null);
}
test "CommentCache - isValid" {
    const allocator = std.testing.allocator;
    var cache = core_mod.CommentCache.init(allocator);
    defer cache.deinit();

    try cache.put("src/bar.zig", "init", "h1", "Initialise the struct.");
    try std.testing.expect(cache.isValid("src/bar.zig", "init", "h1"));
    try std.testing.expect(!cache.isValid("src/bar.zig", "init", "h2"));
}
