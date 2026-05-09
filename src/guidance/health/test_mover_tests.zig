//! Tests for test_mover.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const test_mover_mod = @import("test_mover.zig");

test "test_mover: qualifyPubRefs transforms bare pub symbols" {
    const allocator = std.testing.allocator;
    var pub_names = std.StringHashMap(void).init(allocator);
    defer pub_names.deinit();
    try pub_names.put("myFunc", {});
    try pub_names.put("MyType", {});

    const src: [:0]const u8 =
        \\test "example" {
        \\    const x: MyType = myFunc();
        \\    _ = x.field; // field after '.' not qualified
        \\}
    ;
    const out = try test_mover_mod.qualifyPubRefs(allocator, src, &pub_names, "mod");
    defer allocator.free(out);

    try std.testing.expect(std.mem.indexOf(u8, out, "mod.myFunc") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "mod.MyType") != null);
    // Field access after '.' must NOT be qualified.
    try std.testing.expect(std.mem.indexOf(u8, out, "x.field") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "mod.field") == null);
}
test "test_mover: qualifyPubRefs does not double-qualify" {
    const allocator = std.testing.allocator;
    var pub_names = std.StringHashMap(void).init(allocator);
    defer pub_names.deinit();
    try pub_names.put("foo", {});

    // The identifier `foo` appears only once; it should be qualified exactly once.
    const src: [:0]const u8 = "test \"t\" { foo(); }\n";
    const out = try test_mover_mod.qualifyPubRefs(allocator, src, &pub_names, "m");
    defer allocator.free(out);
    try std.testing.expect(std.mem.indexOf(u8, out, "m.foo") != null);
    // Ensure "m.m.foo" does not appear.
    try std.testing.expect(std.mem.indexOf(u8, out, "m.m.foo") == null);
}
test "test_mover: fixAll moves tests from temp workspace" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realPathFileAlloc(std.testing.io, ".", allocator);
    defer allocator.free(workspace);

    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "widget.zig",
        .data =
        \\const std = @import("std");
        \\fn privHelper() bool { return true; }
        \\pub fn pubCreate() u32 { return 42; }
        \\
        \\test "widget: pubCreate returns 42" {
        \\    try std.testing.expectEqual(@as(u32, 42), pubCreate());
        \\}
        \\
        \\test "widget: uses private" {
        \\    _ = privHelper();
        \\}
        \\
        ,
    });

    const stats = try test_mover_mod.fixAll(allocator, workspace, false, null);

    try std.testing.expectEqual(@as(usize, 1), stats.tests_moved);
    try std.testing.expectEqual(@as(usize, 1), stats.tests_skipped);
    try std.testing.expectEqual(@as(usize, 1), stats.files_created);

    const tests_content = try tmp.dir.readFileAlloc(std.testing.io, "widget_tests.zig", allocator, .limited(64 * 1024));
    defer allocator.free(tests_content);
    // Moved test should be present, qualified.
    try std.testing.expect(std.mem.indexOf(u8, tests_content, "pubCreate returns 42") != null);
    try std.testing.expect(std.mem.indexOf(u8, tests_content, "widget_mod") != null);

    const src_content = try tmp.dir.readFileAlloc(std.testing.io, "widget.zig", allocator, .limited(64 * 1024));
    defer allocator.free(src_content);
    // Moved test must be gone from source.
    try std.testing.expect(std.mem.indexOf(u8, src_content, "pubCreate returns 42") == null);
    // Skipped test must still be in source.
    try std.testing.expect(std.mem.indexOf(u8, src_content, "uses private") != null);
}
test "test_mover: dry_run does not write files" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realPathFileAlloc(std.testing.io, ".", allocator);
    defer allocator.free(workspace);

    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "thing.zig",
        .data =
        \\const std = @import("std");
        \\pub fn add(a: u32, b: u32) u32 { return a + b; }
        \\test "add works" { try std.testing.expectEqual(@as(u32, 3), add(1, 2)); }
        \\
        ,
    });

    _ = try test_mover_mod.fixAll(allocator, workspace, true, null);

    const result = tmp.dir.access(std.testing.io, "thing_tests.zig", .{});
    try std.testing.expectError(error.FileNotFound, result);
}
