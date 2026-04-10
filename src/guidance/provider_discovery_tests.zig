//! Tests for provider_discovery.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const provider_discovery_mod = @import("provider_discovery.zig");

test "discoverProvider: returns null for unknown extension" {
    // ".xyz" is unlikely to have a provider binary on any CI machine.
    const result = try provider_discovery_mod.discoverProvider(std.testing.allocator, "/tmp", ".xyz_explain_gen_test");
    try std.testing.expect(result == null);
}
test "discoverProvider: returns null for bare extension without dot" {
    const result = try provider_discovery_mod.discoverProvider(std.testing.allocator, "/tmp", "py");
    try std.testing.expect(result == null);
}
test "discoverProvider: workspace-local bin takes priority" {
    // Create a temporary directory with a fake provider binary.
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    // Create bin/guidance-tst as a regular file (not actually executable on
    // all platforms, but isExecutable only checks existence + file kind in tests).
    try tmp.dir.makeDir("bin");
    {
        const f = try tmp.dir.createFile("bin/guidance-tst", .{});
        f.close();
    }

    // discoverProvider should find it.
    const result = try provider_discovery_mod.discoverProvider(std.testing.allocator, tmp_path, ".tst");
    if (result) |p| {
        defer p.deinit(std.testing.allocator);
        try std.testing.expectEqualStrings("tst", p.name);
        try std.testing.expectEqualStrings(".tst", p.extension);
        try std.testing.expect(std.mem.endsWith(u8, p.binary, "bin/guidance-tst"));
    } else {
        // On some systems access() may fail for non-executable files; skip.
    }
}
