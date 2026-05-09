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

test "build_validation: fixUncoveredTestFiles adds test target" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(workspace);

    // Create the companion source file.
    try tmp.dir.makePath("src/testing");
    try tmp.dir.writeFile(.{ .sub_path = "src/testing/mock_vtable.zig", .data = "pub fn foo() void {}\n" });
    try tmp.dir.writeFile(.{ .sub_path = "src/testing/mock_vtable_tests.zig", .data = "test \"x\" {}\n" });

    // Write a minimal build.zig that references mock_vtable.zig but not mock_vtable_tests.zig.
    try tmp.dir.writeFile(.{
        .sub_path = "build.zig",
        .data =
        \\    const mock_vtable_tests = b.addTest(.{
        \\        .root_module = b.createModule(.{
        \\            .root_source_file = b.path("src/testing/mock_vtable.zig"),
        \\            .target = target,
        \\            .optimize = optimize,
        \\        }),
        \\    });
        \\
        \\    // -------------------------------------------------------------------------
        \\    // 4. Benchmark step (G5)
        \\    // -------------------------------------------------------------------------
        \\
        \\    test_step.dependOn(&b.addRunArtifact(mock_vtable_tests).step);
        ,
    });

    const uncovered = [_][]const u8{"src/testing/mock_vtable_tests.zig"};
    const stats = try build_validation_mod.fixUncoveredTestFiles(allocator, workspace, &uncovered, false);

    try std.testing.expectEqual(@as(usize, 1), stats.added);
    try std.testing.expectEqual(@as(usize, 0), stats.skipped);

    const build_zig_path = try std.fmt.allocPrint(allocator, "{s}/build.zig", .{workspace});
    defer allocator.free(build_zig_path);
    const result = try std.Io.Dir.cwd().readFileAlloc(allocator, build_zig_path, 1024 * 1024);
    defer allocator.free(result);

    try std.testing.expect(std.mem.indexOf(u8, result, "src/testing/mock_vtable_tests.zig") != null);
    try std.testing.expect(std.mem.indexOf(u8, result, "mock_vtable_tests_tests") == null);
}
test "build_validation: fixUncoveredTestFiles skips if companion not in build.zig" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(workspace);

    try tmp.dir.makePath("src");
    try tmp.dir.writeFile(.{ .sub_path = "src/unrelated.zig", .data = "pub fn x() void {}\n" });
    try tmp.dir.writeFile(.{ .sub_path = "src/orphan_tests.zig", .data = "test \"t\" {}\n" });

    // build.zig does NOT reference src/orphan.zig.
    try tmp.dir.writeFile(.{
        .sub_path = "build.zig",
        .data =
        \\    test_step.dependOn(&b.addRunArtifact(unrelated_tests).step);
        ,
    });

    const uncovered = [_][]const u8{"src/orphan_tests.zig"};
    const stats = try build_validation_mod.fixUncoveredTestFiles(allocator, workspace, &uncovered, false);

    try std.testing.expectEqual(@as(usize, 0), stats.added);
    try std.testing.expectEqual(@as(usize, 1), stats.skipped);
}

test "build_validation: fixUncoveredTestFiles adds test target" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(workspace);

    try tmp.dir.makePath("src/testing");
    try tmp.dir.writeFile(.{ .sub_path = "src/testing/mock_vtable.zig", .data = "pub fn foo() void {}\n" });
    try tmp.dir.writeFile(.{ .sub_path = "src/testing/mock_vtable_tests.zig", .data = "test \"x\" {}\n" });

    // Fixture mirrors real build.zig: addTest declaration first, then test_step.dependOn.
    try tmp.dir.writeFile(.{
        .sub_path = "build.zig",
        .data =
        \\    const mock_vtable_tests = b.addTest(.{
        \\        .root_module = b.createModule(.{
        \\            .root_source_file = b.path("src/testing/mock_vtable.zig"),
        \\            .target = target,
        \\            .optimize = optimize,
        \\        }),
        \\    });
        \\
        \\    test_step.dependOn(&b.addRunArtifact(mock_vtable_tests).step);
        ,
    });

    const uncovered = [_][]const u8{"src/testing/mock_vtable_tests.zig"};
    const stats = try build_validation_mod.fixUncoveredTestFiles(allocator, workspace, &uncovered, false);

    try std.testing.expectEqual(@as(usize, 1), stats.added);
    try std.testing.expectEqual(@as(usize, 0), stats.skipped);

    const build_zig_path = try std.fmt.allocPrint(allocator, "{s}/build.zig", .{workspace});
    defer allocator.free(build_zig_path);
    const result = try std.Io.Dir.cwd().readFileAlloc(allocator, build_zig_path, 1024 * 1024);
    defer allocator.free(result);

    try std.testing.expect(std.mem.indexOf(u8, result, "src/testing/mock_vtable_tests.zig") != null);
    // Variable name must not double the _tests suffix.
    try std.testing.expect(std.mem.indexOf(u8, result, "mock_vtable_tests_tests") == null);
}
test "build_validation: fixUncoveredTestFiles skips if companion not in build.zig" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(workspace);

    try tmp.dir.makePath("src");
    try tmp.dir.writeFile(.{ .sub_path = "src/orphan_tests.zig", .data = "test \"t\" {}\n" });
    // build.zig does NOT reference src/orphan.zig.
    try tmp.dir.writeFile(.{
        .sub_path = "build.zig",
        .data = "    test_step.dependOn(&b.addRunArtifact(unrelated_tests).step);\n",
    });

    const uncovered = [_][]const u8{"src/orphan_tests.zig"};
    const stats = try build_validation_mod.fixUncoveredTestFiles(allocator, workspace, &uncovered, false);

    try std.testing.expectEqual(@as(usize, 0), stats.added);
    try std.testing.expectEqual(@as(usize, 1), stats.skipped);
}
