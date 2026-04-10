//! Tests for todo.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const todo_mod = @import("todo.zig");

test "queryChecklistStatus: nonexistent dir returns zeros" {
    const t = std.testing;
    const result = try todo_mod.queryChecklistStatus(t.allocator, "/nonexistent/todo");
    defer if (result.item_dir) |d| t.allocator.free(d);
    try t.expectEqual(@as(usize, 0), result.total);
    try t.expectEqual(@as(usize, 0), result.incomplete);
}
