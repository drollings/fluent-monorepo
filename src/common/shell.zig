/// shell.zig — Shared shell command execution helpers
///
/// Provides safe command execution utilities without shell intermediary.
/// Used by sync_engine, provider_discovery, and other modules that need
/// to spawn external processes.
const std = @import("std");

pub fn runCommand(allocator: std.mem.Allocator, argv: []const []const u8) !bool {
    const io = std.Io.Threaded.global_single_threaded.io();
    const result = try std.process.run(allocator, io, .{ .argv = argv });
    defer {
        allocator.free(result.stdout);
        allocator.free(result.stderr);
    }
    switch (result.term) {
        .exited => |code| return code == 0,
        else => return false,
    }
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
        std.Io.Dir.accessAbsolute(std.Io.Threaded.global_single_threaded.io(), abs, .{}) catch return false;
    }
    try list.append(allocator, try allocator.dupe(u8, path));
    return true;
}