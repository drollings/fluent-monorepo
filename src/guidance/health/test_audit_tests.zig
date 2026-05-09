//! Tests for test_audit.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const test_audit_mod = @import("test_audit.zig");

test "test_audit: hasNonTestDecl accepts pure test file" {
    const allocator = std.testing.allocator;
    const src: [:0]const u8 =
        \\const std = @import("std");
        \\
        \\test "foo does bar" {
        \\    try std.testing.expect(true);
        \\}
        \\
        \\test "baz" {
        \\    _ = 42;
        \\}
    ;
    var name: ?[]const u8 = null;
    var line: ?u32 = null;
    const bad = try test_audit_mod.hasNonTestDecl(allocator, src, &name, &line);
    try std.testing.expect(!bad);
    try std.testing.expect(name == null);
}
test "test_audit: hasNonTestDecl detects pub fn in test file" {
    const allocator = std.testing.allocator;
    const src: [:0]const u8 =
        \\const std = @import("std");
        \\
        \\pub fn helper() void {}
        \\
        \\test "foo" {
        \\    try std.testing.expect(true);
        \\}
    ;
    var name: ?[]const u8 = null;
    var line: ?u32 = null;
    const bad = try test_audit_mod.hasNonTestDecl(allocator, src, &name, &line);
    defer if (name) |n| allocator.free(n);
    try std.testing.expect(bad);
    try std.testing.expectEqualStrings("helper", name.?);
}
test "test_audit: hasNonTestDecl allows comptime block" {
    const allocator = std.testing.allocator;
    const src: [:0]const u8 =
        \\const std = @import("std");
        \\comptime {
        \\    _ = @import("../foo.zig");
        \\}
        \\test "inline" {
        \\    try std.testing.expect(true);
        \\}
    ;
    var name: ?[]const u8 = null;
    var line: ?u32 = null;
    const bad = try test_audit_mod.hasNonTestDecl(allocator, src, &name, &line);
    try std.testing.expect(!bad);
}
test "test_audit: auditTestFiles detects non-test decl" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(workspace);

    // Write a *_tests.zig with a non-test declaration.
    try tmp.dir.writeFile(.{
        .sub_path = "foo_tests.zig",
        .data =
        \\const std = @import("std");
        \\pub fn helper() void {}
        \\test "t" { try std.testing.expect(true); }
        ,
    });

    const anomalies = try test_audit_mod.auditTestFiles(allocator, workspace);
    defer {
        for (anomalies) |a| {
            allocator.free(a.source);
            if (a.decl_name) |n| allocator.free(n);
        }
        allocator.free(anomalies);
    }

    // Expect at least the non_test_decl_in_test_file anomaly.
    var found_non_test = false;
    for (anomalies) |a| {
        if (a.kind == .non_test_decl_in_test_file) found_non_test = true;
    }
    try std.testing.expect(found_non_test);
}
test "test_audit: auditTestFiles clean test file has no non-test-decl anomaly" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(workspace);

    try tmp.dir.writeFile(.{
        .sub_path = "pure_tests.zig",
        .data =
        \\const std = @import("std");
        \\test "pure" { try std.testing.expect(true); }
        ,
    });

    const anomalies = try test_audit_mod.auditTestFiles(allocator, workspace);
    defer {
        for (anomalies) |a| {
            allocator.free(a.source);
            if (a.decl_name) |n| allocator.free(n);
        }
        allocator.free(anomalies);
    }

    for (anomalies) |a| {
        try std.testing.expect(a.kind != .non_test_decl_in_test_file);
    }
}
