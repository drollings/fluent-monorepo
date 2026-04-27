//! Tests for shell.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const shell_mod = @import("shell.zig");

test "runCommand returns true for successful command" {
    const result = try shell_mod.runCommand(std.testing.allocator, &[_][]const u8{"true"});
    try std.testing.expect(result);
}
test "runCommand returns false for failing command" {
    const result = try shell_mod.runCommand(std.testing.allocator, &[_][]const u8{"false"});
    try std.testing.expect(!result);
}
test "addUniquePath deduplicates paths" {
    var list: std.ArrayList([]const u8) = .empty;
    defer {
        for (list.items) |item| std.testing.allocator.free(item);
        list.deinit(std.testing.allocator);
    }

    const added1 = try shell_mod.addUniquePath(std.testing.allocator, &list, "test/path", "");
    try std.testing.expect(added1);

    const added2 = try shell_mod.addUniquePath(std.testing.allocator, &list, "test/path", "");
    try std.testing.expect(!added2);
}
