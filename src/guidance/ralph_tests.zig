//! Tests for ralph.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const ralph_mod = @import("ralph.zig");

test "RalphState: transitions in order" {
    // Just test that the enum has the expected values.
    const s: ralph_mod.RalphState = .read;
    try std.testing.expectEqual(ralph_mod.RalphState.read, s);
}
test "QueryRecord: can be appended to ArrayList" {
    const allocator = std.testing.allocator;
    var history: std.ArrayList(ralph_mod.QueryRecord) = .empty;
    defer {
        for (history.items) |r| allocator.free(r.query);
        history.deinit(allocator);
    }

    try history.append(allocator, .{
        .query = try allocator.dupe(u8, "cmdExplain"),
        .intent = .identifier_lookup,
        .result_count = 3,
        .had_synthesis = false,
    });

    try std.testing.expectEqual(@as(usize, 1), history.items.len);
    try std.testing.expectEqualStrings("cmdExplain", history.items[0].query);
}
