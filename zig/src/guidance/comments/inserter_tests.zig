//! Tests for inserter.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const inserter_mod = @import("inserter.zig");

test "insertComment - basic" {
    const allocator = std.testing.allocator;
    const source =
        \\const std = @import("std");
        \\
        \\pub fn foo() void {}
    ;
    const result = try inserter_mod.insertComment(allocator, source, 3, "Does something.");
    defer result.deinit(allocator);

    try std.testing.expect(result.changed);
    try std.testing.expect(std.mem.indexOf(u8, result.new_source, "/// Does something.") != null);
    try std.testing.expectEqual(@as(u32, 3), result.line_adjustments[0].old_line);
    try std.testing.expectEqual(@as(u32, 4), result.line_adjustments[0].new_line);
}
test "insertComment - line beyond source" {
    const allocator = std.testing.allocator;
    const result = try inserter_mod.insertComment(allocator, "x", 99, "comment");
    defer result.deinit(allocator);
    try std.testing.expect(!result.changed);
}
test "replaceComment - existing comment replaced" {
    const allocator = std.testing.allocator;
    const source =
        \\const std = @import("std");
        \\/// Old comment.
        \\pub fn foo() void {}
    ;
    const result = try inserter_mod.replaceComment(allocator, source, 3, "New comment.");
    defer result.deinit(allocator);

    try std.testing.expect(result.changed);
    try std.testing.expect(std.mem.indexOf(u8, result.new_source, "/// New comment.") != null);
    try std.testing.expect(std.mem.indexOf(u8, result.new_source, "Old comment") == null);
}
test "extractCommentAtLine - finds comment" {
    const allocator = std.testing.allocator;
    const source =
        \\/// First line.
        \\/// Second line.
        \\pub fn foo() void {}
    ;
    const comment = try inserter_mod.extractCommentAtLine(allocator, source, 3);
    defer if (comment) |c| allocator.free(c);

    try std.testing.expect(comment != null);
    try std.testing.expectEqualStrings("First line.\nSecond line.", comment.?);
}
test "extractCommentAtLine - no comment" {
    const allocator = std.testing.allocator;
    const source = "pub fn foo() void {}\n";
    const comment = try inserter_mod.extractCommentAtLine(allocator, source, 1);
    defer if (comment) |c| allocator.free(c);
    try std.testing.expect(comment == null);
}
test "formatDocComment - multi-line" {
    const allocator = std.testing.allocator;
    const formatted = try inserter_mod.formatDocComment(allocator, "Line one.\nLine two.");
    defer allocator.free(formatted);
    try std.testing.expectEqualStrings("/// Line one.\n/// Line two.\n", formatted);
}
