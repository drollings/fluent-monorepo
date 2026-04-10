//! Tests for main.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const vector = @import("vector");
const common = @import("common");
const main_mod = @import("main.zig");

test "codehealth: parseCodehealthDirective re-export — ignore" {
    const dir = main_mod.parseCodehealthDirective("/// CODEHEALTH: ignore vtable-impl\n/// Invoked by reactor loop.").?;
    try std.testing.expectEqualStrings("vtable-impl", dir.ignore_reason);
}
test "codehealth: parseCodehealthDirective re-export — milestone" {
    const dir = main_mod.parseCodehealthDirective("/// CODEHEALTH: milestone v2.0\n/// Planned for distributed caching.").?;
    try std.testing.expectEqualStrings("v2.0", dir.milestone);
}
test "codehealth: parseCodehealthDirective re-export — deprecated" {
    const dir = main_mod.parseCodehealthDirective("/// CODEHEALTH: deprecated use searchOptimized instead").?;
    try std.testing.expectEqualStrings("use searchOptimized instead", dir.deprecated_by);
}
test "codehealth: parseCodehealthDirective no directive returns null" {
    try std.testing.expect(main_mod.parseCodehealthDirective("/// Regular doc comment") == null);
    try std.testing.expect(main_mod.parseCodehealthDirective("") == null);
}
test "codehealth: SimHash Hamming distance" {
    // Two identical hashes → distance 0.
    const h: u64 = 0xDEADBEEF_CAFEBABE;
    const dist0: u6 = @truncate(@popCount(@as(u64, @bitCast(h ^ h))));
    try std.testing.expectEqual(@as(u6, 0), dist0);

    // Flip one bit → distance 1.
    const h2 = h ^ 1;
    const dist1: u6 = @truncate(@popCount(@as(u64, @bitCast(h ^ h2))));
    try std.testing.expectEqual(@as(u6, 1), dist1);

    // Flip 3 bits → distance 3.
    const h3 = h ^ 0b111;
    const dist3: u6 = @truncate(@popCount(@as(u64, @bitCast(h ^ h3))));
    try std.testing.expectEqual(@as(u6, 3), dist3);
}
test "codehealth: findRedundantPairs empty DB returns empty slice" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    const db_path = try std.fmt.allocPrint(allocator, "{s}/test.ch.db", .{tmp_path});
    defer allocator.free(db_path);

    var noop: common.NoopEmbedding = .{};
    var db = try vector.GuidanceDb.init(allocator, db_path, noop.provider());
    defer db.deinit();

    const pairs = try main_mod.findRedundantPairs(allocator, &db, 3);
    defer allocator.free(pairs);
    try std.testing.expectEqual(@as(usize, 0), pairs.len);
}
test "codehealth: findUnusedModules empty DB returns empty slice" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    const db_path = try std.fmt.allocPrint(allocator, "{s}/test.ch2.db", .{tmp_path});
    defer allocator.free(db_path);

    var noop: common.NoopEmbedding = .{};
    var db = try vector.GuidanceDb.init(allocator, db_path, noop.provider());
    defer db.deinit();

    const unused = try db.findUnusedModules(allocator);
    defer {
        for (unused) |m| allocator.free(m.source);
        allocator.free(unused);
    }
    try std.testing.expectEqual(@as(usize, 0), unused.len);
}
