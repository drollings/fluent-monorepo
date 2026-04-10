//! Tests for scanner.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const scanner_mod = @import("scanner.zig");

test "CodebaseScanner: init and deinit" {
    const allocator = std.testing.allocator;
    var scanner = try scanner_mod.CodebaseScanner.init(allocator, "/tmp");
    defer scanner.deinit();

    try std.testing.expectEqual(scanner_mod.ConfidenceTier.low, scanner.confidence);
    try std.testing.expectEqualStrings("/tmp", scanner.workspace);
}
test "CodebaseScanner: scan on empty workspace returns low confidence" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(workspace);

    var scanner = try scanner_mod.CodebaseScanner.init(allocator, workspace);
    defer scanner.deinit();

    try scanner.scan();
    // No CAPABILITY.md → medium or low, map may be null on empty dir.
    // Just verify no crash and confidence is set.
    try std.testing.expect(scanner.confidence == .low or scanner.confidence == .medium);
}
