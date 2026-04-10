//! Tests for build_validation.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const build_validation_mod = @import("build_validation.zig");

test "build_validation: validateBuildZig detects missing file" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(workspace);

    // Write a build.zig that references a non-existent file.
    try tmp.dir.writeFile(.{
        .sub_path = "build.zig",
        .data =
        \\const tests = b.addTest(.{
        \\    .root_module = b.createModule(.{
        \\        .root_source_file = b.path("src/does_not_exist.zig"),
        \\    }),
        \\});
        ,
    });

    const anomalies = try build_validation_mod.validateBuildZig(allocator, workspace);
    defer {
        for (anomalies) |a| allocator.free(a.referenced_path);
        allocator.free(anomalies);
    }

    try std.testing.expectEqual(@as(usize, 1), anomalies.len);
    try std.testing.expectEqual(build_validation_mod.AnomalyKind.missing_file, anomalies[0].kind);
    try std.testing.expectEqualStrings("src/does_not_exist.zig", anomalies[0].referenced_path);
}
test "build_validation: validateBuildZig no anomaly for existing file" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(workspace);

    // Create the referenced file.
    try tmp.dir.makePath("src");
    try tmp.dir.writeFile(.{ .sub_path = "src/real.zig", .data = "pub fn foo() void {}\n" });

    try tmp.dir.writeFile(.{
        .sub_path = "build.zig",
        .data =
        \\const tests = b.addTest(.{
        \\    .root_module = b.createModule(.{
        \\        .root_source_file = b.path("src/real.zig"),
        \\    }),
        \\});
        ,
    });

    const anomalies = try build_validation_mod.validateBuildZig(allocator, workspace);
    defer {
        for (anomalies) |a| allocator.free(a.referenced_path);
        allocator.free(anomalies);
    }

    try std.testing.expectEqual(@as(usize, 0), anomalies.len);
}
test "build_validation: no build.zig returns empty slice" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(workspace);

    const anomalies = try build_validation_mod.validateBuildZig(allocator, workspace);
    defer allocator.free(anomalies);
    try std.testing.expectEqual(@as(usize, 0), anomalies.len);
}
