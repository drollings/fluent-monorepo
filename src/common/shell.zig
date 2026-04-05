/// shell.zig — Shared shell command execution helpers
///
/// Provides safe command execution utilities without shell intermediary.
/// Used by sync_engine, provider_discovery, and other modules that need
/// to spawn external processes.
const std = @import("std");

/// Executes a Zig command using the provided allocator and argument list, returning success or error status.
pub fn runCommand(allocator: std.mem.Allocator, argv: []const []const u8) !bool {
    var child = std.process.Child.init(argv, allocator);
    child.stdin_behavior = .Ignore;
    child.stdout_behavior = .Inherit;
    child.stderr_behavior = .Inherit;
    try child.spawn();
    const term = try child.wait();
    return term == .Exited and term.Exited == 0;
}

/// Validates and adds a unique path in a Zig project structure using an allocator and list data.
pub fn addUniquePath(
    allocator: std.mem.Allocator,
    list: *std.ArrayList([]const u8),
    path: []const u8,
    project_root: []const u8,
) !bool {
    for (list.items) |existing| {
        if (std.mem.eql(u8, existing, path)) return false;
    }
    if (project_root.len > 0) {
        const abs = try std.fs.path.join(allocator, &.{ project_root, path });
        defer allocator.free(abs);
        std.fs.accessAbsolute(abs, .{}) catch return false;
    }
    try list.append(allocator, try allocator.dupe(u8, path));
    return true;
}

test "runCommand returns true for successful command" {
    const result = try runCommand(std.testing.allocator, &[_][]const u8{"true"});
    try std.testing.expect(result);
}

test "runCommand returns false for failing command" {
    const result = try runCommand(std.testing.allocator, &[_][]const u8{"false"});
    try std.testing.expect(!result);
}

test "addUniquePath deduplicates paths" {
    var list: std.ArrayList([]const u8) = .{};
    defer {
        for (list.items) |item| std.testing.allocator.free(item);
        list.deinit(std.testing.allocator);
    }

    const added1 = try addUniquePath(std.testing.allocator, &list, "test/path", "");
    try std.testing.expect(added1);

    const added2 = try addUniquePath(std.testing.allocator, &list, "test/path", "");
    try std.testing.expect(!added2);
}
