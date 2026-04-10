//! Tests for orphan.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const orphan_mod = @import("orphan.zig");

test "orphan: isTestFileName" {
    try std.testing.expect(orphan_mod.isTestFileName("tests.zig"));
    try std.testing.expect(orphan_mod.isTestFileName("foo_test.zig"));
    try std.testing.expect(orphan_mod.isTestFileName("foo_tests.zig"));
    try std.testing.expect(!orphan_mod.isTestFileName("foo.zig"));
    try std.testing.expect(!orphan_mod.isTestFileName("test_helper.zig"));
}
test "orphan: hasPubFnMain" {
    try std.testing.expect(orphan_mod.hasPubFnMain("pub fn main() void {}"));
    try std.testing.expect(!orphan_mod.hasPubFnMain("fn helper() void {}"));
}
test "orphan: hasEntryPointMarker" {
    try std.testing.expect(orphan_mod.hasEntryPointMarker("// CODEHEALTH: entry-point\npub fn foo() void {}"));
    try std.testing.expect(!orphan_mod.hasEntryPointMarker("// just a comment\n"));
}
test "orphan: extractBuildZigRoots finds b.path strings" {
    const allocator = std.testing.allocator;
    const src =
        \\const tests = b.addTest(.{
        \\    .root_module = b.createModule(.{
        \\        .root_source_file = b.path("src/foo.zig"),
        \\    }),
        \\});
        \\const other = b.addTest(.{
        \\    .root_module = b.createModule(.{
        \\        .root_source_file = b.path("src/bar/baz.zig"),
        \\    }),
        \\});
    ;
    const roots = try orphan_mod.extractBuildZigRoots(allocator, src);
    defer {
        for (roots) |r| allocator.free(r);
        allocator.free(roots);
    }
    try std.testing.expectEqual(@as(usize, 2), roots.len);
    var found_foo = false;
    var found_baz = false;
    for (roots) |r| {
        if (std.mem.eql(u8, r, "src/foo.zig")) found_foo = true;
        if (std.mem.eql(u8, r, "src/bar/baz.zig")) found_baz = true;
    }
    try std.testing.expect(found_foo);
    try std.testing.expect(found_baz);
}
test "orphan: extractImportPaths finds @import file paths" {
    const allocator = std.testing.allocator;
    const src: [:0]const u8 =
        \\const std = @import("std");
        \\const foo = @import("foo.zig");
        \\const bar = @import("../bar/baz.zig");
        \\pub fn main() void {}
    ;
    const paths = try orphan_mod.extractImportPaths(allocator, src);
    defer {
        for (paths) |p| allocator.free(p);
        allocator.free(paths);
    }
    // "std" should be excluded; foo.zig and ../bar/baz.zig should be included.
    var found_foo = false;
    var found_baz = false;
    for (paths) |p| {
        if (std.mem.eql(u8, p, "foo.zig")) found_foo = true;
        if (std.mem.eql(u8, p, "../bar/baz.zig")) found_baz = true;
    }
    try std.testing.expect(found_foo);
    try std.testing.expect(found_baz);
}
test "orphan: findOrphanedFiles detects unimported file" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(workspace);

    // Create a file that imports nothing and is imported by nothing.
    try tmp.dir.writeFile(.{
        .sub_path = "orphaned.zig",
        .data = "pub fn unused() void {}\n",
    });
    // Create a file that is imported by another — not orphaned.
    try tmp.dir.writeFile(.{
        .sub_path = "imported.zig",
        .data = "pub fn helper() void {}\n",
    });
    // The importer references imported.zig.
    try tmp.dir.writeFile(.{
        .sub_path = "main.zig",
        .data = "const h = @import(\"imported.zig\");\npub fn main() void {}\n",
    });

    const orphans = try orphan_mod.findOrphanedFiles(allocator, workspace);
    defer {
        for (orphans) |o| allocator.free(o.source);
        allocator.free(orphans);
    }

    // Only orphaned.zig should be flagged (main.zig has pub fn main, imported.zig is imported).
    try std.testing.expectEqual(@as(usize, 1), orphans.len);
    try std.testing.expectEqualStrings("orphaned.zig", orphans[0].source);
}
test "orphan: findOrphanedFiles skips entry points" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(workspace);

    // Entry point via pub fn main.
    try tmp.dir.writeFile(.{
        .sub_path = "main.zig",
        .data = "pub fn main() void {}\n",
    });
    // Entry point via CODEHEALTH marker.
    try tmp.dir.writeFile(.{
        .sub_path = "special.zig",
        .data = "// CODEHEALTH: entry-point\npub fn entry() void {}\n",
    });
    // Test file — skipped by naming convention.
    try tmp.dir.writeFile(.{
        .sub_path = "foo_test.zig",
        .data = "test \"foo\" {}\n",
    });

    const orphans = try orphan_mod.findOrphanedFiles(allocator, workspace);
    defer {
        for (orphans) |o| allocator.free(o.source);
        allocator.free(orphans);
    }

    try std.testing.expectEqual(@as(usize, 0), orphans.len);
}
