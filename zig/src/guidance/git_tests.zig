//! Tests for git.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const git_mod = @import("git.zig");

test "GitignoreFilter always excludes .git" {
    var filter = git_mod.GitignoreFilter.init(std.testing.allocator, "/project");
    defer filter.deinit();

    try std.testing.expect(filter.shouldIgnore("/project/.git/config"));
    try std.testing.expect(filter.shouldIgnore("/project/.zig-cache/foo"));
}
