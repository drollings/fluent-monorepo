//! Tests for types.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const types_mod = @import("types.zig");

test "FileType.fromExtension: zig extension maps to source" {
    try std.testing.expectEqual(types_mod.FileType.source, types_mod.FileType.fromExtension(".zig"));
}
test "FileType.fromExtension: py extension maps to source" {
    try std.testing.expectEqual(types_mod.FileType.source, types_mod.FileType.fromExtension(".py"));
}
test "FileType.fromExtension: md extension maps to markdown" {
    try std.testing.expectEqual(types_mod.FileType.markdown, types_mod.FileType.fromExtension(".md"));
}
test "FileType.fromExtension: json extension maps to config" {
    try std.testing.expectEqual(types_mod.FileType.config, types_mod.FileType.fromExtension(".json"));
}
test "FileType.fromExtension: txt extension maps to unknown" {
    try std.testing.expectEqual(types_mod.FileType.unknown, types_mod.FileType.fromExtension(".txt"));
}
test "FileType.fromExtension: case insensitive ZIG maps to source" {
    try std.testing.expectEqual(types_mod.FileType.source, types_mod.FileType.fromExtension(".ZIG"));
}
test "FileType.toStr: source round-trip" {
    try std.testing.expectEqualStrings("source", types_mod.FileType.source.toStr());
}
test "FileType.toStr: markdown round-trip" {
    try std.testing.expectEqualStrings("markdown", types_mod.FileType.markdown.toStr());
}
test "FileType.toStr: config round-trip" {
    try std.testing.expectEqualStrings("config", types_mod.FileType.config.toStr());
}
test "FileType.toStr: unknown round-trip" {
    try std.testing.expectEqualStrings("unknown", types_mod.FileType.unknown.toStr());
}
test "freeStages: no leak on empty slice" {
    const allocator = std.testing.allocator;
    const empty: []const types_mod.Stage = &.{};
    types_mod.freeStages(allocator, empty);
}
test "freeStages: frees all stage content" {
    const allocator = std.testing.allocator;
    var stages = try allocator.alloc(types_mod.Stage, 2);
    stages[0] = .{
        .kind = .prose,
        .content = try allocator.dupe(u8, "content one"),
        .source = try allocator.dupe(u8, "src/foo.zig"),
    };
    stages[1] = .{
        .kind = .code,
        .content = try allocator.dupe(u8, "content two"),
        .source = try allocator.dupe(u8, "src/bar.zig"),
        .line = 42,
    };
    types_mod.freeStages(allocator, stages);
    allocator.free(stages);
}
test "jsonifyMember: minimal fn_decl contains expected JSON fields" {
    const allocator = std.testing.allocator;
    const member = types_mod.Member{
        .type = .fn_decl,
        .name = "myFunc",
        .is_pub = true,
    };
    const json = try types_mod.jsonifyMember(allocator, member);
    defer if (json) |j| allocator.free(j);
    try std.testing.expect(json != null);
    const j = json.?;
    try std.testing.expect(std.mem.indexOf(u8, j, "\"type\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, j, "\"fn_decl\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, j, "\"name\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, j, "\"myFunc\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, j, "\"is_pub\"") != null);
    // Validate as JSON
    const parsed = try std.json.parseFromSlice(std.json.Value, allocator, j, .{});
    defer parsed.deinit();
}
test "jsonifyMember: special chars are escaped in name" {
    const allocator = std.testing.allocator;
    const member = types_mod.Member{
        .type = .fn_decl,
        .name = "say\"hello\\world\nnew",
        .is_pub = false,
    };
    const json = try types_mod.jsonifyMember(allocator, member);
    defer if (json) |j| allocator.free(j);
    try std.testing.expect(json != null);
    const j = json.?;
    try std.testing.expect(std.mem.indexOf(u8, j, "\\\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, j, "\\\\") != null);
    try std.testing.expect(std.mem.indexOf(u8, j, "\\n") != null);
    // Validate as JSON
    const parsed = try std.json.parseFromSlice(std.json.Value, allocator, j, .{});
    defer parsed.deinit();
}
test "jsonifyMember: with line field included" {
    const allocator = std.testing.allocator;
    const member = types_mod.Member{
        .type = .fn_decl,
        .name = "lineFunc",
        .is_pub = true,
        .line = 42,
    };
    const json = try types_mod.jsonifyMember(allocator, member);
    defer if (json) |j| allocator.free(j);
    try std.testing.expect(json != null);
    const j = json.?;
    try std.testing.expect(std.mem.indexOf(u8, j, "\"line\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, j, "42") != null);
    // Validate as JSON
    const parsed = try std.json.parseFromSlice(std.json.Value, allocator, j, .{});
    defer parsed.deinit();
}
test "jsonifyMember: with params" {
    const allocator = std.testing.allocator;
    const params = [_]types_mod.Param{
        .{ .name = "alloc", .type = "std.mem.Allocator" },
        .{ .name = "val", .type = "u32", .default = "0" },
    };
    const member = types_mod.Member{
        .type = .fn_decl,
        .name = "withParams",
        .is_pub = true,
        .params = &params,
    };
    const json = try types_mod.jsonifyMember(allocator, member);
    defer if (json) |j| allocator.free(j);
    try std.testing.expect(json != null);
    const j = json.?;
    try std.testing.expect(std.mem.indexOf(u8, j, "\"params\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, j, "\"alloc\"") != null);
    // Validate as JSON
    const parsed = try std.json.parseFromSlice(std.json.Value, allocator, j, .{});
    defer parsed.deinit();
}
test "jsonifyMember: no trailing comma when no members and no line" {
    const allocator = std.testing.allocator;
    const member = types_mod.Member{
        .type = .fn_decl,
        .name = "bare",
        .is_pub = false,
    };
    const json = try types_mod.jsonifyMember(allocator, member);
    defer if (json) |j| allocator.free(j);
    try std.testing.expect(json != null);
    const j = json.?;
    // Must parse as valid JSON (no trailing comma)
    const parsed = try std.json.parseFromSlice(std.json.Value, allocator, j, .{});
    defer parsed.deinit();
}
test "jsonifyMember: member comment NOT written to JSON" {
    const allocator = std.testing.allocator;
    const member = types_mod.Member{
        .type = .fn_decl,
        .name = "documentedFunc",
        .is_pub = true,
        .signature = "fn documentedFunc() void",
        .comment = "This is a doc comment that should NOT appear in JSON.",
        .match_hash = "abc123",
    };
    const json = try types_mod.jsonifyMember(allocator, member);
    defer if (json) |j| allocator.free(j);
    try std.testing.expect(json != null);
    const j = json.?;
    // Verify the comment field is NOT present in JSON
    try std.testing.expect(std.mem.indexOf(u8, j, "\"comment\"") == null);
    try std.testing.expect(std.mem.indexOf(u8, j, "This is a doc comment") == null);
    // Verify other fields ARE present
    try std.testing.expect(std.mem.indexOf(u8, j, "\"name\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, j, "\"documentedFunc\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, j, "\"match_hash\"") != null);
    // Parse as valid JSON
    const parsed = try std.json.parseFromSlice(std.json.Value, allocator, j, .{});
    defer parsed.deinit();
}
test "jsonifyGuidanceDoc: minimal doc with just meta" {
    const allocator = std.testing.allocator;
    const doc = types_mod.GuidanceDoc{
        .meta = .{
            .module = "mymod",
            .source = "src/mymod.zig",
        },
    };
    const json = try types_mod.jsonifyGuidanceDoc(allocator, doc);
    defer allocator.free(json);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"meta\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"mymod\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"src/mymod.zig\"") != null);
    const parsed = try std.json.parseFromSlice(std.json.Value, allocator, json, .{});
    defer parsed.deinit();
}
test "jsonifyGuidanceDoc: doc with comment and keywords" {
    const allocator = std.testing.allocator;
    const kws = [_][]const u8{ "alpha", "beta" };
    const doc = types_mod.GuidanceDoc{
        .meta = .{
            .module = "things",
            .source = "src/things.zig",
        },
        .comment = "Does things.",
        .keywords = &kws,
    };
    const json = try types_mod.jsonifyGuidanceDoc(allocator, doc);
    defer allocator.free(json);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"comment\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "Does things.") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"keywords\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"alpha\"") != null);
    const parsed = try std.json.parseFromSlice(std.json.Value, allocator, json, .{});
    defer parsed.deinit();
}
