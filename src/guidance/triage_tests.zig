//! Tests for triage.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const triage_mod = @import("triage.zig");

test "assessRisk high for delete" {
    const risk = triage_mod.assessRisk("We need to delete the old auth module", 2);
    try std.testing.expect(std.mem.startsWith(u8, risk, "**High**"));
}
test "assessRisk medium for refactor" {
    const risk = triage_mod.assessRisk("Refactor the sync logic for clarity", 3);
    try std.testing.expect(std.mem.startsWith(u8, risk, "**Medium**"));
}
test "assessRisk medium for wide scope" {
    const risk = triage_mod.assessRisk("Add logging", 8);
    try std.testing.expect(std.mem.startsWith(u8, risk, "**Medium**"));
}
test "assessRisk low for simple change" {
    const risk = triage_mod.assessRisk("Add a new unit test for the parser", 1);
    try std.testing.expect(std.mem.startsWith(u8, risk, "**Low**"));
}
test "findAffectedFiles detects backtick paths" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    // Create src/foo.zig in temp dir.
    try tmp.dir.makePath("src");
    const sf = try tmp.dir.createFile("src/foo.zig", .{});
    sf.close();

    const content = "Update `src/foo.zig` to add logging support";
    const files = try triage_mod.findAffectedFiles(allocator, content, tmp_path);
    defer {
        for (files) |f| allocator.free(f);
        allocator.free(files);
    }
    try std.testing.expect(files.len >= 1);
    try std.testing.expectEqualStrings("src/foo.zig", files[0]);
}
